//! The render cache: one directory per (source, imports, standard, rate), holding every layer
//! as an interleaved f32 file, the canonical mix when the source has a whole-buffer master, and
//! a `meta.json`. The files double as the buffer the mixer reads from while a render is still
//! writing them, which is why a file exists at full size from the start and carries a frontier.
//!
//! Nothing here is syrinx-specific except the key; the layout is the CLI's `compile -o x.f32`.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Total size the cache is trimmed to, oldest directories first.
pub const CACHE_CAP_BYTES: u64 = 4 << 30;

const META: &str = "meta.json";
const PART: &str = ".part";

pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// `~/.cache/syrinx/player` (`$XDG_CACHE_HOME` honoured), `%LOCALAPPDATA%\syrinx\player`
    /// on Windows; created if missing.
    pub fn open() -> Result<Cache> {
        let base = dirs::cache_dir().context("no cache directory for this user")?;
        let root = base.join("syrinx").join("player");
        fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        Ok(Cache { root })
    }

    #[cfg(test)]
    pub fn at(root: PathBuf) -> Cache {
        Cache { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    /// Removes whole track directories, oldest `meta.json` first, until the cache fits `cap`.
    /// A directory without a `meta.json` (a render that never finished) counts as oldest.
    pub fn evict(&self, cap: u64) -> Result<()> {
        let mut dirs: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
        let mut total = 0u64;
        for entry in fs::read_dir(&self.root).with_context(|| format!("reading {}", self.root.display()))? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let size = dir_size(&dir)?;
            let stamp =
                fs::metadata(dir.join(META)).and_then(|m| m.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            total += size;
            dirs.push((stamp, dir, size));
        }
        dirs.sort_by_key(|(stamp, _, _)| *stamp);
        for (_, dir, size) in dirs {
            if total <= cap {
                break;
            }
            fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
            total -= size;
        }
        Ok(())
    }

    /// Empties the cache.
    pub fn clear(&self) -> Result<()> {
        for entry in fs::read_dir(&self.root).with_context(|| format!("reading {}", self.root.display()))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }

    /// Marks a track directory as used now, for eviction order.
    pub fn touch(dir: &Path) {
        let meta = dir.join(META);
        if let Ok(file) = File::options().write(true).open(&meta) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
    }
}

fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// The cache key: BLAKE3 over the source bytes, each dependency's bytes in the given order,
/// the standard's version, this library's version and the render rate. Two tracks share a key
/// iff every byte that can reach the render is the same.
pub fn key(source: &[u8], dependencies: &[PathBuf], sample_rate: u32) -> Result<String> {
    let mut h = blake3::Hasher::new();
    h.update(&(source.len() as u64).to_le_bytes());
    h.update(source);
    for dep in dependencies {
        let bytes = fs::read(dep).with_context(|| format!("reading {}", dep.display()))?;
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
    }
    h.update(&syrinx_core::PRELUDE_VERSION.to_le_bytes());
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(&sample_rate.to_le_bytes());
    Ok(h.finalize().to_hex().to_string())
}

/// What a track directory holds. Written when the render completes; its modification time is
/// the eviction clock.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Meta {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub frames: usize,
    pub stems: Vec<String>,
    pub has_mix: bool,
    pub source: PathBuf,
    pub dependencies: Vec<PathBuf>,
}

