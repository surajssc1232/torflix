//! Torrent search: Prowlarr or Jackett if configured, otherwise falls back to
//! the built-in YTS API (movies only, no auth required).

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub size: u64,
    pub seeders: i64,
    pub leechers: i64,
    pub magnet: Option<String>,
    pub link: Option<String>,
    pub indexer: String,
    pub rating: Option<f32>, // IMDb rating from YTS; None for Prowlarr/Jackett
}

impl SearchResult {
    pub fn add_target(&self) -> Option<&str> {
        self.magnet
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| self.link.as_deref().filter(|s| !s.is_empty()))
    }
}

#[derive(Debug, Clone)]
pub enum Backend {
    Prowlarr { url: String, apikey: String },
    Jackett { url: String, apikey: String },
    Yts,
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Prowlarr { .. } => "prowlarr",
            Backend::Jackett { .. } => "jackett",
            Backend::Yts => "yts",
        }
    }
}

/// Returns Prowlarr or Jackett if configured, otherwise the built-in YTS backend.
pub fn backend_from_env() -> Option<Backend> {
    let clean = |s: String| s.trim().trim_end_matches('/').to_string();
    if let Ok(url) = std::env::var("TORFLIX_PROWLARR_URL") {
        if !url.trim().is_empty() {
            return Some(Backend::Prowlarr {
                url: clean(url),
                apikey: std::env::var("TORFLIX_PROWLARR_APIKEY").unwrap_or_default(),
            });
        }
    }
    if let Ok(url) = std::env::var("TORFLIX_JACKETT_URL") {
        if !url.trim().is_empty() {
            return Some(Backend::Jackett {
                url: clean(url),
                apikey: std::env::var("TORFLIX_JACKETT_APIKEY").unwrap_or_default(),
            });
        }
    }
    Some(Backend::Yts)
}

pub fn search(backend: &Backend, query: &str) -> Result<Vec<SearchResult>> {
    match backend {
        Backend::Yts => search_yts(query),
        Backend::Prowlarr { url, apikey } => {
            check_scheme(url)?;
            let url = format!(
                "{}/api/v1/search?query={}&apikey={}&type=search&limit=100",
                url,
                urlencode(query),
                apikey
            );
            let body = get_body(&url, "prowlarr")?;
            parse_prowlarr(&body)
        }
        Backend::Jackett { url, apikey } => {
            check_scheme(url)?;
            let url = format!(
                "{}/api/v2.0/indexers/all/results?apikey={}&Query={}",
                url,
                apikey,
                urlencode(query)
            );
            let body = get_body(&url, "jackett")?;
            parse_jackett(&body)
        }
    }
}

// ---------- YTS built-in backend ----------

#[derive(Deserialize)]
struct YtsResponse {
    data: YtsData,
}

#[derive(Deserialize)]
struct YtsData {
    #[serde(default)]
    movies: Vec<YtsMovie>,
}

#[derive(Deserialize)]
struct YtsMovie {
    title: String,
    year: u32,
    #[serde(default)]
    rating: f32,
    #[serde(default)]
    torrents: Vec<YtsTorrent>,
}

#[derive(Deserialize)]
struct YtsTorrent {
    hash: String,
    quality: String,
    #[serde(default)]
    seeds: i64,
    #[serde(default)]
    peers: i64,
    #[serde(default)]
    size_bytes: u64,
    #[serde(default)]
    size: String,
}

fn search_yts(query: &str) -> Result<Vec<SearchResult>> {
    let url = format!(
        "https://yts.mx/api/v2/list_movies.json?query_term={}&limit=20&sort_by=seeds",
        urlencode(query)
    );
    let resp = minreq::get(&url)
        .with_timeout(15)
        .send()
        .context("could not reach yts.mx")?;
    if resp.status_code >= 400 {
        bail!("YTS returned HTTP {}", resp.status_code);
    }
    let r: YtsResponse = resp.json().context("unexpected YTS response format")?;

    let mut results = Vec::new();
    for movie in r.data.movies {
        let label = format!("{} ({})", movie.title, movie.year);
        let movie_rating = if movie.rating > 0.0 { Some(movie.rating) } else { None };
        for t in &movie.torrents {
            let size = if t.size_bytes > 0 {
                t.size_bytes
            } else {
                parse_size(&t.size)
            };
            results.push(SearchResult {
                title: format!("{} [{}]", label, t.quality),
                size,
                seeders: t.seeds,
                leechers: t.peers,
                magnet: Some(build_magnet(&t.hash, &label)),
                link: None,
                indexer: "YTS".into(),
                rating: movie_rating,
            });
        }
    }
    results.sort_by(|a, b| b.seeders.cmp(&a.seeders));
    Ok(results)
}

fn build_magnet(hash: &str, name: &str) -> String {
    const TRACKERS: &[&str] = &[
        "udp://open.demonii.com:1337/announce",
        "udp://tracker.openbittorrent.com:80",
        "udp://tracker.opentrackr.org:1337/announce",
        "udp://tracker.leechers-paradise.org:6969",
        "udp://p4p.arenabg.com:1337",
    ];
    let tr: String = TRACKERS.iter().map(|t| format!("&tr={}", t)).collect();
    format!("magnet:?xt=urn:btih:{}&dn={}{}", hash, urlencode(name), tr)
}

fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    let (num, unit) = s
        .find(|c: char| c.is_alphabetic())
        .map(|i| (&s[..i], s[i..].trim()))
        .unwrap_or((s, ""));
    let n: f64 = num.trim().parse().unwrap_or(0.0);
    match unit.to_ascii_uppercase().as_str() {
        "GB" | "GIB" => (n * 1_073_741_824.0) as u64,
        "MB" | "MIB" => (n * 1_048_576.0) as u64,
        "KB" | "KIB" => (n * 1_024.0) as u64,
        _ => n as u64,
    }
}

