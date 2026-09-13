//! Resume where you left off.
//!
//! mpv runs a small Lua script that writes the playback position to a state
//! file every few seconds and when it exits. torflix folds those into a
//! position store and passes `--start` (mpv) or `--start-time` (VLC) the next
//! time the same file plays. VLC can't run the script, so positions are only
//! recorded while watching in mpv. The tracker design is adapted from
//! MovieBox-Tui (MIT, see THIRD_PARTY_NOTICES.md).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 500;
/// Positions this early aren't worth resuming from.
const MIN_RESUME_SECS: u64 = 30;
/// Resume slightly before where you stopped, for context.
const REWIND_SECS: u64 = 5;
/// A state file mpv never wrote into (player failed to start, say) is dropped after this.
const STALE_SEED_SECS: u64 = 2 * 86_400;

const TRACKER_LUA: &str = r#"-- torflix playback tracker: records how far you got so torflix can resume.
local options = require 'mp.options'
local opts = { state_file = "" }
options.read_options(opts, "torflix")

local function read_key()
    local f = io.open(opts.state_file, "r")
    if not f then return nil end
    local content = f:read("*a")
    f:close()
    return content and content:match('"key"%s*:%s*"([^"]*)"')
end

-- Read the key now: torflix may consume the state file while we're still playing.
local key = opts.state_file ~= "" and read_key() or nil
local pos, dur, completed = 0, 0, false

mp.observe_property("time-pos", "number", function(_, v) if v then pos = v end end)
mp.observe_property("duration", "number", function(_, v) if v then dur = v end end)

local function write_state(finished)
    if not key or (pos <= 0 and dur <= 0) then return end
    if finished or (dur > 0 and pos >= 0.9 * dur) then completed = true end
    local json = string.format(
        '{"key":"%s","seconds":%d,"duration":%s,"completed":%s,"timestamp":%d}',
        key,
        math.floor(pos + 0.5),
        dur > 0 and string.format("%d", math.floor(dur + 0.5)) or "null",
        completed and "true" or "false",
        os.time())
    local tmp = opts.state_file .. ".tmp"
    local f = io.open(tmp, "w")
    if not f then return end
    f:write(json)
    f:close()
    os.remove(opts.state_file)
    os.rename(tmp, opts.state_file)
end

mp.register_event("end-file", function(e) if e and e.reason == "eof" then write_state(true) end end)
mp.register_event("shutdown", function() write_state(false) end)
mp.add_periodic_timer(5, function() write_state(false) end)
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub seconds: u64,
    #[serde(default)]
    pub duration: Option<u64>,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub updated_at: u64,
}

impl Position {
    /// Where to resume from, or `None` if it had barely started or is effectively finished.
    pub fn resume_at(&self) -> Option<u64> {
        if self.completed || self.seconds < MIN_RESUME_SECS {
            return None;
        }
        if let Some(d) = self.duration {
            if d > 0 && self.seconds * 10 >= d * 9 {
                return None;
            }
        }
        Some(self.seconds.saturating_sub(REWIND_SECS))
    }

    pub fn percent(&self) -> Option<u64> {
        self.duration
            .filter(|d| *d > 0)
            .map(|d| self.seconds.min(d) * 100 / d)
    }

    /// "42m left", "1h 5m left".
    pub fn remaining_label(&self) -> Option<String> {
        let left = self.duration?.checked_sub(self.seconds)?;
        Some(match left {
            0..=59 => "<1m left".to_string(),
            60..=3_599 => format!("{}m left", left / 60),
            _ => match (left / 3_600, (left % 3_600) / 60) {
                (h, 0) => format!("{h}h left"),
                (h, m) => format!("{h}h {m}m left"),
            },
        })
    }
}

/// What the Lua tracker writes.
#[derive(Debug, Deserialize)]
struct StateFile {
    key: String,
    seconds: u64,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    completed: bool,
    #[serde(default)]
    timestamp: u64,
}

/// Identifies one file inside a torrent across sessions: info hash plus file index.
pub fn torrent_key(target: &str, file_idx: usize) -> String {
    match info_hash_of(target) {
        Some(hash) => format!("btih:{hash}:{file_idx}"),
        None => format!("src:{}:{file_idx}", sanitize(target)),
    }
}

pub fn url_key(url: &str) -> String {
    format!("url:{}", sanitize(url))
}