pub fn write_meta(dir: &Path, meta: &Meta) -> Result<()> {
    let path = dir.join(META);
    let text = serde_json::to_string_pretty(meta)? + "\n";
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

/// One interleaved f32 file with a frontier: how many frames have been written, published with
/// Release so a reader that loads it with Acquire sees the bytes below it.
///
/// A file is complete iff it carries its final name and its size is exactly
/// `frames * channels * 4`; anything else is created afresh as `.part` at full size.
pub struct StemFile {
    file: File,
    final_path: PathBuf,
    part_path: PathBuf,
    frames: usize,
    channels: u32,
    frontier: AtomicUsize,
    complete: AtomicBool,
}

impl StemFile {
    pub fn open(dir: &Path, name: &str, frames: usize, channels: u32) -> Result<StemFile> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let final_path = dir.join(format!("{name}.f32"));
        let part_path = dir.join(format!("{name}.f32{PART}"));
        let expected = (frames as u64) * (channels as u64) * 4;
        if let Ok(meta) = fs::metadata(&final_path) {
            if meta.is_file() && meta.len() == expected {
                let file = File::open(&final_path).with_context(|| format!("opening {}", final_path.display()))?;
                let _ = fs::remove_file(&part_path);
                return Ok(StemFile {
                    file,
                    final_path,
                    part_path,
                    frames,
                    channels,
                    frontier: AtomicUsize::new(frames),
                    complete: AtomicBool::new(true),
                });
            }
            // Wrong size: a render that was interrupted between write and rename, or a
            // different geometry under the same name. Neither is served.
            fs::remove_file(&final_path).with_context(|| format!("removing {}", final_path.display()))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&part_path)
            .with_context(|| format!("creating {}", part_path.display()))?;
        file.set_len(expected).with_context(|| format!("sizing {}", part_path.display()))?;
        Ok(StemFile {
            file,
            final_path,
            part_path,
            frames,
            channels,
            frontier: AtomicUsize::new(0),
            complete: AtomicBool::new(false),
        })
    }

    /// Where the file is right now: the `.part` name while writing, the final name after.
    pub fn path(&self) -> &Path {
        if self.is_complete() { &self.final_path } else { &self.part_path }
    }

    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
    }

    /// Frames written so far.
    pub fn frontier(&self) -> usize {
        self.frontier.load(Ordering::Acquire)
    }

    /// Writes `samples` (interleaved, a whole number of frames) at `first_frame`, which must be
    /// the current frontier: a file fills front to back, in order, from one writer.
    pub fn write_frames(&self, first_frame: usize, samples: &[f32]) -> Result<()> {
        let ch = self.channels as usize;
        if samples.len() % ch != 0 {
            bail!("{} samples is not a whole number of {ch}-channel frames", samples.len());
        }
        if first_frame != self.frontier.load(Ordering::Acquire) {
            bail!("write at frame {first_frame} but the frontier is {}", self.frontier());
        }
        let n = samples.len() / ch;
        if first_frame + n > self.frames {
            bail!("write of {n} frames at {first_frame} exceeds {} frames", self.frames);
        }
        let bytes = as_bytes(samples);
        write_all_at(&self.file, bytes, (first_frame * ch * 4) as u64)
            .with_context(|| format!("writing {}", self.part_path.display()))?;
        self.frontier.store(first_frame + n, Ordering::Release);
        Ok(())
    }

    /// Every frame is in: sync and take the final name. Called by the one writer.
    pub fn finish(&self) -> Result<()> {
        if self.frontier() != self.frames {
            bail!("{} of {} frames written", self.frontier(), self.frames);
        }
        if self.complete.load(Ordering::Acquire) {
            return Ok(());
        }
        self.file.sync_all().with_context(|| format!("syncing {}", self.part_path.display()))?;
        fs::rename(&self.part_path, &self.final_path)
            .with_context(|| format!("renaming {} to {}", self.part_path.display(), self.final_path.display()))?;
        self.complete.store(true, Ordering::Release);
        Ok(())
    }

    /// Removes a partial file that will never be finished (a master that turned out to stream
    /// leaves no canonical mix behind). A complete file is left alone.
    pub fn discard(&self) {
        if !self.is_complete() {
            let _ = fs::remove_file(&self.part_path);
        }
    }

    /// Reads `frames` frames from `first_frame` into `out` (replaced). The caller stays below the
    /// frontier; the file has no way to know what a writer is in the middle of.
    pub fn read_frames(&self, first_frame: usize, frames: usize, out: &mut Vec<f32>) -> Result<()> {
        let ch = self.channels as usize;
        if first_frame + frames > self.frontier() {
            bail!("read of {frames} frames at {first_frame} is past the frontier {}", self.frontier());
        }
        out.clear();
        out.resize(frames * ch, 0.0);
        let bytes = as_bytes_mut(out);
        read_exact_at(&self.file, bytes, (first_frame * ch * 4) as u64)
            .with_context(|| format!("reading {}", self.path().display()))?;
        Ok(())
    }
}

fn as_bytes(samples: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding and every bit pattern is a valid u8; the slice covers exactly
    // the samples' bytes and lives as long as the input borrow.
    unsafe { std::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), samples.len() * 4) }
}

fn as_bytes_mut(samples: &mut [f32]) -> &mut [u8] {
    // SAFETY: as above, and every byte pattern read back is a valid f32 (the file is our own
    // little-endian writes; the host is little-endian on every supported target).
    unsafe { std::slice::from_raw_parts_mut(samples.as_mut_ptr().cast::<u8>(), samples.len() * 4) }
}

#[cfg(unix)]
fn write_all_at(file: &File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(bytes, offset)
}

#[cfg(unix)]
fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(bytes, offset)
}

