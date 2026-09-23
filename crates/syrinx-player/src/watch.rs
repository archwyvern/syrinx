//! Reload on change: watches the directories holding a track's source and imports (editors
//! save by rename, so a file watched by name goes stale), keeps only events that mean the
//! bytes moved, and reports a change once a burst has settled.
//!
//! Only content changes count. notify's inotify backend also delivers OPEN and CLOSE_NOWRITE
//! for every read, and the player's own render reads every import: treating those as changes
//! reloaded the track, which read the files, which reloaded the track. Access and metadata
//! events are dropped here on purpose.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Editors write, rename and touch in bursts; a change is reported once this long has passed
/// since the last relevant event.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct Watch {
    _watcher: RecommendedWatcher,
    rx: Receiver<Instant>,
    /// The latest relevant event not yet reported.
    pending: Mutex<Option<Instant>>,
}

impl Watch {
    /// `files` is the closure: the source and every import, canonical. `repaint` runs when a
    /// relevant event arrives so the window wakes up and can poll `changed`.
    pub fn new(files: Vec<PathBuf>, repaint: impl Fn() + Send + 'static) -> Result<Watch> {
        let set: HashSet<PathBuf> = files.iter().cloned().collect();
        let (tx, rx) = mpsc::channel();
        let handler = move |result: notify::Result<Event>| {
            if let Ok(event) = result
                && is_change(&event.kind)
                && event.paths.iter().any(|p| is_relevant(p, &set))
            {
                let _ = tx.send(Instant::now());
                repaint();
            }
        };
        let mut watcher =
            RecommendedWatcher::new(handler, notify::Config::default()).context("starting the file watcher")?;
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for file in &files {
            if let Some(dir) = file.parent() {
                dirs.insert(dir.to_path_buf());
            }
        }
        for dir in dirs {
            watcher.watch(&dir, RecursiveMode::NonRecursive).with_context(|| format!("watching {}", dir.display()))?;
        }
        Ok(Watch { _watcher: watcher, rx, pending: Mutex::new(None) })
    }

    fn drain(&self) {
        let mut pending = self.pending.lock().unwrap();
        loop {
            match self.rx.try_recv() {
                Ok(at) => *pending = Some(at),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    /// True once a change has arrived and the burst it belongs to has been quiet for
    /// [`DEBOUNCE`]. Reported once per burst.
    pub fn changed(&self) -> bool {
        self.drain();
        let mut pending = self.pending.lock().unwrap();
        match *pending {
            Some(at) if at.elapsed() >= DEBOUNCE => {
                *pending = None;
                true
            }
            _ => false,
        }
    }

    /// A change is waiting for its burst to settle: poll again soon.
    pub fn pending(&self) -> bool {
        self.drain();
        self.pending.lock().unwrap().is_some()
    }
}

/// Whether an event kind means the file's bytes (or its existence) moved. Reads and metadata
/// do not.
pub fn is_change(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Any => true,
        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any | ModifyKind::Other) => true,
        EventKind::Modify(ModifyKind::Metadata(_)) | EventKind::Access(_) | EventKind::Other => false,
    }
}

/// Whether an event's path is one of the watched files: canonicalised when it still exists
/// (a rename-save delivers the temporary name first, the real one last), else as given.
pub fn is_relevant(event_path: &Path, files: &HashSet<PathBuf>) -> bool {
    let canonical = event_path.canonicalize().unwrap_or_else(|_| event_path.to_path_buf());
    files.contains(&canonical) || files.contains(event_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, MetadataKind};
    use std::fs;

    #[test]
    fn only_paths_in_the_closure_are_relevant() {
        let set: HashSet<PathBuf> = [PathBuf::from("/t/a.syr"), PathBuf::from("/t/lib/x.js")].into_iter().collect();
        assert!(is_relevant(Path::new("/t/lib/x.js"), &set));
        assert!(is_relevant(Path::new("/t/a.syr"), &set));
        assert!(!is_relevant(Path::new("/t/lib/other.js"), &set));
        assert!(!is_relevant(Path::new("/t/a.syr~"), &set));
    }

    #[test]
    fn reads_and_metadata_are_not_changes() {
        assert!(!is_change(&EventKind::Access(AccessKind::Open(AccessMode::Read))));
        assert!(!is_change(&EventKind::Access(AccessKind::Close(AccessMode::Read))));
        assert!(!is_change(&EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any))));
        assert!(is_change(&EventKind::Modify(ModifyKind::Data(DataChange::Any))));
        assert!(is_change(&EventKind::Create(CreateKind::File)));
        assert!(is_change(&EventKind::Remove(notify::event::RemoveKind::File)));
    }

    #[test]
    fn an_edit_triggers_once_and_reading_the_file_does_not() {
        let dir = std::env::temp_dir().join(format!("syrinx-player-watch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("a.syr");
        fs::write(&source, "one").unwrap();
        let source = source.canonicalize().unwrap();
        let watch = Watch::new(vec![source.clone()], || {}).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(!watch.changed());

        fs::write(&source, "two").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        fs::write(&source, "three").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(watch.changed(), "an edit to the source must be reported");
        assert!(!watch.changed(), "and reported once");

        // The player's own render reads every import; a read is not a change.
        let _ = fs::read_to_string(&source).unwrap();
        fs::write(dir.join("other.txt"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(!watch.pending() && !watch.changed(), "reading the file or writing a neighbour is not a change");
        drop(watch);
        fs::remove_dir_all(dir).unwrap();
    }
}
