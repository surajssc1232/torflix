//! Starred catalog titles.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Favorite {
    /// Catalog id, e.g. `tt0903747`.
    pub id: String,
    /// `movie` or `series`.
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub year: String,
    #[serde(default)]
    pub added_at: u64,
}

fn path() -> PathBuf {
    crate::config::data_dir().join("favorites.json")
}

pub fn load() -> Vec<Favorite> {
    crate::config::load_json(&path())
}

pub fn save(list: &[Favorite]) {
    crate::config::write_json(&path(), list);
}

pub fn is_favorite(list: &[Favorite], id: &str) -> bool {
    list.iter().any(|f| f.id == id)
}

/// Star or unstar; returns whether it's starred afterwards. Newest first.
pub fn toggle(list: &mut Vec<Favorite>, fav: Favorite) -> bool {
    match list.iter().position(|f| f.id == fav.id) {
        Some(pos) => {
            list.remove(pos);
            false
        }
        None => {
            list.insert(0, fav);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fav(id: &str) -> Favorite {
        Favorite {
            id: id.into(),
            kind: "movie".into(),
            name: id.into(),
            year: "2010".into(),
            added_at: 1,
        }
    }

    #[test]
    fn toggle_is_reversible_and_newest_first() {
        let mut list = vec![fav("a")];
        assert!(toggle(&mut list, fav("b")));
        assert_eq!(list[0].id, "b");
        assert!(is_favorite(&list, "a"));
        assert!(!toggle(&mut list, fav("a")));
        assert!(!is_favorite(&list, "a"));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn persists() {
        let dir = std::env::temp_dir().join(format!("torflix-fav-test-{}", std::process::id()));
        let path = dir.join("favorites.json");
        crate::config::write_json(&path, &[fav("tt1"), fav("tt2")][..]);
        let back: Vec<Favorite> = crate::config::load_json(&path);
        assert_eq!(back.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["tt1", "tt2"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
