//! Stremio addon protocol: catalog search and metadata (Cinemeta), torrent and
//! HTTP streams, and subtitles (OpenSubtitles v3).
//!
//! The protocol types, lenient JSON handling and release-name parsers are
//! adapted from MovieBox-Tui (https://github.com/mesamirh/MovieBox-Tui),
//! Copyright (c) 2026 MovieBox Contributors, used under the MIT license — see
//! THIRD_PARTY_NOTICES.md. Reworked here for blocking I/O and torrent streams.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::mpsc;
use std::thread;

const UA: &str = concat!("torflix/", env!("CARGO_PKG_VERSION"));
pub const CINEMETA: &str = "https://v3-cinemeta.strem.io/manifest.json";
pub const OPENSUBTITLES: &str = "https://opensubtitles-v3.strem.io/manifest.json";

// ---------------------------------------------------------------------------
// Installed addons
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Addon {
    pub manifest_url: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub catalog: bool,
    #[serde(default)]
    pub meta: bool,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub subtitles: bool,
}

fn yes() -> bool {
    true
}

impl Addon {
    pub fn cinemeta() -> Self {
        Self {
            manifest_url: CINEMETA.into(),
            name: "Cinemeta".into(),
            version: None,
            enabled: true,
            catalog: true,
            meta: true,
            stream: false,
            subtitles: false,
        }
    }

    pub fn opensubtitles() -> Self {
        Self {
            manifest_url: OPENSUBTITLES.into(),
            name: "OpenSubtitles v3".into(),
            version: None,
            enabled: true,
            catalog: false,
            meta: false,
            stream: false,
            subtitles: true,
        }
    }

    /// Cinemeta backs search, browse and details, so it can't be removed or disabled.
    pub fn is_core(&self) -> bool {
        self.manifest_url == CINEMETA
    }

    pub fn base_url(&self) -> String {
        base_url(&self.manifest_url)
    }

    pub fn from_manifest(manifest_url: String, m: &Manifest) -> Self {
        Self {
            manifest_url,
            name: m.name.trim().to_string(),
            version: m.version.clone(),
            enabled: true,
            catalog: m.provides("catalog") || !m.catalogs.is_empty(),
            meta: m.provides("meta"),
            stream: m.provides("stream"),
            subtitles: m.provides("subtitles"),
        }
    }

