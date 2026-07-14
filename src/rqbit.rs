//! Thin client for the rqbit HTTP API (default: http://127.0.0.1:3030).
//! Response shapes verified against rqbit 8.1.1.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::{Child, Command, Stdio};

pub const DEFAULT_API: &str = "http://127.0.0.1:3030";

#[derive(Clone)]
pub struct Client {
    base: String,
}

// ---------- API response types ----------

#[derive(Debug, Deserialize)]
struct TorrentList {
    torrents: Vec<TorrentListItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TorrentListItem {
    pub id: u64,
    #[serde(default)]
    pub info_hash: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileDetails {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub length: u64,
    #[serde(default = "yes")]
    pub included: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct TorrentDetails {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub files: Vec<FileDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Speed {
    #[serde(default)]
    pub mbps: f64,
    #[serde(default)]
    pub human_readable: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PeerStats {
    #[serde(default)]
    pub live: u64,
    #[serde(default)]
    pub seen: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub peer_stats: PeerStats,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LiveStats {
    #[serde(default)]
    pub snapshot: Snapshot,
    #[serde(default)]
    pub download_speed: Speed,
    #[serde(default)]
    pub upload_speed: Speed,
    /// rqbit returns an object with a `human_readable` field (or null).
    #[serde(default)]
    pub time_remaining: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TorrentStats {
    #[serde(default)]
    pub state: String, // "initializing" | "live" | "paused" | "error"
    #[serde(default)]
    pub progress_bytes: u64,
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub live: Option<LiveStats>,
}

impl TorrentStats {
    pub fn progress_pct(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.progress_bytes as f64 / self.total_bytes as f64 * 100.0
        }
    }

    pub fn eta(&self) -> Option<String> {
        let v = self.live.as_ref()?.time_remaining.as_ref()?;
        Some(
            v.get("human_readable")
                .and_then(|h| h.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string()),
        )
    }

    pub fn peers(&self) -> u64 {
        self.live
            .as_ref()
            .map(|l| l.snapshot.peer_stats.live)
            .unwrap_or(0)
    }

    pub fn down_speed(&self) -> String {
        self.live
            .as_ref()
            .map(|l| l.download_speed.human_readable.clone())
            .unwrap_or_else(|| "-".into())
    }
}

// ---------- Client ----------

impl Client {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
        }
    }


    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base, path);
        let resp = minreq::get(&url)
            .with_timeout(10)
            .send()
            .context("could not reach the rqbit engine")?;
        if resp.status_code >= 400 {
            return Err(anyhow!(
                "HTTP {}: {}",
                resp.status_code,
                snip(resp.as_str().unwrap_or(""), 160)
            ));
        }
        Ok(resp.json::<T>()?)
    }

    pub fn is_up(&self) -> bool {
        minreq::get(&self.base)
            .with_timeout(2)
            .send()
            .map(|r| r.status_code < 500)
            .unwrap_or(false)
    }

    pub fn list(&self) -> Result<Vec<TorrentListItem>> {
        let resp: TorrentList = self.get_json("/torrents")?;
        Ok(resp.torrents)
    }

    pub fn details(&self, id: u64) -> Result<TorrentDetails> {
        self.get_json(&format!("/torrents/{}", id))
    }

    pub fn stats(&self, id: u64) -> Result<TorrentStats> {
        self.get_json(&format!("/torrents/{}/stats/v1", id))
    }

    /// Add a magnet link, an http(s) URL to a .torrent, or a local .torrent path.
    /// Returns the assigned torrent ID.
    pub fn add(&self, magnet_url_or_path: &str) -> Result<u64> {
        self.add_inner(magnet_url_or_path, None)
    }

    /// Like `add`, but tells rqbit to download pieces into `output_folder`
    /// instead of the engine's default download directory.
    pub fn add_to_dir(&self, magnet_url_or_path: &str, output_folder: &Path) -> Result<u64> {
        self.add_inner(magnet_url_or_path, Some(output_folder))
    }

    fn add_inner(&self, magnet_url_or_path: &str, output_folder: Option<&Path>) -> Result<u64> {
        let mut url = format!("{}/torrents?overwrite=true", self.base);
        if let Some(folder) = output_folder {
            url.push_str(&format!("&output_folder={}", folder.to_string_lossy()));
        }
        let input = magnet_url_or_path.trim();
        let req = if Path::new(input).is_file() {
            let bytes = std::fs::read(input)?;
            minreq::post(&url).with_body(bytes)
        } else {
            minreq::post(&url).with_body(input)
        };
        let resp = req
            .with_timeout(120) // magnet resolution can take a while
            .send()
            .context("could not reach the rqbit engine")?;
        if resp.status_code >= 400 {
            return Err(anyhow!(
                "rqbit returned HTTP {}: {}",
                resp.status_code,
                snip(resp.as_str().unwrap_or(""), 180)
            ));
        }
        #[derive(serde::Deserialize)]
        struct AddResponse { id: u64 }
        let r: AddResponse = resp.json()?;
        Ok(r.id)
    }

    pub fn pause(&self, id: u64) -> Result<()> {
        self.post(&format!("/torrents/{}/pause", id))
    }
    pub fn resume(&self, id: u64) -> Result<()> {
        self.post(&format!("/torrents/{}/start", id))
    }
    /// Remove from session, keep downloaded files.
    pub fn forget(&self, id: u64) -> Result<()> {
        self.post(&format!("/torrents/{}/forget", id))
    }
    /// Remove from session AND delete files.
    pub fn delete(&self, id: u64) -> Result<()> {
        self.post(&format!("/torrents/{}/delete", id))
    }

    fn post(&self, path: &str) -> Result<()> {
        let resp = minreq::post(&format!("{}{}", self.base, path))
            .with_timeout(10)
            .send()
            .context("could not reach the rqbit engine")?;
        if resp.status_code >= 400 {
            return Err(anyhow!(
                "HTTP {}: {}",
                resp.status_code,
                snip(resp.as_str().unwrap_or(""), 120)
            ));
        }
        Ok(())
    }

    pub fn stream_url(&self, id: u64, file_idx: usize) -> String {
        format!("{}/torrents/{}/stream/{}", self.base, id, file_idx)
    }

    pub fn playlist_url(&self, id: u64) -> String {
        format!("{}/torrents/{}/playlist", self.base, id)
    }
}

fn snip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{}…", cut)
    }
}

