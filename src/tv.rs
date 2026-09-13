//! Live TV from M3U playlists, by URL or local file.
//!
//! Playlist parsing is adapted from MovieBox-Tui (MIT, see THIRD_PARTY_NOTICES.md).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

/// Remote playlists are re-downloaded at most this often.
const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Channel {
    pub name: String,
    pub group: String,
    pub logo: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TvConfig {
    #[serde(default)]
    pub playlists: Vec<String>,
}

fn config_path() -> PathBuf {
    crate::config::config_dir().join("tv.json")
}

pub fn load_config() -> TvConfig {
    crate::config::load_json(&config_path())
}

pub fn save_config(config: &TvConfig) {
    crate::config::write_json(&config_path(), config);
}

pub fn parse_m3u(content: &str) -> Vec<Channel> {
    let mut channels = Vec::new();
    let mut current = Channel::default();
    for line in content.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("#EXTINF:") {
            current.logo = attr(line, "tvg-logo").to_string();
            current.group = attr(line, "group-title").to_string();
            if let Some(idx) = title_comma(line) {
                current.name = line[idx + 1..].trim().to_string();
            }
        } else if !line.starts_with('#') {
            current.url = line.to_string();
            if current.name.is_empty() {
                current.name = line.to_string();
            }
            channels.push(std::mem::take(&mut current));
        }
    }
    channels
}

/// Value of `name="…"` (or single-quoted) in an `#EXTINF` line.
fn attr<'a>(line: &'a str, name: &str) -> &'a str {
    let bytes = line.as_bytes();
    let name = name.as_bytes();
    let mut i = 0;
    while i + name.len() + 2 <= bytes.len() {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'-';
        if boundary && &bytes[i..i + name.len()] == name && bytes[i + name.len()] == b'=' {
            let quote = bytes[i + name.len() + 1];
            if quote == b'"' || quote == b'\'' {
                let start = i + name.len() + 2;
                if let Some(len) = bytes[start..].iter().position(|&b| b == quote) {
                    return &line[start..start + len];
                }
            }
        }
        i += 1;
    }
    ""
}

/// The comma separating attributes from the title — skipping commas inside quoted attributes.
fn title_comma(line: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (idx, ch) in line.char_indices() {
        match quote {
            Some(q) if ch == q => quote = None,
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == ',' => return Some(idx),
            _ => {}
        }
    }
    line.find(',')
}

fn expand_tilde(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(s)),
        None => PathBuf::from(s),
    }
}

fn is_remote(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

pub fn fetch(source: &str) -> Result<Vec<Channel>> {
    let source = source.trim();
    let content = if is_remote(source) {
        fetch_remote(source)?
    } else {
        let path = expand_tilde(source);
        std::fs::read_to_string(&path).with_context(|| format!("can't read {}", path.display()))?
    };
    let channels = parse_m3u(&content);
    if channels.is_empty() {
        bail!("no channels in {source}");
    }
    Ok(channels)
}

fn fetch_remote(url: &str) -> Result<String> {
    let cache = crate::config::cache_dir()
        .join("tv")
        .join(format!("{:016x}.m3u", crate::config::stable_hash(url)));
    let fresh = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map_or(false, |age| age < CACHE_TTL);
    if fresh {
        if let Ok(content) = std::fs::read_to_string(&cache) {
            if !parse_m3u(&content).is_empty() {
                return Ok(content);
            }
        }
    }
    let resp = minreq::get(url)
        .with_header("User-Agent", concat!("torflix/", env!("CARGO_PKG_VERSION")))
        .with_timeout(30)
        .send()
        .context("playlist unreachable")?;
    if resp.status_code >= 400 {
        bail!("playlist returned HTTP {}", resp.status_code);
    }
    let body = resp.as_str().context("playlist isn't text")?.to_string();
    if let Some(parent) = cache.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&cache, &body).ok();
    Ok(body)
}

