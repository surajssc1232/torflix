//! Loading progress for the status bar, from pressing Enter to the first frame.
//!
//! Pressing play used to leave a line of status text that said "resolving
//! metadata…" and then "streaming", with no telling whether anything was
//! moving or when the movie would actually be up. A playback now owns a slot
//! the status bar reads from: the stage it has reached, the torrent feeding it,
//! and — from mpv, over its IPC socket — whether the first frame is up yet,
//! then the position and length.

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long to wait for mpv to open its IPC socket.
pub const PLAYER_IPC_WAIT: Duration = Duration::from_secs(10);

/// How far a playback has got on the way to the first frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Handing the torrent to the engine.
    Adding,
    /// Waiting for peers to send the torrent's file list.
    Metadata,
    /// Waiting on subtitles, for a player that needs them up front.
    Subtitles,
    /// The player is up and buffering the stream.
    Starting,
    Playing,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Adding => "adding torrent",
            Stage::Metadata => "finding peers",
            Stage::Subtitles => "fetching subtitles",
            Stage::Starting => "buffering",
            Stage::Playing => "playing",
        }
    }
}

/// What the player has told us. Everything is `None` until it says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Player {
    /// Seconds in.
    pub position: Option<f64>,
    /// Seconds long. Live streams have none.
    pub duration: Option<f64>,
    pub paused: bool,
    /// Stopped because the data isn't there yet.
    pub stalled: bool,
    /// How full the buffer is, 0–100, while it fills.
    pub buffer_pct: Option<u8>,
}

