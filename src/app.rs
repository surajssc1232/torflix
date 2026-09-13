use crate::history::{self, Kind as HistoryKind};
use crate::omdb;
use crate::commands::Command as SlashCommand;
use crate::config::SearchSource;
use crate::favorites;
use crate::progress;
use crate::stremio::{self, Addon, MetaItem, Source, Stream};
use crate::tv;
use crate::rqbit::{Client, FileDetails, TorrentStats};
use crate::search::{self, SearchResult};
use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub fn download_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("TORFLIX_DOWNLOAD_DIR") {
        return std::path::PathBuf::from(dir);
    }
    if let Some(dir) = crate::config::load().download_dir.filter(|d| !d.trim().is_empty()) {
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
    pub info_hash: String,
    pub stats: Option<TorrentStats>,
}

#[derive(PartialEq, Clone)]
pub enum View {
    Home,
    Torrents,
    Files,
    AddInput,
    ConfirmDelete,
    ConfirmQuit,
    SearchResults,
    History,
    /// Catalog list: search results, a /browse list, or favorites.
    Discover,
    Details,
    Tv,
    Addons,
    Settings,
}

/// What the text-input popup is collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPurpose {
    Torrent,
    Playlist,
    Addon,
    DownloadDir,
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

/// Something fetched in the background.
pub enum Load<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Seasons,
    Episodes,
    Streams,
}

/// The title open in the details view.
pub struct Details {
    pub item: MetaItem,
    pub meta: Arc<Mutex<Load<stremio::MetaDetail>>>,
    pub pane: Pane,
    pub season_idx: usize,
    pub episode_idx: usize,
    /// Streams for `streams_for`. Replaced rather than mutated when the episode
    /// changes, so a slow reply for the previous episode can't land in the new list.
    pub streams: Arc<Mutex<Load<(Vec<Stream>, Vec<String>)>>>,
    pub streams_for: Option<(usize, usize)>,
    pub stream_selected: usize,
}

/// Looks subtitles up right before playback, on the background thread.
#[derive(Clone)]
pub struct SubtitleQuery {
    pub addons: Vec<Addon>,
    pub kind: String,
    pub id: String,
    pub season: usize,
    pub episode: usize,
    pub lang: String,
    pub release: String,
}

impl SubtitleQuery {
    /// The best few subtitle URLs, most likely to be in sync first.
    fn resolve(&self) -> Vec<String> {
        if self.lang.is_empty() || self.lang.eq_ignore_ascii_case("off") {
            return Vec::new();
        }
        let subs = stremio::subtitles(&self.addons, &self.kind, &self.id, self.season, self.episode);
        stremio::rank_subtitles(&subs, &self.lang, Some(&self.release))
            .into_iter()
            .take(MAX_SUBTITLES)
            .map(|s| s.url.clone())
            .collect()
    }

    /// Start the lookup in the background; the caller waits for it with a timeout.
    fn spawn(&self) -> std::sync::mpsc::Receiver<Vec<String>> {
        let (tx, rx) = channel();
        let query = self.clone();
        thread::spawn(move || {
            let _ = tx.send(query.resolve());
        });
        rx
    }
}

/// How long playback may wait for subtitles once the video itself is ready.
const SUBTITLE_WAIT: Duration = Duration::from_secs(4);

/// Subtitles handed to mpv: the best first, the rest a keypress (j) away if it's off.
const MAX_SUBTITLES: usize = 4;

/// Optional extras when streaming a torrent.
#[derive(Default)]
pub struct StreamExtras {
    /// Which file to play (a stream addon's fileIdx); otherwise the largest video.
    pub file_idx: Option<usize>,
    pub subtitles: Option<SubtitleQuery>,
}

#[derive(Clone, PartialEq)]
pub enum FileSortMode {
    Name,
    Size,
}

pub struct SearchPreview {
    pub result_idx: usize,
    /// What was added (magnet/URL) and the result's title — kept for history.
    pub target: String,
    pub title: String,
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

    // History view
    pub history: Vec<history::Entry>,
    pub history_selected: usize,

    /// Torrents added only to stream or preview. They live in temp dirs and are
    /// forgotten afterwards, so they don't count as downloads worth keeping alive.
    pub stream_ids: Arc<Mutex<HashSet<u64>>>,
    /// True when this process owns the engine, so quitting would stop downloads.
    pub embedded: bool,
    /// Set by the quit prompt: hand unfinished downloads to a background engine.
    pub keep_downloading: bool,
    /// View to go back to if the quit prompt is cancelled.
    pub quit_return_view: View,

    pub config: crate::config::Config,
    pub addons: Vec<Addon>,
    pub favorites: Vec<favorites::Favorite>,

    // Catalog list (catalog search, /browse, /favorites)
    pub catalog: Arc<Mutex<Load<Vec<MetaItem>>>>,
    pub catalog_title: String,
    pub catalog_selected: usize,
    /// Which /browse list is showing, if any.
    pub browse_idx: Option<usize>,
    pub details: Option<Details>,

    // Text-input popup
    pub input_purpose: InputPurpose,
    /// View to return to when the popup closes.
    pub input_return: View,

    // Live TV
    pub tv_channels: Arc<Mutex<Load<(Vec<tv::Channel>, Vec<String>)>>>,
    pub tv_playlists: Vec<String>,
    pub tv_filter: String,
    pub tv_filter_active: bool,
    pub tv_selected: usize,
    /// Showing the playlist list instead of channels.
    pub tv_manage: bool,
    pub tv_playlist_selected: usize,

