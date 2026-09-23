//! A track: one opened `.syr`, its cache directory, its layers as files, and the render that
//! fills them. Opening is cheap (read, inspect, key, open files); rendering runs on its own
//! threads and the mixer follows the files' frontiers, so playback never waits for this module,
//! only for the bytes it produces.
//!
//! Every layer goes through core's `Stem`: a streaming layer arrives block by block and is
//! written as it comes, a whole-buffer layer arrives complete at setup and is written at once.
//! The master is core's `Mixer`: a mix stream is handed to the player's mixer thread to run
//! live over the gained layers; a whole-buffer master runs once every layer is in and its
//! result is cached as the canonical mix.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use syrinx_core::{ErrorKind, RenderOptions, Source, Stem, Target};

use crate::cache::{self, Cache, Meta, StemFile};

/// Wall-clock budget for a render: a track runs for minutes over a dozen layers.
pub const RENDER_TIMEOUT: Duration = Duration::from_secs(120);

/// Columns in the seek bar's picture of the track.
pub const OVERVIEW_COLUMNS: usize = 1024;

const STEMS_DIR: &str = "stems";
const MIX_NAME: &str = "mix";

/// The seek bar's picture: per column, the lowest and highest sample of the unity sum of the
/// layers over every channel. `ready` columns are final; the rest are not yet rendered.
#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    pub frames_per_column: usize,
    pub columns: Vec<(f32, f32)>,
    pub ready: usize,
}

