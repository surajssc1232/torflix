use crate::letterboxd::{self, ListKind, Movie};
use crate::omdb;
use crate::rqbit::{Client, FileDetails, TorrentStats};
use crate::search::{self, SearchResult};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
    Torrents,
    Files,
    AddInput,
    ConfirmDelete,
    SearchInput,
    SearchResults,
    Popular,
}

pub enum PopularStatus {
    Idle,
    Loading,
    Done(Vec<Movie>),
    Failed(String),
}

pub enum SearchStatus {
    Idle,
    Searching,
    Done(Vec<SearchResult>),
    Failed(String),
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
    pub search_origin: View,   // which view to return to on Esc from SearchResults

    // Popular (Letterboxd)
    pub popular: Arc<Mutex<PopularStatus>>,
    pub popular_selected: usize,
    pub popular_list: ListKind,

    // Ratings cache (IMDb + RT via OMDb, keyed by "title|year")
    pub ratings: Arc<Mutex<HashMap<String, omdb::Ratings>>>,
    pub ratings_fetching: Arc<Mutex<Option<String>>>,

    // Async status messages from background jobs (adds, etc.)
    pub status_tx: Sender<String>,
    pub status_rx: Receiver<String>,

    pub status: String,
    pub should_quit: bool,
    pub stop_engine_on_quit: bool,
}

