//! A compiler for sounds: JavaScript in, PCM out.
//!
//! A sound source is an ES module exporting `meta`, its layers in `stems` and an optional
//! default export combining them. It imports the embedded [`PRELUDE`] as `"syrinx"` and may
//! import other files under the project root; it runs inside an embedded V8 and the samples it
//! returns come back as interleaved `f32`, whole ([`render`]) or one block at a time
//! ([`Stream`], or [`Source`] with its [`Stem`]s and [`Mixer`] for a host that wants the pieces).
//! The same source always produces the same bytes: every module in the graph is statically
//! checked for anything that could make it non-deterministic before it runs.
//!
//! This crate has no audio output of any kind. Playback belongs to whoever hosts it.

pub mod analyze;
pub mod check;
pub mod docs;
pub mod framework;
mod host;
pub mod wav;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

pub use check::Diagnostic;
pub use host::{Mixer, PRELUDE_SPECIFIER, Source, Stem, Stream};
#[doc(hidden)]
pub use host::{prelude_exports, standard_block_frames};

/// Version of the source contract and prelude: what `meta.api` is checked against. Bump for any
/// change to the standard, so caches keyed on it invalidate. 4 is syrinx 0.9: the prelude is the
/// core alone, and every source declares the contract it was written against.
pub const PRELUDE_VERSION: u32 = 4;

/// The oldest `meta.api` this compiler accepts. 0.9 broke with everything before it, so nothing
/// older is a valid source.
pub const API_FLOOR: u32 = 4;

/// Frames per block of a stream. A constant of the standard, pinned to the prelude's and the
/// run wrapper's `BLOCK_FRAMES` by a test; the last block of a sound is shorter.
pub const BLOCK_FRAMES: usize = 4096;

/// The JavaScript standard library every source is compiled against; imported as `"syrinx"`.
pub const PRELUDE: &str = include_str!("../../../prelude/prelude.js");

/// The standard math: a classic script that replaces the implementation-defined `Math` functions
/// with fdlibm ports and freezes `Math`. Every host runs it once per isolate, before the prelude
/// and before any source, so a render is the same bytes whatever engine computes it.
pub const MATH: &str = include_str!("../../../prelude/math.js");

/// The run wrapper: an object every host evaluates once per isolate, whose `stem` and `mix`
/// entry points turn a layer's or the mix's return value into planes, whole or one block at a
/// time, and sum layers. One file for every host, because how a return value becomes frames is
/// part of what the sound IS.
pub const RUN: &str = include_str!("../../../prelude/run.js");

/// TypeScript declarations for the prelude and the source contract, for editors.
pub const TYPES: &str = include_str!("../../../prelude/syrinx.d.ts");

/// V8 version string, for diagnostics.
pub fn v8_version() -> &'static str {
    v8::V8::get_version()
}

/// What a source declares about itself in `meta`.
#[derive(Debug, Clone, PartialEq)]
pub struct Meta {
    /// The declared name. There is no default: a tool that needs a label picks one itself (the
    /// CLI and the player use the file name).
    pub name: Option<String>,
    /// Seconds.
    pub duration: f64,
    /// 1 or 2.
    pub channels: u32,
    /// The source's preferred sample rate, if it declared one.
    pub sample_rate: Option<u32>,
    pub seed: u32,
    /// Declared seamless loop. The compiler does nothing with this yet; it is passed through
    /// for consumers.
    pub looping: bool,
}

/// What a render should produce.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Target {
    /// Every stem, combined by the source's default export, or summed when it has none.
    #[default]
    Mix,
    /// Only the named stems, summed in the order the source declares them. The mix stage does
    /// not run: it reads stems by name and cannot be given a subset.
    Stems(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Overrides the source's `meta.sampleRate`; when both are absent, 48 000.
    pub sample_rate: Option<u32>,
    /// Wall-clock budget for the whole render, stems included. Exceeding it is an error.
    pub timeout: Duration,
    /// Imports may not resolve outside this directory, and the entry file must be under it.
    /// `None` = no restriction.
    pub root: Option<PathBuf>,
    /// What to produce.
    pub target: Target,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { sample_rate: None, timeout: Duration::from_secs(20), root: None, target: Target::Mix }
    }
}