pub fn info_hash_of(target: &str) -> Option<String> {
    let lower = target.to_ascii_lowercase();
    let start = lower.find("xt=urn:btih:")? + "xt=urn:btih:".len();
    let hash: String = lower[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    (hash.len() == 40 || hash.len() == 32).then_some(hash)
}

/// Keys go into a JSON seed the Lua script pattern-matches, so no quotes or backslashes.
fn sanitize(s: &str) -> String {
    s.chars()
        .take(300)
        .map(|c| if c == '"' || c == '\\' || c.is_control() { '_' } else { c })
        .collect()
}

pub fn position(key: &str) -> Option<Position> {
    let dir = crate::config::data_dir();
    reconcile_in(&dir);
    load_store(&dir).get(key).copied()
}

pub fn resume_at(key: &str) -> Option<u64> {
    position(key).and_then(|p| p.resume_at())
}

/// mpv arguments that load the tracker for `key`; empty if the script can't be set up.
pub fn mpv_tracking_args(key: &str) -> Vec<String> {
    mpv_tracking_args_in(&crate::config::data_dir(), key)
}

fn store_path(dir: &Path) -> PathBuf {
    dir.join("progress.json")
}

fn state_dir(dir: &Path) -> PathBuf {
    dir.join("playback")
}

fn script_path(dir: &Path) -> PathBuf {
    dir.join("scripts").join("torflix_tracker.lua")
}

fn load_store(dir: &Path) -> BTreeMap<String, Position> {
    crate::config::load_json(&store_path(dir))
}

/// Fold finished or in-progress state files into the store. Returns whether it changed.
fn reconcile_in(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(state_dir(dir)) else {
        return false;
    };
    let mut store = load_store(dir);
    let mut changed = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let state = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<StateFile>(&b).ok());
        match state {
            Some(s) => {
                let newer = store.get(&s.key).map_or(true, |p| s.timestamp >= p.updated_at);
                if newer {
                    store.insert(
                        s.key,
                        Position {
                            seconds: s.seconds,
                            duration: s.duration,
                            completed: s.completed,
                            updated_at: s.timestamp,
                        },
                    );
                    changed = true;
                }
                // mpv rewrites it on its next tick if it's still playing.
                std::fs::remove_file(&path).ok();
            }
            // Just the seed: mpv hasn't recorded anything yet. Keep it unless it's old.
            None => {
                if is_stale(&path) {
                    std::fs::remove_file(&path).ok();
                }
            }
        }
    }
    if changed {
        trim(&mut store);
        crate::config::write_json(&store_path(dir), &store);
    }
    changed
}

fn trim(store: &mut BTreeMap<String, Position>) {
    while store.len() > MAX_ENTRIES {
        let Some(oldest) = store
            .iter()
            .min_by_key(|(_, p)| p.updated_at)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        store.remove(&oldest);
    }
}

fn mpv_tracking_args_in(dir: &Path, key: &str) -> Vec<String> {
    let script = script_path(dir);
    if !write_if_changed(&script, TRACKER_LUA) {
        return Vec::new();
    }
    let states = state_dir(dir);
    if std::fs::create_dir_all(&states).is_err() {
        return Vec::new();
    }
    let key = sanitize(key);
    let state = states.join(format!("{}.json", file_stem_for(&key)));
    let seed = serde_json::json!({ "key": key }).to_string();
    if std::fs::write(&state, seed).is_err() {
        return Vec::new();
    }
    vec![
        format!("--script={}", player_path(&script)),
        format!("--script-opts=torflix-state_file={}", player_path(&state)),
    ]
}

fn file_stem_for(key: &str) -> String {
    let readable: String = key.chars().filter(|c| c.is_ascii_alphanumeric()).take(48).collect();
    format!("{readable}-{:016x}", crate::config::stable_hash(key))
}

fn write_if_changed(path: &Path, content: &str) -> bool {
    if std::fs::read_to_string(path).map_or(false, |c| c == content) {
        return true;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    std::fs::write(path, content).is_ok()
}

fn is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map_or(false, |age| age.as_secs() > STALE_SEED_SECS)
}