/// Every channel from every playlist, deduplicated by stream URL, plus per-playlist errors.
pub fn load_all(sources: &[String]) -> (Vec<Channel>, Vec<String>) {
    let mut seen = HashSet::new();
    let mut channels = Vec::new();
    let mut errors = Vec::new();
    for source in sources {
        match fetch(source) {
            Ok(list) => channels.extend(list.into_iter().filter(|c| seen.insert(c.url.clone()))),
            Err(e) => errors.push(format!("{source}: {e:#}")),
        }
    }
    (channels, errors)
}

/// Channels whose name or group contains `query` (case-insensitive).
pub fn filter<'a>(channels: &'a [Channel], query: &str) -> Vec<&'a Channel> {
    let q = query.trim().to_lowercase();
    channels
        .iter()
        .filter(|c| q.is_empty() || c.name.to_lowercase().contains(&q) || c.group.to_lowercase().contains(&q))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_attributes_and_titles() {
        let channels = parse_m3u(
            r#"#EXTM3U
#EXTINF:-1 tvg-id="cnn.us" tvg-logo="http://logo/cnn.png" group-title="News",CNN HD
http://example.com/cnn.m3u8
#EXTINF:-1 tvg-id='bbc.uk' tvg-logo='http://logo/bbc.png' group-title='News, World',BBC World News, Live
http://example.com/bbc.m3u8

#EXTINF:-1,Discovery Channel
#EXTVLCOPT:http-user-agent=Foo
http://example.com/discovery.m3u8
"#,
        );
        assert_eq!(channels.len(), 3);
        assert_eq!(channels[0].name, "CNN HD");
        assert_eq!(channels[0].logo, "http://logo/cnn.png");
        assert_eq!(channels[0].group, "News");
        assert_eq!(channels[1].name, "BBC World News, Live");
        assert_eq!(channels[1].group, "News, World");
        assert_eq!(channels[2].name, "Discovery Channel");
        assert_eq!(channels[2].group, "");
        assert_eq!(channels[2].url, "http://example.com/discovery.m3u8");
    }

    #[test]
    fn attribute_names_need_a_boundary() {
        assert_eq!(attr(r#"#EXTINF:-1 xtvg-logo="no" tvg-logo="yes",A"#, "tvg-logo"), "yes");
    }

    #[test]
    fn metadata_does_not_leak_into_the_next_channel() {
        let channels = parse_m3u("#EXTINF:-1 group-title=\"Kids\",A\nhttp://a\n#EXTINF:-1,B\nhttp://b\n");
        assert_eq!(channels[1].group, "");
    }

    #[test]
    fn local_files_dedupe_and_filter() {
        let dir = std::env::temp_dir().join(format!("torflix-tv-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.m3u");
        let b = dir.join("b.m3u");
        let empty = dir.join("empty.m3u");
        std::fs::write(&a, "#EXTINF:-1 group-title=\"Sports\",ESPN\nhttp://x/espn\n#EXTINF:-1,News 24\nhttp://x/news\n").unwrap();
        std::fs::write(&b, "#EXTINF:-1,ESPN again\nhttp://x/espn\n").unwrap();
        std::fs::write(&empty, "#EXTM3U\n").unwrap();
        let sources: Vec<String> = [&a, &b, &empty, &dir.join("missing.m3u")]
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let (channels, errors) = load_all(&sources);
        assert_eq!(channels.len(), 2, "same URL listed twice counts once");
        assert_eq!(errors.len(), 2, "empty and missing playlists reported: {errors:?}");
        assert_eq!(filter(&channels, "sport").len(), 1, "matches group");
        assert_eq!(filter(&channels, "NEWS")[0].name, "News 24");
        assert_eq!(filter(&channels, "").len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tilde_expands_to_home() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde("~/tv/list.m3u"), home.join("tv/list.m3u"));
        }
        assert_eq!(expand_tilde("/abs/list.m3u"), PathBuf::from("/abs/list.m3u"));
    }
}