    /// e.g. "catalog · meta · streams".
    pub fn capabilities(&self) -> String {
        let caps: Vec<&str> = [
            (self.catalog, "catalog"),
            (self.meta, "meta"),
            (self.stream, "streams"),
            (self.subtitles, "subtitles"),
        ]
        .iter()
        .filter(|(has, _)| *has)
        .map(|(_, name)| *name)
        .collect();
        caps.join(" · ")
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Resource {
    Name(String),
    Detailed { name: String },
}

impl Resource {
    fn name(&self) -> &str {
        match self {
            Self::Name(n) | Self::Detailed { name: n } => n,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    resources: Vec<Resource>,
    #[serde(default)]
    catalogs: Vec<serde_json::Value>,
}

impl Manifest {
    fn provides(&self, resource: &str) -> bool {
        self.resources
            .iter()
            .any(|r| r.name().eq_ignore_ascii_case(resource))
    }
}

fn addons_path() -> std::path::PathBuf {
    crate::config::config_dir().join("addons.json")
}

/// Installed addons; first run gets Cinemeta and OpenSubtitles.
pub fn load_addons() -> Vec<Addon> {
    let path = addons_path();
    if !path.exists() {
        let defaults = vec![Addon::cinemeta(), Addon::opensubtitles()];
        save_addons(&defaults);
        return defaults;
    }
    let mut list: Vec<Addon> = crate::config::load_json(&path);
    ensure_core(&mut list);
    list
}

fn ensure_core(list: &mut Vec<Addon>) {
    match list.iter_mut().find(|a| a.is_core()) {
        Some(core) => core.enabled = true,
        None => list.insert(0, Addon::cinemeta()),
    }
}

pub fn save_addons(list: &[Addon]) {
    crate::config::write_json(&addons_path(), list);
}

/// Fetch a manifest and describe the addon it belongs to (doesn't install it).
pub fn fetch_addon(raw_url: &str) -> Result<Addon> {
    let url = normalize_manifest_url(raw_url);
    let value = get_json(&url)?;
    let manifest: Manifest =
        serde_json::from_value(value).context("that URL isn't a Stremio addon manifest")?;
    if manifest.name.trim().is_empty() {
        bail!("addon manifest has no name");
    }
    Ok(Addon::from_manifest(url, &manifest))
}

/// Accepts `stremio://…`, a bare host, or a base URL, and returns the manifest URL.
pub fn normalize_manifest_url(raw: &str) -> String {
    let mut url = raw.trim().to_string();
    if let Some(rest) = url.strip_prefix("stremio://") {
        url = format!("https://{rest}");
    } else if !url.starts_with("http://") && !url.starts_with("https://") {
        url = format!("https://{url}");
    }
    if !url.ends_with("/manifest.json") && !url.contains("/manifest.json?") {
        if !url.ends_with('/') {
            url.push('/');
        }
        url.push_str("manifest.json");
    }
    url
}

pub fn base_url(manifest_url: &str) -> String {
    let normalized = normalize_manifest_url(manifest_url);
    match normalized.rfind("/manifest.json") {
        Some(pos) => normalized[..pos].to_string(),
        None => normalized.trim_end_matches('/').to_string(),
    }
}

fn host_of(url: &str) -> &str {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

fn get_json(url: &str) -> Result<serde_json::Value> {
    let resp = minreq::get(url)
        .with_header("User-Agent", UA)
        .with_timeout(15)
        .send()
        .with_context(|| format!("could not reach {}", host_of(url)))?;
    if resp.status_code >= 400 {
        bail!("{} returned HTTP {}", host_of(url), resp.status_code);
    }
    resp.json()
        .with_context(|| format!("{} sent invalid JSON", host_of(url)))
}

// ---------------------------------------------------------------------------
// Catalog and metadata
// ---------------------------------------------------------------------------

pub fn is_series_kind(kind: &str) -> bool {
    ["series", "tv", "anime"]
        .iter()
        .any(|k| kind.eq_ignore_ascii_case(k))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaItem {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(rename = "releaseInfo", default, deserialize_with = "opt_string_or_number")]
    pub release_info: Option<String>,
    #[serde(rename = "imdbRating", default, deserialize_with = "opt_string_or_number")]
    pub imdb_rating: Option<String>,
    #[serde(default, deserialize_with = "string_or_vec")]
    pub genres: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl MetaItem {
    pub fn is_series(&self) -> bool {
        is_series_kind(&self.kind)
    }

    pub fn year_label(&self) -> String {
        self.release_info.as_deref().map(extract_year).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MetaVideo {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "opt_usize_lenient")]
    pub season: Option<usize>,
    #[serde(default, deserialize_with = "opt_usize_lenient")]
    pub episode: Option<usize>,
    #[serde(default, deserialize_with = "opt_usize_lenient")]
    pub number: Option<usize>,
    #[serde(default)]
    pub released: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MetaDetail {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "releaseInfo", default, deserialize_with = "opt_string_or_number")]
    pub release_info: Option<String>,
    #[serde(default, deserialize_with = "opt_string_or_number")]
    pub year: Option<String>,
    #[serde(rename = "imdbRating", default, deserialize_with = "opt_string_or_number")]
    pub imdb_rating: Option<String>,
    #[serde(default, deserialize_with = "opt_string_or_number")]
    pub runtime: Option<String>,
    #[serde(default, deserialize_with = "string_or_vec")]
    pub genres: Vec<String>,
    #[serde(default, deserialize_with = "string_or_vec")]
    pub genre: Vec<String>,
    #[serde(default, deserialize_with = "string_or_vec")]
    pub cast: Vec<String>,
    #[serde(default, deserialize_with = "string_or_vec")]
    pub director: Vec<String>,
    #[serde(default)]
    pub videos: Vec<MetaVideo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub season: usize,
    pub number: usize,
    pub title: String,
    pub released: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Season {
    pub number: usize,
    pub episodes: Vec<Episode>,
}

impl MetaDetail {
    pub fn is_series(&self) -> bool {
        is_series_kind(&self.kind) || !self.videos.is_empty()
    }

    pub fn year_label(&self) -> String {
        self.release_info
            .as_deref()
            .or(self.year.as_deref())
            .map(|raw| {
                // Keep ranges like "2008-2013" intact; otherwise pull out the year.
                if raw.len() <= 9 && raw.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '–') {
                    raw.trim_end_matches(['-', '–']).to_string()
                } else {
                    extract_year(raw)
                }
            })
            .unwrap_or_default()
    }

    pub fn all_genres(&self) -> &[String] {
        if self.genres.is_empty() {
            &self.genre
        } else {
            &self.genres
        }
    }

    /// Seasons in order, with specials (season 0) last so season 1 is picked first.
    pub fn seasons(&self) -> Vec<Season> {
        let mut map: BTreeMap<usize, BTreeMap<usize, Episode>> = BTreeMap::new();
        for v in &self.videos {
            let season = v.season.unwrap_or(1);
            let Some(number) = v.episode.or(v.number) else {
                continue;
            };
            let title = v
                .name
                .clone()
                .or_else(|| v.title.clone())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_default();
            map.entry(season).or_default().entry(number).or_insert(Episode {
                season,
                number,
                title,
                released: v.released.clone(),
            });
        }
        let mut seasons: Vec<Season> = map
            .into_iter()
            .map(|(number, eps)| Season {
                number,
                episodes: eps.into_values().collect(),
            })
            .collect();
        if let Some(pos) = seasons.iter().position(|s| s.number == 0) {
            let specials = seasons.remove(pos);
            seasons.push(specials);
        }
        seasons
    }
}

fn metas_from(value: &serde_json::Value) -> Vec<MetaItem> {
    let list = value
        .get("metas")
        .or_else(|| value.get("items"))
        .or_else(|| value.get("results"))
        .unwrap_or(value);
    // Parse item by item, so one malformed entry doesn't sink the whole page.
    list.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|m| serde_json::from_value::<MetaItem>(m.clone()).ok())
                .filter(|m| !m.id.is_empty() && !m.name.trim().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Alternate movies and series so the top hits of both stay near the top.
fn interleave(movies: Vec<MetaItem>, series: Vec<MetaItem>) -> Vec<MetaItem> {
    let mut out = Vec::with_capacity(movies.len() + series.len());
    let mut m = movies.into_iter();
    let mut s = series.into_iter();
    loop {
        match (m.next(), s.next()) {
            (None, None) => break,
            (a, b) => out.extend(a.into_iter().chain(b)),
        }
    }
    out
}

/// Best title matches first — exact, then starts-with, then contains — keeping
/// the catalog's own order within each group. Otherwise alternating movies and
/// series can bury the obvious hit ("breaking bad" put El Camino above the show).
fn rank_by_title(query: &str, items: &mut [MetaItem]) {
    fn normalize(s: &str) -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }
    let q = normalize(query);
    if q.is_empty() {
        return;
    }
    items.sort_by_key(|m| {
        let name = normalize(&m.name);
        if name == q {
            0
        } else if name.starts_with(&q) {
            1
        } else if name.contains(&q) {
            2
        } else {
            3
        }
    });
}

/// Search enabled catalog addons for movies and series matching `query`.
pub fn search(addons: &[Addon], query: &str) -> Result<Vec<MetaItem>> {
    let catalogs: Vec<&Addon> = addons.iter().filter(|a| a.enabled && a.catalog).collect();
    if catalogs.is_empty() {
        bail!("no catalog addon is enabled — check /addons");
    }
    let q = crate::search::urlencode(query.trim());
    let (tx, rx) = mpsc::channel();
    for (order, addon) in catalogs.iter().enumerate() {
        for kind in ["movie", "series"] {
            let url = format!("{}/catalog/{}/top/search={}.json", addon.base_url(), kind, q);
            let tx = tx.clone();
            thread::spawn(move || {
                let _ = tx.send((order, kind, get_json(&url).map(|v| metas_from(&v))));
            });
        }
    }
    drop(tx);

    let mut by_addon: BTreeMap<usize, (Vec<MetaItem>, Vec<MetaItem>)> = BTreeMap::new();
    let mut errors = Vec::new();
    for (order, kind, result) in rx {
        match result {
            Ok(items) => {
                let slot = by_addon.entry(order).or_default();
                if kind == "movie" {
                    slot.0 = items;
                } else {
                    slot.1 = items;
                }
            }
            Err(e) => errors.push(e.to_string()),
        }
    }
    let mut seen = HashSet::new();
    let mut items: Vec<MetaItem> = by_addon
        .into_values()
        .flat_map(|(movies, series)| interleave(movies, series))
        .filter(|m| seen.insert(m.id.clone()))
        .collect();
    rank_by_title(query, &mut items);
    if items.is_empty() {
        if let Some(e) = errors.into_iter().next() {
            bail!(e);
        }
    }
    Ok(items)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTarget {
    pub label: &'static str,
    pub base: String,
    pub kind: &'static str,
    pub id: &'static str,
}

/// The curated lists shown by /browse, from Cinemeta.
pub fn browse_targets(addons: &[Addon]) -> Vec<CatalogTarget> {
    let Some(core) = addons.iter().find(|a| a.is_core() && a.enabled) else {
        return Vec::new();
    };
    let base = core.base_url();
    [
        ("Popular movies", "movie", "top"),
        ("Popular series", "series", "top"),
        ("Top rated movies", "movie", "imdbRating"),
        ("Top rated series", "series", "imdbRating"),
    ]
    .into_iter()
    .map(|(label, kind, id)| CatalogTarget {
        label,
        base: base.clone(),
        kind,
        id,
    })
    .collect()
}

pub fn catalog(target: &CatalogTarget) -> Result<Vec<MetaItem>> {
    let url = format!("{}/catalog/{}/{}.json", target.base, target.kind, target.id);
    Ok(metas_from(&get_json(&url)?))
}

/// Full details (including every episode for series) from the first meta addon that has them.
pub fn meta(addons: &[Addon], kind: &str, id: &str) -> Result<MetaDetail> {
    let mut last_err = None;
    for addon in addons.iter().filter(|a| a.enabled && a.meta) {
        let url = format!("{}/meta/{}/{}.json", addon.base_url(), kind, id);
        let attempt = get_json(&url).and_then(|v| {
            let body = v.get("meta").cloned().unwrap_or(v);
            serde_json::from_value::<MetaDetail>(body).context("invalid metadata")
        });
        match attempt {
            Ok(d) if !d.name.trim().is_empty() => return Ok(d),
            Ok(_) => last_err = Some(anyhow!("{} has no details for {}", addon.name, id)),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("no metadata addon is enabled — check /addons")))
}

/// The id stream and subtitle addons expect: `tt123` for movies, `tt123:1:2` for episodes.
pub fn video_id(kind: &str, id: &str, season: usize, episode: usize) -> String {
    if is_series_kind(kind) && episode > 0 {
        format!("{id}:{season}:{episode}")
    } else {
        id.to_string()
    }
}

// ---------------------------------------------------------------------------
// Streams
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Torrent {
        info_hash: String,
        file_idx: Option<usize>,
        trackers: Vec<String>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub addon: String,
    pub source: Source,
    /// Release name, e.g. "Breaking.Bad.S01E01.1080p.BluRay.x264".
    pub release: String,
    pub quality: Option<String>,
    pub codec: Option<String>,
    pub languages: Option<String>,
    pub size: Option<u64>,
    pub seeders: Option<u64>,
    /// Where the addon found it (an indexer, say), when it says.
    pub origin: Option<String>,
}

impl Stream {
    pub fn is_torrent(&self) -> bool {
        matches!(self.source, Source::Torrent { .. })
    }

    /// A magnet for rqbit, or the URL to hand the player.
    pub fn target(&self) -> String {
        match &self.source {
            Source::Torrent {
                info_hash,
                trackers,
                ..
            } => {
                if trackers.is_empty() {
                    return crate::search::build_magnet(info_hash, &self.release);
                }
                let mut m = format!(
                    "magnet:?xt=urn:btih:{}&dn={}",
                    info_hash,
                    crate::search::urlencode(&self.release)
                );
                for t in trackers {
                    m.push_str("&tr=");
                    m.push_str(t);
                }
                m
            }
            Source::Http { url, .. } => url.clone(),
        }
    }

    pub fn file_idx(&self) -> Option<usize> {
        match &self.source {
            Source::Torrent { file_idx, .. } => *file_idx,
            Source::Http { .. } => None,
        }
    }

    pub fn headers(&self) -> &[(String, String)] {
        match &self.source {
            Source::Http { headers, .. } => headers,
            Source::Torrent { .. } => &[],
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawStream {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(rename = "infoHash", default)]
    info_hash: Option<String>,
    #[serde(rename = "fileIdx", default, deserialize_with = "opt_usize_lenient")]
    file_idx: Option<usize>,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(rename = "behaviorHints", default)]
    hints: Option<Hints>,
}

#[derive(Debug, Default, Deserialize)]
struct Hints {
    #[serde(rename = "videoSize", default)]
    video_size: Option<u64>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    headers: Option<HashMap<String, String>>,
    #[serde(rename = "proxyHeaders", default)]
    proxy_headers: Option<ProxyHeaders>,
}

#[derive(Debug, Default, Deserialize)]
struct ProxyHeaders {
    #[serde(default)]
    request: Option<HashMap<String, String>>,
}

fn raw_streams(value: &serde_json::Value) -> Vec<RawStream> {
    value
        .get("streams")
        .unwrap_or(value)
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|s| serde_json::from_value(s.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn is_info_hash(h: &str) -> bool {
    (h.len() == 40 && h.chars().all(|c| c.is_ascii_hexdigit()))
        || (h.len() == 32 && h.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn to_stream(addon: &str, raw: RawStream, season: usize, episode: usize) -> Option<Stream> {
    let text = [raw.name.as_deref(), raw.title.as_deref(), raw.description.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");

    // Stream addons sometimes return neighbouring episodes; keep only the requested one.
    if season > 0 && episode > 0 {
        if let Some(found) = parse_season_episode(&text) {
            if found != (season, episode) {
                return None;
            }
        }
    }

    let hints = raw.hints.unwrap_or_default();
    let source = if let Some(hash) = raw.info_hash.filter(|h| is_info_hash(h)) {
        let trackers = raw
            .sources
            .iter()
            .filter_map(|s| s.strip_prefix("tracker:"))
            .map(str::to_string)
            .collect();
        Source::Torrent {
            info_hash: hash.to_ascii_lowercase(),
            file_idx: raw.file_idx,
            trackers,
        }
    } else if let Some(url) = raw
        .url
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
    {
        let mut headers: Vec<(String, String)> =
            hints.headers.clone().unwrap_or_default().into_iter().collect();
        if let Some(req) = hints.proxy_headers.as_ref().and_then(|p| p.request.clone()) {
            headers.extend(req);
        }
        headers.sort();
        headers.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
        Source::Http { url, headers }
    } else {
        return None;
    };

    let release = raw
        .title
        .as_deref()
        .and_then(|t| t.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(str::to_string)
        .or_else(|| hints.filename.clone())
        .or_else(|| {
            raw.name
                .as_deref()
                .map(|n| n.lines().next().unwrap_or(n).trim().to_string())
        })
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| format!("{addon} stream"));

    Some(Stream {
        addon: addon.to_string(),
        source,
        release,
        quality: parse_quality(&text),
        codec: parse_codec(&text),
        languages: parse_audio_tracks(&text),
        size: hints
            .video_size
            .or_else(|| parse_size_bytes_from_text(&text)),
        seeders: number_after(&text, "👤"),
        origin: origin_after(&text),
    })
}

fn number_after(text: &str, marker: &str) -> Option<u64> {
    let start = text.find(marker)? + marker.len();
    let digits: String = text[start..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Torrentio marks the indexer with a gear emoji: "⚙️ ThePirateBay".
fn origin_after(text: &str) -> Option<String> {
    let gear = '\u{2699}';
    let start = text.find(gear)? + gear.len_utf8();
    let rest = text[start..].trim_start_matches(|c: char| c == '\u{FE0F}' || c.is_whitespace());
    let line = rest.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Resolutions by how well they stream over a typical home connection: 4K needs
/// several times the bitrate of 1080p, so it buffers slowly or stalls.
fn stream_score(quality: Option<&str>) -> u32 {
    match quality {
        Some("1080p") => 40,
        Some("720p") => 30,
        Some("2160p") => 20,
        Some("480p") => 10,
        _ => 0,
    }
}

/// Above this a single file (typically a 4K remux) is impractical to stream.
const OVERSIZED: u64 = 20 * 1_073_741_824;

/// Prefer streams that start quickly and keep playing: healthy torrents first,
/// then sensibly sized files, then 1080p over 720p over 4K, then more seeders.
pub fn rank(streams: &mut Vec<Stream>) {
    let mut seen = HashSet::new();
    streams.retain(|s| {
        seen.insert(match &s.source {
            Source::Torrent {
                info_hash,
                file_idx,
                ..
            } => format!("t:{info_hash}:{file_idx:?}"),
            Source::Http { url, .. } => format!("h:{url}"),
        })
    });
    let healthy = |s: &Stream| s.seeders.map_or(true, |n| n >= 5);
    let oversized = |s: &Stream| s.size.map_or(false, |n| n > OVERSIZED);
    streams.sort_by(|a, b| {
        healthy(b)
            .cmp(&healthy(a))
            .then(oversized(a).cmp(&oversized(b)))
            .then(stream_score(b.quality.as_deref()).cmp(&stream_score(a.quality.as_deref())))
            .then(b.seeders.unwrap_or(0).cmp(&a.seeders.unwrap_or(0)))
            .then(b.size.unwrap_or(0).cmp(&a.size.unwrap_or(0)))
    });
}

/// Ask every enabled stream addon at once. Returns the streams plus per-addon errors.
pub fn streams(
    addons: &[Addon],
    kind: &str,
    id: &str,
    season: usize,
    episode: usize,
) -> (Vec<Stream>, Vec<String>) {
    let vid = video_id(kind, id, season, episode);
    let (tx, rx) = mpsc::channel();
    for addon in addons.iter().filter(|a| a.enabled && a.stream) {
        let url = format!("{}/stream/{}/{}.json", addon.base_url(), kind, vid);
        let name = addon.name.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let _ = tx.send((name, get_json(&url).map(|v| raw_streams(&v))));
        });
    }
    drop(tx);

    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (name, result) in rx {
        match result {
            Ok(raws) => out.extend(
                raws.into_iter()
                    .filter_map(|raw| to_stream(&name, raw, season, episode)),
            ),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    rank(&mut out);
    (out, errors)
}

// ---------------------------------------------------------------------------
// Subtitles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Subtitle {
    #[serde(default)]
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub lang: String,
    #[serde(rename = "releaseGroup", default)]
    pub release_group: Option<String>,
    #[serde(rename = "movieReleaseName", default)]
    pub release_name: Option<String>,
    #[serde(rename = "releaseFormat", default)]
    pub release_format: Option<String>,
    #[serde(rename = "fpsMilli", default, deserialize_with = "opt_usize_lenient")]
    pub fps_milli: Option<usize>,
}

/// Ask every enabled subtitle addon at once; each can take seconds to answer.
pub fn subtitles(addons: &[Addon], kind: &str, id: &str, season: usize, episode: usize) -> Vec<Subtitle> {
    let vid = video_id(kind, id, season, episode);
    let (tx, rx) = mpsc::channel();
    for (order, addon) in addons.iter().filter(|a| a.enabled && a.subtitles).enumerate() {
        let url = format!("{}/subtitles/{}/{}.json", addon.base_url(), kind, vid);
        let tx = tx.clone();
        thread::spawn(move || {
            let items: Vec<Subtitle> = get_json(&url)
                .ok()
                .and_then(|v| {
                    v.get("subtitles").and_then(|s| s.as_array()).map(|items| {
                        items
                            .iter()
                            .filter_map(|s| serde_json::from_value::<Subtitle>(s.clone()).ok())
                            .filter(|s| s.url.starts_with("http"))
                            .collect()
                    })
                })
                .unwrap_or_default();
            let _ = tx.send((order, items));
        });
    }
    drop(tx);
    // Keep addon order, so ties between equally good subtitles go to the first addon.
    let mut results: Vec<(usize, Vec<Subtitle>)> = rx.into_iter().collect();
    results.sort_by_key(|(order, _)| *order);
    results.into_iter().flat_map(|(_, items)| items).collect()
}

/// The broad source a release was made from. A subtitle is timed to one of these,
/// and releases in the same family (BluRay rips, or WEB-DL and WEBRip) usually
/// share timing; cam/telesync copies never match a real release.
fn release_family(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |names: &[&str]| words.iter().any(|w| names.contains(w));
    if has(&["cam", "camrip", "hdcam", "ts", "telesync", "hdts", "tc", "telecine", "scr", "screener", "dvdscr", "bdscr", "r5", "workprint"]) {
        Some("prerelease")
    } else if has(&["bluray", "blu", "bdrip", "brrip", "bdremux", "remux", "bd"]) {
        Some("bluray")
    } else if has(&["web", "webdl", "webrip", "amzn", "nf", "dsnp", "hmax", "atvp"]) {
        Some("web")
    } else if has(&["hdtv", "pdtv", "tvrip", "dsr"]) {
        Some("hdtv")
    } else if has(&["dvdrip", "dvd", "dvd5", "dvd9"]) {
        Some("dvd")
    } else {
        None
    }
}

/// Subtitles in `lang`, most likely to be in sync first: same release group, then
/// the same source family, then the 23.976 fps that BluRay and WEB releases use (a
/// 25 fps subtitle drifts steadily). Subtitles timed to cam/telesync copies go last.
/// `off` (or empty) returns nothing.
pub fn rank_subtitles<'a>(subs: &'a [Subtitle], lang: &str, release: Option<&str>) -> Vec<&'a Subtitle> {
    if lang.is_empty() || lang.eq_ignore_ascii_case("off") {
        return Vec::new();
    }
    let release = release.unwrap_or_default().to_ascii_lowercase();
    let video_family = release_family(&release);
    let score = |s: &Subtitle| -> i32 {
        let family = s
            .release_format
            .as_deref()
            .and_then(release_family)
            .or_else(|| s.release_name.as_deref().and_then(release_family));
        let mut score = 0;
        if family == Some("prerelease") && video_family != Some("prerelease") {
            score -= 10;
        }
        if let Some(group) = s.release_group.as_deref().filter(|g| g.len() >= 2) {
            if !release.is_empty() && release.contains(&group.to_ascii_lowercase()) {
                score += 4;
            }
        }
        if video_family.is_some() && family == video_family {
            score += 2;
        }
        if matches!(video_family, Some("bluray") | Some("web")) {
            match s.fps_milli {
                Some(23_976 | 23_980 | 24_000) => score += 1,
                Some(fps) if fps > 0 => score -= 1,
                _ => {}
            }
        }
        score
    };
    let mut ranked: Vec<&Subtitle> = subs.iter().filter(|s| s.lang.eq_ignore_ascii_case(lang)).collect();
    // Stable sort, so equally good subtitles keep the addon's order.
    ranked.sort_by_key(|s| std::cmp::Reverse(score(s)));
    ranked
}

pub fn pick_subtitle<'a>(subs: &'a [Subtitle], lang: &str, release: Option<&str>) -> Option<&'a Subtitle> {
    rank_subtitles(subs, lang, release).into_iter().next()
}

// ---------------------------------------------------------------------------
// Release-name parsing
// ---------------------------------------------------------------------------

pub fn extract_year(raw: &str) -> String {
    raw.as_bytes()
        .windows(4)
        .find(|w| w.iter().all(u8::is_ascii_digit) && matches!(w[0], b'1' | b'2'))
        .and_then(|w| std::str::from_utf8(w).ok())
        .map(str::to_string)
        .unwrap_or_default()
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
}

pub fn parse_quality(text: &str) -> Option<String> {
    let upper = text.to_ascii_uppercase();
    let has_word = |w: &str| words(&upper).any(|x| x == w);
    let q = if upper.contains("2160P") || has_word("4K") || has_word("UHD") {
        "2160p"
    } else if upper.contains("1080P") || has_word("FHD") {
        "1080p"
    } else if upper.contains("720P") || has_word("HD") {
        "720p"
    } else if upper.contains("480P") || upper.contains("576P") || has_word("SD") {
        "480p"
    } else {
        return None;
    };
    Some(q.to_string())
}

pub fn quality_score(quality: Option<&str>) -> u32 {
    match quality {
        Some("2160p") => 40,
        Some("1080p") => 30,
        Some("720p") => 20,
        Some("480p") => 10,
        _ => 0,
    }
}

pub fn parse_codec(text: &str) -> Option<String> {
    let upper = text.to_ascii_uppercase();
    let codec = if ["HEVC", "X265", "H.265", "H265"].iter().any(|n| upper.contains(n)) {
        "HEVC"
    } else if ["X264", "H.264", "H264", "AVC"].iter().any(|n| upper.contains(n)) {
        "x264"
    } else if upper.contains("AV1") {
        "AV1"
    } else {
        return None;
    };
    Some(codec.to_string())
}

/// Audio languages named in a release. Matches whole words only, so "LENGTH"
/// isn't English and "SHINING" isn't Hindi.
pub fn parse_audio_tracks(text: &str) -> Option<String> {
    const CANDIDATES: &[(&str, &str)] = &[
        ("ENGLISH", "English"),
        ("ENG", "English"),
        ("HINDI", "Hindi"),
        ("HIN", "Hindi"),
        ("TAMIL", "Tamil"),
        ("TELUGU", "Telugu"),
        ("MALAYALAM", "Malayalam"),
        ("KANNADA", "Kannada"),
        ("BENGALI", "Bengali"),
        ("MARATHI", "Marathi"),
        ("PUNJABI", "Punjabi"),
        ("URDU", "Urdu"),
        ("SPANISH", "Spanish"),
        ("LATINO", "Spanish"),
        ("FRENCH", "French"),
        ("GERMAN", "German"),
        ("ITALIAN", "Italian"),
        ("PORTUGUESE", "Portuguese"),
        ("RUSSIAN", "Russian"),
        ("JAPANESE", "Japanese"),
        ("KOREAN", "Korean"),
        ("CHINESE", "Chinese"),
        ("DUAL", "Dual audio"),
        ("MULTI", "Multi audio"),
    ];
    let upper = text.to_ascii_uppercase();
    let found: HashSet<&str> = words(&upper).collect();
    let mut langs: Vec<&str> = Vec::new();
    for (needle, label) in CANDIDATES {
        if found.contains(needle) && !langs.contains(label) {
            langs.push(label);
        }
    }
    (!langs.is_empty()).then(|| langs.join(" + "))
}

pub fn parse_size_bytes_from_text(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    // Also split on ':' and '|' so "Size:1.5GB" parses. Not on ',', which is a
    // decimal separator in "1,5 GB".
    let parts: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || c == ':' || c == '|')
        .filter(|p| !p.is_empty())
        .collect();
    for (i, part) in parts.iter().enumerate() {
        let clean = part.trim_matches(|c: char| !c.is_alphanumeric() && c != '.');
        if let Ok(num) = clean.parse::<f64>() {
            if let Some(next) = parts.get(i + 1) {
                let unit = next.trim_matches(|c: char| !c.is_alphabetic());
                match unit {
                    "gb" | "gib" => return Some((num * 1_073_741_824.0) as u64),
                    "mb" | "mib" => return Some((num * 1_048_576.0) as u64),
                    _ => {}
                }
            }
        }
        for (suffix, mult) in [("gib", 1_073_741_824.0), ("gb", 1_073_741_824.0), ("mib", 1_048_576.0), ("mb", 1_048_576.0)] {
            if let Some(num) = clean.strip_suffix(suffix).and_then(|n| n.parse::<f64>().ok()) {
                return Some((num * mult) as u64);
            }
        }
    }
    None
}

/// Finds `S01E02`, `1x02`, or "Season 1 Episode 2" in a release name.
pub fn parse_season_episode(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let len = bytes.len();

    for i in 0..len {
        if (bytes[i] == b'S' || bytes[i] == b's')
            && i + 1 < len
            && bytes[i + 1].is_ascii_digit()
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        {
            let s_start = i + 1;
            let mut s_end = s_start;
            while s_end < len && bytes[s_end].is_ascii_digit() {
                s_end += 1;
            }
            if s_end - s_start <= 3 {
                let mut e_idx = s_end;
                while e_idx < len && matches!(bytes[e_idx], b'.' | b' ' | b'_' | b'-') {
                    e_idx += 1;
                }
                if e_idx + 1 < len
                    && (bytes[e_idx] == b'E' || bytes[e_idx] == b'e')
                    && bytes[e_idx + 1].is_ascii_digit()
                {
                    let e_start = e_idx + 1;
                    let mut e_end = e_start;
                    while e_end < len && bytes[e_end].is_ascii_digit() {
                        e_end += 1;
                    }
                    if e_end - e_start <= 4 {
                        if let (Ok(s), Ok(e)) = (text[s_start..s_end].parse(), text[e_start..e_end].parse()) {
                            return Some((s, e));
                        }
                    }
                }
            }
        }

        if (bytes[i] == b'x' || bytes[i] == b'X')
            && i > 0
            && bytes[i - 1].is_ascii_digit()
            && i + 1 < len
            && bytes[i + 1].is_ascii_digit()
        {
            let mut s_start = i - 1;
            while s_start > 0 && bytes[s_start - 1].is_ascii_digit() {
                s_start -= 1;
            }
            if s_start == 0 || !bytes[s_start - 1].is_ascii_alphanumeric() {
                let mut e_end = i + 1;
                while e_end < len && bytes[e_end].is_ascii_digit() {
                    e_end += 1;
                }
                let (s_str, e_str) = (&text[s_start..i], &text[i + 1..e_end]);
                if s_str.len() <= 3 && e_str.len() <= 4 {
                    if let (Ok(s), Ok(e)) = (s_str.parse::<usize>(), e_str.parse::<usize>()) {
                        if s > 0 && s < 100 && e > 0 {
                            return Some((s, e));
                        }
                    }
                }
            }
        }
    }

    let upper = text.to_ascii_uppercase();
    let number_after = |at: usize| -> Option<usize> {
        upper[at..]
            .chars()
            .skip_while(|c| *c == ' ')
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()
    };
    let episode = upper.find("EPISODE ").and_then(|p| number_after(p + 8));
    let season = upper.find("SEASON ").and_then(|p| number_after(p + 7));
    match (season, episode) {
        (Some(s), Some(e)) => Some((s, e)),
        (None, Some(e)) => Some((1, e)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Lenient deserializers: addons disagree on whether numbers are strings, and
// whether lists are arrays, comma-separated strings, or arrays of objects.
// ---------------------------------------------------------------------------

fn opt_string_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Option<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string, number, or null")
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            let v = v.trim();
            Ok((!v.is_empty()).then(|| v.to_string()))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }
        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }
    }
    d.deserialize_any(V)
}

fn opt_usize_lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<usize>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Option<usize>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a non-negative number, numeric string, or null")
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v as usize))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
            Ok((v >= 0).then_some(v as usize))
        }
        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
            Ok((v >= 0.0).then_some(v as usize))
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            Ok(v.trim().parse().ok())
        }
    }
    d.deserialize_any(V)
}

fn string_or_vec<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or a list")
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            Ok(v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
        }
        fn visit_seq<S: serde::de::SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
            let mut out = Vec::new();
            while let Some(item) = seq.next_element::<serde_json::Value>()? {
                let text = item
                    .as_str()
                    .or_else(|| item.get("name").and_then(|n| n.as_str()))
                    .map(str::trim);
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    out.push(t.to_string());
                }
            }
            Ok(out)
        }
    }
    d.deserialize_any(V)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shapes captured from the live Torrentio / Cinemeta / OpenSubtitles responses,
    // trimmed to what matters.
    const TORRENTIO: &str = r#"{"streams":[
      {"name":"Torrentio\n4k","title":"Breaking Bad (2008) S01 (2160p AMZN WEB-DL H265 SDR DDP 5.1 English - HONE)\nBreaking Bad (2008) S01E01 (2160p AMZN WEB-DL H265 SDR DDP 5.1 English - HONE).mkv\n👤 83 💾 6.27 GB ⚙️ ThePirateBay","infoHash":"F0FD1CAD0E24B69FF6AAADB9DF40C50286327188","fileIdx":0,"behaviorHints":{"bingeGroup":"torrentio|x","filename":"Breaking Bad (2008) S01E01.mkv"}},
      {"name":"Torrentio\n1080p","title":"Breaking.Bad.S01E02.1080p.BluRay.x264-DEMAND\n👤 500 💾 1.2 GB ⚙️ 1337x","infoHash":"0123456789abcdef0123456789abcdef01234567","fileIdx":"3","sources":["tracker:udp://tracker.example:1337/announce","dht:0123456789abcdef0123456789abcdef01234567"]},
      {"name":"Direct","title":"Some upload 720p","url":"https://cdn.example/v.mp4","behaviorHints":{"proxyHeaders":{"request":{"Referer":"https://example.org"}}}},
      {"name":"Junk","title":"neither a url nor a hash"},
      {"name":"BadHash","infoHash":"nothex!","title":"x"}
    ]}"#;

    fn parsed(season: usize, episode: usize) -> Vec<Stream> {
        let v: serde_json::Value = serde_json::from_str(TORRENTIO).unwrap();
        raw_streams(&v)
            .into_iter()
            .filter_map(|r| to_stream("Torrentio", r, season, episode))
            .collect()
    }

    #[test]
    fn torrentio_stream_fields() {
        let s = parsed(1, 1);
        assert_eq!(s.len(), 2, "wrong episode, junk and bad hash are dropped: {s:#?}");
        let t = &s[0];
        assert_eq!(
            t.source,
            Source::Torrent {
                info_hash: "f0fd1cad0e24b69ff6aaadb9df40c50286327188".into(),
                file_idx: Some(0),
                trackers: vec![]
            }
        );
        assert_eq!(t.release, "Breaking Bad (2008) S01 (2160p AMZN WEB-DL H265 SDR DDP 5.1 English - HONE)");
        assert_eq!(t.quality.as_deref(), Some("2160p"));
        assert_eq!(t.codec.as_deref(), Some("HEVC"));
        assert_eq!(t.seeders, Some(83));
        assert_eq!(t.origin.as_deref(), Some("ThePirateBay"));
        assert_eq!(t.languages.as_deref(), Some("English"));
        assert_eq!(t.size, Some((6.27 * 1_073_741_824.0) as u64));
        assert!(t.target().starts_with("magnet:?xt=urn:btih:f0fd1cad"));

        let h = &s[1];
        assert_eq!(
            h.source,
            Source::Http {
                url: "https://cdn.example/v.mp4".into(),
                headers: vec![("Referer".into(), "https://example.org".into())]
            }
        );
        assert_eq!(h.quality.as_deref(), Some("720p"));
    }

    #[test]
    fn movie_request_keeps_everything_and_reads_string_file_idx_and_trackers() {
        let s = parsed(0, 0);
        assert_eq!(s.len(), 3);
        let t = &s[1];
        assert_eq!(t.file_idx(), Some(3));
        assert_eq!(t.seeders, Some(500));
        assert_eq!(t.origin.as_deref(), Some("1337x"));
        assert!(t.target().contains("&tr=udp://tracker.example:1337/announce"));
        assert!(!t.target().contains("dht:"));
    }

    #[test]
    fn ranking_prefers_healthy_then_quality() {
        let mut s = parsed(0, 0);
        let mut dead_4k = s[0].clone();
        dead_4k.seeders = Some(1);
        if let Source::Torrent { info_hash, .. } = &mut dead_4k.source {
            *info_hash = "ffffffffffffffffffffffffffffffffffffffff".into();
        }
        s.push(dead_4k);
        s.push(s[0].clone()); // exact duplicate
        rank(&mut s);
        assert_eq!(s.len(), 4, "duplicate removed");
        assert_eq!(s[0].quality.as_deref(), Some("1080p"), "1080p streams more reliably than 4K");
        assert_eq!(s[1].quality.as_deref(), Some("720p"));
        assert_eq!(s[2].seeders, Some(83), "then the healthy 4K");
        assert_eq!(s.last().unwrap().seeders, Some(1), "dead torrent last");
    }

    #[test]
    fn ranking_pushes_oversized_files_down() {
        let mut s = parsed(0, 0);
        s.truncate(2); // healthy 4K (6.27 GB) and 1080p (1.2 GB, 500 seeders)
        let mut remux = s[1].clone();
        remux.size = Some(40 * 1_073_741_824);
        remux.seeders = Some(900);
        if let Source::Torrent { info_hash, .. } = &mut remux.source {
            *info_hash = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into();
        }
        s.push(remux);
        rank(&mut s);
        assert_eq!(s.last().unwrap().size, Some(40 * 1_073_741_824), "a 40 GB 1080p ranks below a 6 GB 4K despite more seeders");
    }

    #[test]
    fn seasons_from_cinemeta_videos() {
        let meta: MetaDetail = serde_json::from_str(r#"{
            "id":"tt0903747","type":"series","name":"Breaking Bad","releaseInfo":"2008-2013",
            "genres":["Drama","Crime"],"director":null,"cast":"Bryan Cranston, Aaron Paul",
            "videos":[
              {"id":"tt0903747:1:2","season":1,"episode":2,"name":"Cat's in the Bag..."},
              {"id":"tt0903747:1:1","season":1,"episode":1,"name":"Pilot"},
              {"id":"tt0903747:0:1","season":0,"episode":1,"name":"Special"},
              {"id":"tt0903747:2:1","season":"2","number":1,"title":"Seven Thirty-Seven"},
              {"id":"tt0903747:1:1","season":1,"episode":1,"name":"Pilot (dupe)"}
            ]}"#).unwrap();
        assert!(meta.is_series());
        assert_eq!(meta.year_label(), "2008-2013");
        assert_eq!(meta.cast, vec!["Bryan Cranston", "Aaron Paul"]);
        let seasons = meta.seasons();
        assert_eq!(seasons.iter().map(|s| s.number).collect::<Vec<_>>(), [1, 2, 0]);
        assert_eq!(seasons[0].episodes.iter().map(|e| e.number).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(seasons[0].episodes[0].title, "Pilot");
        assert_eq!(seasons[1].episodes[0].title, "Seven Thirty-Seven");
    }

    #[test]
    fn catalog_items_parse_leniently() {
        let v: serde_json::Value = serde_json::from_str(r#"{"metas":[
            {"id":"tt1375666","type":"movie","name":"Inception","releaseInfo":2010,"imdbRating":8.8},
            {"type":"movie","name":"missing id"},
            {"id":"tt0903747","type":"series","name":"Breaking Bad","releaseInfo":"2008-2013","genres":"Drama, Crime"}
        ]}"#).unwrap();
        let items = metas_from(&v);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].year_label(), "2010");
        assert_eq!(items[0].imdb_rating.as_deref(), Some("8.8"));
        assert!(items[1].is_series());
        assert_eq!(items[1].genres, vec!["Drama", "Crime"]);
    }

    #[test]
    fn interleaves_movies_and_series() {
        let item = |id: &str| MetaItem {
            id: id.into(),
            kind: String::new(),
            name: id.into(),
            poster: None,
            release_info: None,
            imdb_rating: None,
            genres: vec![],
            description: None,
        };
        let out = interleave(vec![item("m1"), item("m2"), item("m3")], vec![item("s1")]);
        assert_eq!(out.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["m1", "s1", "m2", "m3"]);
    }

    #[test]
    fn search_results_rank_exact_titles_first() {
        let item = |id: &str, name: &str| MetaItem {
            id: id.into(),
            kind: "movie".into(),
            name: name.into(),
            poster: None,
            release_info: None,
            imdb_rating: None,
            genres: vec![],
            description: None,
        };
        // The order Cinemeta's interleaved results really arrived in for "breaking bad".
        let mut items = vec![
            item("tt9243946", "El Camino"),
            item("tt0903747", "Breaking Bad"),
            item("a", "The Bad News Bears in Breaking Training"),
            item("b", "Breaking Bad: Original Minisodes"),
            item("c", "Ismo: Breaking Bad English"),
        ];
        rank_by_title("  breaking  BAD ", &mut items);
        assert_eq!(
            items.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["tt0903747", "b", "c", "tt9243946", "a"],
            "exact, starts-with, contains, then the rest in original order"
        );
    }

    #[test]
    fn manifests_map_to_capabilities() {
        let torrentio: Manifest = serde_json::from_str(
            r#"{"id":"com.stremio.torrentio.addon","name":"Torrentio","resources":[{"name":"stream","types":["movie","series"],"idPrefixes":["tt","kitsu"]}]}"#,
        ).unwrap();
        let a = Addon::from_manifest("https://torrentio.strem.fun/manifest.json".into(), &torrentio);
        assert!(a.stream && !a.catalog && !a.meta && !a.subtitles);

        let subs: Manifest = serde_json::from_str(r#"{"name":"OpenSubtitles v3","resources":["subtitles"]}"#).unwrap();
        assert!(Addon::from_manifest(OPENSUBTITLES.into(), &subs).subtitles);

        let cinemeta: Manifest = serde_json::from_str(
            r#"{"id":"org.stremio.cinemeta","name":"Cinemeta","resources":["catalog",{"name":"meta","types":["movie"]}],"catalogs":[{"type":"movie","id":"top"}]}"#,
        ).unwrap();
        let c = Addon::from_manifest(CINEMETA.into(), &cinemeta);
        assert!(c.catalog && c.meta && !c.stream);
        assert!(c.is_core());
        assert_eq!(c.capabilities(), "catalog · meta");
    }

    #[test]
    fn core_addon_is_restored_and_enabled() {
        let mut list = vec![Addon::opensubtitles()];
        ensure_core(&mut list);
        assert!(list[0].is_core());
        list[0].enabled = false;
        ensure_core(&mut list);
        assert!(list[0].enabled);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn manifest_urls_normalize() {
        assert_eq!(normalize_manifest_url("torrentio.strem.fun"), "https://torrentio.strem.fun/manifest.json");
        assert_eq!(normalize_manifest_url("stremio://torrentio.strem.fun/manifest.json"), "https://torrentio.strem.fun/manifest.json");
        assert_eq!(normalize_manifest_url("https://x.io/conf=abc/"), "https://x.io/conf=abc/manifest.json");
        assert_eq!(base_url("https://x.io/conf=abc/manifest.json"), "https://x.io/conf=abc");
    }

    #[test]
    fn video_ids() {
        assert_eq!(video_id("movie", "tt1", 0, 0), "tt1");
        assert_eq!(video_id("series", "tt1", 2, 5), "tt1:2:5");
        assert_eq!(video_id("series", "tt1", 0, 3), "tt1:0:3", "specials keep season 0");
    }

    #[test]
    fn season_episode_formats() {
        assert_eq!(parse_season_episode("Show.S01E02.1080p"), Some((1, 2)));
        assert_eq!(parse_season_episode("show s1 e10"), Some((1, 10)));
        assert_eq!(parse_season_episode("Show 3x07 HDTV"), Some((3, 7)));
        assert_eq!(parse_season_episode("Show Season 2 Episode 9"), Some((2, 9)));
        assert_eq!(parse_season_episode("Breaking Bad (2008) S01 (2160p)\nBreaking Bad S01E01.mkv"), Some((1, 1)));
        assert_eq!(parse_season_episode("Complete S01-S05 1080p x264"), None);
        assert_eq!(parse_season_episode("Movie.2010.1080p.x264"), None);
    }

    #[test]
    fn languages_match_whole_words() {
        assert_eq!(parse_audio_tracks("Movie LENGTH extended SHINING"), None);
        // Listed in a fixed order, not order of appearance, so rows read consistently.
        assert_eq!(parse_audio_tracks("Dual Audio [Hindi + English] 1080p"), Some("English + Hindi + Dual audio".into()));
        assert_eq!(parse_audio_tracks("MULTi.FRENCH.1080p"), Some("French + Multi audio".into()));
    }

    #[test]
    fn quality_and_sizes() {
        assert_eq!(parse_quality("x.HDR.2160p"), Some("2160p".into()));
        assert_eq!(parse_quality("HDRip"), None, "HDR isn't HD");
        assert_eq!(parse_size_bytes_from_text("💾 700 MB"), Some(700 * 1_048_576));
        assert_eq!(parse_size_bytes_from_text("size:1.5GB"), Some((1.5 * 1_073_741_824.0) as u64));
        assert_eq!(parse_size_bytes_from_text("5.1 English"), None);
    }

    #[test]
    fn subtitles_rank_by_how_likely_they_are_in_sync() {
        // Shaped like real OpenSubtitles v3 data: for Inception the English subtitles
        // offered were almost all timed to cam/telesync copies, at mixed frame rates.
        let subs: Vec<Subtitle> = serde_json::from_str(r#"[
            {"id":"cam","url":"https://s/1","lang":"eng","releaseGroup":"LKRG","movieReleaseName":"Inception.2010.CAM.Xvid-LKRG","releaseFormat":"Cam","fpsMilli":25000},
            {"id":"spa","url":"https://s/2","lang":"spa","releaseGroup":"SPARKS","movieReleaseName":"Inception.2010.BluRay.x264-SPARKS","releaseFormat":"BluRay","fpsMilli":23976},
            {"id":"pal","url":"https://s/3","lang":"eng","movieReleaseName":"Inception.2010.BRRip.XviD","fpsMilli":25000},
            {"id":"other-br","url":"https://s/4","lang":"eng","releaseGroup":"OTHER","movieReleaseName":"Inception.2010.BDRip.x264-OTHER","fpsMilli":23976},
            {"id":"sparks","url":"https://s/5","lang":"eng","releaseGroup":"SPARKS","movieReleaseName":"Inception.2010.BluRay.x264-SPARKS","releaseFormat":"BluRay","fpsMilli":23976},
            {"id":"web","url":"https://s/6","lang":"eng","movieReleaseName":"Inception.2010.1080p.WEB-DL","fpsMilli":"23976"}
        ]"#).unwrap();
        let ids = |release| rank_subtitles(&subs, "eng", release).iter().map(|s| s.id.as_str()).collect::<Vec<_>>();
        assert_eq!(ids(Some("Inception.2010.1080p.BluRay.x264-SPARKS")), ["sparks", "other-br", "pal", "web", "cam"],
            "same group; then BluRay family at 23.976; a 25 fps BluRay ties with WEB at 23.976; cam last");
        assert_eq!(ids(Some("Inception.2010.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb")), ["web", "other-br", "sparks", "pal", "cam"]);
        assert_eq!(ids(Some("Inception.2010.HDCAM-LKRG")), ["cam", "pal", "other-br", "sparks", "web"], "a cam video wants the cam subtitle");
        assert_eq!(ids(None), ["pal", "other-br", "sparks", "web", "cam"], "no release name: just keep cam last");
        assert_eq!(pick_subtitle(&subs, "eng", Some("x.BluRay-SPARKS")).map(|s| s.id.as_str()), Some("sparks"));
        assert!(rank_subtitles(&subs, "off", None).is_empty());
        assert!(rank_subtitles(&subs, "jpn", None).is_empty());
    }

    #[test]
    #[ignore = "hits the real OpenSubtitles addon"]
    fn live_subtitles_avoid_cam_copies_for_a_real_release() {
        let addons = vec![Addon::opensubtitles()];
        let release = "Breaking.Bad.S01E01.720p.BluRay.x264-DEMAND";
        let subs = subtitles(&addons, "series", "tt0903747", 1, 1);
        let ranked = rank_subtitles(&subs, "eng", Some(release));
        assert!(!ranked.is_empty(), "OpenSubtitles returned English subtitles");
        let family = |s: &Subtitle| {
            s.release_format.as_deref().and_then(release_family).or_else(|| s.release_name.as_deref().and_then(release_family))
        };
        for s in ranked.iter().take(4) {
            println!("  {:?} fps={:?} group={:?} {:?}", family(s), s.fps_milli, s.release_group, s.release_name);
        }
        if ranked.iter().any(|s| family(s) != Some("prerelease")) {
            assert_ne!(family(ranked[0]), Some("prerelease"), "a proper release's subtitle exists, so it must rank first");
        }
        if ranked.iter().any(|s| family(s) == Some("bluray")) {
            assert_eq!(family(ranked[0]), Some("bluray"), "a BluRay-timed subtitle exists for this BluRay video");
        }
    }

    #[test]
    fn release_families() {
        assert_eq!(release_family("Movie.2010.1080p.WEBRip.x264"), Some("web"));
        assert_eq!(release_family("Show.S01E01.720p.AMZN.WEB-DL"), Some("web"));
        assert_eq!(release_family("Movie.2010.Blu-ray.Remux"), Some("bluray"));
        assert_eq!(release_family("Movie 2010 HDTS x264"), Some("prerelease"));
        assert_eq!(release_family("Show.S02E03.HDTV.x264"), Some("hdtv"));
        assert_eq!(release_family("Telesync"), Some("prerelease"), "OpenSubtitles releaseFormat value");
        assert_eq!(release_family("Movie.2010.1080p.x264"), None);
    }

    #[test]
    #[ignore = "hits the real Cinemeta and OpenSubtitles addons"]
    fn live_cinemeta_and_opensubtitles() {
        let addons = vec![Addon::cinemeta(), Addon::opensubtitles()];

        let hits = search(&addons, "breaking bad").expect("search");
        let bb = hits.iter().find(|m| m.id == "tt0903747").expect("Breaking Bad in results");
        assert!(bb.is_series());

        let detail = meta(&addons, "series", "tt0903747").expect("meta");
        let seasons = detail.seasons();
        assert!(seasons.len() >= 5, "{} seasons", seasons.len());
        assert_eq!(seasons[0].number, 1);
        assert_eq!(seasons[0].episodes[0].title, "Pilot");

        // Cinemeta's catalog 307-redirects to another host; this proves redirects are followed.
        let popular = catalog(&browse_targets(&addons)[0]).expect("browse");
        assert!(popular.len() > 10, "{} popular movies", popular.len());

        let subs = subtitles(&addons, "series", "tt0903747", 1, 1);
        assert!(subs.iter().any(|s| s.lang == "eng"), "english subtitle for S01E01");
    }
}