impl App {
    pub fn new(client: Client) -> Self {
        let (status_tx, status_rx) = channel();
        Self {
            client,
            view: View::Torrents,
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
            search_origin: View::Torrents,
            popular: Arc::new(Mutex::new(PopularStatus::Idle)),
            popular_selected: 0,
            popular_list: ListKind::Popular,
            ratings: Arc::new(Mutex::new(HashMap::new())),
            ratings_fetching: Arc::new(Mutex::new(None)),
            status_tx,
            status_rx,
            status: String::from("a: add magnet/URL  Enter: files  Space: pause  q: quit"),
            should_quit: false,
            stop_engine_on_quit: false,
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
        let player_cmd =
            std::env::var("TORFLIX_PLAYER").unwrap_or_else(|_| "mpv".to_string());
        let mut parts = player_cmd.split_whitespace();
        let bin = parts.next().unwrap_or("mpv");
        let extra: Vec<&str> = parts.collect();

        let mut cmd = Command::new(bin);
        cmd.args(&extra);
        if bin == "mpv" {
            cmd.arg(format!("--force-media-title={}", title));
        }
        cmd.arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        match cmd.spawn() {
            Ok(_) => {
                self.status = format!("▶ playing: {} — buffering may take a moment", title)
            }
            Err(e) => {
                self.status = format!(
                    "✗ couldn't launch '{}' ({}). Install mpv or set TORFLIX_PLAYER",
                    bin, e
                )
            }
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

    /// Add torrent to a temp dir, stream the largest video in MPV,
    /// then delete all downloaded pieces when MPV exits — nothing kept on disk.
    pub fn add_and_play_async(&mut self, target: &str, label: &str) {
        let client = self.client.clone();
        let tx = self.status_tx.clone();
        let target = target.to_string();
        let label = label.to_string();
        let player_cmd =
            std::env::var("TORFLIX_PLAYER").unwrap_or_else(|_| "mpv".to_string());
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

            // Poll until file list is available (up to 60 s).
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

            let mut parts = player_cmd.split_whitespace();
            let bin = parts.next().unwrap_or("mpv");
            let extra: Vec<&str> = parts.collect();

            let mut cmd = Command::new(bin);
            cmd.args(&extra);
            if bin == "mpv" {
                cmd.arg(format!("--force-media-title={}", title));
            }
            cmd.arg(&url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());

            match cmd.spawn() {
                Ok(mut child) => {
                    let _ = tx.send(format!(
                        "▶ streaming: {}  [tmp: {}]",
                        title,
                        temp_dir.display()
                    ));
                    // Block until MPV exits, then wipe everything.
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
                    let _ = tx.send(format!(
                        "✗ couldn't launch '{}' ({}). Install mpv or set TORFLIX_PLAYER",
                        bin, e
                    ));
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
        let Some(backend) = search::backend_from_env() else {
            self.status = "✗ no search backend — set TORFLIX_PROWLARR_URL(+_APIKEY) or TORFLIX_JACKETT_URL(+_APIKEY), see README".into();
            self.view = View::Torrents;
            return;
        };
        *self.search.lock().unwrap() = SearchStatus::Searching;
        self.search_selected = 0;
        self.search_origin = if self.view == View::SearchInput {
            View::Torrents
        } else {
            self.view.clone()
        };
        self.view = View::SearchResults;
        self.status = format!("searching {} for '{}' …", backend.name(), q);
        let state = Arc::clone(&self.search);
        thread::spawn(move || {
            let out = match search::search(&backend, &q) {
                Ok(v) => SearchStatus::Done(v),
                Err(e) => SearchStatus::Failed(e.to_string()),
            };
            *state.lock().unwrap() = out;
        });
    }

    pub fn search_results_len(&self) -> usize {
        match &*self.search.lock().unwrap() {
            SearchStatus::Done(v) => v.len(),
            _ => 0,
        }
    }

    pub fn add_search_selected(&mut self) {
        let picked: Option<SearchResult> = match &*self.search.lock().unwrap() {
            SearchStatus::Done(v) => v.get(self.search_selected).cloned(),
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

    // ---------- popular (letterboxd) ----------

    pub fn browse_popular(&mut self, kind: ListKind) {
        self.popular_list = kind;
        self.popular_selected = 0;
        self.view = View::Popular;
        *self.popular.lock().unwrap() = PopularStatus::Loading;
        self.status = format!("loading {} from Letterboxd…", self.popular_list.label());

        let state = Arc::clone(&self.popular);
        let url_kind = match &self.popular_list {
            ListKind::Popular => ListKind::Popular,
            ListKind::PopularThisWeek => ListKind::PopularThisWeek,
            ListKind::PopularThisMonth => ListKind::PopularThisMonth,
            ListKind::TopRated => ListKind::TopRated,
        };
        thread::spawn(move || {
            let out = match letterboxd::fetch(&url_kind) {
                Ok(v) if v.is_empty() => PopularStatus::Failed(
                    "no films parsed — Letterboxd may have changed its HTML".into(),
                ),
                Ok(v) => PopularStatus::Done(v),
                Err(e) => PopularStatus::Failed(e.to_string()),
            };
            *state.lock().unwrap() = out;
        });
    }

    pub fn popular_len(&self) -> usize {
        match &*self.popular.lock().unwrap() {
            PopularStatus::Done(v) => v.len(),
            _ => 0,
        }
    }

    /// Search for the selected popular movie via Prowlarr/Jackett.
    pub fn search_popular_selected(&mut self) {
        let movie = match &*self.popular.lock().unwrap() {
            PopularStatus::Done(v) => v.get(self.popular_selected).cloned(),
            _ => None,
        };
        if let Some(m) = movie {
            let query = if m.year.is_empty() {
                m.title.clone()
            } else {
                format!("{} {}", m.title, m.year)
            };
            self.search_query = query;
            self.start_search();
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

    /// Lazily fetch OMDb ratings for the currently highlighted popular film.
    pub fn maybe_fetch_ratings(&self) {
        let Some(api_key) = std::env::var("TORFLIX_OMDB_KEY").ok() else { return };
        let (title, year) = {
            let lock = self.popular.lock().unwrap();
            match &*lock {
                PopularStatus::Done(v) => match v.get(self.popular_selected) {
                    Some(m) => (m.title.clone(), m.year.clone()),
                    None => return,
                },
                _ => return,
            }
        };
        self.fetch_ratings_for(&title, &year, &api_key);
    }

    pub fn popular_ratings_line(&self) -> String {
        let (title, year) = {
            let lock = self.popular.lock().unwrap();
            match &*lock {
                PopularStatus::Done(v) => match v.get(self.popular_selected) {
                    Some(m) => (m.title.clone(), m.year.clone()),
                    None => return String::new(),
                },
                _ => return String::new(),
            }
        };
        self.ratings_line_for(&title, &year)
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