#[cfg(windows)]
fn write_all_at(file: &File, mut bytes: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        let n = file.seek_write(bytes, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "wrote nothing"));
        }
        bytes = &bytes[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        let n = file.seek_read(bytes, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short read"));
        }
        bytes = &mut bytes[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "syrinx-player-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn key_changes_when_a_dependency_or_the_source_changes() {
        let dir = temp();
        let dep = dir.join("x.js");
        fs::write(&dep, b"export const A = 1;").unwrap();
        let source = b"import { A } from './x.js';";
        let k1 = key(source, &[dep.clone()], 48_000).unwrap();
        let again = key(source, &[dep.clone()], 48_000).unwrap();
        assert_eq!(k1, again, "the same bytes must give the same key");

        fs::write(&dep, b"export const A = 2;").unwrap();
        let k2 = key(source, &[dep.clone()], 48_000).unwrap();
        assert_ne!(k1, k2, "an edited import must change the key");

        let k3 = key(b"import { A } from './x.js'; ", &[dep.clone()], 48_000).unwrap();
        assert_ne!(k2, k3, "an edited source must change the key");

        let k4 = key(source, &[dep], 44_100).unwrap();
        assert_ne!(k2, k4, "a different rate is a different render");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_partial_file_is_never_complete_and_finish_makes_it_so() {
        let dir = temp();
        let file = StemFile::open(&dir, "drums", 3, 2).unwrap();
        assert!(!file.is_complete());
        assert_eq!(file.frontier(), 0);
        assert!(file.path().to_string_lossy().ends_with(".f32.part"));

        file.write_frames(0, &[0.5, -0.25]).unwrap();
        assert_eq!(file.frontier(), 1);
        assert!(file.finish().is_err(), "finishing with frames missing must fail");
        file.write_frames(1, &[0.125, 1.0, 0.0, -1.0]).unwrap();
        file.finish().unwrap();
        assert!(file.is_complete());
        assert!(file.path().to_string_lossy().ends_with("drums.f32"));

        let reopened = StemFile::open(&dir, "drums", 3, 2).unwrap();
        assert!(reopened.is_complete());
        assert_eq!(reopened.frontier(), 3);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_final_file_of_the_wrong_size_is_not_complete() {
        let dir = temp();
        fs::write(dir.join("bass.f32"), vec![0u8; 3 * 2 * 4 - 4]).unwrap();
        let file = StemFile::open(&dir, "bass", 3, 2).unwrap();
        assert!(!file.is_complete());
        assert_eq!(file.frontier(), 0);
        assert!(!dir.join("bass.f32").exists(), "the wrong-sized file must be gone");
        assert!(dir.join("bass.f32.part").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_back_is_bit_exact() {
        let dir = temp();
        let file = StemFile::open(&dir, "lead", 2, 2).unwrap();
        file.write_frames(0, &[0.5, -0.25, 0.125, 1.0]).unwrap();
        file.finish().unwrap();
        let mut out = Vec::new();
        file.read_frames(1, 1, &mut out).unwrap();
        assert_eq!(out, vec![0.125, 1.0]);
        file.read_frames(0, 2, &mut out).unwrap();
        assert_eq!(out, vec![0.5, -0.25, 0.125, 1.0]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_past_the_frontier_are_refused() {
        let dir = temp();
        let file = StemFile::open(&dir, "pad", 4, 1).unwrap();
        file.write_frames(0, &[1.0, 2.0]).unwrap();
        let mut out = Vec::new();
        assert!(file.read_frames(1, 2, &mut out).is_err());
        file.read_frames(0, 2, &mut out).unwrap();
        assert_eq!(out, vec![1.0, 2.0]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn evict_removes_the_oldest_first() {
        let root = temp();
        let cache = Cache::at(root.clone());
        let meta = |name: &str| Meta {
            name: name.into(),
            sample_rate: 48_000,
            channels: 2,
            frames: 1,
            stems: vec![],
            has_mix: false,
            source: PathBuf::from(name),
            dependencies: vec![],
        };
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        for (i, name) in ["old", "middle", "new"].iter().enumerate() {
            let dir = cache.dir(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("a.f32"), vec![0u8; 1 << 20]).unwrap();
            write_meta(&dir, &meta(name)).unwrap();
            File::options()
                .write(true)
                .open(dir.join(META))
                .unwrap()
                .set_modified(base + std::time::Duration::from_secs(i as u64 * 10))
                .unwrap();
        }
        cache.evict((2 << 20) + (1 << 19)).unwrap();
        assert!(!cache.dir("old").exists(), "the oldest directory must go first");
        assert!(cache.dir("middle").exists());
        assert!(cache.dir("new").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