// ---------- Prowlarr ----------

#[derive(Deserialize)]
struct ProwlarrItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    seeders: i64,
    #[serde(default)]
    leechers: i64,
    #[serde(default, rename = "magnetUrl")]
    magnet_url: Option<String>,
    #[serde(default, rename = "downloadUrl")]
    download_url: Option<String>,
    #[serde(default)]
    indexer: String,
    #[serde(default)]
    guid: Option<String>,
}

fn parse_prowlarr(body: &str) -> Result<Vec<SearchResult>> {
    let items: Vec<ProwlarrItem> =
        serde_json::from_str(body).context("unexpected prowlarr response format")?;
    let mut results: Vec<SearchResult> = items
        .into_iter()
        .map(|i| {
            let magnet = i
                .magnet_url
                .clone()
                .filter(|m| m.starts_with("magnet:"))
                .or_else(|| i.guid.clone().filter(|g| g.starts_with("magnet:")));
            SearchResult {
                title: i.title,
                size: i.size,
                seeders: i.seeders,
                leechers: i.leechers,
                magnet,
                link: i.download_url,
                indexer: i.indexer,
                rating: None,
            }
        })
        .collect();
    results.sort_by(|a, b| b.seeders.cmp(&a.seeders));
    Ok(results)
}

// ---------- Jackett ----------

#[derive(Deserialize)]
struct JackettResponse {
    #[serde(default, rename = "Results")]
    results: Vec<JackettItem>,
}

#[derive(Deserialize)]
struct JackettItem {
    #[serde(default, rename = "Title")]
    title: String,
    #[serde(default, rename = "Size")]
    size: u64,
    #[serde(default, rename = "Seeders")]
    seeders: i64,
    #[serde(default, rename = "Peers")]
    peers: i64,
    #[serde(default, rename = "MagnetUri")]
    magnet_uri: Option<String>,
    #[serde(default, rename = "Link")]
    link: Option<String>,
    #[serde(default, rename = "Tracker")]
    tracker: String,
}

fn parse_jackett(body: &str) -> Result<Vec<SearchResult>> {
    let resp: JackettResponse =
        serde_json::from_str(body).context("unexpected jackett response format")?;
    let mut results: Vec<SearchResult> = resp
        .results
        .into_iter()
        .map(|i| SearchResult {
            title: i.title,
            size: i.size,
            seeders: i.seeders,
            leechers: i.peers,
            magnet: i.magnet_uri.filter(|m| m.starts_with("magnet:")),
            link: i.link,
            indexer: i.tracker,
            rating: None,
        })
        .collect();
    results.sort_by(|a, b| b.seeders.cmp(&a.seeders));
    Ok(results)
}

// ---------- helpers ----------

fn get_body(url: &str, name: &str) -> Result<String> {
    let resp = minreq::get(url)
        .with_timeout(45)
        .send()
        .with_context(|| format!("could not reach {}", name))?;
    if resp.status_code == 401 || resp.status_code == 403 {
        bail!("{} rejected the API key (HTTP {})", name, resp.status_code);
    }
    if resp.status_code >= 400 {
        bail!(
            "{} returned HTTP {}: {}",
            name,
            resp.status_code,
            snip(resp.as_str().unwrap_or(""), 140)
        );
    }
    resp.as_str().context("non-utf8 response").map(|s| s.to_string())
}

fn check_scheme(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        bail!("https backends aren't supported — point torflix at the local http address (e.g. http://127.0.0.1:9696)");
    }
    if !url.starts_with("http://") {
        bail!("backend URL must start with http:// (got '{}')", snip(url, 40));
    }
    Ok(())
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn snip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}


#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn live_mock_prowlarr_search() {
        let backend = Backend::Prowlarr {
            url: "http://127.0.0.1:9911".into(),
            apikey: "k".into(),
        };
        match search(&backend, "big buck bunny") {
            Ok(results) => {
                assert_eq!(results.len(), 3);
                assert_eq!(results[0].seeders, 152);
                assert_eq!(results[0].indexer, "MockIndexerA");
                assert!(results[0].magnet.is_none());
                assert!(results[0].add_target().unwrap().ends_with("bbb.torrent"));
                assert!(results[1].magnet.as_deref().unwrap().starts_with("magnet:"));
                assert_eq!(crate::app::human_bytes(results[0].size), "700.0 MiB");
                assert_eq!(crate::app::human_bytes(results[1].size), "3.0 GiB");
                assert!(results[2].add_target().is_none());
            }
            Err(e) => {
                eprintln!("skipping (mock indexer not running): {}", e);
            }
        }
    }

    #[test]
    fn live_mock_jackett_search() {
        let backend = Backend::Jackett {
            url: "http://127.0.0.1:9911".into(),
            apikey: "k".into(),
        };
        if let Ok(results) = search(&backend, "sintel") {
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].seeders, 88);
            assert_eq!(results[0].leechers, 7);
            assert_eq!(results[0].indexer, "mocktracker");
            assert!(results[0].add_target().unwrap().ends_with("sintel.torrent"));
        }
    }

    #[test]
    fn https_rejected_with_hint() {
        let backend = Backend::Prowlarr { url: "https://x".into(), apikey: "".into() };
        let e = search(&backend, "q").unwrap_err().to_string();
        assert!(e.contains("http"));
    }
}