    // Addons and settings
    pub addon_selected: usize,
    /// Filled by the background manifest fetch; picked up by `poll_background`.
    pub installed_addon: Arc<Mutex<Option<Addon>>>,
    pub settings_selected: usize,
    /// Auto-detected player, looked up when settings open (probing runs a process).
    pub detected_player: Option<String>,
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
            history: Vec::new(),
            history_selected: 0,
            stream_ids: Arc::new(Mutex::new(HashSet::new())),
            embedded: true,
            keep_downloading: false,
            quit_return_view: View::Home,
            config: crate::config::load(),
            addons: stremio::load_addons(),
            favorites: favorites::load(),
            catalog: Arc::new(Mutex::new(Load::Idle)),
            catalog_title: String::new(),
            catalog_selected: 0,
            browse_idx: None,
            details: None,
            input_purpose: InputPurpose::Torrent,
            input_return: View::Torrents,
            tv_channels: Arc::new(Mutex::new(Load::Idle)),
            tv_playlists: tv::load_config().playlists,
            tv_filter: String::new(),
            tv_filter_active: false,
            tv_selected: 0,
            tv_manage: false,
            tv_playlist_selected: 0,
            addon_selected: 0,
            installed_addon: Arc::new(Mutex::new(None)),
            settings_selected: 0,
            detected_player: None,
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
                            info_hash: t.info_hash,
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
        let hash = self
            .rows_snapshot()
            .into_iter()
            .find(|r| r.id == self.files_torrent_id)
            .map(|r| r.info_hash)
            .unwrap_or_default();
        let opts = PlayOpts::resuming(progress::torrent_key(&format!("magnet:?xt=urn:btih:{hash}"), stream_idx));
        self.launch_player(&url, &title, &opts);
    }

    pub fn play_playlist(&mut self) {
        let url = self.client.playlist_url(self.files_torrent_id);
        let title = self.files_torrent_name.clone();
        self.launch_player(&url, &title, &PlayOpts::default());
    }

    fn launch_player(&mut self, url: &str, title: &str, opts: &PlayOpts) {
        let Some(player_cmd) = find_player() else {
            self.status =
                "✗ no player found — install mpv or vlc, or set TORFLIX_PLAYER".into();
            return;
        };
        match spawn_player(&player_cmd, url, title, opts) {
            Ok(_) => self.status = format!("▶ playing: {}{} — buffering may take a moment", title, opts.resume_note()),
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
        self.add_and_play_with(target, label, StreamExtras::default());
    }

    /// Like `add_and_play_async`, optionally choosing the file and fetching subtitles.
    pub fn add_and_play_with(&mut self, target: &str, label: &str, extras: StreamExtras) {
        let client = self.client.clone();
        let tx = self.status_tx.clone();
        let target = target.to_string();
        let label = label.to_string();

        // Detect the player now — if none, fall back to permanent download.
        let player_cmd = find_player();

        if player_cmd.is_none() {
            history::record(&label, &target, HistoryKind::Download, None);
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
        history::record(&label, &target, HistoryKind::Stream, None);
        self.status = format!("⧗ adding: {} …", label);
        let stream_ids = Arc::clone(&self.stream_ids);

        thread::spawn(move || {
            // Look subtitles up while the torrent's metadata resolves, not after.
            let subtitles = extras.subtitles.as_ref().map(SubtitleQuery::spawn);
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
            stream_ids.lock().unwrap().insert(id);
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

            let requested = extras.file_idx.and_then(|i| details.files.get(i).map(|f| (i, f)));
            let best = requested.or_else(|| {
                details
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| is_video(&f.name))
                    .max_by_key(|(_, f)| f.length)
            });

            let Some((idx, file)) = best else {
                let _ = tx.send(format!(
                    "✓ added: {} (no video file — navigate manually)",
                    label
                ));
                return;
            };

            // Fetch just this file — the rest of a pack would compete with it for peers.
            client.update_only_files(id, &[idx]).ok();
            let url = client.stream_url(id, idx);
            let title = file.name.clone();
            let mut opts = PlayOpts::resuming(progress::torrent_key(&target, idx));
            if let Some(pending) = &subtitles {
                let _ = tx.send("⧗ looking for subtitles…".into());
                opts.subtitles = pending.recv_timeout(SUBTITLE_WAIT).unwrap_or_default();
            }

            match spawn_player(&player_cmd, &url, &title, &opts) {
                Ok(mut child) => {
                    let _ = tx.send(format!(
                        "▶ streaming: {}{}{}  [tmp: {}]",
                        title,
                        opts.resume_note(),
                        subtitle_note(&opts),
                        temp_dir.display()
                    ));
                    child.wait().ok();
                    client.forget(id).ok();
                    stream_ids.lock().unwrap().remove(&id);
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
        self.view = self.input_return.clone();
        match self.input_purpose {
            InputPurpose::Torrent => {
                if !input.is_empty() {
                    let label = snip_label(&input);
                    self.add_and_play_async(&input, &label);
                }
            }
            InputPurpose::Playlist => self.add_playlist(input),
            InputPurpose::Addon => self.install_addon(input),
            InputPurpose::DownloadDir => {
                self.config.download_dir = (!input.is_empty()).then_some(input);
                crate::config::save(&self.config);
                self.status = format!("downloads go to {}", download_dir().display());
            }
        }
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
                self.download_to_disk_async(&target, &r.title, None);
            }
            None => self.status = "✗ result has no magnet or download link".into(),
        }
    }

    /// Download permanently; `file_idx` limits it to one file of a multi-file torrent.
    fn download_to_disk_async(&mut self, target: &str, label: &str, file_idx: Option<usize>) {
        let client = self.client.clone();
        let tx = self.status_tx.clone();
        let target = target.to_string();
        let label = label.to_string();
        let dest = download_dir();
        history::record(&label, &target, HistoryKind::Download, Some(dest.display().to_string()));
        self.status = format!("⬇ queuing: {} → {}", label, dest.display());
        thread::spawn(move || {
            std::fs::create_dir_all(&dest).ok();
            match client.add_to_dir(&target, &dest) {
                Ok(id) => {
                    if let Some(idx) = file_idx {
                        client.update_only_files(id, &[idx]).ok();
                    }
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
                filtered.get(self.search_selected).and_then(|r| r.add_target().map(|t| (t.to_string(), r.title.clone())))
            }
            SearchStatus::Searching(partial) => {
                let v = partial.lock().unwrap();
                let filtered = self.filtered_results(&v);
                filtered.get(self.search_selected).and_then(|r| r.add_target().map(|t| (t.to_string(), r.title.clone())))
            }
            _ => None,
        };
        let Some((target, title)) = target else { return };

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
            target: target.clone(),
            title,
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
        let stream_ids = Arc::clone(&self.stream_ids);
        thread::spawn(move || {
            let id = match client.add_to_dir(&target, &temp_dir) {
                Ok(id) => id,
                Err(e) => {
                    *state.lock().unwrap() = PreviewState::Error(e.to_string());
                    std::fs::remove_dir_all(&temp_dir).ok();
                    return;
                }
            };
            stream_ids.lock().unwrap().insert(id);
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
                        // Rather than pulling the whole pack while the user browses,
                        // fetch only the likeliest pick until they actually choose.
                        let best = d
                            .files
                            .iter()
                            .enumerate()
                            .filter(|(_, f)| is_video(&f.name))
                            .max_by_key(|(_, f)| f.length)
                            .map(|(i, _)| i);
                        if let Some(best) = best {
                            client.update_only_files(id, &[best]).ok();
                        }
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
                self.stream_ids.lock().unwrap().remove(&id);
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
        history::record(
            &format!("{} — {}", preview.title, file_name),
            &preview.target,
            HistoryKind::Stream,
            None,
        );
        // Mark cancelled so the background loader thread won't touch it if still running
        *preview.cancelled.lock().unwrap() = true;

        let Some(player_cmd) = find_player() else {
            self.status = "✗ no player found — install mpv or vlc".into();
            self.client.forget(id).ok();
            std::fs::remove_dir_all(&temp_dir).ok();
            return;
        };

        // Switch the fetch over to the file they picked, and only that file.
        self.client.update_only_files(id, &[file_idx]).ok();
        let url = self.client.stream_url(id, file_idx);
        let client = self.client.clone();
        let tx = self.status_tx.clone();
        let stream_ids = Arc::clone(&self.stream_ids);
        let title = file_name.clone();
        let opts = PlayOpts::resuming(progress::torrent_key(&preview.target, file_idx));

        match spawn_player(&player_cmd, &url, &title, &opts) {
            Ok(mut child) => {
                self.status = format!("▶ streaming: {}{}", title, opts.resume_note());
                thread::spawn(move || {
                    child.wait().ok();
                    client.forget(id).ok();
                    stream_ids.lock().unwrap().remove(&id);
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
                self.download_to_disk_async(&target, &r.title, None);
            }
        }
    }

    // ---------- catalog (Stremio addons) ----------

    pub fn toggle_search_source(&mut self) {
        self.config.search_source = self.config.search_source.toggle();
        crate::config::save(&self.config);
        self.status = format!("search box now searches: {}", self.config.search_source.label());
    }

    /// Enter on the home screen: a slash command, or a search in the chosen source.
    pub fn submit_home(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.starts_with('/') {
            self.run_command(&q);
        } else if self.config.search_source == SearchSource::Catalog {
            self.start_catalog_search();
        } else {
            self.start_search();
        }
    }

    pub fn run_command(&mut self, input: &str) {
        self.search_query.clear();
        match SlashCommand::parse(input) {
            Some(SlashCommand::Browse) => self.open_browse(0),
            Some(SlashCommand::Favorites) => self.open_favorites(),
            Some(SlashCommand::History) => self.open_history(),
            Some(SlashCommand::Downloads) => self.view = View::Torrents,
            Some(SlashCommand::Help) => self.show_help = true,
            Some(SlashCommand::Quit) => self.request_quit(),
            Some(SlashCommand::Tv) => self.open_tv(),
            Some(SlashCommand::Addons) => self.open_addons(),
            Some(SlashCommand::Settings) => self.open_settings(),
            None => self.status = format!("✗ unknown command '{input}' — type / to see them"),
        }
    }

    fn load_catalog(
        &mut self,
        title: String,
        job: impl FnOnce() -> anyhow::Result<Vec<MetaItem>> + Send + 'static,
    ) {
        let slot = Arc::new(Mutex::new(Load::Loading));
        self.catalog = Arc::clone(&slot);
        self.catalog_title = title;
        self.catalog_selected = 0;
        self.view = View::Discover;
        thread::spawn(move || {
            let result = match job() {
                Ok(items) => Load::Ready(items),
                Err(e) => Load::Failed(format!("{e:#}")),
            };
            *slot.lock().unwrap() = result;
        });
    }

    pub fn start_catalog_search(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.browse_idx = None;
        let addons = self.addons.clone();
        self.load_catalog(format!("catalog — '{q}'"), move || stremio::search(&addons, &q));
    }

    /// Show one of the /browse lists.
    pub fn open_browse(&mut self, which: usize) {
        let targets = stremio::browse_targets(&self.addons);
        if targets.is_empty() {
            self.status = "✗ enable Cinemeta in /addons to browse".into();
            return;
        }
        let which = which % targets.len();
        self.browse_idx = Some(which);
        let target = targets[which].clone();
        self.load_catalog(target.label.to_string(), move || stremio::catalog(&target));
    }

    /// `b` in a /browse list: the next list.
    pub fn next_browse(&mut self) {
        if let Some(i) = self.browse_idx {
            self.open_browse(i + 1);
        }
    }

    pub fn open_favorites(&mut self) {
        self.favorites = favorites::load();
        let items = self
            .favorites
            .iter()
            .map(|f| MetaItem {
                id: f.id.clone(),
                kind: f.kind.clone(),
                name: f.name.clone(),
                poster: None,
                release_info: (!f.year.is_empty()).then(|| f.year.clone()),
                imdb_rating: None,
                genres: Vec::new(),
                description: None,
            })
            .collect();
        self.browse_idx = None;
        self.catalog = Arc::new(Mutex::new(Load::Ready(items)));
        self.catalog_title = "favorites".into();
        self.catalog_selected = 0;
        self.view = View::Discover;
    }

    pub fn catalog_len(&self) -> usize {
        match &*self.catalog.lock().unwrap() {
            Load::Ready(v) => v.len(),
            _ => 0,
        }
    }

    fn selected_catalog_item(&self) -> Option<MetaItem> {
        match &*self.catalog.lock().unwrap() {
            Load::Ready(v) => v.get(self.catalog_selected).cloned(),
            _ => None,
        }
    }

    pub fn open_details(&mut self) {
        if let Some(item) = self.selected_catalog_item() {
            self.open_details_for(item);
        }
    }

    fn open_details_for(&mut self, item: MetaItem) {
        let meta = Arc::new(Mutex::new(Load::Loading));
        let (slot, addons, kind, id) = (Arc::clone(&meta), self.addons.clone(), item.kind.clone(), item.id.clone());
        thread::spawn(move || {
            let result = match stremio::meta(&addons, &kind, &id) {
                Ok(m) => Load::Ready(m),
                Err(e) => Load::Failed(format!("{e:#}")),
            };
            *slot.lock().unwrap() = result;
        });
        let series = item.is_series();
        self.details = Some(Details {
            item,
            meta,
            pane: if series { Pane::Seasons } else { Pane::Streams },
            season_idx: 0,
            episode_idx: 0,
            streams: Arc::new(Mutex::new(Load::Idle)),
            streams_for: None,
            stream_selected: 0,
        });
        self.view = View::Details;
        if !series {
            self.load_streams(0, 0);
        }
    }

    /// Seasons of the open series; empty until its details load, and for movies.
    pub fn details_seasons(&self) -> Vec<stremio::Season> {
        let Some(d) = self.details.as_ref() else {
            return Vec::new();
        };
        let meta = d.meta.lock().unwrap();
        match &*meta {
            Load::Ready(m) => m.seasons(),
            _ => Vec::new(),
        }
    }

    fn load_streams(&mut self, season: usize, episode: usize) {
        let addons = self.addons.clone();
        let Some(d) = self.details.as_mut() else { return };
        if d.streams_for == Some((season, episode)) {
            return;
        }
        let slot = Arc::new(Mutex::new(Load::Loading));
        d.streams = Arc::clone(&slot);
        d.streams_for = Some((season, episode));
        d.stream_selected = 0;
        let (kind, id) = (d.item.kind.clone(), d.item.id.clone());
        let query = indexer_query(&d.item, season, episode);
        let name = d.item.name.clone();
        let year = d.item.year_label();
        thread::spawn(move || {
            let (mut streams, mut errors) = stremio::streams(&addons, &kind, &id, season, episode);
            // No stream addon, or nothing from it: use torflix's own torrent search.
            if streams.is_empty() {
                let backend = search::backend_from_env().expect("backend_from_env always returns Some");
                let partial = Arc::new(Mutex::new(Vec::new()));
                match search::search(&backend, &query, &partial) {
                    Ok(results) => streams = streams_from_results(results, &name, &year, season, episode),
                    Err(e) => errors.push(format!("torrent search: {e:#}")),
                }
                if streams.is_empty() && errors.is_empty() {
                    errors.push(format!("no torrents found for '{query}' — press t for a broader search"));
                }
            }
            let result = if streams.is_empty() && !errors.is_empty() {
                Load::Failed(errors.join("; "))
            } else {
                Load::Ready((streams, errors))
            };
            *slot.lock().unwrap() = result;
        });
    }

    fn stream_count(&self) -> usize {
        let Some(d) = self.details.as_ref() else { return 0 };
        let streams = d.streams.lock().unwrap();
        match &*streams {
            Load::Ready((s, _)) => s.len(),
            _ => 0,
        }
    }

    pub fn details_move(&mut self, down: bool) {
        let seasons = self.details_seasons();
        let streams = self.stream_count();
        let Some(d) = self.details.as_mut() else { return };
        let step = |i: usize, len: usize| {
            if len == 0 {
                0
            } else if down {
                (i + 1).min(len - 1)
            } else {
                i.saturating_sub(1)
            }
        };
        match d.pane {
            Pane::Seasons => {
                let next = step(d.season_idx, seasons.len());
                if next != d.season_idx {
                    d.season_idx = next;
                    d.episode_idx = 0;
                }
            }
            Pane::Episodes => {
                let len = seasons.get(d.season_idx).map_or(0, |s| s.episodes.len());
                d.episode_idx = step(d.episode_idx, len);
            }
            Pane::Streams => d.stream_selected = step(d.stream_selected, streams),
        }
    }

    /// Tab / Shift+Tab. Series go seasons → episodes → streams; movies only have streams.
    pub fn details_pane(&mut self, forward: bool) {
        let Some(d) = self.details.as_mut() else { return };
        if !d.item.is_series() {
            return;
        }
        d.pane = match (d.pane, forward) {
            (Pane::Seasons, true) => Pane::Episodes,
            (Pane::Episodes, true) if d.streams_for.is_some() => Pane::Streams,
            (Pane::Episodes, false) => Pane::Seasons,
            (Pane::Streams, false) => Pane::Episodes,
            (pane, _) => pane,
        };
    }

    pub fn details_enter(&mut self) {
        let Some(pane) = self.details.as_ref().map(|d| d.pane) else { return };
        match pane {
            Pane::Seasons => self.details_pane(true),
            Pane::Episodes => {
                if let Some(ep) = self.current_episode() {
                    self.load_streams(ep.season, ep.number);
                    if let Some(d) = self.details.as_mut() {
                        d.pane = Pane::Streams;
                    }
                }
            }
            Pane::Streams => self.play_selected_stream(),
        }
    }

    /// Esc: back a pane, then out to the list.
    pub fn details_back(&mut self) {
        let series = self.details.as_ref().map_or(false, |d| d.item.is_series());
        match self.details.as_ref().map(|d| d.pane) {
            Some(Pane::Streams) | Some(Pane::Episodes) if series => self.details_pane(false),
            _ => {
                self.details = None;
                self.view = View::Discover;
            }
        }
    }

    pub fn current_episode(&self) -> Option<stremio::Episode> {
        let d = self.details.as_ref()?;
        let seasons = self.details_seasons();
        seasons.get(d.season_idx)?.episodes.get(d.episode_idx).cloned()
    }

    /// "Breaking Bad S01E02 · Cat's in the Bag..." or "Inception (2010)".
    fn playing_label(&self) -> Option<String> {
        let d = self.details.as_ref()?;
        let (s, e) = d.streams_for?;
        if d.item.is_series() && e > 0 {
            let title = self
                .details_seasons()
                .iter()
                .find(|x| x.number == s)
                .and_then(|x| x.episodes.iter().find(|ep| ep.number == e))
                .map(|ep| ep.title.clone())
                .filter(|t| !t.is_empty());
            Some(match title {
                Some(t) => format!("{} S{s:02}E{e:02} · {t}", d.item.name),
                None => format!("{} S{s:02}E{e:02}", d.item.name),
            })
        } else {
            let year = d.item.year_label();
            Some(if year.is_empty() { d.item.name.clone() } else { format!("{} ({year})", d.item.name) })
        }
    }

    fn selected_stream(&self) -> Option<(Stream, String, SubtitleQuery)> {
        let d = self.details.as_ref()?;
        let stream = {
            let streams = d.streams.lock().unwrap();
            match &*streams {
                Load::Ready((s, _)) => s.get(d.stream_selected).cloned(),
                _ => None,
            }
        }?;
        let (season, episode) = d.streams_for?;
        let subs = SubtitleQuery {
            addons: self.addons.clone(),
            kind: d.item.kind.clone(),
            id: d.item.id.clone(),
            season,
            episode,
            lang: self.config.subtitle_lang.clone(),
            release: stream.release.clone(),
        };
        Some((stream, self.playing_label()?, subs))
    }

    pub fn play_selected_stream(&mut self) {
        let Some((stream, label, subs)) = self.selected_stream() else { return };
        match &stream.source {
            Source::Torrent { .. } => self.add_and_play_with(
                &stream.target(),
                &label,
                StreamExtras { file_idx: stream.file_idx(), subtitles: Some(subs) },
            ),
            Source::Http { .. } => self.play_http(stream, label, subs),
        }
    }

    fn play_http(&mut self, stream: Stream, label: String, subs: SubtitleQuery) {
        let Some(player_cmd) = find_player() else {
            self.status = "✗ no player found — install mpv or vlc".into();
            return;
        };
        let url = stream.target();
        history::record(&label, &url, HistoryKind::Stream, None);
        self.status = format!("⧗ opening: {label}");
        let tx = self.status_tx.clone();
        let headers = stream.headers().to_vec();
        let pending = subs.spawn();
        thread::spawn(move || {
            let mut opts = PlayOpts::resuming(progress::url_key(&url));
            opts.headers = headers;
            opts.subtitles = pending.recv_timeout(SUBTITLE_WAIT).unwrap_or_default();
            match spawn_player(&player_cmd, &url, &label, &opts) {
                Ok(mut child) => {
                    let _ = tx.send(format!("▶ playing: {label}{}{}", opts.resume_note(), subtitle_note(&opts)));
                    child.wait().ok();
                    let _ = tx.send(format!("✓ done: {label}"));
                }
                Err(e) => {
                    let _ = tx.send(format!("✗ couldn't launch '{player_cmd}': {e}"));
                }
            }
        });
    }

    pub fn download_selected_stream(&mut self) {
        let Some((stream, label, _)) = self.selected_stream() else { return };
        if stream.is_torrent() {
            self.download_to_disk_async(&stream.target(), &label, stream.file_idx());
        } else {
            self.status = "✗ only torrent streams can be downloaded".into();
        }
    }

    /// `t` in details: fall back to the torrent indexers for what's open.
    pub fn search_torrents_for_details(&mut self) {
        let Some(d) = self.details.as_ref() else { return };
        let q = match (d.item.is_series(), self.current_episode()) {
            (true, Some(ep)) => format!("{} S{:02}E{:02}", d.item.name, ep.season, ep.number),
            _ => format!("{} {}", d.item.name, d.item.year_label()),
        };
        self.search_query = q.trim().to_string();
        self.start_search();
    }

    pub fn toggle_favorite(&mut self) {
        let item = match self.view {
            View::Details => self.details.as_ref().map(|d| d.item.clone()),
            _ => self.selected_catalog_item(),
        };
        let Some(item) = item else { return };
        let fav = favorites::Favorite {
            id: item.id.clone(),
            kind: item.kind.clone(),
            name: item.name.clone(),
            year: item.year_label(),
            added_at: history::now(),
        };
        let starred = favorites::toggle(&mut self.favorites, fav);
        favorites::save(&self.favorites);
        self.status = if starred {
            format!("★ starred {}", item.name)
        } else {
            format!("☆ unstarred {}", item.name)
        };
    }

    pub fn is_favorite(&self, id: &str) -> bool {
        favorites::is_favorite(&self.favorites, id)
    }

    // ---------- text input ----------

    pub fn open_input(&mut self, purpose: InputPurpose) {
        self.input = match purpose {
            InputPurpose::DownloadDir => self.config.download_dir.clone().unwrap_or_default(),
            _ => String::new(),
        };
        self.input_purpose = purpose;
        if self.view != View::AddInput {
            self.input_return = self.view.clone();
        }
        self.view = View::AddInput;
    }

    pub fn cancel_input(&mut self) {
        self.input.clear();
        self.view = self.input_return.clone();
    }

    // ---------- live TV ----------

    pub fn open_tv(&mut self) {
        self.view = View::Tv;
        if matches!(*self.tv_channels.lock().unwrap(), Load::Idle) {
            self.reload_tv();
        }
    }

    pub fn reload_tv(&mut self) {
        let playlists = tv::load_config().playlists;
        self.tv_playlists = playlists.clone();
        self.tv_selected = 0;
        if playlists.is_empty() {
            self.tv_channels = Arc::new(Mutex::new(Load::Ready((Vec::new(), Vec::new()))));
            return;
        }
        let slot = Arc::new(Mutex::new(Load::Loading));
        self.tv_channels = Arc::clone(&slot);
        thread::spawn(move || {
            let loaded = tv::load_all(&playlists);
            *slot.lock().unwrap() = Load::Ready(loaded);
        });
    }

    pub fn tv_visible_len(&self) -> usize {
        match &*self.tv_channels.lock().unwrap() {
            Load::Ready((channels, _)) => tv::filter(channels, &self.tv_filter).len(),
            _ => 0,
        }
    }

    pub fn play_channel(&mut self) {
        let channel = match &*self.tv_channels.lock().unwrap() {
            Load::Ready((channels, _)) => tv::filter(channels, &self.tv_filter)
                .get(self.tv_selected)
                .map(|c| (*c).clone()),
            _ => None,
        };
        let Some(channel) = channel else { return };
        let Some(player_cmd) = find_player() else {
            self.status = "✗ no player found — install mpv or vlc".into();
            return;
        };
        match spawn_player(&player_cmd, &channel.url, &channel.name, &PlayOpts::default()) {
            Ok(mut child) => {
                self.status = format!("▶ live: {}", channel.name);
                thread::spawn(move || {
                    child.wait().ok();
                });
            }
            Err(e) => self.status = format!("✗ couldn't launch '{player_cmd}': {e}"),
        }
    }

    fn add_playlist(&mut self, source: String) {
        if source.is_empty() {
            return;
        }
        let mut cfg = tv::load_config();
        if !cfg.playlists.contains(&source) {
            cfg.playlists.push(source.clone());
            tv::save_config(&cfg);
        }
        self.status = format!("⧗ loading playlist {source}");
        self.reload_tv();
    }

    pub fn remove_playlist(&mut self) {
        let mut cfg = tv::load_config();
        if self.tv_playlist_selected >= cfg.playlists.len() {
            return;
        }
        let removed = cfg.playlists.remove(self.tv_playlist_selected);
        tv::save_config(&cfg);
        self.tv_playlist_selected = self.tv_playlist_selected.min(cfg.playlists.len().saturating_sub(1));
        self.status = format!("removed playlist {removed}");
        self.reload_tv();
    }

    // ---------- addons ----------

    pub fn open_addons(&mut self) {
        self.addons = stremio::load_addons();
        self.addon_selected = self.addon_selected.min(self.addons.len().saturating_sub(1));
        self.view = View::Addons;
    }

    fn install_addon(&mut self, url: String) {
        if url.is_empty() {
            return;
        }
        self.status = "⧗ fetching addon manifest…".into();
        let (slot, tx) = (Arc::clone(&self.installed_addon), self.status_tx.clone());
        thread::spawn(move || match stremio::fetch_addon(&url) {
            Ok(addon) => *slot.lock().unwrap() = Some(addon),
            Err(e) => {
                let _ = tx.send(format!("✗ couldn't install addon: {e:#}"));
            }
        });
    }

    /// Apply results of background work that change app state, not just the status line.
    pub fn poll_background(&mut self) {
        let installed = self.installed_addon.lock().unwrap().take();
        if let Some(addon) = installed {
            let (name, caps) = (addon.name.clone(), addon.capabilities());
            match self.addons.iter().position(|a| a.manifest_url == addon.manifest_url) {
                Some(i) => self.addons[i] = addon,
                None => self.addons.push(addon),
            }
            stremio::save_addons(&self.addons);
            self.status = if caps.is_empty() {
                format!("⚠ installed {name}, but it offers nothing torflix uses")
            } else {
                format!("✓ installed {name} — {caps}")
            };
        }
    }

    pub fn toggle_addon(&mut self) {
        let Some(addon) = self.addons.get_mut(self.addon_selected) else { return };
        if addon.is_core() {
            self.status = "Cinemeta powers search and details, so it stays on".into();
            return;
        }
        addon.enabled = !addon.enabled;
        let msg = format!("{} {}", addon.name, if addon.enabled { "enabled" } else { "disabled" });
        stremio::save_addons(&self.addons);
        self.status = msg;
    }

    pub fn remove_addon(&mut self) {
        let Some(addon) = self.addons.get(self.addon_selected) else { return };
        if addon.is_core() {
            self.status = "Cinemeta can't be removed".into();
            return;
        }
        let removed = self.addons.remove(self.addon_selected);
        stremio::save_addons(&self.addons);
        self.addon_selected = self.addon_selected.min(self.addons.len().saturating_sub(1));
        self.status = format!("removed {}", removed.name);
    }

    // ---------- settings ----------

    pub const SETTINGS_ROWS: usize = 5;

    pub fn open_settings(&mut self) {
        self.detected_player = player_candidates().into_iter().find(|c| player_exists(c));
        self.view = View::Settings;
    }

    /// (label, value) rows for the settings screen.
    pub fn settings_rows(&self) -> Vec<(&'static str, String)> {
        let env_player = std::env::var("TORFLIX_PLAYER").ok().filter(|p| !p.trim().is_empty());
        let player = match (env_player, &self.config.player) {
            (Some(env), _) => format!("{env}  (set by TORFLIX_PLAYER)"),
            (None, Some(p)) => p.clone(),
            (None, None) => format!("auto — {}", self.detected_player.as_deref().unwrap_or("none found")),
        };
        let lang = &self.config.subtitle_lang;
        let subtitles = if lang.eq_ignore_ascii_case("off") {
            "off".to_string()
        } else {
            format!("{} ({lang})", crate::config::subtitle_lang_label(lang))
        };
        vec![
            ("search box", self.config.search_source.label().to_string()),
            ("player", player),
            ("subtitles", subtitles),
            ("download folder", download_dir().display().to_string()),
            (
                "addons",
                format!(
                    "{} installed, {} enabled",
                    self.addons.len(),
                    self.addons.iter().filter(|a| a.enabled).count()
                ),
            ),
        ]
    }

    /// Enter / → / ←: change the selected setting.
    pub fn settings_change(&mut self, forward: bool) {
        match self.settings_selected {
            0 => self.config.search_source = self.config.search_source.toggle(),
            1 => {
                let options: [Option<&str>; 3] = [None, Some("mpv"), Some("vlc")];
                let current = options
                    .iter()
                    .position(|o| *o == self.config.player.as_deref())
                    .unwrap_or(0);
                let next = if forward { (current + 1) % 3 } else { (current + 2) % 3 };
                self.config.player = options[next].map(str::to_string);
            }
            2 => {
                let langs = crate::config::SUBTITLE_LANGS;
                let current = langs
                    .iter()
                    .position(|(code, _)| code.eq_ignore_ascii_case(&self.config.subtitle_lang))
                    .unwrap_or(0);
                let next = if forward {
                    (current + 1) % langs.len()
                } else {
                    (current + langs.len() - 1) % langs.len()
                };
                self.config.subtitle_lang = langs[next].0.to_string();
            }
            3 => return self.open_input(InputPurpose::DownloadDir),
            4 => return self.open_addons(),
            _ => return,
        }
        crate::config::save(&self.config);
    }

    // ---------- quitting ----------

    /// Unfinished downloads that quitting would stop: excludes streams/previews
    /// and anything paused or errored, which wouldn't progress anyway.
    pub fn active_download_count(&self) -> usize {
        let streams = self.stream_ids.lock().unwrap();
        self.rows_snapshot()
            .iter()
            .filter(|r| !streams.contains(&r.id))
            .filter(|r| match &r.stats {
                Some(s) => !s.finished && s.state != "paused" && s.state != "error",
                None => false,
            })
            .count()
    }

    /// Quit — but when this process owns the engine and downloads are still
    /// running, first ask whether to keep them going in the background.
    pub fn request_quit(&mut self) {
        if self.embedded && self.active_download_count() > 0 {
            if self.view != View::ConfirmQuit {
                self.quit_return_view = self.view.clone();
            }
            self.view = View::ConfirmQuit;
        } else {
            self.should_quit = true;
        }
    }

    // ---------- history ----------

    pub fn open_history(&mut self) {
        self.history = history::load();
        self.history_selected = self.history_selected.min(self.history.len().saturating_sub(1));
        self.view = View::History;
        self.status = "Enter: stream again   d: download   x: remove   Tab/Esc: home".into();
    }

    fn selected_history(&self) -> Option<history::Entry> {
        self.history.get(self.history_selected).cloned()
    }

    pub fn history_play(&mut self) {
        let Some(e) = self.selected_history() else { return };
        self.add_and_play_async(&e.target, &e.title);
        self.history = history::load();
        self.history_selected = 0;
    }

    pub fn history_download(&mut self) {
        let Some(e) = self.selected_history() else { return };
        self.download_to_disk_async(&e.target, &e.title, None);
        self.history = history::load();
        self.history_selected = 0;
    }

    pub fn history_remove(&mut self) {
        if self.history_selected >= self.history.len() {
            return;
        }
        self.history.remove(self.history_selected);
        history::save(&self.history);
        self.history_selected = self.history_selected.min(self.history.len().saturating_sub(1));
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
    if let Some(p) = crate::config::load().player.filter(|p| !p.trim().is_empty()) {
        return Some(p.trim().to_string());
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

/// Extras for a playback: where to resume, subtitles, and HTTP headers the source needs.
#[derive(Debug, Clone, Default)]
pub struct PlayOpts {
    /// Seconds to start from.
    pub start: Option<u64>,
    /// Subtitle URLs or paths, best first.
    pub subtitles: Vec<String>,
    pub headers: Vec<(String, String)>,
    /// Progress key: mpv records the position under it.
    pub track_key: Option<String>,
}

impl PlayOpts {
    /// Track progress under `key`, starting from wherever it was left.
    pub fn resuming(key: String) -> Self {
        Self {
            start: progress::resume_at(&key),
            track_key: Some(key),
            ..Self::default()
        }
    }

    pub fn resume_note(&self) -> String {
        match self.start {
            Some(s) => format!(" (resuming at {})", format_clock(s)),
            None => String::new(),
        }
    }
}

/// What to ask the torrent indexers for: "The Office S01E03" or "Inception 2010".
fn indexer_query(item: &MetaItem, season: usize, episode: usize) -> String {
    if item.is_series() && episode > 0 {
        format!("{} S{season:02}E{episode:02}", item.name)
    } else {
        format!("{} {}", item.name, item.year_label()).trim().to_string()
    }
}

fn normalize_title(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Is `release` for this title? It has to start with the name, and whatever sits
/// between the name and the episode tag can only be the show's year or a country
/// code — so "The Office US S01E03" passes but "The Office Movers S01E03" and the
/// 2024 "The Office" don't. For movies, the word after the name must be the year.
fn title_matches(release: &str, name: &str, year: &str, series: bool) -> bool {
    const COUNTRIES: &[&str] = &["us", "uk", "au", "ca", "nz"];
    let name = normalize_title(name);
    let release = normalize_title(release);
    let name_words: Vec<&str> = name.split(' ').collect();
    let words: Vec<&str> = release.split(' ').collect();
    if name.is_empty() || words.len() <= name_words.len() || words[..name_words.len()] != name_words[..] {
        return false;
    }
    let year: Option<i32> = year.get(..4).and_then(|y| y.parse().ok());
    let is_year = |w: &str| w.len() == 4 && w.chars().all(|c| c.is_ascii_digit());
    // Release years are often off by one from the catalog's.
    let right_year = |w: &str| match (year, w.parse::<i32>()) {
        (Some(want), Ok(got)) => (want - got).abs() <= 1,
        _ => true,
    };
    let rest = &words[name_words.len()..];
    if series {
        for w in rest {
            if stremio::parse_season_episode(w).is_some() {
                return true;
            }
            if !(COUNTRIES.contains(w) || (is_year(w) && right_year(w))) {
                return false;
            }
        }
        false
    } else {
        matches!(rest.first(), Some(w) if is_year(w) && right_year(w))
    }
}

/// Indexer results for a catalog title, as streams. Episodes keep only exact
/// SxxEyy releases: a season pack would need the right file picked out of it.
fn streams_from_results(results: Vec<SearchResult>, name: &str, year: &str, season: usize, episode: usize) -> Vec<Stream> {
    let mut streams: Vec<Stream> = results
        .into_iter()
        .filter_map(|r| {
            let info_hash = progress::info_hash_of(r.magnet.as_deref()?)?;
            if !title_matches(&r.title, name, year, episode > 0) {
                return None;
            }
            if episode > 0 && stremio::parse_season_episode(&r.title) != Some((season, episode)) {
                return None;
            }
            Some(Stream {
                addon: "torrent search".into(),
                source: Source::Torrent { info_hash, file_idx: None, trackers: Vec::new() },
                quality: stremio::parse_quality(&r.title),
                codec: stremio::parse_codec(&r.title),
                languages: stremio::parse_audio_tracks(&r.title),
                size: (r.size > 0).then_some(r.size),
                seeders: u64::try_from(r.seeders).ok(),
                origin: Some(r.indexer.clone()),
                release: r.title,
            })
        })
        .collect();
    stremio::rank(&mut streams);
    streams
}

fn subtitle_note(opts: &PlayOpts) -> String {
    match opts.subtitles.len() {
        0 => String::new(),
        1 => " + subtitles (z/x in mpv to shift timing)".to_string(),
        n => format!(" + {n} subtitles (j in mpv for the next if out of sync, z/x to shift)"),
    }
}

pub fn format_clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// VLC can't fetch subtitles itself, so download them next to the temp streams.
fn local_subtitle(url: &str) -> Option<String> {
    if !url.starts_with("http") {
        return Some(url.to_string());
    }
    let resp = minreq::get(url).with_timeout(15).send().ok()?;
    if resp.status_code >= 400 {
        return None;
    }
    let path = std::env::temp_dir().join(format!("torflix-sub-{:016x}.srt", crate::config::stable_hash(url)));
    std::fs::write(&path, resp.as_bytes()).ok()?;
    Some(path.display().to_string())
}

/// Spawns the media player with title, resume, subtitle and header flags.
fn spawn_player(player_cmd: &str, url: &str, title: &str, opts: &PlayOpts) -> std::io::Result<std::process::Child> {
    // Only rqbit streams benefit from pre-fetching; other hosts may need headers.
    if url.contains("/torrents/") && url.contains("/stream/") {
        warm_stream(url);
    }

    let mut parts = player_cmd.split_whitespace();
    let bin = parts.next().unwrap_or("mpv");
    let extra: Vec<&str> = parts.collect();

    let mut cmd = Command::new(bin);
    cmd.args(&extra);
    let bin_lc = bin.to_lowercase();
    if bin_lc.contains("mpv") {
        cmd.arg(format!("--force-media-title={}", title));
        if let Some(start) = opts.start {
            cmd.arg(format!("--start={start}"));
        }
        for sub in &opts.subtitles {
            cmd.arg(format!("--sub-file={sub}"));
        }
        if let Some(key) = &opts.track_key {
            cmd.args(progress::mpv_tracking_args(key));
        }
        let mut fields = Vec::new();
        for (name, value) in &opts.headers {
            if name.eq_ignore_ascii_case("user-agent") {
                cmd.arg(format!("--user-agent={value}"));
            } else if name.eq_ignore_ascii_case("referer") {
                cmd.arg(format!("--referrer={value}"));
            } else {
                fields.push(format!("{name}: {value}"));
            }
        }
        if !fields.is_empty() {
            cmd.arg(format!("--http-header-fields={}", fields.join(",")));
        }
    } else if bin_lc.contains("vlc") {
        if let Some(start) = opts.start {
            cmd.arg(format!("--start-time={start}"));
        }
        // VLC takes one subtitle file; give it the best match.
        if let Some(sub) = opts.subtitles.first().and_then(|s| local_subtitle(s)) {
            cmd.arg(format!("--sub-file={sub}"));
        }
        for (name, value) in &opts.headers {
            if name.eq_ignore_ascii_case("referer") {
                cmd.arg(format!("--http-referrer={value}"));
            } else if name.eq_ignore_ascii_case("user-agent") {
                cmd.arg(format!("--http-user-agent={value}"));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn result(title: &str, hash: &str, seeders: i64) -> SearchResult {
        SearchResult {
            title: title.into(),
            size: 700_000_000,
            seeders,
            leechers: 1,
            magnet: Some(search::build_magnet(hash, title)),
            link: None,
            indexer: "Knaben".into(),
            rating: None,
        }
    }

    fn item(name: &str, kind: &str, year: &str) -> MetaItem {
        MetaItem {
            id: "tt1".into(),
            kind: kind.into(),
            name: name.into(),
            poster: None,
            release_info: Some(year.into()),
            imdb_rating: None,
            genres: vec![],
            description: None,
        }
    }

    #[test]
    fn indexer_results_become_episode_streams() {
        let h = |c: char| c.to_string().repeat(40);
        let results = vec![
            result("The.Office.US.S01E03.720p.WEB", &h('a'), 50),
            result("The Office S01E04 1080p", &h('b'), 90),
            result("The.Office.S01.Complete.1080p", &h('c'), 200),
            result("Parks and Recreation S01E03 1080p", &h('d'), 80),
            SearchResult { magnet: None, link: Some("https://x/t.torrent".into()), ..result("The Office S01E03 480p", &h('e'), 10) },
            result("The.Office.S01E03.1080p.BluRay.x264", &h('f'), 20),
            // Real results for this query that belong to other shows.
            result("The Office Movers S01E03 Pop-Up 1080p AMZN WEB", &h('g'), 30),
            result("The.Office.2024.S01E03.480p.x264-RUBiK", &h('i'), 30),
            result("At.The.Office.Microwave.S01E03.720p.HEVC.x265", &h('j'), 30),
            result("Joe and Davids Magical Sitcom Tour S01E03 The Office", &h('k'), 30),
        ];
        let s = streams_from_results(results, "The Office", "2005–2013", 1, 3);
        let releases: Vec<&str> = s.iter().map(|x| x.release.as_str()).collect();
        assert_eq!(
            releases,
            ["The.Office.S01E03.1080p.BluRay.x264", "The.Office.US.S01E03.720p.WEB"],
            "other episodes, packs, other shows and magnet-less results are dropped; 1080p ranks first"
        );
        assert_eq!(s[0].seeders, Some(20));
        assert_eq!(s[0].origin.as_deref(), Some("Knaben"));
        assert!(matches!(&s[0].source, Source::Torrent { info_hash, file_idx: None, .. } if *info_hash == h('f')));
    }

    #[test]
    fn movie_results_need_the_title_then_the_year() {
        let h = |c: char| c.to_string().repeat(40);
        let results = vec![
            result("Inception.2010.1080p.BluRay", &h('a'), 30),
            result("Inception 2011 720p", &h('b'), 30),
            result("Inception Behind the Scenes 2010", &h('c'), 30),
            result("Inception 1999 VHS", &h('d'), 30),
            result("Inception 1080p no year", &h('e'), 30),
        ];
        let s = streams_from_results(results, "Inception", "2010", 0, 0);
        let mut releases: Vec<&str> = s.iter().map(|x| x.release.as_str()).collect();
        releases.sort();
        assert_eq!(releases, ["Inception 2011 720p", "Inception.2010.1080p.BluRay"], "year within one, right after the title");
    }

    #[test]
    fn show_title_matching() {
        assert!(title_matches("The Office US S01E03 Health Care", "The Office", "2005–2013", true));
        assert!(title_matches("The.Office.2005.S01E03.720p", "The Office", "2005–2013", true));
        assert!(!title_matches("The.Office.S01.Complete", "The Office", "2005–2013", true), "no episode tag");
        assert!(!title_matches("The Office", "The Office", "2005", true), "nothing after the name");
        assert!(title_matches("Grey's Anatomy S02E01", "Grey's Anatomy", "2005", true), "punctuation in the name");
    }

    #[test]
    fn indexer_queries() {
        assert_eq!(indexer_query(&item("The Office", "series", "2005–2013"), 1, 3), "The Office S01E03");
        assert_eq!(indexer_query(&item("Inception", "movie", "2010"), 0, 0), "Inception 2010");
    }
}