/// Min and max per column of an interleaved buffer, over all channels.
pub fn overview_of(sum: &[f32], channels: u32, frames_per_column: usize) -> Vec<(f32, f32)> {
    let ch = channels.max(1) as usize;
    let frames = sum.len() / ch;
    let mut out = Vec::with_capacity(frames.div_ceil(frames_per_column.max(1)));
    let mut i = 0;
    while i < frames {
        let end = (i + frames_per_column.max(1)).min(frames);
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for &s in &sum[i * ch..end * ch] {
            lo = lo.min(s);
            hi = hi.max(s);
        }
        out.push((lo, hi));
        i = end;
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum Render {
    Rendering { started: Instant },
    Done { took: Duration },
    Failed(String),
}

/// What the source's default export is, once the render thread has asked core.
pub enum Master {
    /// Not probed yet: the mixer waits.
    Unknown,
    /// No default export: the mix is the sum of the layers.
    Sum,
    /// A whole-buffer master: its result is `Track::mix`, written when every layer is in.
    Whole,
    /// A mix stream, to be run live over the gained layers. Taken once by the mixer thread.
    Live(Option<syrinx_core::Mixer>),
}

pub struct Track {
    pub path: PathBuf,
    pub name: String,
    pub dir: PathBuf,
    pub sample_rate: u32,
    pub channels: u32,
    pub frames: usize,
    pub duration: f64,
    /// Declaration order, parallel to `stems`.
    pub stem_names: Vec<String>,
    pub stems: Vec<Arc<StemFile>>,
    /// The canonical render of a whole-buffer master, once it has run. `None` for a source
    /// without a default export or with a mix stream.
    pub mix: Option<Arc<StemFile>>,
    pub master: Mutex<Master>,
    pub dependencies: Vec<PathBuf>,
    pub render: Mutex<Render>,
    pub overview: Mutex<Overview>,
    /// Set when the track is dropped: the layer writers stop pulling and drop their streams.
    cancel: Arc<AtomicBool>,
}

impl Track {
    /// Reads, inspects and keys the source, opens its files, and starts rendering whatever is
    /// missing. `repaint` is called whenever something the window shows has changed.
    pub fn open(cache: &Cache, path: &Path, repaint: impl Fn() + Send + Sync + 'static) -> Result<Arc<Track>> {
        let path = path.canonicalize().with_context(|| format!("{}", path.display()))?;
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let name = path.to_string_lossy().into_owned();
        let opts = RenderOptions { sample_rate: None, timeout: RENDER_TIMEOUT, root: None, target: Target::Mix };
        let source = Source::open(&text, &name, &opts).map_err(|e| anyhow!("{}", describe_error(&path, &e)))?;

        let sample_rate = source.sample_rate();
        let channels = source.channels();
        let frames = source.frames();
        let key = cache::key(text.as_bytes(), source.dependencies(), sample_rate)?;
        let dir = cache.dir(&key);
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Cache::touch(&dir);

        let stems_dir = dir.join(STEMS_DIR);
        let stems: Vec<Arc<StemFile>> = source
            .stem_names()
            .iter()
            .map(|s| StemFile::open(&stems_dir, s, frames, channels).map(Arc::new))
            .collect::<Result<_>>()?;
        // A whole-buffer master leaves its render behind; a mix stream never does. So a complete
        // mix file says which form the master is without asking core again.
        let (mix, master) = if source.has_mix() {
            let file = StemFile::open(&dir, MIX_NAME, frames, channels)?;
            if file.is_complete() {
                (Some(Arc::new(file)), Master::Whole)
            } else {
                (Some(Arc::new(file)), Master::Unknown)
            }
        } else {
            (None, Master::Sum)
        };

        let frames_per_column = frames.div_ceil(OVERVIEW_COLUMNS).max(1);
        // The contract gives a name no default; the player shows the file's stem instead.
        let name = source.meta().name.clone().unwrap_or_else(|| {
            path.file_stem().map_or_else(|| "sound".to_string(), |s| s.to_string_lossy().into_owned())
        });
        let track = Arc::new(Track {
            path: path.clone(),
            name: name.clone(),
            dir: dir.clone(),
            sample_rate,
            channels,
            frames,
            duration: source.meta().duration,
            stem_names: source.stem_names().to_vec(),
            stems,
            mix,
            master: Mutex::new(master),
            dependencies: source.dependencies().to_vec(),
            render: Mutex::new(Render::Rendering { started: Instant::now() }),
            overview: Mutex::new(Overview { frames_per_column, columns: Vec::new(), ready: 0 }),
            cancel: Arc::new(AtomicBool::new(false)),
        });

        let meta = Meta {
            name,
            sample_rate,
            channels,
            frames,
            stems: source.stem_names().to_vec(),
            has_mix: source.has_mix(),
            source: path,
            dependencies: source.dependencies().to_vec(),
        };
        let repaint = Arc::new(repaint);
        let worker = Arc::clone(&track);
        let wake = Arc::clone(&repaint);
        std::thread::Builder::new()
            .name("syrinx-player-render".into())
            .spawn(move || worker.render(source, meta, wake))
            .context("spawning the render thread")?;
        // The picture follows the layers' frontiers, so it fills left to right while a stream
        // renders rather than appearing when it is over.
        let worker = Arc::clone(&track);
        std::thread::Builder::new()
            .name("syrinx-player-overview".into())
            .spawn(move || worker.build_overview(&*repaint))
            .context("spawning the overview thread")?;
        Ok(track)
    }

    /// Frames every layer has reached.
    pub fn stem_frontier(&self) -> usize {
        self.stems.iter().map(|s| s.frontier()).min().unwrap_or(0)
    }

    /// Frames the canonical mix has reached; every frame when there is no mix file.
    pub fn mix_frontier(&self) -> usize {
        self.mix.as_ref().map_or(self.frames, |m| m.frontier())
    }

    /// How far playback could go right now: the slowest layer, and, when the canonical mix is
    /// what would play (a whole-buffer master with every fader at unity), the mix as well.
    /// `None` while the master's form is not known yet.
    pub fn rendered_to(&self, unity: bool) -> Option<usize> {
        let master = self.master.lock().unwrap();
        match &*master {
            Master::Unknown => None,
            Master::Whole if unity => Some(self.stem_frontier().min(self.mix_frontier())),
            _ => Some(self.stem_frontier()),
        }
    }

    pub fn failed(&self) -> Option<String> {
        match &*self.render.lock().unwrap() {
            Render::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }

    pub fn render_state(&self) -> Render {
        self.render.lock().unwrap().clone()
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn fail(&self, message: String) {
        let mut render = self.render.lock().unwrap();
        if !matches!(*render, Render::Failed(_)) {
            *render = Render::Failed(message);
        }
    }

    /// The render: every missing layer through core's `Stem` (whole layers arrive complete at
    /// setup, streams block by block), the master probed and either handed to the mixer thread
    /// or run whole once the layers are in, then the seek bar's picture.
    fn render(self: Arc<Self>, source: Source, meta: Meta, repaint: Arc<impl Fn() + Send + Sync + 'static>) {
        let started = Instant::now();
        let result = (|| -> Result<()> {
            // The master first: a probe is quick and tells the mixer thread what to do while
            // the layers are still coming.
            let needs_master = matches!(*self.master.lock().unwrap(), Master::Unknown);
            let mut whole_master: Option<syrinx_core::Mixer> = None;
            if needs_master {
                let mixer = source.mixer(&Target::Mix).map_err(|e| anyhow!("{}", describe_error(&self.path, &e)))?;
                if mixer.streaming() {
                    // A mix stream never writes a canonical mix: the file opened for one goes.
                    if let Some(mix) = &self.mix {
                        mix.discard();
                    }
                    *self.master.lock().unwrap() = Master::Live(Some(mixer));
                } else {
                    *self.master.lock().unwrap() = Master::Whole;
                    whole_master = Some(mixer);
                }
                repaint();
            }

            let missing: Vec<&str> = self
                .stem_names
                .iter()
                .zip(&self.stems)
                .filter(|(_, f)| !f.is_complete())
                .map(|(n, _)| n.as_str())
                .collect();
            if !missing.is_empty() {
                let stems = source.stems(&missing).map_err(|e| anyhow!("{}", describe_error(&self.path, &e)))?;
                self.write_layers(stems, &repaint)?;
            }
            if self.cancelled() {
                return Ok(());
            }

            if let Some(mut mixer) = whole_master {
                let mix = self.mix.as_ref().expect("a whole master has a mix file");
                if !mix.is_complete() {
                    let mut layers: Vec<Vec<f32>> = Vec::with_capacity(self.stems.len());
                    for file in &self.stems {
                        let mut samples = Vec::new();
                        file.read_frames(0, self.frames, &mut samples)?;
                        layers.push(samples);
                    }
                    let slices: Vec<&[f32]> = layers.iter().map(|l| l.as_slice()).collect();
                    let mixed = mixer.mix_all(&slices).map_err(|e| anyhow!("{}", describe_error(&self.path, &e)))?;
                    drop(layers);
                    mix.write_frames(0, &mixed.samples)?;
                    mix.finish()?;
                    repaint();
                }
            }
            cache::write_meta(&self.dir, &meta)?;
            Ok(())
        })();

        match result {
            Ok(()) => *self.render.lock().unwrap() = Render::Done { took: started.elapsed() },
            Err(e) => self.fail(format!("{e:#}")),
        }
        repaint();
    }

    /// One writer thread per layer, pulling blocks into its file. Returns when every layer is
    /// complete, or with the first failure.
    fn write_layers(&self, stems: Vec<Stem>, repaint: &Arc<impl Fn() + Send + Sync + 'static>) -> Result<()> {
        let (tx, rx) = mpsc::channel::<Result<()>>();
        let mut handles = Vec::with_capacity(stems.len());
        for mut stem in stems {
            let index =
                self.stem_names.iter().position(|n| n == stem.name()).context("core returned an unknown layer")?;
            let file = Arc::clone(&self.stems[index]);
            let path = self.path.clone();
            let tx = tx.clone();
            let repaint = Arc::clone(repaint);
            let cancel = Arc::clone(&self.cancel);
            let handle = std::thread::Builder::new()
                .name(format!("syrinx-player-layer-{}", stem.name()))
                .spawn(move || {
                    let result = (|| -> Result<()> {
                        loop {
                            if cancel.load(Ordering::Relaxed) {
                                // The track is gone; dropping the stream stops its isolate.
                                anyhow::bail!("cancelled");
                            }
                            match stem.next_block().map_err(|e| anyhow!("{}", describe_error(&path, &e)))? {
                                Some(block) => {
                                    file.write_frames(block.offset, &block.samples)?;
                                    repaint();
                                }
                                None => break,
                            }
                        }
                        file.finish()?;
                        Ok(())
                    })();
                    let _ = tx.send(result);
                })
                .context("spawning a layer writer")?;
            handles.push(handle);
        }
        drop(tx);
        let mut first_error: Option<anyhow::Error> = None;
        for result in rx {
            if let Err(e) = result {
                first_error.get_or_insert(e);
            }
        }
        for handle in handles {
            let _ = handle.join();
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The seek bar's picture, column by column as the layers' frontiers allow, published every
    /// 64 columns. Works for a render still in flight: a column waits for the slowest layer.
    fn build_overview(&self, repaint: &(impl Fn() + Send + Sync + 'static)) {
        let ch = self.channels as usize;
        let fpc = self.overview.lock().unwrap().frames_per_column;
        let total = self.frames.div_ceil(fpc);
        let mut columns: Vec<(f32, f32)> = Vec::with_capacity(total);
        let mut acc: Vec<f32> = Vec::new();
        let mut buf: Vec<f32> = Vec::new();
        for c in 0..total {
            let first = c * fpc;
            let n = fpc.min(self.frames - first);
            while self.stem_frontier() < first + n {
                if self.cancelled() || self.failed().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // The unity sum, the way run.js sums: a copy of the first layer, then the rest in
            // declaration order.
            for (i, file) in self.stems.iter().enumerate() {
                if file.read_frames(first, n, &mut buf).is_err() {
                    return;
                }
                if i == 0 {
                    acc.clear();
                    acc.extend_from_slice(&buf);
                } else {
                    for (a, b) in acc.iter_mut().zip(&buf) {
                        *a += *b;
                    }
                }
            }
            columns.extend(overview_of(&acc[..n * ch], self.channels, n));
            if c % 64 == 63 || c + 1 == total {
                let mut overview = self.overview.lock().unwrap();
                overview.columns.clone_from(&columns);
                overview.ready = columns.len();
                drop(overview);
                repaint();
            }
        }
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The CLI's wording for a core error: `<file>:<line>:<col>: <kind>: <message>`, the file
/// being the one core blames or, when it blames none, the track itself.
pub fn describe_error(path: &Path, e: &syrinx_core::Error) -> String {
    let kind = match e.kind {
        ErrorKind::Check => "determinism check failed",
        ErrorKind::Compile => "compile error",
        ErrorKind::Runtime => "runtime error",
        ErrorKind::Timeout => "timed out",
        ErrorKind::Contract => "contract error",
        ErrorKind::Internal => "internal error",
    };
    let file = e.file.clone().unwrap_or_else(|| path.display().to_string());
    if e.diagnostics.len() > 1 {
        let lines: Vec<String> =
            e.diagnostics.iter().map(|d| format!("{file}:{}:{}: {}", d.line, d.column, d.message)).collect();
        return format!("{kind}\n{}", lines.join("\n"));
    }
    match (e.line, e.column) {
        (Some(l), Some(c)) => format!("{file}:{l}:{c}: {kind}: {}", e.message),
        (Some(l), None) => format!("{file}:{l}: {kind}: {}", e.message),
        _ => format!("{file}: {kind}: {}", e.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_min_max_are_literal() {
        let stereo = [0.5, -0.5, 0.25, 0.0, -1.0, 0.75, 0.0, 0.0];
        assert_eq!(overview_of(&stereo, 2, 2), vec![(-0.5, 0.5), (-1.0, 0.75)]);
        // A short last column is its own column.
        assert_eq!(overview_of(&stereo, 2, 3), vec![(-1.0, 0.75), (0.0, 0.0)]);
        assert_eq!(overview_of(&[], 2, 4), Vec::<(f32, f32)>::new());
    }
}
