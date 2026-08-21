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

pub enum PreviewState {
    Loading,
    // (original_rqbit_index, file) — original index is needed for the stream URL
    Ready(Vec<(usize, crate::rqbit::FileDetails)>),
    Error(String),
}

#[derive(Clone, PartialEq)]
pub enum FileSortMode {
    Name,
    Size,
}

pub struct SearchPreview {
    pub result_idx: usize,
    pub file_selected: usize,
    pub file_sort: FileSortMode,
    pub state: Arc<Mutex<PreviewState>>,
    pub torrent_id: Arc<Mutex<Option<u64>>>,
    pub temp_dir: std::path::PathBuf,
    pub cancelled: Arc<Mutex<bool>>,
}

pub struct App {
    pub client: Client,
    pub view: View,
    pub rows: Arc<Mutex<Vec<TorrentRow>>>,
    pub engine_up: Arc<Mutex<bool>>,
    pub selected: usize,

    // Files view
    pub files: Vec<FileDetails>,
    pub files_stream_indices: Vec<usize>, // original rqbit indices (files are sorted by name for display)
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
    pub search_filter: String,
    pub search_filter_active: bool,
    pub search_preview: Option<SearchPreview>,

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
    pub tick: u64,
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
            files_stream_indices: Vec::new(),
            files_torrent_id: 0,
            files_torrent_name: String::new(),
            file_selected: 0,
            input: String::new(),
            delete_with_files: false,
            search_query: String::new(),
            search: Arc::new(Mutex::new(SearchStatus::Idle)),
            search_selected: 0,
            search_sort: SortMode::Seeders,
            search_filter: String::new(),
            search_filter_active: false,
            search_preview: None,
            ratings: Arc::new(Mutex::new(HashMap::new())),
            ratings_fetching: Arc::new(Mutex::new(None)),
            status_tx,
            status_rx,
            status: String::from("a: add magnet/URL  Enter: files  Space: pause  q: quit"),
            should_quit: false,
            stop_engine_on_quit: false,
            show_help: !help_seen(),
            tick: 0,
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
                // Sort files alphabetically so episodes appear in order (E01, E02, …).
                // Keep the original rqbit index for each file — the stream URL uses that.
                let mut indexed: Vec<(usize, FileDetails)> = d.files.into_iter().enumerate().collect();
                indexed.sort_by(|(_, a), (_, b)| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                self.files_stream_indices = indexed.iter().map(|(i, _)| *i).collect();
                self.files = indexed.into_iter().map(|(_, f)| f).collect();
                self.files_torrent_id = row.id;
                self.files_torrent_name = d.name.unwrap_or(row.name);
                // Default to the first video file in sorted order (E01 for a season pack).
                self.file_selected = self
                    .files
                    .iter()
                    .position(|f| is_video(&f.name))
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
        let stream_idx = self.files_stream_indices
            .get(self.file_selected)
            .copied()
            .unwrap_or(self.file_selected);
        let url = self.client.stream_url(self.files_torrent_id, stream_idx);
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
                match client.details(id) {
                    Ok(d) if !d.files.is_empty() => break d,
                    _ => {}
                }
                thread::sleep(Duration::from_millis(250));
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
        self.search_filter.clear();
        self.search_filter_active = false;
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
            SearchStatus::Done(v) => self.filtered_results(v).len(),
            SearchStatus::Searching(partial) => {
                let v = partial.lock().unwrap();
                self.filtered_results(&v).len()
            }
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

    pub fn filtered_results<'a>(&self, v: &'a [SearchResult]) -> Vec<&'a SearchResult> {
        let sorted = self.sort_results(v);
        if self.search_filter.is_empty() {
            return sorted;
        }
        let needle = self.search_filter.to_lowercase();
        sorted.into_iter().filter(|r| r.title.to_lowercase().contains(&needle)).collect()
    }

    pub fn download_search_selected(&mut self) {
        let picked: Option<SearchResult> = match &*self.search.lock().unwrap() {
            SearchStatus::Done(v) => {
                let filtered = self.filtered_results(v);
                filtered.get(self.search_selected).map(|r| (*r).clone())
            }
            SearchStatus::Searching(partial) => {
                let v = partial.lock().unwrap();
                let filtered = self.filtered_results(&v);
                filtered.get(self.search_selected).map(|r| (*r).clone())
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
                let filtered = self.filtered_results(v);
                filtered.get(self.search_selected).map(|r| (*r).clone())
            }
            SearchStatus::Searching(partial) => {
                let v = partial.lock().unwrap();
                let filtered = self.filtered_results(&v);
                filtered.get(self.search_selected).map(|r| (*r).clone())
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

    pub fn open_search_preview(&mut self) {
        self.close_search_preview();

        let target = match &*self.search.lock().unwrap() {
            SearchStatus::Done(v) => {
                let filtered = self.filtered_results(v);
                filtered.get(self.search_selected).and_then(|r| r.add_target()).map(str::to_string)
            }
            SearchStatus::Searching(partial) => {
                let v = partial.lock().unwrap();
                let filtered = self.filtered_results(&v);
                filtered.get(self.search_selected).and_then(|r| r.add_target()).map(str::to_string)
            }
            _ => None,
        };
        let Some(target) = target else { return };

        let state: Arc<Mutex<PreviewState>> = Arc::new(Mutex::new(PreviewState::Loading));
        let torrent_id: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let cancelled: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

        let temp_dir = std::env::temp_dir().join(format!(
            "torflix-preview-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).ok();

        self.search_preview = Some(SearchPreview {
            result_idx: self.search_selected,
            file_selected: 0,
            file_sort: FileSortMode::Name,
            state: Arc::clone(&state),
            torrent_id: Arc::clone(&torrent_id),
            temp_dir: temp_dir.clone(),
            cancelled: Arc::clone(&cancelled),
        });
        self.status = "⧗ fetching file list…".into();

        let client = self.client.clone();
        let tx = self.status_tx.clone();
        thread::spawn(move || {
            let id = match client.add_to_dir(&target, &temp_dir) {
                Ok(id) => id,
                Err(e) => {
                    *state.lock().unwrap() = PreviewState::Error(e.to_string());
                    std::fs::remove_dir_all(&temp_dir).ok();
                    return;
                }
            };
            *torrent_id.lock().unwrap() = Some(id);

            if *cancelled.lock().unwrap() {
                client.forget(id).ok();
                std::fs::remove_dir_all(&temp_dir).ok();
                return;
            }

            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                if *cancelled.lock().unwrap() {
                    client.forget(id).ok();
                    std::fs::remove_dir_all(&temp_dir).ok();
                    return;
                }
                if std::time::Instant::now() > deadline {
                    *state.lock().unwrap() = PreviewState::Error("metadata timeout".into());
                    client.forget(id).ok();
                    std::fs::remove_dir_all(&temp_dir).ok();
                    let _ = tx.send("✗ file list: metadata timeout".into());
                    return;
                }
                match client.details(id) {
                    Ok(d) if !d.files.is_empty() => {
                        let _ = tx.send(format!("✓ {} files", d.files.len()));
                        let indexed = d.files.into_iter().enumerate().collect();
                        *state.lock().unwrap() = PreviewState::Ready(indexed);
                        return;
                    }
                    _ => {}
                }
                thread::sleep(Duration::from_millis(250));
            }
        });
    }

    pub fn close_search_preview(&mut self) {
        if let Some(preview) = self.search_preview.take() {
            *preview.cancelled.lock().unwrap() = true;
            if let Some(id) = *preview.torrent_id.lock().unwrap() {
                self.client.forget(id).ok();
            }
            std::fs::remove_dir_all(&preview.temp_dir).ok();
        }
    }

    pub fn preview_up(&mut self) {
        if let Some(p) = &mut self.search_preview {
            p.file_selected = p.file_selected.saturating_sub(1);
        }
    }

    pub fn preview_down(&mut self) {
        if let Some(p) = &mut self.search_preview {
            let len = match &*p.state.lock().unwrap() {
                PreviewState::Ready(files) => files.len(),
                _ => 0,
            };
            if len > 0 {
                p.file_selected = (p.file_selected + 1).min(len - 1);
            }
        }
    }

    /// Sort files the same way draw_preview_panel does, so play_from_preview
    /// picks the right original rqbit index for the visually selected row.
    fn preview_sorted_files<'a>(
        files: &'a [(usize, FileDetails)],
        sort: &FileSortMode,
    ) -> Vec<&'a (usize, FileDetails)> {
        let mut v: Vec<&(usize, FileDetails)> = files.iter().collect();
        match sort {
            FileSortMode::Name => v.sort_by(|a, b| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())),
            FileSortMode::Size => v.sort_by(|a, b| b.1.length.cmp(&a.1.length)),
        }
        v
    }

    pub fn preview_cycle_sort(&mut self) {
        if let Some(p) = &mut self.search_preview {
            p.file_sort = match p.file_sort {
                FileSortMode::Name => FileSortMode::Size,
                FileSortMode::Size => FileSortMode::Name,
            };
            p.file_selected = 0;
        }
    }

    pub fn play_from_preview(&mut self) {
        let data = {
            let Some(preview) = &self.search_preview else { return };
            let id = match *preview.torrent_id.lock().unwrap() {
                Some(id) => id,
                None => return,
            };
            let (idx, name) = match &*preview.state.lock().unwrap() {
                PreviewState::Ready(files) => {
                    // Sort the same way the UI displays them so file_selected
                    // maps to the correct visual row and original rqbit index.
                    let sorted = Self::preview_sorted_files(files, &preview.file_sort);
                    match sorted.get(preview.file_selected) {
                        Some((orig_idx, f)) => (*orig_idx, f.name.clone()),
                        None => return,
                    }
                }
                _ => return,
            };
            (id, idx, name)
        };
        let (id, file_idx, file_name) = data;

        // Promote the preview to a live stream — take ownership so cleanup doesn't double-forget
        let preview = self.search_preview.take().unwrap();
        let temp_dir = preview.temp_dir.clone();
        // Mark cancelled so the background loader thread won't touch it if still running
        *preview.cancelled.lock().unwrap() = true;

        let Some(player_cmd) = find_player() else {
            self.status = "✗ no player found — install mpv or vlc".into();
            self.client.forget(id).ok();
            std::fs::remove_dir_all(&temp_dir).ok();
            return;
        };

        let url = self.client.stream_url(id, file_idx);
        let client = self.client.clone();
        let tx = self.status_tx.clone();
        let title = file_name.clone();

        match spawn_player(&player_cmd, &url, &title) {
            Ok(mut child) => {
                self.status = format!("▶ streaming: {}", title);
                thread::spawn(move || {
                    child.wait().ok();
                    client.forget(id).ok();
                    std::fs::remove_dir_all(&temp_dir).ok();
                    let _ = tx.send(format!("✓ done: {} — temp files deleted", title));
                });
            }
            Err(e) => {
                self.status = format!("✗ couldn't launch player: {}", e);
                self.client.forget(id).ok();
                std::fs::remove_dir_all(&temp_dir).ok();
            }
        }
    }

    pub fn download_from_preview(&mut self) {
        // Get the original result by index so we can re-add to the permanent download dir
        let result = {
            match &*self.search.lock().unwrap() {
                SearchStatus::Done(v) => {
                    let filtered = self.filtered_results(v);
                    let idx = self.search_preview.as_ref().map(|p| p.result_idx).unwrap_or(self.search_selected);
                    filtered.get(idx).map(|r| (*r).clone())
                }
                SearchStatus::Searching(partial) => {
                    let v = partial.lock().unwrap();
                    let filtered = self.filtered_results(&v);
                    let idx = self.search_preview.as_ref().map(|p| p.result_idx).unwrap_or(self.search_selected);
                    filtered.get(idx).map(|r| (*r).clone())
                }
                _ => None,
            }
        };
        self.close_search_preview(); // forget temp torrent
        if let Some(r) = result {
            if let Some(target) = r.add_target() {
                let target = target.to_string();
                self.download_to_disk_async(&target, &r.title);
            }
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
    player_candidates().into_iter().find(|c| player_exists(c))
}

/// True if `candidate` names a runnable player.
///
/// On Windows we resolve on disk rather than executing: `vlc.exe --version`
/// opens a modal dialog there instead of printing and exiting, so probing it
/// would block startup until a human dismissed the window.
#[cfg(target_os = "windows")]
fn player_exists(candidate: &str) -> bool {
    let p = std::path::Path::new(candidate);
    if p.is_absolute() {
        return p.is_file();
    }
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path)
        .any(|dir| dir.join(format!("{}.exe", candidate)).is_file() || dir.join(candidate).is_file())
}

#[cfg(not(target_os = "windows"))]
fn player_exists(candidate: &str) -> bool {
    Command::new(candidate)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn player_candidates() -> Vec<String> {
    // Ordered by preference: every mpv location before any vlc one.
    let mut v: Vec<String> = vec!["mpv".into()];
    // On Windows, neither player is usually in PATH — add their install locations.
    #[cfg(target_os = "windows")]
    {
        // mpv portable is commonly unpacked into %LOCALAPPDATA%\mpv or %APPDATA%\mpv.
        for var in &["LOCALAPPDATA", "APPDATA"] {
            if let Ok(base) = std::env::var(var) {
                v.push(format!(r"{}\mpv\mpv.exe", base));
            }
        }
    }
    v.push("vlc".into());
    #[cfg(target_os = "windows")]
    {
        // Registry is the most reliable source: VLC always writes its path there.
        if let Some(path) = vlc_from_registry() {
            v.push(path);
        }
        // Fallback: common default install locations.
        for var in &["PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMW6432"] {
            if let Ok(pf) = std::env::var(var) {
                v.push(format!(r"{}\VideoLAN\VLC\vlc.exe", pf));
            }
        }
    }
    v
}

/// Query the Windows registry for VLC's install path.
/// VLC writes `HKLM\SOFTWARE\VideoLAN\VLC` (default value) = path to vlc.exe.
#[cfg(target_os = "windows")]
fn vlc_from_registry() -> Option<String> {
    for key in &[
        r"HKLM\SOFTWARE\VideoLAN\VLC",
        r"HKLM\SOFTWARE\WOW6432Node\VideoLAN\VLC",
    ] {
        let out = Command::new("reg")
            .args(["query", key, "/ve"])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            if line.contains("REG_SZ") {
                if let Some(path) = line.splitn(3, "REG_SZ").nth(1) {
                    let path = path.trim().to_string();
                    if !path.is_empty() {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

/// Ask rqbit for the head of the stream so it starts fetching the first pieces
/// straight away, instead of sitting idle for the second or two the player
/// spends starting up before it makes its own first request. Fire-and-forget:
/// rqbit caches pieces, so the player's request is served from what this pulled.
fn warm_stream(url: &str) {
    let url = url.to_string();
    thread::spawn(move || {
        let _ = minreq::get(&url)
            .with_header("Range", "bytes=0-1048575")
            .with_timeout(30)
            .send();
    });
}

/// Spawns the media player with an appropriate title flag.
fn spawn_player(player_cmd: &str, url: &str, title: &str) -> std::io::Result<std::process::Child> {
    warm_stream(url);

    let mut parts = player_cmd.split_whitespace();
    let bin = parts.next().unwrap_or("mpv");
    let extra: Vec<&str> = parts.collect();

    let mut cmd = Command::new(bin);
    cmd.args(&extra);
    let bin_lc = bin.to_lowercase();
    if bin_lc.contains("mpv") {
        cmd.arg(format!("--force-media-title={}", title));
    } else if bin_lc.contains("vlc") {
        // `--opt=value`, not `--opt value`: given a detached value VLC treats it
        // as a second playlist entry, which leaves the stream queued but never
        // started (you have to double-click it in the playlist to play).
        cmd.arg(format!("--meta-title={}", title));
        // Hand the URL to a *fresh* instance. If one is already running, VLC
        // would otherwise enqueue into it rather than play.
        cmd.arg("--no-one-instance");
        // Quit when the stream ends so the temp dir gets cleaned up.
        cmd.arg("--play-and-exit");
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
