//! Choosing a download folder without typing it.
//!
//! A list of the folders inside one folder, with a row at the top that takes
//! the one you're standing in. Arrow keys walk the tree; nothing is typed
//! unless you want to. The folders used recently are offered first, since the
//! same handful of destinations come up again and again.

use std::path::{Path, PathBuf};

/// How many recently-used destinations to offer.
pub const RECENTS: usize = 5;
/// A guard against listing a directory with a pathological number of entries.
const MAX_ENTRIES: usize = 2000;

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// Download into the folder being shown.
    Use,
    /// Go to the parent folder.
    Up,
    /// Jump to a folder used before.
    Recent(PathBuf),
    /// Go into a subfolder.
    Dir(PathBuf),
}

pub struct Picker {
    /// The folder being shown.
    pub dir: PathBuf,
    pub rows: Vec<Row>,
    pub selected: usize,
    /// Offered until the first step away from where the picker opened.
    recents: Vec<PathBuf>,
    /// True while a system file chooser is open over the terminal.
    pub waiting: bool,
}

impl Picker {
    pub fn open(dir: PathBuf, recents: Vec<PathBuf>) -> Self {
        let mut p = Picker { dir, rows: Vec::new(), selected: 0, recents, waiting: false };
        p.refresh();
        p
    }

    /// Re-read the folder being shown.
    pub fn refresh(&mut self) {
        let mut rows = vec![Row::Use];
        if self.dir.parent().is_some() {
            rows.push(Row::Up);
        }
        // Recents are a shortcut out of the starting folder, not a fixture of
        // every folder — once you've walked somewhere they're just noise.
        if !self.recents.is_empty() {
            rows.extend(
                self.recents
                    .iter()
                    .filter(|r| **r != self.dir)
                    .take(RECENTS)
                    .map(|r| Row::Recent(r.clone())),
            );
        }
        rows.extend(subfolders(&self.dir).into_iter().map(Row::Dir));
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(d.unsigned_abs()),
            d => (self.selected + d as usize).min(last),
        };
    }

    /// Act on the highlighted row. `Some(folder)` means a choice was made.
    pub fn enter(&mut self) -> Option<PathBuf> {
        match self.rows.get(self.selected).cloned() {
            Some(Row::Use) => return Some(self.dir.clone()),
            Some(Row::Up) => self.up(),
            Some(Row::Dir(d)) | Some(Row::Recent(d)) => self.go(d),
            None => {}
        }
        None
    }

    /// Up to the parent folder, keeping the one just left highlighted.
    pub fn up(&mut self) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let was = std::mem::replace(&mut self.dir, parent);
        self.recents.clear();
        self.selected = 0;
        self.refresh();
        if let Some(i) = self.rows.iter().position(|r| matches!(r, Row::Dir(d) if *d == was)) {
            self.selected = i;
        }
    }

    fn go(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.recents.clear();
        self.selected = 0;
        self.refresh();
    }

    /// What to show for a row, and whether it's a folder name.
    pub fn label(&self, row: &Row) -> String {
        match row {
            Row::Use => format!("download into {}", crate::config::tilde(&self.dir)),
            Row::Up => "..".into(),
            Row::Recent(d) => format!("recent · {}", crate::config::tilde(d)),
            Row::Dir(d) => file_name(d),
        }
    }
}

/// The visible folders inside `dir`, sorted the way a file manager would.
fn subfolders(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .take(MAX_ENTRIES)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !file_name(p).starts_with('.'))
        .collect();
    dirs.sort_by_key(|p| file_name(p).to_lowercase());
    dirs
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree to walk: <root>/{Movies/Kids, Shows, .hidden}
    fn tree() -> PathBuf {
        let root = std::env::temp_dir().join(format!("torflix-pick-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::remove_dir_all(&root).ok();
        for d in ["Movies/Kids", "Shows", ".hidden"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("notes.txt"), b"not a folder").unwrap();
        root
    }

    #[test]
    fn lists_folders_only_and_skips_hidden_ones() {
        let root = tree();
        let p = Picker::open(root.clone(), Vec::new());
        let names: Vec<String> = p.rows.iter().map(|r| p.label(r)).collect();
        assert_eq!(names[0], format!("download into {}", root.display()));
        assert_eq!(names[1], "..");
        assert_eq!(&names[2..], ["Movies", "Shows"], "no files, no dot folders, sorted");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn walking_in_and_out_keeps_your_place() {
        let root = tree();
        let mut p = Picker::open(root.clone(), Vec::new());
        p.selected = 3; // Shows
        assert_eq!(p.enter(), None, "entering a folder is not a choice");
        assert_eq!(p.dir, root.join("Shows"));
        p.up();
        assert_eq!(p.dir, root);
        assert_eq!(p.label(&p.rows[p.selected]), "Shows", "back on the folder just left");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_first_row_chooses_the_folder_you_are_in() {
        let root = tree();
        let mut p = Picker::open(root.clone(), Vec::new());
        p.selected = 2; // Movies
        p.enter();
        p.selected = 0;
        assert_eq!(p.enter(), Some(root.join("Movies")));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn recents_are_offered_until_you_walk_away() {
        let root = tree();
        let recent = root.join("Shows");
        let mut p = Picker::open(root.clone(), vec![recent.clone()]);
        assert_eq!(p.rows[2], Row::Recent(recent.clone()), "offered at the top");
        p.selected = 2;
        assert_eq!(p.enter(), None);
        assert_eq!(p.dir, recent, "a recent row jumps there");
        assert!(!p.rows.iter().any(|r| matches!(r, Row::Recent(_))), "and stops being offered");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn moving_stops_at_both_ends() {
        let root = tree();
        let mut p = Picker::open(root.clone(), Vec::new());
        p.move_by(-1);
        assert_eq!(p.selected, 0);
        p.move_by(100);
        assert_eq!(p.selected, p.rows.len() - 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_folder_that_cannot_be_read_is_empty_rather_than_fatal() {
        let p = Picker::open(PathBuf::from("/definitely/not/here"), Vec::new());
        assert_eq!(p.rows, vec![Row::Use, Row::Up]);
    }
}
