//! Torrent search. Uses Prowlarr/Jackett when configured, otherwise falls
//! back to a built-in scraper of public torrent sites (TorrentGalaxy → apibay).

use anyhow::{bail, Context, Result};
use serde::Deserialize;

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub size: u64,
    pub seeders: i64,
    pub leechers: i64,
    pub magnet: Option<String>,
    pub link: Option<String>,
    pub indexer: String,
    pub rating: Option<f32>,
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
    Builtin, // scrape public torrent sites — no setup required
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Prowlarr { .. } => "prowlarr",
            Backend::Jackett { .. } => "jackett",
            Backend::Builtin => "built-in",
        }
    }
}

/// Prowlarr/Jackett if configured (via env vars), otherwise the built-in scraper.
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
    Some(Backend::Builtin)
}

pub fn search(backend: &Backend, query: &str) -> Result<Vec<SearchResult>> {
    match backend {
        Backend::Builtin => search_builtin(query),
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

// ============================================================================
// Built-in scraper: tries TorrentGalaxy first (fast, magnets in one page),
// then falls back to apibay.org (Pirate Bay JSON API) on failure or empty.
// ============================================================================

fn search_builtin(query: &str) -> Result<Vec<SearchResult>> {
    let mut all: Vec<SearchResult> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    match search_apibay(query) {
        Ok(r) => all.extend(r),
        Err(e) => errors.push(format!("apibay: {}", e)),
    }
    match search_torrentgalaxy(query) {
        Ok(r) => all.extend(r),
        Err(e) => errors.push(format!("TorrentGalaxy: {}", e)),
    }
    match search_1337x(query) {
        Ok(r) => all.extend(r),
        Err(e) => errors.push(format!("1337x: {}", e)),
    }

    if all.is_empty() {
        bail!(
            "no results — sources tried: {}\n\nIf results are empty, your ISP may be blocking torrent sites.\n→ Enable a VPN and try again, or set up Prowlarr for more sources (see README).",
            errors.join(" | ")
        );
    }

    // Deduplicate by title (case-insensitive), keep highest-seeder entry.
    all.sort_by(|a, b| b.seeders.cmp(&a.seeders));
    let mut seen = std::collections::HashSet::new();
    all.retain(|r| seen.insert(r.title.to_ascii_lowercase()));
    Ok(all)
}

// ---- TorrentGalaxy (HTML scrape, magnets inline in search results) ----
// Layout inspected 2025-01: each result is a row with class "tgxtablerow"
// containing a <a href="magnet:?xt=..."> and the title/size/seed/leech cells.

fn search_torrentgalaxy(query: &str) -> Result<Vec<SearchResult>> {
    let url = format!(
        "https://torrentgalaxy.to/torrents.php?search={}&sort=seeders&order=desc",
        urlencode(query)
    );
    let resp = minreq::get(&url)
        .with_header("User-Agent", UA)
        .with_header("Accept", "text/html,application/xhtml+xml")
        .with_header("Accept-Language", "en-US,en;q=0.9")
        .with_timeout(8) // fail fast if the site is ISP-blocked
        .send()
        .context("torrentgalaxy unreachable")?;
    if resp.status_code >= 400 {
        bail!("HTTP {}", resp.status_code);
    }
    let html = resp.as_str().context("non-utf8 response")?;
    Ok(parse_torrentgalaxy(html))
}

fn parse_torrentgalaxy(html: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();
    // Each row starts at `tgxtablerow` and ends at the next one (or end of doc).
    for row in html.split("tgxtablerow").skip(1) {
        // Skip adult categories entirely
        let low = row.to_ascii_lowercase();
        if low.contains("/cat/xxx") || low.contains(">xxx<") || low.contains("adult") {
            continue;
        }

        // Magnet link
        let Some(magnet) = extract_between(row, "href=\"magnet:", "\"")
            .map(|m| format!("magnet:{}", m))
        else { continue };

        // Title: `title="..."` on the torrent detail link
        let title = extract_between(row, "title=\"", "\"")
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
            .replace("&amp;", "&");

        // Size: usually inside `<span class="badge badge-secondary txlight">SIZE</span>`
        let size = extract_between(row, "badge-secondary txlight\">", "<")
            .map(|s| parse_size(&s))
            .unwrap_or(0);

        // Seeders: green cell; leechers: red cell. Both wrapped in <font color=...>
        let seeders = extract_between(row, "<font color=\"green\"><b>", "<")
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let leechers = extract_between(row, "<font color=\"#ff0000\"><b>", "<")
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);

        results.push(SearchResult {
            title,
            size,
            seeders,
            leechers,
            magnet: Some(magnet),
            link: None,
            indexer: "TorrentGalaxy".into(),
            rating: None,
        });
    }
    results.sort_by(|a, b| b.seeders.cmp(&a.seeders));
    results
}

// ---- 1337x (HTML scrape) ----

fn search_1337x(query: &str) -> Result<Vec<SearchResult>> {
    let url = format!("https://1337x.to/search/{}/1/", urlencode(query));
    let resp = minreq::get(&url)
        .with_header("User-Agent", UA)
        .with_header("Accept", "text/html,application/xhtml+xml")
        .with_timeout(10)
        .send()
        .context("1337x unreachable")?;
    if resp.status_code >= 400 {
        bail!("HTTP {}", resp.status_code);
    }
    let html = resp.as_str().context("non-utf8 response")?;
    let mut results = parse_1337x_listing(html);

    // Fetch magnet links from detail pages for the top 15 results.
    results.truncate(15);
    for r in &mut results {
        if let Some(detail_url) = r.link.as_deref() {
            if let Ok(magnet) = fetch_1337x_magnet(detail_url) {
                r.magnet = Some(magnet);
            }
        }
    }
    results.retain(|r| r.magnet.is_some());
    Ok(results)
}

fn fetch_1337x_magnet(detail_url: &str) -> Result<String> {
    let resp = minreq::get(detail_url)
        .with_header("User-Agent", UA)
        .with_timeout(8)
        .send()
        .context("1337x detail unreachable")?;
    let html = resp.as_str().context("non-utf8")?;
    extract_between(html, "href=\"magnet:", "\"")
        .map(|m| format!("magnet:{}", m))
        .ok_or_else(|| anyhow::anyhow!("no magnet on detail page"))
}

fn parse_1337x_listing(html: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();
    for chunk in html.split("/torrent/").skip(1) {
        let id = match chunk.find('/') {
            Some(i) => &chunk[..i],
            None => continue,
        };
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let title = match extract_between(chunk, "\">", "</a>") {
            Some(t) if !t.is_empty() && !t.starts_with('<') => t.replace("&amp;", "&"),
            _ => continue,
        };

        let seeders = extract_between(chunk, "seeds\">", "<")
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let leechers = extract_between(chunk, "leeches\">", "<")
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let size_str = extract_between(chunk, "coll-4 size mob-uploader\">", "<")
            .unwrap_or_default();
        let size = parse_size(size_str.split('<').next().unwrap_or("").trim());

        // Build a stable detail URL using only the numeric ID.
        let detail_url = format!("https://1337x.to/torrent/{}/{}/", id, urlencode(&title));

        results.push(SearchResult {
            title,
            size,
            seeders,
            leechers,
            magnet: None,
            link: Some(detail_url),
            indexer: "1337x".into(),
            rating: None,
        });
    }
    results.sort_by(|a, b| b.seeders.cmp(&a.seeders));
    results
}

// ---- apibay.org fallback (Pirate Bay JSON API) ----

#[derive(Deserialize)]
struct ApibayItem {
    #[serde(default)]
    name: String,
    #[serde(default)]
    info_hash: String,
    #[serde(default)]
    seeders: String,
    #[serde(default)]
    leechers: String,
    #[serde(default)]
    size: String,
}

fn search_apibay(query: &str) -> Result<Vec<SearchResult>> {
    let url = format!("https://apibay.org/q.php?q={}&cat=0", urlencode(query));
    // Try up to 3 times: 0s, 2s, 4s. Retries on 429 or TLS errors.
    for delay_ms in [0u64, 2000, 4000] {
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        let resp = match minreq::get(&url)
            .with_header("User-Agent", UA)
            .with_timeout(20)
            .send()
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        if resp.status_code == 429 { continue; }
        if resp.status_code >= 400 {
            bail!("HTTP {}", resp.status_code);
        }
        let items: Vec<ApibayItem> = resp.json().context("bad JSON")?;
        let zero_hash = "0000000000000000000000000000000000000000";
        let mut results: Vec<SearchResult> = items
            .into_iter()
            .filter(|i| !i.info_hash.is_empty() && i.info_hash != zero_hash && !i.name.is_empty())
            .map(|i| SearchResult {
                title: i.name.clone(),
                size: i.size.parse().unwrap_or(0),
                seeders: i.seeders.parse().unwrap_or(0),
                leechers: i.leechers.parse().unwrap_or(0),
                magnet: Some(build_magnet(&i.info_hash, &i.name)),
                link: None,
                indexer: "TPB".into(),
                rating: None,
            })
            .collect();
        results.sort_by(|a, b| b.seeders.cmp(&a.seeders));
        return Ok(results);
    }
    bail!("rate-limited after 3 attempts")
}

fn build_magnet(hash: &str, name: &str) -> String {
    const TRACKERS: &[&str] = &[
        "udp://tracker.opentrackr.org:1337/announce",
        "udp://open.demonii.com:1337/announce",
        "udp://tracker.openbittorrent.com:80",
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

// Extract the substring between the first occurrence of `start` and the
// next `end`. Returns None if either isn't found.
fn extract_between(s: &str, start: &str, end: &str) -> Option<String> {
    let i = s.find(start)? + start.len();
    let rest = &s[i..];
    let j = rest.find(end)?;
    Some(rest[..j].to_string())
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

    #[test]
    fn extract_between_basic() {
        assert_eq!(extract_between("hello [world] end", "[", "]"), Some("world".to_string()));
        assert_eq!(extract_between("no brackets", "[", "]"), None);
    }

    #[test]
    #[ignore]
    fn live_torrentgalaxy_search() {
        let results = search_torrentgalaxy("interstellar 2014").expect("TG unreachable");
        println!("TG returned {} results", results.len());
        for r in results.iter().take(5) {
            println!("  {}s/{}l  {}  {}", r.seeders, r.leechers, crate::app::human_bytes(r.size), r.title);
        }
        assert!(!results.is_empty(), "TG returned zero results — parser probably needs updating");
    }

    #[test]
    #[ignore]
    fn live_apibay_search() {
        let results = search_apibay("interstellar").expect("apibay unreachable");
        println!("apibay returned {} results", results.len());
        assert!(!results.is_empty());
    }
}