impl Player {
    /// Take one `property-change` from mpv.
    fn apply(&mut self, name: &str, data: &Value) {
        match name {
            "time-pos" => self.position = data.as_f64(),
            "duration" => self.duration = data.as_f64().filter(|d| *d > 0.0),
            "pause" => self.paused = data.as_bool().unwrap_or(false),
            "paused-for-cache" => self.stalled = data.as_bool().unwrap_or(false),
            "cache-buffering-state" => {
                self.buffer_pct = data.as_u64().map(|p| p.min(100) as u8)
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub title: String,
    pub stage: Stage,
    /// The torrent being streamed, when it is one: its stats come from the poller.
    pub torrent: Option<u64>,
    pub player: Player,
    /// When the current stage began.
    pub since: Instant,
    generation: u64,
}

impl NowPlaying {
    /// How far from Enter to the first frame, 0–100. Only mpv's buffer can be
    /// measured; the steps before it creep along so a slow one still moves,
    /// each within its own stretch so the number never goes backwards.
    /// Finding peers gets the biggest stretch: it's most of the wait.
    pub fn percent(&self) -> u8 {
        let t = self.since.elapsed().as_secs_f64();
        let ease = |tau: f64| 1.0 - (-t / tau).exp();
        let p = match self.stage {
            Stage::Adding => 2.0,
            Stage::Metadata => 5.0 + 55.0 * ease(8.0),
            Stage::Subtitles => 60.0 + 5.0 * ease(2.0),
            Stage::Starting => {
                let filled = self.player.buffer_pct.map(|p| p as f64 / 100.0).unwrap_or(0.0);
                65.0 + 34.0 * ease(4.0).max(filled)
            }
            Stage::Playing => 100.0,
        };
        p.floor().min(100.0) as u8
    }
}

#[derive(Default)]
struct Inner {
    current: Option<NowPlaying>,
    generation: u64,
}

/// The one playback the status bar shows: the most recently started.
#[derive(Clone, Default)]
pub struct Slot(Arc<Mutex<Inner>>);

impl Slot {
    /// Start showing a playback. It shows until the returned value is dropped,
    /// or until another playback starts.
    pub fn begin(&self, title: &str, stage: Stage) -> Playback {
        let mut inner = self.0.lock().unwrap();
        inner.generation += 1;
        let generation = inner.generation;
        inner.current = Some(NowPlaying {
            title: title.to_string(),
            stage,
            torrent: None,
            player: Player::default(),
            since: Instant::now(),
            generation,
        });
        Playback(Updater { slot: self.clone(), generation })
    }

    pub fn snapshot(&self) -> Option<NowPlaying> {
        self.0.lock().unwrap().current.clone()
    }
}

/// Changes one playback's entry, and does nothing once a newer playback has
/// taken the slot. Cheap to clone into the thread that watches the player.
#[derive(Clone)]
struct Updater {
    slot: Slot,
    generation: u64,
}

impl Updater {
    fn with(&self, f: impl FnOnce(&mut NowPlaying)) {
        let mut inner = self.slot.0.lock().unwrap();
        if let Some(np) = inner.current.as_mut().filter(|np| np.generation == self.generation) {
            f(np);
        }
    }

    fn stage(&self, stage: Stage) {
        self.with(|np| {
            if np.stage != stage {
                np.stage = stage;
                np.since = Instant::now();
            }
        });
    }
}

/// A playback in the status bar. Dropping it takes it off, so every way out of
/// a playback — the player quitting, the add failing, metadata timing out —
/// clears it without each having to remember to.
pub struct Playback(Updater);

impl Playback {
    pub fn stage(&self, stage: Stage) {
        self.0.stage(stage);
    }

    pub fn torrent(&self, id: u64) {
        self.0.with(|np| np.torrent = Some(id));
    }

    /// The player has started. With an IPC socket we can tell when the first
    /// frame is up; without one, the most we know is that it's going.
    pub fn follow(&self, ipc: Option<PathBuf>) {
        match ipc {
            Some(sock) => {
                // Before the watcher starts, so it can't be overtaken by this.
                self.stage(Stage::Starting);
                watch_mpv(sock, self.0.clone());
            }
            None => self.stage(Stage::Playing),
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        let mut inner = self.0.slot.0.lock().unwrap();
        if inner.current.as_ref().is_some_and(|np| np.generation == self.0.generation) {
            inner.current = None;
        }
    }
}

/// The mpv properties the status bar draws from.
const WATCHED: &[&str] = &["time-pos", "duration", "pause", "paused-for-cache", "cache-buffering-state"];

/// Follow mpv over its IPC socket until it quits.
fn watch_mpv(sock: PathBuf, up: Updater) {
    thread::spawn(move || {
        let Some(stream) = connect_to_player(&sock) else {
            // Can't hear from it, so take its word that it started.
            up.stage(Stage::Playing);
            return;
        };
        let Ok(mut writer) = stream.try_clone() else { return };
        for (i, name) in WATCHED.iter().enumerate() {
            let cmd = serde_json::json!({ "command": ["observe_property", i + 1, name] });
            if writeln!(writer, "{cmd}").is_err() {
                return;
            }
        }
        let _ = writer.flush();
        // The socket closes when mpv does, which ends this.
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg.get("event").and_then(Value::as_str) != Some("property-change") {
                continue;
            }
            let Some(name) = msg.get("name").and_then(Value::as_str) else { continue };
            let data = msg.get("data").cloned().unwrap_or(Value::Null);
            up.with(|np| {
                np.player.apply(name, &data);
                // The first position means the first frame is up.
                if np.stage < Stage::Playing && np.player.position.is_some() {
                    np.stage = Stage::Playing;
                    np.since = Instant::now();
                }
            });
        }
    });
}

/// Wait for mpv to create its IPC socket, then open it.
#[cfg(unix)]
pub fn connect_to_player(sock: &std::path::Path) -> Option<std::os::unix::net::UnixStream> {
    let deadline = Instant::now() + PLAYER_IPC_WAIT;
    while Instant::now() < deadline {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(sock) {
            return Some(stream);
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

#[cfg(not(unix))]
pub fn connect_to_player(_sock: &std::path::Path) -> Option<std::fs::File> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn properties_from_mpv() {
        let mut p = Player::default();
        p.apply("time-pos", &json!(12.5));
        p.apply("duration", &json!(100.0));
        p.apply("pause", &json!(true));
        p.apply("cache-buffering-state", &json!(140));
        assert_eq!(p.position, Some(12.5));
        assert!(p.paused);
        assert_eq!(p.buffer_pct, Some(100));
        // A property going away (a new file loading) clears it.
        p.apply("time-pos", &Value::Null);
        assert_eq!(p.position, None);
        // A zero length is mpv not knowing yet.
        p.apply("duration", &json!(0));
        assert_eq!(p.duration, None);
    }

    #[test]
    fn percent_climbs_through_the_stages_and_ends_at_100() {
        let slot = Slot::default();
        let pb = slot.begin("x", Stage::Adding);
        let mut last = 0;
        for stage in [Stage::Adding, Stage::Metadata, Stage::Subtitles, Stage::Starting, Stage::Playing] {
            pb.stage(stage);
            let mut np = slot.snapshot().unwrap();
            let start = np.percent();
            assert!(start >= last, "{stage:?} went back from {last} to {start}");
            // Long into a stage, still short of the next one's start.
            np.since -= Duration::from_secs(600);
            np.player.buffer_pct = Some(100);
            last = np.percent();
        }
        assert_eq!(last, 100);
        // Only the first frame says 100.
        let mut np = slot.snapshot().unwrap();
        np.stage = Stage::Starting;
        np.since -= Duration::from_secs(600);
        assert!(np.percent() < 100);
    }

    #[test]
    fn dropping_a_playback_clears_it() {
        let slot = Slot::default();
        let pb = slot.begin("Inception (2010)", Stage::Adding);
        pb.stage(Stage::Metadata);
        assert_eq!(slot.snapshot().unwrap().stage, Stage::Metadata);
        drop(pb);
        assert!(slot.snapshot().is_none());
    }

    #[test]
    fn an_older_playback_leaves_a_newer_one_alone() {
        let slot = Slot::default();
        let old = slot.begin("first", Stage::Playing);
        let new = slot.begin("second", Stage::Adding);
        old.stage(Stage::Starting);
        drop(old);
        let shown = slot.snapshot().unwrap();
        assert_eq!(shown.title, "second");
        assert_eq!(shown.stage, Stage::Adding);
        drop(new);
        assert!(slot.snapshot().is_none());
    }

    #[test]
    fn following_without_ipc_counts_as_playing() {
        let slot = Slot::default();
        let pb = slot.begin("vlc", Stage::Adding);
        pb.follow(None);
        assert_eq!(slot.snapshot().unwrap().stage, Stage::Playing);
    }
}