/// mpv's option parser treats backslashes specially; forward slashes work on Windows too.
fn player_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("torflix-progress-test-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_state(dir: &Path, file: &str, json: &str) -> PathBuf {
        let p = state_dir(dir).join(file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, json).unwrap();
        p
    }

    #[test]
    fn info_hash_extraction() {
        let m = "magnet:?xt=urn:btih:F0FD1CAD0E24B69FF6AAADB9DF40C50286327188&dn=x&tr=udp://t";
        assert_eq!(info_hash_of(m).as_deref(), Some("f0fd1cad0e24b69ff6aaadb9df40c50286327188"));
        assert_eq!(info_hash_of("magnet:?xt=urn:btih:ABCDEFGHIJKLMNOPQRSTUVWXYZ234567").as_deref(), Some("abcdefghijklmnopqrstuvwxyz234567"));
        assert_eq!(info_hash_of("https://example.com/file.torrent"), None);
        assert_eq!(torrent_key(m, 3), "btih:f0fd1cad0e24b69ff6aaadb9df40c50286327188:3");
        assert_eq!(torrent_key("/tmp/a \"b\".torrent", 0), "src:/tmp/a _b_.torrent:0");
    }

    #[test]
    fn resume_thresholds() {
        let p = |seconds, duration, completed| Position { seconds, duration, completed, updated_at: 0 };
        assert_eq!(p(10, Some(3000), false).resume_at(), None, "barely started");
        assert_eq!(p(600, Some(3000), false).resume_at(), Some(595));
        assert_eq!(p(2800, Some(3000), false).resume_at(), None, "past 90%");
        assert_eq!(p(600, Some(3000), true).resume_at(), None, "completed");
        assert_eq!(p(600, None, false).resume_at(), Some(595), "live/unknown length");
        assert_eq!(p(600, Some(3000), false).percent(), Some(20));
        assert_eq!(p(600, Some(3000), false).remaining_label().as_deref(), Some("40m left"));
        assert_eq!(p(0, Some(3900), false).remaining_label().as_deref(), Some("1h 5m left"));
    }

    #[test]
    fn reconcile_merges_newer_and_keeps_seeds() {
        let dir = scratch("reconcile");
        let key = "btih:aaaa:0";
        let played = write_state(&dir, "a.json", r#"{"key":"btih:aaaa:0","seconds":600,"duration":3000,"completed":false,"timestamp":200}"#);
        let seed = write_state(&dir, "b.json", r#"{"key":"btih:bbbb:0"}"#);
        let tmp = write_state(&dir, "a.json.tmp", "partial");

        assert!(reconcile_in(&dir));
        let store = load_store(&dir);
        assert_eq!(store[key].seconds, 600);
        assert!(!played.exists(), "consumed");
        assert!(seed.exists(), "seed left for mpv");
        assert!(tmp.exists(), "not a .json file, untouched");

        // An older write (e.g. a stale file from a previous session) doesn't clobber it.
        write_state(&dir, "c.json", r#"{"key":"btih:aaaa:0","seconds":5,"completed":false,"timestamp":100}"#);
        assert!(!reconcile_in(&dir));
        assert_eq!(load_store(&dir)[key].seconds, 600);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tracking_args_write_script_and_seed() {
        let dir = scratch("args");
        let args = mpv_tracking_args_in(&dir, "btih:abc:2");
        assert_eq!(args.len(), 2);
        assert!(args[0].starts_with("--script=") && args[0].ends_with("torflix_tracker.lua"));
        let state = args[1].strip_prefix("--script-opts=torflix-state_file=").unwrap();
        let seed = std::fs::read_to_string(state).unwrap();
        assert_eq!(seed, r#"{"key":"btih:abc:2"}"#);
        assert_eq!(std::fs::read_to_string(script_path(&dir)).unwrap(), TRACKER_LUA);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_is_capped_by_age() {
        let mut store = BTreeMap::new();
        for i in 0..(MAX_ENTRIES as u64 + 5) {
            store.insert(format!("k{i}"), Position { seconds: 60, duration: None, completed: false, updated_at: i });
        }
        trim(&mut store);
        assert_eq!(store.len(), MAX_ENTRIES);
        assert!(!store.contains_key("k0") && store.contains_key(&format!("k{}", MAX_ENTRIES + 4)));
    }

    /// Runs the real Lua script in mpv against a 3-second generated video.
    #[test]
    #[ignore = "needs mpv on PATH"]
    fn live_mpv_tracker_records_position() {
        let dir = scratch("live");
        let key = "btih:0123456789abcdef0123456789abcdef01234567:0";
        let args = mpv_tracking_args_in(&dir, key);
        let status = std::process::Command::new("mpv")
            .args(["--no-config", "--vo=null", "--ao=null", "--really-quiet"])
            .args(&args)
            .arg("av://lavfi:testsrc=duration=3:size=64x64:rate=10")
            .status()
            .expect("run mpv");
        assert!(status.success());
        assert!(reconcile_in(&dir), "tracker wrote a state file");
        let p = load_store(&dir)[key];
        assert!(p.completed, "played to the end: {p:?}");
        assert!(p.seconds >= 2, "{p:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
