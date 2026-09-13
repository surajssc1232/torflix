//! User settings, plus the small JSON-on-disk helpers the other stores share.
//!
//! Environment variables still win where they exist (`TORFLIX_PLAYER`,
//! `TORFLIX_DOWNLOAD_DIR`), so existing setups keep working unchanged.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchSource {
    /// Torrent indexers (Knaben, apibay, Prowlarr, …).
    #[default]
    Torrents,
    /// Stremio catalog addons (Cinemeta): titles, seasons and episodes.
    Catalog,
}

impl SearchSource {
    pub fn toggle(self) -> Self {
        match self {
            Self::Torrents => Self::Catalog,
            Self::Catalog => Self::Torrents,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Torrents => "torrents",
            Self::Catalog => "catalog",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Player command such as `mpv` or `vlc --fullscreen`; `None` auto-detects.
    pub player: Option<String>,
    /// Where permanent downloads go; `None` uses the platform default.
    pub download_dir: Option<String>,
    /// Subtitle language as OpenSubtitles tags it (ISO 639-2, e.g. `eng`), or `off`.
    pub subtitle_lang: String,
    /// What the home search box searches.
    pub search_source: SearchSource,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            player: None,
            download_dir: None,
            subtitle_lang: "eng".into(),
            search_source: SearchSource::Torrents,
        }
    }
}

/// Subtitle languages offered in settings: (OpenSubtitles code, label).
pub const SUBTITLE_LANGS: &[(&str, &str)] = &[
    ("off", "off"),
    ("eng", "English"),
    ("spa", "Spanish"),
    ("fre", "French"),
    ("ger", "German"),
    ("ita", "Italian"),
    ("por", "Portuguese"),
    ("pob", "Portuguese (BR)"),
    ("dut", "Dutch"),
    ("pol", "Polish"),
    ("rus", "Russian"),
    ("tur", "Turkish"),
    ("ara", "Arabic"),
    ("hin", "Hindi"),
    ("jpn", "Japanese"),
    ("kor", "Korean"),
    ("chi", "Chinese"),
];

pub fn subtitle_lang_label(code: &str) -> &'static str {
    SUBTITLE_LANGS
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(code))
        .map(|(_, label)| *label)
        .unwrap_or("custom")
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TORFLIX_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("torflix")
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("torflix")
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("torflix")
}

pub fn load() -> Config {
    load_json(&config_dir().join("config.json"))
}

pub fn save(config: &Config) {
    write_json(&config_dir().join("config.json"), config);
}

/// Read a JSON file, falling back to the default when it's missing. An
/// unreadable file is set aside as `*.corrupt.<time>` rather than silently
/// overwritten on the next save.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(bytes) = std::fs::read(path) else {
        return T::default();
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            let stamp = crate::history::now();
            std::fs::rename(path, path.with_extension(format!("corrupt.{stamp}"))).ok();
            T::default()
        }
    }
}

/// Write-then-rename, so a crash mid-write can't leave a truncated file.
pub fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) {
    let Ok(json) = serde_json::to_vec_pretty(value) else {
        return;
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, json).is_ok() {
        std::fs::rename(&tmp, path).ok();
    }
}

/// FNV-1a: a stable hash for naming cache and state files (unlike std's
/// `DefaultHasher`, it doesn't change between Rust releases).
pub fn stable_hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_is_fnv1a() {
        assert_eq!(stable_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(stable_hash("a"), 0xaf63_dc4c_8601_ec8c);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("torflix-config-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.subtitle_lang, "eng");
        assert_eq!(c.search_source, SearchSource::Torrents);
        assert!(c.player.is_none() && c.download_dir.is_none());
    }

    #[test]
    fn roundtrip_missing_and_unknown_fields() {
        let dir = scratch("roundtrip");
        let path = dir.join("config.json");
        let mut c = Config::default();
        c.player = Some("vlc --fullscreen".into());
        c.search_source = SearchSource::Catalog;
        write_json(&path, &c);
        assert_eq!(load_json::<Config>(&path), c);

        // Fields a newer or older torflix doesn't know about must not break loading.
        std::fs::write(&path, br#"{"player":"mpv","some_future_option":true}"#).unwrap();
        let c2: Config = load_json(&path);
        assert_eq!(c2.player.as_deref(), Some("mpv"));
        assert_eq!(c2.subtitle_lang, "eng");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_file_is_set_aside() {
        let dir = scratch("corrupt");
        let path = dir.join("config.json");
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_json::<Config>(&path), Config::default());
        assert!(!path.exists());
        assert!(std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("corrupt")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn source_toggle_and_labels() {
        assert_eq!(SearchSource::Torrents.toggle(), SearchSource::Catalog);
        assert_eq!(SearchSource::Catalog.toggle(), SearchSource::Torrents);
        assert_eq!(subtitle_lang_label("ENG"), "English");
        assert_eq!(subtitle_lang_label("xyz"), "custom");
    }
}
