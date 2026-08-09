use crate::omdb;
use crate::rqbit::{Client, FileDetails, TorrentStats};
use crate::search::{self, SearchResult};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub fn download_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("TORFLIX_DOWNLOAD_DIR") {
        return std::path::PathBuf::from(dir);
    }
    dirs::download_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("torflix")
}

fn help_seen() -> bool {
    let path = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("torflix")
        .join("help_seen");
    if path.exists() {
        return true;
    }
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    std::fs::write(&path, b"").ok();
    false
}

pub const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "avi", "webm", "mov", "m4v", "ts", "flv", "wmv", "mpg", "mpeg",
];

pub fn is_video(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .map(|ext| VIDEO_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[derive(Clone)]
pub struct TorrentRow {
    pub id: u64,
    pub name: String,
    pub stats: Option<TorrentStats>,
}

#[derive(PartialEq, Clone)]
pub enum View {
    Home,
    Torrents,
    Files,
    AddInput,
    ConfirmDelete,
    SearchResults,
}

pub enum SearchStatus {
    Idle,
    Searching(Arc<Mutex<Vec<SearchResult>>>),
    Done(Vec<SearchResult>),
    Failed(String),
}

#[derive(Clone, PartialEq)]
pub enum SortMode {
    Seeders, // default: highest seeders first
    Name,    // alphabetical: groups S01E01, S01E02, S01E03 together
    Size,    // largest file first
}

impl SortMode {
    pub fn next(&self) -> SortMode {
        match self {
            SortMode::Seeders => SortMode::Name,
            SortMode::Name => SortMode::Size,
            SortMode::Size => SortMode::Seeders,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            SortMode::Seeders => "seeders",
            SortMode::Name => "name",
            SortMode::Size => "size",
        }
    }
}

pub struct App {
    pub client: Client,
    pub view: View,
    pub rows: Arc<Mutex<Vec<TorrentRow>>>,
    pub engine_up: Arc<Mutex<bool>>,
    pub selected: usize,

    // Files view
    pub files: Vec<FileDetails>,
    pub files_torrent_id: u64,
    pub files_torrent_name: String,
    pub file_selected: usize,

    // Add-magnet input
    pub input: String,

    // Delete confirmation
    pub delete_with_files: bool,

    // Search
    pub search_query: String,
    pub search: Arc<Mutex<SearchStatus>>,
    pub search_selected: usize,
    pub search_sort: SortMode,

    // Ratings cache (IMDb + RT via OMDb, keyed by "title|year")
    pub ratings: Arc<Mutex<HashMap<String, omdb::Ratings>>>,
    pub ratings_fetching: Arc<Mutex<Option<String>>>,

    // Async status messages from background jobs (adds, etc.)
    pub status_tx: Sender<String>,
    pub status_rx: Receiver<String>,

    pub status: String,
    pub should_quit: bool,
    pub stop_engine_on_quit: bool,
    pub show_help: bool,
}

impl App {
    pub fn new(client: Client) -> Self {
        let (status_tx, status_rx) = channel();
        Self {
            client,
            view: View::Home,
            rows: Arc::new(Mutex::new(Vec::new())),
            engine_up: Arc::new(Mutex::new(true)),
            selected: 0,
            files: Vec::new(),
            files_torrent_id: 0,
            files_torrent_name: String::new(),
            file_selected: 0,
            input: String::new(),
            delete_with_files: false,
            search_query: String::new(),
            search: Arc::new(Mutex::new(SearchStatus::Idle)),
            search_selected: 0,
            search_sort: SortMode::Seeders,
            ratings: Arc::new(Mutex::new(HashMap::new())),
            ratings_fetching: Arc::new(Mutex::new(None)),
            status_tx,
            status_rx,
            status: String::from("a: add magnet/URL  Enter: files  Space: pause  q: quit"),
            should_quit: false,
            stop_engine_on_quit: false,
            show_help: !help_seen(),
        }
    }

    /// Background thread: refresh the torrent table every second.
    pub fn spawn_poller(&self) {
        let client = self.client.clone();
        let rows = Arc::clone(&self.rows);
        let engine_up = Arc::clone(&self.engine_up);
        thread::spawn(move || loop {
            match client.list() {
                Ok(list) => {
                    let mut fresh = Vec::with_capacity(list.len());
                    for t in list {
                        let stats = client.stats(t.id).ok();
                        fresh.push(TorrentRow {
                            id: t.id,
                            name: t.name.unwrap_or_else(|| t.info_hash.clone()),
                            stats,
                        });
                    }
                    *rows.lock().unwrap() = fresh;
                    *engine_up.lock().unwrap() = true;
                }
                Err(_) => {
                    *engine_up.lock().unwrap() = false;
                }
            }
            thread::sleep(Duration::from_millis(1000));
        });
    }

    pub fn rows_snapshot(&self) -> Vec<TorrentRow> {
        self.rows.lock().unwrap().clone()
    }

    pub fn selected_row(&self) -> Option<TorrentRow> {
        self.rows_snapshot().get(self.selected).cloned()
    }

    pub fn clamp_selection(&mut self, len: usize) {
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    pub fn open_files(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        match self.client.details(row.id) {
            Ok(d) => {
                self.files = d.files;
                self.files_torrent_id = row.id;
                self.files_torrent_name = d.name.unwrap_or(row.name);
                // Preselect the largest video file — almost always the movie.
                self.file_selected = self
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| is_video(&f.name))
                    .max_by_key(|(_, f)| f.length)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                self.view = View::Files;
                self.status =
                    "Enter: play in mpv  p: play all (playlist)  Esc: back".into();
            }
            Err(e) => self.status = format!("✗ {}", e),
        }
    }

    pub fn play_selected_file(&mut self) {
        let Some(file) = self.files.get(self.file_selected) else {
            return;
        };
        let url = self
            .client
            .stream_url(self.files_torrent_id, self.file_selected);
        let title = file.name.clone();
        self.launch_player(&url, &title);
    }

    pub fn play_playlist(&mut self) {
        let url = self.client.playlist_url(self.files_torrent_id);
        let title = self.files_torrent_name.clone();
        self.launch_player(&url, &title);
    }

    fn launch_player(&mut self, url: &str, title: &str) {
        let Some(player_cmd) = find_player() else {
            self.status =
                "✗ no player found — install mpv or vlc, or set TORFLIX_PLAYER".into();
            return;
        };
        match spawn_player(&player_cmd, url, title) {
            Ok(_) => self.status = format!("▶ playing: {} — buffering may take a moment", title),
            Err(e) => self.status = format!("✗ couldn't launch '{}': {}", player_cmd, e),
        }
    }

    pub fn toggle_pause(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let paused = row
            .stats
            .as_ref()
            .map(|s| s.state == "paused")
            .unwrap_or(false);
        let res = if paused {
            self.client.resume(row.id)
        } else {
            self.client.pause(row.id)
        };
        self.status = match res {
            Ok(_) => {
                if paused {
                    format!("resumed: {}", row.name)
                } else {
                    format!("paused: {}", row.name)
                }
            }
            Err(e) => format!("✗ {}", e),
        };
    }

    /// Add a torrent and either stream it (if a player is available) or download it permanently.
    pub fn add_and_play_async(&mut self, target: &str, label: &str) {
        let client = self.client.clone();
        let tx = self.status_tx.clone();
        let target = target.to_string();
        let label = label.to_string();

        // Detect the player now — if none, fall back to permanent download.
        let player_cmd = find_player();

        if player_cmd.is_none() {
            self.status = format!("⬇ adding: {} — no player found, downloading…", label);
            thread::spawn(move || match client.add(&target) {
                Ok(_) => {
                    let _ = tx.send(format!(
                        "⬇ downloading: {}  (install mpv or vlc to stream instead)",
                        label
                    ));
                }
                Err(e) => {
                    let _ = tx.send(format!("✗ add failed: {}", e));
                }
            });
            return;
        }

        let player_cmd = player_cmd.unwrap();
        self.status = format!("⧗ adding: {} …", label);

        thread::spawn(move || {
            // Unique temp dir so concurrent streams don't collide.
            let temp_dir = std::env::temp_dir().join(format!(
                "torflix-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&temp_dir).ok();

            let id = match client.add_to_dir(&target, &temp_dir) {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.send(format!("✗ add failed: {}", e));
                    std::fs::remove_dir_all(&temp_dir).ok();
                    return;
                }
            };
            let _ = tx.send("⧗ resolving metadata…".into());

            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            let details = loop {
                if std::time::Instant::now() > deadline {
                    let _ = tx.send(format!("✗ metadata timeout for {}", label));
                    client.forget(id).ok();
                    std::fs::remove_dir_all(&temp_dir).ok();
                    return;
                }
                thread::sleep(Duration::from_millis(1000));
                match client.details(id) {
                    Ok(d) if !d.files.is_empty() => break d,
                    _ => {}
                }
            };

            let best = details
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| is_video(&f.name))
                .max_by_key(|(_, f)| f.length);

            let Some((idx, file)) = best else {
                let _ = tx.send(format!(
                    "✓ added: {} (no video file — navigate manually)",
                    label
                ));
                return;
            };

            let url = client.stream_url(id, idx);
            let title = file.name.clone();

            match spawn_player(&player_cmd, &url, &title) {
                Ok(mut child) => {
                    let _ = tx.send(format!(
                        "▶ streaming: {}  [tmp: {}]",
                        title,
                        temp_dir.display()
                    ));
                    child.wait().ok();
                    client.forget(id).ok();
                    let cleaned = std::fs::remove_dir_all(&temp_dir).is_ok();
                    let _ = tx.send(if cleaned {
                        format!("✓ done: {} — temp files deleted", title)
                    } else {
                        format!("⚠ done: {} — could not delete {}", title, temp_dir.display())
                    });
                }
                Err(e) => {
                    let _ = tx.send(format!("✗ couldn't launch '{}': {}", player_cmd, e));
                    client.forget(id).ok();
                    std::fs::remove_dir_all(&temp_dir).ok();
                }
            }
        });
    }

    pub fn submit_add(&mut self) {
        let input = self.input.trim().to_string();
        self.input.clear();
        self.view = View::Torrents;
        if input.is_empty() {
            return;
        }
        let label = snip_label(&input);
        self.add_and_play_async(&input, &label);
    }

    // ---------- search ----------

    pub fn start_search(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let backend = search::backend_from_env().expect("backend_from_env always returns Some");
        let partial: Arc<Mutex<Vec<SearchResult>>> = Arc::new(Mutex::new(Vec::new()));
        *self.search.lock().unwrap() = SearchStatus::Searching(Arc::clone(&partial));
        self.search_selected = 0;
        self.search_sort = SortMode::Seeders;
        self.view = View::SearchResults;
        self.status = format!("searching {} for '{}' …", backend.name(), q);
        let state = Arc::clone(&self.search);
        thread::spawn(move || {
            let out = match search::search(&backend, &q, &partial) {
                Ok(v) => SearchStatus::Done(v),
                Err(e) => SearchStatus::Failed(e.to_string()),
            };
            *state.lock().unwrap() = out;
        });
    }

    pub fn search_results_len(&self) -> usize {
        match &*self.search.lock().unwrap() {
            SearchStatus::Done(v) => v.len(),
            SearchStatus::Searching(partial) => partial.lock().unwrap().len(),
            _ => 0,
        }
    }

    pub fn sort_results<'a>(&self, v: &'a [SearchResult]) -> Vec<&'a SearchResult> {
        let mut refs: Vec<&SearchResult> = v.iter().collect();
        match self.search_sort {
            SortMode::Seeders => refs.sort_by(|a, b| b.seeders.cmp(&a.seeders)),
            SortMode::Name => refs.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
            SortMode::Size => refs.sort_by(|a, b| b.size.cmp(&a.size)),
        }
        refs
    }

    pub fn download_search_selected(&mut self) {
        let picked: Option<SearchResult> = match &*self.search.lock().unwrap() {
            SearchStatus::Done(v) => {
                let sorted = self.sort_results(v);
                sorted.get(self.search_selected).map(|r| (*r).clone())
            }
            SearchStatus::Searching(partial) => {
                let v = partial.lock().unwrap();
                let sorted = self.sort_results(&v);
                sorted.get(self.search_selected).map(|r| (*r).clone())
            }
            _ => None,
        };
        let Some(r) = picked else { return };
        match r.add_target() {
            Some(target) => {
                let target = target.to_string();
                self.download_to_disk_async(&target, &r.title);
            }
            None => self.status = "✗ result has no magnet or download link".into(),
        }
    }

    fn download_to_disk_async(&mut self, target: &str, label: &str) {
        let client = self.client.clone();
        let tx = self.status_tx.clone();
        let target = target.to_string();
        let label = label.to_string();
        let dest = download_dir();
        self.status = format!("⬇ queuing: {} → {}", label, dest.display());
        thread::spawn(move || {
            std::fs::create_dir_all(&dest).ok();
            match client.add_to_dir(&target, &dest) {
                Ok(_) => {
                    let _ = tx.send(format!("⬇ downloading: {}  →  {}", label, dest.display()));
                }
                Err(e) => {
                    let _ = tx.send(format!("✗ download failed: {}", e));
                }
            }
        });
    }

    pub fn add_search_selected(&mut self) {
        let picked: Option<SearchResult> = match &*self.search.lock().unwrap() {
            SearchStatus::Done(v) => {
                let sorted = self.sort_results(v);
                sorted.get(self.search_selected).map(|r| (*r).clone())
            }
            SearchStatus::Searching(partial) => {
                let v = partial.lock().unwrap();
                let sorted = self.sort_results(&v);
                sorted.get(self.search_selected).map(|r| (*r).clone())
            }
            _ => None,
        };
        let Some(r) = picked else { return };
        match r.add_target() {
            Some(target) => {
                let target = target.to_string();
                self.add_and_play_async(&target, &r.title);
            }
            None => self.status = "✗ result has no magnet or download link".into(),
        }
    }

    fn fetch_ratings_for(&self, title: &str, year: &str, api_key: &str) {
        let key = format!("{}|{}", title, year);
        if self.ratings.lock().unwrap().contains_key(&key) {
            return;
        }
        {
            let mut f = self.ratings_fetching.lock().unwrap();
            if f.as_deref() == Some(key.as_str()) {
                return;
            }
            *f = Some(key.clone());
        }
        let title = title.to_string();
        let year = year.to_string();
        let api_key = api_key.to_string();
        let ratings = Arc::clone(&self.ratings);
        let fetching = Arc::clone(&self.ratings_fetching);
        thread::spawn(move || {
            let result = omdb::fetch(&title, &year, &api_key).unwrap_or_default();
            ratings.lock().unwrap().insert(key, result);
            *fetching.lock().unwrap() = None;
        });
    }

    fn ratings_line_for(&self, title: &str, year: &str) -> String {
        let key = format!("{}|{}", title, year);
        let cache = self.ratings.lock().unwrap();
        if let Some(r) = cache.get(&key) {
            let mut parts = Vec::new();
            if let Some(imdb) = &r.imdb {
                parts.push(format!("IMDb: {}", imdb));
            }
            if let Some(rt) = &r.rt {
                parts.push(format!("RT: {}", rt));
            }
            return parts.join("   ");
        }
        drop(cache);
        let fetching = self.ratings_fetching.lock().unwrap();
        if fetching.as_deref() == Some(key.as_str()) {
            "fetching ratings…".into()
        } else {
            String::new()
        }
    }

    /// Lazily fetch OMDb ratings for the current search query (one lookup per search).
    pub fn maybe_fetch_search_ratings(&self) {
        let Some(api_key) = std::env::var("TORFLIX_OMDB_KEY").ok() else { return };
        let q = self.search_query.trim().to_string();
        if q.is_empty() { return; }
        self.fetch_ratings_for(&q, "", &api_key);
    }

    pub fn search_ratings_line(&self) -> String {
        let q = self.search_query.trim().to_string();
        if q.is_empty() { return String::new(); }
        self.ratings_line_for(&q, "")
    }

    pub fn confirm_delete(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let res = if self.delete_with_files {
            self.client.delete(row.id)
        } else {
            self.client.forget(row.id)
        };
        self.status = match res {
            Ok(_) => {
                if self.delete_with_files {
                    format!("deleted (with files): {}", row.name)
                } else {
                    format!("removed (files kept): {}", row.name)
                }
            }
            Err(e) => format!("✗ {}", e),
        };
        self.view = View::Torrents;
    }
}

/// Returns the first available media player command, or None if none found.
/// Priority: TORFLIX_PLAYER env var → mpv → vlc.
pub fn find_player() -> Option<String> {
    if let Ok(p) = std::env::var("TORFLIX_PLAYER") {
        let p = p.trim().to_string();
        if !p.is_empty() {
            return Some(p);
        }
    }
    for candidate in &["mpv", "vlc"] {
        if Command::new(candidate)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Spawns the media player with an appropriate title flag.
fn spawn_player(player_cmd: &str, url: &str, title: &str) -> std::io::Result<std::process::Child> {
    let mut parts = player_cmd.split_whitespace();
    let bin = parts.next().unwrap_or("mpv");
    let extra: Vec<&str> = parts.collect();

    let mut cmd = Command::new(bin);
    cmd.args(&extra);
    match bin {
        "mpv" => {
            cmd.arg(format!("--force-media-title={}", title));
        }
        "vlc" => {
            cmd.args(["--meta-title", title]);
        }
        _ => {}
    }
    cmd.arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn snip_label(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= 48 {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(48).collect::<String>())
    }
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[i])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}
