//! Thin client for the rqbit HTTP API (default: http://127.0.0.1:3030).
//! Response shapes verified against rqbit 8.1.1.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;

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
    pub output_folder: String,
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

    /// Download only these files (original rqbit indices). Streaming one episode
    /// out of a season pack otherwise pulls every episode at once, and they all
    /// compete with the one being watched for bandwidth and peers.
    pub fn update_only_files(&self, id: u64, files: &[usize]) -> Result<()> {
        let resp = minreq::post(&format!("{}/torrents/{}/update_only_files", self.base, id))
            .with_json(&serde_json::json!({ "only_files": files }))?
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

/// Drop stream/preview torrents left behind by a torflix that exited mid-stream.
/// With session persistence on they'd otherwise be restored on the next start
/// and carry on downloading into a temp dir that is about to be deleted.
pub fn forget_temp_torrents(client: &Client) {
    let Ok(list) = client.list() else { return };
    for t in list {
        if let Ok(d) = client.details(t.id) {
            if is_temp_folder(Path::new(&d.output_folder)) {
                client.forget(t.id).ok();
            }
        }
    }
}

/// True for the per-stream `torflix-*` dirs created directly under the temp dir.
pub fn is_temp_folder(p: &Path) -> bool {
    let named = p
        .file_name()
        .map(|n| n.to_string_lossy().starts_with("torflix-"))
        .unwrap_or(false);
    let tmp = std::env::temp_dir();
    // Compare canonicalized too: the temp dir is often a symlink (e.g. /tmp → /private/tmp on macOS).
    let under_tmp = p.parent() == Some(tmp.as_path())
        || match (p.parent().and_then(|x| x.canonicalize().ok()), tmp.canonicalize().ok()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
    named && under_tmp
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

/// Handle to the embedded rqbit engine running in a background thread.
pub struct EmbeddedEngine {
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EmbeddedEngine {
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Where the embedded engine remembers its torrents between runs. Kept apart
/// from rqbit's own default so a standalone rqbit install doesn't share state.
fn session_dir() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("torflix")
        .join("session")
}

/// Where the embedded engine listens: the host:port of `api_url`, so pointing
/// `TORFLIX_RQBIT_URL` at another port runs the engine there rather than on a
/// port the client never talks to.
fn bind_addr(api_url: &str) -> std::net::SocketAddr {
    let rest = api_url.split("://").nth(1).unwrap_or(api_url);
    let hostport = rest.split('/').next().unwrap_or("").replacen("localhost", "127.0.0.1", 1);
    hostport.parse().unwrap_or_else(|_| "127.0.0.1:3030".parse().unwrap())
}

/// Start the rqbit HTTP API server embedded in-process, listening on `api_url`'s address.
/// Returns once the API socket is bound, or with the reason it couldn't be.
pub fn start_embedded_engine(download_dir: &Path, api_url: &str) -> Result<EmbeddedEngine> {
    let addr = bind_addr(api_url);
    std::fs::create_dir_all(download_dir).ok();
    let download_dir = download_dir.to_path_buf();
    let session_dir = session_dir();
    std::fs::create_dir_all(&session_dir).ok();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();

    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async move {
            use librqbit::http_api::{HttpApi, HttpApiOptions};
            use librqbit::{Api, Session, SessionOptions, SessionPersistenceConfig};

            let opts = SessionOptions {
                // Remember torrents across restarts, so unfinished downloads pick
                // up where they left off instead of vanishing when torflix quits.
                persistence: Some(SessionPersistenceConfig::Json { folder: Some(session_dir) }),
                // Record which pieces are done, so a resume doesn't re-hash every file first.
                fastresume: true,
                ..Default::default()
            };
            let session = match Session::new_with_opts(download_dir, opts).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("rqbit session error: {e:#}")));
                    return;
                }
            };

            let api = Api::new(session.clone(), None, None);
            let http = HttpApi::new(api, Some(HttpApiOptions {
                read_only: false,
                allow_create: true,
                ..Default::default()
            }));

            let listener = match librqbit_dualstack_sockets::TcpListener::bind_tcp(addr, Default::default()) {
                Ok(l) => l,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("rqbit could not bind {addr}: {e:#}")));
                    session.stop().await;
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));

            tokio::select! {
                _ = http.make_http_api_and_run(listener, None) => {}
                _ = shutdown_rx => {}
            }
            // Pause everything so fastresume state is written before the runtime goes away.
            // (This pause isn't persisted as "paused", so torrents still resume next start.)
            session.stop().await;
        });
    });

    let mut engine = EmbeddedEngine {
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    };
    match ready_rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Ok(())) => Ok(engine),
        Ok(Err(e)) => {
            engine.stop();
            Err(anyhow!(e))
        }
        Err(_) => {
            engine.stop();
            Err(anyhow!("rqbit engine did not start"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_addr_follows_api_url() {
        assert_eq!(bind_addr("http://127.0.0.1:3030"), "127.0.0.1:3030".parse().unwrap());
        assert_eq!(bind_addr("http://localhost:4040/"), "127.0.0.1:4040".parse().unwrap());
        assert_eq!(bind_addr("http://[::1]:5050"), "[::1]:5050".parse().unwrap());
        // Unparseable (e.g. no port) falls back to the default rather than failing to start.
        assert_eq!(bind_addr("http://example.com"), "127.0.0.1:3030".parse().unwrap());
    }

    #[test]
    fn temp_folder_detection() {
        let tmp = std::env::temp_dir();
        assert!(is_temp_folder(&tmp.join("torflix-1712345")));
        assert!(is_temp_folder(&tmp.join("torflix-preview-99")));
        assert!(!is_temp_folder(&tmp.join("other-123")));
        // A permanent download dir that merely happens to be named torflix-*.
        assert!(!is_temp_folder(Path::new("/home/someone/Videos/torflix-stuff")));
        assert!(!is_temp_folder(&tmp.join("torflix-1").join("nested")));
    }

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
