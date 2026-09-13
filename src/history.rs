//! Watch/download history, kept as JSON in the local data dir.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Oldest entries beyond this are dropped.
const MAX_ENTRIES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Stream,
    Download,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub title: String,
    /// Magnet link, torrent URL, or local .torrent path — whatever was added.
    pub target: String,
    pub kind: Kind,
    /// Download destination, for downloads.
    #[serde(default)]
    pub dest: Option<String>,
    /// Unix seconds.
    pub at: u64,
}

fn history_file() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("torflix")
        .join("history.json")
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn load() -> Vec<Entry> {
    load_from(&history_file())
}

pub fn save(entries: &[Entry]) {
    save_to(&history_file(), entries);
}

/// Record an entry at the top of the history file.
pub fn record(title: &str, target: &str, kind: Kind, dest: Option<String>) {
    let mut entries = load();
    push(
        &mut entries,
        Entry { title: title.to_string(), target: target.to_string(), kind, dest, at: now() },
    );
    save(&entries);
}

fn load_from(path: &Path) -> Vec<Entry> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_to(path: &Path, entries: &[Entry]) {
    let Ok(json) = serde_json::to_vec_pretty(entries) else { return };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // Write-then-rename so a crash mid-write can't truncate the history.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        std::fs::rename(&tmp, path).ok();
    }
}

/// Newest first. Re-watching the same thing moves it to the top instead of duplicating it.
fn push(entries: &mut Vec<Entry>, entry: Entry) {
    entries.retain(|e| !(e.target == entry.target && e.kind == entry.kind));
    entries.insert(0, entry);
    entries.truncate(MAX_ENTRIES);
}

/// "just now", "12m ago", "3h ago", "2d ago", then a plain date after a week.
pub fn format_when(at: u64, now: u64) -> String {
    let secs = now.saturating_sub(at);
    match secs {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{}m ago", secs / 60),
        3_600..=86_399 => format!("{}h ago", secs / 3_600),
        86_400..=604_799 => format!("{}d ago", secs / 86_400),
        _ => {
            let (y, m, d) = civil_from_days((at / 86_400) as i64);
            format!("{:04}-{:02}-{:02}", y, m, d)
        }
    }
}

/// Days since the Unix epoch → (year, month, day), proleptic Gregorian.
/// Howard Hinnant's `civil_from_days`; avoids pulling in a date crate for one label.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe as i64 + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(target: &str, kind: Kind, at: u64) -> Entry {
        Entry { title: target.into(), target: target.into(), kind, dest: None, at }
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29)); // leap day
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn relative_labels() {
        let now = 1_704_067_200; // 2024-01-01
        assert_eq!(format_when(now - 5, now), "just now");
        assert_eq!(format_when(now - 125, now), "2m ago");
        assert_eq!(format_when(now - 3 * 3_600, now), "3h ago");
        assert_eq!(format_when(now - 2 * 86_400, now), "2d ago");
        assert_eq!(format_when(now - 30 * 86_400, now), "2023-12-02");
        // A clock that moved backwards shouldn't underflow.
        assert_eq!(format_when(now + 100, now), "just now");
    }

    #[test]
    fn push_dedupes_and_caps() {
        let mut v = vec![entry("a", Kind::Stream, 1), entry("b", Kind::Stream, 2)];
        push(&mut v, entry("a", Kind::Stream, 3));
        assert_eq!(v.iter().map(|e| e.target.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(v[0].at, 3);

        // Same target, different kind is a separate entry.
        push(&mut v, entry("a", Kind::Download, 4));
        assert_eq!(v.len(), 3);

        for i in 0..(MAX_ENTRIES as u64 + 10) {
            push(&mut v, entry(&format!("x{i}"), Kind::Stream, i));
        }
        assert_eq!(v.len(), MAX_ENTRIES);
    }

    #[test]
    fn roundtrip_and_missing_file() {
        let dir = std::env::temp_dir().join(format!("torflix-history-test-{}", std::process::id()));
        let path = dir.join("history.json");
        assert!(load_from(&path).is_empty());
        let v = vec![entry("magnet:?xt=urn:btih:abc", Kind::Download, 42)];
        save_to(&path, &v);
        let back = load_from(&path);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].kind, Kind::Download);
        assert_eq!(back[0].at, 42);
        std::fs::remove_dir_all(&dir).ok();
    }
}
