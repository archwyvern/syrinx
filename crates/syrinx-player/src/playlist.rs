//! The playlist: an ordered list of sources, nothing more. Rows come from launch arguments,
//! drops and the open dialog; a background thread fills in each row's name, duration and
//! layers by inspecting it, so the list shows what the file says it is rather than its name.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use syrinx_core::RenderOptions;

use crate::track::{RENDER_TIMEOUT, describe_error};

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub path: PathBuf,
    /// The file's stem until inspected, then `meta.name`.
    pub name: String,
    /// Seconds, once inspected.
    pub duration: Option<f64>,
    pub stems: Vec<String>,
    /// What went wrong the last time this row was inspected or played.
    pub error: Option<String>,
}

impl Row {
    fn new(path: PathBuf) -> Row {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Row { path, name, duration: None, stems: Vec::new(), error: None }
    }
}

#[derive(Debug, Default)]
pub struct Playlist {
    pub rows: Vec<Row>,
    /// The row that is loaded (playing or paused).
    pub current: Option<usize>,
}

impl Playlist {
    /// Appends what `paths` expand to, skipping anything already listed. Returns the indices
    /// of the new rows, for inspection.
    pub fn append(&mut self, paths: &[PathBuf]) -> Vec<usize> {
        let mut added = Vec::new();
        for path in expand(paths) {
            if self.rows.iter().any(|r| r.path == path) {
                continue;
            }
            self.rows.push(Row::new(path));
            added.push(self.rows.len() - 1);
        }
        added
    }

    /// Replaces the list. Returns every index, for inspection.
    pub fn replace(&mut self, paths: &[PathBuf]) -> Vec<usize> {
        self.rows.clear();
        self.current = None;
        self.append(paths)
    }

    pub fn remove(&mut self, i: usize) {
        if i >= self.rows.len() {
            return;
        }
        self.rows.remove(i);
        self.current = match self.current {
            Some(c) if c == i => None,
            Some(c) if c > i => Some(c - 1),
            other => other,
        };
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.current = None;
    }

    pub fn next(&self) -> Option<usize> {
        match self.current {
            Some(c) if c + 1 < self.rows.len() => Some(c + 1),
            None if !self.rows.is_empty() => Some(0),
            _ => None,
        }
    }

    pub fn prev(&self) -> Option<usize> {
        match self.current {
            Some(c) if c > 0 => Some(c - 1),
            _ => None,
        }
    }
}

/// Files as given when they are `.syr`, directories walked for `.syr` in sorted order; canonical
/// paths, input order kept, duplicates dropped. Anything else is ignored: a player is handed
/// whole folders and a `.wav` beside a source is not an error.
pub fn expand(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        let canonical = p.canonicalize().unwrap_or(p);
        if !out.contains(&canonical) {
            out.push(canonical);
        }
    };
    for path in paths {
        if path.is_dir() {
            for entry in walkdir::WalkDir::new(path).sort_by_file_name().into_iter().flatten() {
                if entry.file_type().is_file() && is_source(entry.path()) {
                    push(entry.path().to_path_buf());
                }
            }
        } else if path.is_file() && is_source(path) {
            push(path.clone());
        }
    }
    out
}

pub fn is_source(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("syr"))
}

/// What inspecting a row found.
pub struct Inspected {
    pub path: PathBuf,
    pub result: Result<(String, f64, Vec<String>), String>,
}

/// Inspects each row on one background thread, in order; results arrive on the receiver and
/// `repaint` is called after each so the window redraws.
pub fn inspect_rows(rows: Vec<(usize, PathBuf)>, repaint: impl Fn() + Send + 'static) -> Receiver<Inspected> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("syrinx-player-inspect".into())
        .spawn(move || {
            let opts = RenderOptions { timeout: RENDER_TIMEOUT, ..RenderOptions::default() };
            for (_, path) in rows {
                let result = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display())).and_then(|source| {
                    syrinx_core::inspect(&source, &path.to_string_lossy(), &opts)
                        .map(|info| (info.meta.name, info.meta.duration, info.stems))
                        .map_err(|e| describe_error(&path, &e))
                });
                if tx.send(Inspected { path, result }).is_err() {
                    return;
                }
                repaint();
            }
        })
        .expect("spawn the inspect thread");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn expand_walks_only_syr_sorted_and_deduplicated() {
        let dir = std::env::temp_dir().join(format!("syrinx-player-playlist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::create_dir_all(dir.join("lib")).unwrap();
        for f in ["b.syr", "a.syr", "x.wav", "lib/y.js", "sub/c.syr"] {
            fs::write(dir.join(f), "").unwrap();
        }
        let got = expand(&[dir.clone(), dir.join("a.syr")]);
        let want: Vec<PathBuf> = ["a.syr", "b.syr", "sub/c.syr"].iter().map(|f| dir.join(f).canonicalize().unwrap()).collect();
        assert_eq!(got, want);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remove_keeps_current_pointing_at_the_same_row() {
        let mut p = Playlist::default();
        p.rows = ["a", "b", "c"].iter().map(|n| Row::new(PathBuf::from(format!("/t/{n}.syr")))).collect();
        p.current = Some(2);
        p.remove(0);
        assert_eq!(p.current, Some(1));
        assert_eq!(p.rows[1].name, "c");
        p.remove(1);
        assert_eq!(p.current, None);
        assert_eq!(p.next(), Some(0));
    }
}
