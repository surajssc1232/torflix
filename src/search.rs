//! In-TUI torrent search, qBittorrent-style: torflix doesn't scrape sites
//! itself — it queries a local Prowlarr or Jackett instance, which aggregates
//! whichever indexers you have configured there.

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
}

impl SearchResult {
    /// What we hand to rqbit: prefer the magnet, fall back to the .torrent URL.
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
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Prowlarr { .. } => "prowlarr",
            Backend::Jackett { .. } => "jackett",
        }
    }
}

/// Reads TORFLIX_PROWLARR_URL / TORFLIX_JACKETT_URL (+ matching _APIKEY).
/// Prowlarr wins if both are set.
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
    None
}

pub fn search(backend: &Backend, query: &str) -> Result<Vec<SearchResult>> {
    let url = match backend {
        Backend::Prowlarr { url, apikey } => {
            check_scheme(url)?;
            format!(
                "{}/api/v1/search?query={}&apikey={}&type=search&limit=100",
                url,
                urlencode(query),
                apikey
            )
        }
        Backend::Jackett { url, apikey } => {
            check_scheme(url)?;
            format!(
                "{}/api/v2.0/indexers/all/results?apikey={}&Query={}",
                url,
                apikey,
                urlencode(query)
            )
        }
    };

    let resp = minreq::get(&url)
        .with_timeout(45)
        .send()
        .with_context(|| format!("could not reach {}", backend.name()))?;
    if resp.status_code == 401 || resp.status_code == 403 {
        bail!("{} rejected the API key (HTTP {})", backend.name(), resp.status_code);
    }
    if resp.status_code >= 400 {
        bail!(
            "{} returned HTTP {}: {}",
            backend.name(),
            resp.status_code,
            snip(resp.as_str().unwrap_or(""), 140)
        );
    }

    let body = resp.as_str().context("non-utf8 response")?;
    let mut results = match backend {
        Backend::Prowlarr { .. } => parse_prowlarr(body)?,
        Backend::Jackett { .. } => parse_jackett(body)?,
    };
    // Most seeders first — the ones that will actually stream.
    results.sort_by(|a, b| b.seeders.cmp(&a.seeders));
    Ok(results)
}

fn check_scheme(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        bail!("https backends aren't supported in this build — point torflix at the local http address (e.g. http://127.0.0.1:9696)");
    }
    if !url.starts_with("http://") {
        bail!("backend URL must start with http:// (got '{}')", snip(url, 40));
    }
    Ok(())
}

// ---------- Prowlarr: GET /api/v1/search -> JSON array ----------

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
    /// Prowlarr sometimes puts a magnet in `guid`.
    #[serde(default)]
    guid: Option<String>,
}

fn parse_prowlarr(body: &str) -> Result<Vec<SearchResult>> {
    let items: Vec<ProwlarrItem> =
        serde_json::from_str(body).context("unexpected prowlarr response format")?;
    Ok(items
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
            }
        })
        .collect())
}

// ---------- Jackett: GET /api/v2.0/indexers/all/results -> {"Results":[..]} ----------

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
    Ok(resp
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
        })
        .collect())
}

// ---------- small helpers ----------

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
                // sorted by seeders desc
                assert_eq!(results[0].seeders, 152);
                assert_eq!(results[0].indexer, "MockIndexerA");
                assert!(results[0].magnet.is_none());
                assert!(results[0].add_target().unwrap().ends_with("bbb.torrent"));
                assert!(results[1].magnet.as_deref().unwrap().starts_with("magnet:"));
                assert_eq!(crate::app::human_bytes(results[0].size), "700.0 MiB");
                assert_eq!(crate::app::human_bytes(results[1].size), "3.0 GiB");
                // seedless CAM result has no target at all
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