/// Launch a local rqbit engine if one isn't already running.
pub fn spawn_engine(download_dir: &Path) -> Result<Child> {
    std::fs::create_dir_all(download_dir).ok();
    Command::new("rqbit")
        .arg("server")
        .arg("start")
        .arg(download_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context(
            "could not launch `rqbit`. Install it (e.g. `nix-shell -p rqbit`, or grab the \
             static binary from github.com/ikatson/rqbit/releases) and make sure it's in PATH",
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Integration test against a live local rqbit engine.
    /// Skips silently when no engine is running.
    #[test]
    fn live_engine_roundtrip() {
        let c = Client::new(DEFAULT_API);
        if !c.is_up() {
            eprintln!("skipping: no rqbit engine on {}", DEFAULT_API);
            return;
        }
        let torrents = c.list().expect("list torrents");
        if let Some(t) = torrents.first() {
            let d = c.details(t.id).expect("details");
            assert!(!d.files.is_empty(), "torrent should have files");
            let s = c.stats(t.id).expect("stats");
            assert!(s.total_bytes > 0);
            c.pause(t.id).ok();
            let s = c.stats(t.id).expect("stats after pause");
            assert_eq!(s.state, "paused");
            c.resume(t.id).expect("resume");
        }
        assert!(c.stream_url(0, 0).ends_with("/torrents/0/stream/0"));

        // Exercise the add-by-local-path branch when a fixture exists.
        if std::path::Path::new("/tmp/movie.torrent").is_file() {
            c.add("/tmp/movie.torrent").expect("add local .torrent");
        }
        // A garbage magnet should come back as a clean error, not a panic.
        assert!(c.add("magnet:?xt=urn:btih:zzzz").is_err());
    }
}