/// A compiled sound.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub meta: Meta,
    pub sample_rate: u32,
    pub channels: u32,
    pub frames: usize,
    /// Interleaved, `frames * channels` long, nominally in [-1, 1] but not clipped.
    pub samples: Vec<f32>,
    /// Every file the source imported, transitively, as canonical paths. A cache keyed on the
    /// source must also be keyed on these.
    pub dependencies: Vec<PathBuf>,
    /// Which stem this is, when it is one. `None` for a mix, for a summed subset, and for a
    /// single-stem source whose one layer IS the whole sound.
    pub stem: Option<String>,
    /// Every stem the source declares, in declaration order.
    pub stem_names: Vec<String>,
}

/// One block of a stream: `frames` frames from `offset`, interleaved. Every block but the last
/// is [`BLOCK_FRAMES`] long.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// The first frame of this block.
    pub offset: usize,
    pub frames: usize,
    /// Interleaved, `frames * channels` long.
    pub samples: Vec<f32>,
}

impl Rendered {
    pub fn peak(&self) -> f32 {
        self.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        (self.samples.iter().map(|s| (s * s) as f64).sum::<f64>() / self.samples.len() as f64).sqrt() as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The static determinism check rejected the source.
    Check,
    /// The module graph could not be built: V8 refused a module (a syntax error, an import
    /// naming an export its module lacks) or an import could not be resolved.
    Compile,
    /// The source threw while running.
    Runtime,
    /// The source ran past its time budget and was killed.
    Timeout,
    /// The source ran fine but did not honour the contract (bad `meta`, bad return value).
    Contract,
    /// A failure inside the host: a bug to report, not the source's fault.
    Internal,
}

#[derive(Debug, Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    /// The module the position refers to (the entry source's name, or an imported file's path).
    pub file: Option<String>,
    /// 1-based line in the source, when known.
    pub line: Option<u32>,
    /// 1-based column, when known.
    pub column: Option<u32>,
    /// All findings, for [`ErrorKind::Check`]; `message` summarises the first.
    pub diagnostics: Vec<Diagnostic>,
}

impl Error {
    fn of(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), file: None, line: None, column: None, diagnostics: Vec::new() }
    }

    fn contract(message: impl Into<String>) -> Self {
        Self::of(ErrorKind::Contract, message)
    }

    fn compile(message: impl Into<String>) -> Self {
        Self::of(ErrorKind::Compile, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::of(ErrorKind::Internal, message)
    }

    fn timeout() -> Self {
        Self::of(ErrorKind::Timeout, "source exceeded its time budget")
    }

    fn check(diagnostics: Vec<Diagnostic>) -> Self {
        let first = &diagnostics[0];
        Self {
            kind: ErrorKind::Check,
            message: first.message.clone(),
            file: None,
            line: Some(first.line),
            column: Some(first.column),
            diagnostics,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{file}:")?;
        }
        match (self.line, self.column) {
            (Some(l), Some(c)) => write!(f, "{l}:{c}: {}", self.message),
            (Some(l), None) => write!(f, "{l}: {}", self.message),
            _ => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for Error {}

/// What a source declares, without rendering it.
#[derive(Debug, Clone, PartialEq)]
pub struct Inspected {
    pub meta: Meta,
    /// The rate it renders at: the options' override, else its own, else 48 000.
    pub sample_rate: u32,
    /// `round(duration * sample_rate)`, never zero.
    pub frames: usize,
    /// The stems it declares, in declaration order.
    pub stems: Vec<String>,
    /// Whether it has a default export combining them. Without one the mix is their sum.
    pub has_mix: bool,
    /// Every file it imports, transitively, as canonical paths.
    pub dependencies: Vec<PathBuf>,
}

/// Runs the static check and the module graph, and returns what the source declares, without
/// rendering it.
pub fn inspect(source: &str, name: &str, opts: &RenderOptions) -> Result<Inspected, Error> {
    host::inspect(source, name, opts)
}

/// Compiles `source` (named `name` in error messages) to samples, per [`RenderOptions::target`]:
/// a [`Stream`] opened and drained.
pub fn render(source: &str, name: &str, opts: &RenderOptions) -> Result<Rendered, Error> {
    host::render(source, name, opts)
}

/// Renders stems separately, one isolate each, concurrently. An empty `which` renders them all.
/// Results come back in the order asked for.
pub fn render_each(source: &str, name: &str, opts: &RenderOptions, which: &[String]) -> Result<Vec<Rendered>, Error> {
    host::render_each(source, name, opts, which)
}

/// Runs only the mix stage, over stems rendered earlier. `stems` are interleaved, as decoded from
/// files. The geometry must match what the source declares.
pub fn mix_from(
    source: &str,
    name: &str,
    opts: &RenderOptions,
    stems: &[(String, Vec<f32>)],
) -> Result<Rendered, Error> {
    host::mix_from(source, name, opts, stems)
}
