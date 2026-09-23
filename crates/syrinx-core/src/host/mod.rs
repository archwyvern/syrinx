//! The V8 host: loads the source as an ES module graph, runs it, calls its layers and its mix
//! stage through the run wrapper, and takes the samples out, whole or one block at a time.
//!
//! One isolate per layer and one for the mix stage, each on a thread the library owns (V8
//! insists on the thread's own stack, and FFI bridges do not always provide one), thrown away
//! afterwards. V8 itself is initialised once per process. A re-armable watchdog terminates
//! execution if a source runs past its budget: the whole setup of a call shares one deadline,
//! and every block of a stream gets the budget afresh, armed only while the block computes, so
//! a consumer that pauses trips nothing.
//!
//! Module resolution is ours: `"syrinx"` is the built-in prelude, relative specifiers are files
//! under the project root, nothing else exists. Every user module in the graph goes through the
//! static determinism check before it is compiled.
//!
//! The public pieces are [`Source`] (checked and inspected, spawns nothing), [`Stem`] (one layer,
//! its blocks pulled in order), [`Mixer`] (the mix stage, fed blocks or whole layers) and
//! [`Stream`] (the three composed: pre-mixed blocks in order). `render` drains a `Stream`.
//!
//! This file holds what every piece shares: the isolate skeleton (`with_source`), the module
//! graph's evaluation, the errors and `meta`. The pieces live beside it: `loader` (resolution and
//! the jail), `watchdog`, `standard` (the prelude on its own), `wrapper` (the bridge to `run.js`),
//! `whole` (layers rendered whole), `source`, `stem`, `mixer`, `stream`.

mod loader;
mod mixer;
mod source;
mod standard;
mod stem;
mod stream;
mod watchdog;
mod whole;
mod wrapper;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{Error, ErrorKind, Inspected, Meta, RenderOptions, Rendered, Target, API_FLOOR, MATH, PRELUDE_VERSION};

use loader::{compile_registered, resolve_module, with_loader, Loader, LOADER};
use source::validate_selection;
use watchdog::{Guard, Watchdog};
use whole::render_stems;
use wrapper::read_stems;

pub use mixer::Mixer;
pub use source::Source;
pub use standard::{prelude_exports, standard_block_frames};
pub use stem::Stem;
pub use stream::Stream;

/// Per-isolate heap ceiling. Sized for a whole-buffer mix stage, which holds every layer at
/// once: three minutes of stereo at 48 kHz is 69 MB per layer, so a six-layer track is already
/// 414 MB before the mix allocates anything of its own.
const MAX_HEAP_BYTES: usize = 2048 * 1024 * 1024;

/// Stack for the thread V8 runs on. Deep recursion in a source is bounded by this, not by
/// whatever stack the caller happens to be on.
const V8_THREAD_STACK_BYTES: usize = 16 * 1024 * 1024;

/// How many blocks a streaming layer computes ahead of its consumer. Bounds memory to
/// `layers * PREFETCH * BLOCK_FRAMES * channels` floats and keeps every layer's thread busy while
/// the mix stage works on the block before.
const PREFETCH_BLOCKS: usize = 8;

/// The module specifier of the built-in prelude.
pub const PRELUDE_SPECIFIER: &str = "syrinx";

/// Origin name of the harness script; never reported as a source position.
const RUN_ORIGIN: &str = "syrinx:run";

/// Origin name of the standard math script; never reported as a source position.
const MATH_ORIGIN: &str = "syrinx:math";

fn init_v8() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

/// Runs `f` on a fresh OS thread and waits for it.
fn on_own_thread<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("syrinx-v8".into())
            .stack_size(V8_THREAD_STACK_BYTES)
            .spawn_scoped(s, f)
            .expect("spawn syrinx-v8 thread")
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// Spawns a detached V8 thread with the stack V8 needs.
fn spawn_v8_thread(f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("syrinx-v8".into())
        .stack_size(V8_THREAD_STACK_BYTES)
        .spawn(f)
        .expect("spawn syrinx-v8 thread")
}

/// A wall-clock budget shared by every isolate one call sets up. Layers are set up concurrently,
/// so each watchdog is given what is left of the whole budget rather than a fresh copy of it.
#[derive(Clone, Copy)]
struct Deadline(Instant);

impl Deadline {
    fn new(budget: Duration) -> Self {
        Self(Instant::now() + budget)
    }

    fn remaining(&self) -> Duration {
        self.0.saturating_duration_since(Instant::now())
    }
}

/// The rate and frame count a source renders at.
fn geometry(meta: &Meta, opts: &RenderOptions) -> Result<(u32, usize), Error> {
    let sample_rate = opts.sample_rate.or(meta.sample_rate).unwrap_or(48_000);
    let frames = (meta.duration * sample_rate as f64).round() as usize;
    if frames == 0 {
        return Err(Error::contract("meta.duration rounds to zero frames"));
    }
    Ok((sample_rate, frames))
}

// ---------------------------------------------------------------------------------------------
// Entry points

/// Runs the static check and the module graph, and returns what the source declares.
pub fn inspect(source: &str, name: &str, opts: &RenderOptions) -> Result<Inspected, Error> {
    inspect_with(source, name, opts, Deadline::new(opts.timeout))
}

fn inspect_with(source: &str, name: &str, opts: &RenderOptions, deadline: Deadline) -> Result<Inspected, Error> {
    on_own_thread(|| {
        with_source(source, name, opts, deadline.remaining(), |scope, _name, meta, dependencies, namespace, _guard| {
            let stems = read_stems(scope, namespace)?.into_iter().map(|(n, _)| n).collect();
            let has_mix = get(scope, namespace, "default").is_some();
            // Here rather than at render: a source whose duration rounds to no frames at all is
            // as broken when it is only checked as when it is compiled.
            let (sample_rate, frames) = geometry(&meta, opts)?;
            Ok(Inspected { meta, sample_rate, frames, stems, has_mix, dependencies })
        })
    })
}

/// Compiles `source` to samples, per `opts.target`: a [`Stream`], drained.
pub fn render(source: &str, name: &str, opts: &RenderOptions) -> Result<Rendered, Error> {
    let mut stream = Stream::open_with(source, name, opts, Deadline::new(opts.timeout))?;
    let frames = stream.source.frames();
    let channels = stream.source.channels();
    let mut samples = Vec::with_capacity(frames * channels as usize);
    while let Some(block) = stream.next_block()? {
        samples.extend_from_slice(&block.samples);
    }
    Ok(Rendered {
        meta: stream.source.meta().clone(),
        sample_rate: stream.source.sample_rate(),
        channels,
        frames,
        samples,
        dependencies: stream.source.dependencies().to_vec(),
        stem: None,
        stem_names: stream.source.stem_names().to_vec(),
    })
}

/// Renders layers whole, separately, concurrently. An empty `which` renders every layer.
pub fn render_each(source: &str, name: &str, opts: &RenderOptions, which: &[String]) -> Result<Vec<Rendered>, Error> {
    let deadline = Deadline::new(opts.timeout);
    let info = inspect_with(source, name, opts, deadline)?;
    // Separate renders in the order asked for: nothing is summed here, so the order is the
    // caller's to choose.
    let selected = if which.is_empty() {
        info.stems.clone()
    } else {
        validate_selection(&info, which)?;
        which.to_vec()
    };
    let rendered = render_stems(source, name, opts, deadline, &selected)?;
    Ok(rendered.into_iter().zip(selected).map(|(audio, stem)| audio.into_rendered(Some(stem))).collect())
}

/// Runs only the mix stage, over layers rendered earlier. `stems` are interleaved.
pub fn mix_from(source: &str, name: &str, opts: &RenderOptions, stems: &[(String, Vec<f32>)]) -> Result<Rendered, Error> {
    let deadline = Deadline::new(opts.timeout);
    let src = Source::open_with(source, name, opts, deadline)?;
    let frames = src.frames();
    let channels = src.channels();

    let supplied: Vec<&String> = stems.iter().map(|(n, _)| n).collect();
    for stem in src.stem_names() {
        if !supplied.contains(&stem) {
            return Err(Error::contract(format!("no audio supplied for stem \"{stem}\"")));
        }
    }
    for (stem, _) in stems {
        if !src.stem_names().contains(stem) {
            return Err(Error::contract(format!(
                "no stem named \"{stem}\"; this source declares {}",
                src.stem_names().join(", ")
            )));
        }
    }
    // In declaration order, whatever order they were supplied in.
    let mut ordered: Vec<&[f32]> = Vec::with_capacity(src.stem_names().len());
    for stem in src.stem_names() {
        let samples = &stems.iter().find(|(n, _)| n == stem).expect("checked above").1;
        if samples.len() != frames * channels as usize {
            return Err(Error::contract(format!(
                "stem \"{stem}\" has {} samples, expected {} ({frames} frames x {channels} channels)",
                samples.len(),
                frames * channels as usize
            )));
        }
        ordered.push(samples);
    }
    let mut mixer = src.mixer_with(&Target::Mix, deadline)?;
    mixer.mix_all_with(&ordered, deadline.remaining())
}

/// Shared skeleton: isolate, watchdog armed for `budget`, module graph, evaluation, meta
/// validation, then `body`. The body disarms the watchdog once its setup is over and re-arms it
/// per block through the [`Guard`] it is handed.
fn with_source<T>(
    source: &str,
    name: &str,
    opts: &RenderOptions,
    budget: Duration,
    body: impl for<'a, 'b> FnOnce(
        &mut v8::PinScope<'a, 'b>,
        &str,
        Meta,
        Vec<PathBuf>,
        v8::Local<'a, v8::Object>,
        &Guard,
    ) -> Result<T, Error>,
) -> Result<T, Error> {
    let diagnostics = crate::check::check(source);
    if !diagnostics.is_empty() {
        let mut e = Error::check(diagnostics);
        e.file = Some(name.to_string());
        return Err(e);
    }

    // The entry module's own location, for resolving its relative imports. A name that is
    // not a real path resolves against the working directory.
    let entry_path = {
        let p = Path::new(name);
        let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
        match abs.parent().and_then(|d| std::fs::canonicalize(d).ok()) {
            Some(dir) => dir.join(abs.file_name().unwrap_or_default()),
            None => abs,
        }
    };
    let root = match &opts.root {
        Some(r) => Some(std::fs::canonicalize(r).map_err(|e| Error::contract(format!("project root {}: {e}", r.display())))?),
        None => None,
    };
    if let Some(root) = &root
        && !entry_path.starts_with(root)
    {
        return Err(Error::contract(format!("{} is outside the project root {}", entry_path.display(), root.display())));
    }

    init_v8();
    let params = v8::CreateParams::default().heap_limits(0, MAX_HEAP_BYTES);
    let isolate = &mut v8::Isolate::new(params);
    let handle = isolate.thread_safe_handle();
    let guard = Guard { watchdog: Watchdog::start(handle.clone()), handle };
    guard.watchdog.arm(budget);

    LOADER.with(|l| {
        *l.borrow_mut() = Some(Loader {
            root,
            modules: HashMap::new(),
            origins: HashMap::new(),
            dependencies: Vec::new(),
            failure: None,
        })
    });
    // Globals in the loader must die before the isolate does.
    struct Uninstall;
    impl Drop for Uninstall {
        fn drop(&mut self) {
            LOADER.with(|l| *l.borrow_mut() = None);
        }
    }
    let _uninstall = Uninstall;

    v8::scope!(let handle_scope, isolate);
    let context = v8::Context::new(handle_scope, Default::default());
    let scope = &mut v8::ContextScope::new(handle_scope, context);
    install_standard_math(scope)?;

    let (namespace, dependencies) = load_entry(scope, source, name, &entry_path)?;

    let meta_value = get(scope, namespace, "meta");
    let meta = read_meta(scope, meta_value)?;

    body(scope, name, meta, dependencies, namespace, &guard)
}

/// Compiles, instantiates and evaluates the entry module; returns its namespace object and the
/// files it pulled in.
fn load_entry<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source: &str,
    name: &str,
    entry_path: &Path,
) -> Result<(v8::Local<'s, v8::Object>, Vec<PathBuf>), Error> {
    v8::tc_scope!(let tc, scope);

    let module = match compile_registered(tc, source, name, Some(entry_path.to_path_buf())) {
        Some(m) => m,
        None => return Err(caught_in(tc, ErrorKind::Compile)),
    };

    if module.instantiate_module(tc, resolve_module).is_none() {
        if let Some(e) = with_loader(|l| l.failure.take()) {
            return Err(e);
        }
        return Err(caught_in(tc, ErrorKind::Compile));
    }

    if module.evaluate(tc).is_none() {
        return Err(caught_in(tc, ErrorKind::Runtime));
    }
    if module.get_status() == v8::ModuleStatus::Errored {
        let exception = module.get_exception();
        return Err(error_from_exception(tc, exception, ErrorKind::Runtime));
    }
    if tc.has_terminated() {
        return Err(Error::timeout());
    }

    let namespace = v8::Local::<v8::Object>::try_from(module.get_module_namespace())
        .map_err(|_| Error::internal("module namespace is not an object"))?;
    let dependencies = with_loader(|l| std::mem::take(&mut l.dependencies));
    Ok((namespace, dependencies))
}

/// Runs the standard math in a fresh context. Must precede the prelude and every source module:
/// a module compiled before it would still see the engine's own `Math`.
fn install_standard_math(scope: &mut v8::PinScope<'_, '_>) -> Result<(), Error> {
    run_script(scope, MATH, MATH_ORIGIN).map(|_| ())
}

fn run_script<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str, origin_name: &str) -> Result<v8::Local<'s, v8::Value>, Error> {
    v8::tc_scope!(let tc, scope);
    let code = v8::String::new(tc, code).ok_or_else(|| Error::contract("source too large for V8"))?;
    let name = v8::String::new(tc, origin_name).unwrap();
    let origin = v8::ScriptOrigin::new(tc, name.into(), 0, 0, false, 0, None, false, false, false, None);
    let script = match v8::Script::compile(tc, code, Some(&origin)) {
        Some(s) => s,
        None => return Err(caught_in(tc, ErrorKind::Compile)),
    };
    script.run(tc).ok_or_else(|| caught_in(tc, ErrorKind::Runtime))
}

fn get<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<v8::Object>, key: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = v8::String::new(scope, key).unwrap();
    obj.get(scope, k.into()).filter(|v| !v.is_undefined())
}

/// Builds an [`Error`] from what `tc` caught.
fn caught_in(tc: &mut v8::PinnedRef<'_, v8::TryCatch<v8::HandleScope>>, kind: ErrorKind) -> Error {
    if tc.has_terminated() {
        return Error::timeout();
    }
    if let Some(e) = with_loader(|l| l.failure.take()) {
        return e;
    }
    let Some(exception) = tc.exception() else {
        return Error { kind, message: "unknown error".into(), file: None, line: None, column: None, diagnostics: Vec::new() };
    };
    let message = exception.to_string(tc).map(|s| s.to_rust_string_lossy(tc)).unwrap_or_else(|| "unknown error".into());
    let stack = tc.stack_trace().and_then(|s| s.to_string(tc)).map(|s| s.to_rust_string_lossy(tc));
    // The TryCatch's message carries the throw site; a message created from the exception
    // value afterwards does not (its location is already gone).
    let (file, line, column) = match tc.message() {
        Some(m) => position_of(tc, m),
        None => (None, None, None),
    };
    let message = match stack {
        Some(stack) if stack.len() > message.len() => stack,
        _ => message,
    };
    Error { kind, message, file, line, column, diagnostics: Vec::new() }
}

/// File/line/column of a message, unless it points into the prelude or the harness.
fn position_of(scope: &mut v8::PinScope<'_, '_>, msg: v8::Local<v8::Message>) -> (Option<String>, Option<u32>, Option<u32>) {
    let resource = msg.get_script_resource_name(scope).and_then(|n| n.to_string(scope)).map(|n| n.to_rust_string_lossy(scope));
    let positioned = resource
        .as_deref()
        .is_some_and(|r| r != PRELUDE_SPECIFIER && r != RUN_ORIGIN && r != MATH_ORIGIN);
    if positioned {
        (resource, msg.get_line_number(scope).map(|l| l as u32), Some(msg.get_start_column() as u32 + 1))
    } else {
        (None, None, None)
    }
}

/// Builds an [`Error`] from an exception value that was not caught by a TryCatch (a module
/// evaluation error). Positions are reported for any user module; throws inside the prelude or
/// the harness carry no position.
fn error_from_exception(scope: &mut v8::PinScope<'_, '_>, exception: v8::Local<v8::Value>, kind: ErrorKind) -> Error {
    let message = exception.to_string(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_else(|| "unknown error".into());
    let stack = v8::Local::<v8::Object>::try_from(exception)
        .ok()
        .and_then(|o| get(scope, o, "stack"))
        .and_then(|s| s.to_string(scope))
        .map(|s| s.to_rust_string_lossy(scope));
    let msg = v8::Exception::create_message(scope, exception);
    let (file, line, column) = position_of(scope, msg);
    let message = match stack {
        Some(stack) if stack.len() > message.len() => stack,
        _ => message,
    };
    Error { kind, message, file, line, column, diagnostics: Vec::new() }
}

/// The source's `meta`, checked field by field in the order SPEC.md's table gives, with the
/// messages every host reports (js/contract.js is held to these by test/contract.test.js).
fn read_meta(scope: &mut v8::PinScope<'_, '_>, value: Option<v8::Local<v8::Value>>) -> Result<Meta, Error> {
    let Some(value) = value else {
        return Err(Error::contract("source has no `export const meta = { ... }`"));
    };
    let obj = v8::Local::<v8::Object>::try_from(value).map_err(|_| Error::contract("`meta` is not an object"))?;

    // Required: a source says which contract it was written against, so no later host can read
    // it as something else.
    let Some(v) = get(scope, obj, "api") else {
        return Err(Error::contract(format!(
            "meta.api is required: this compiler provides api {PRELUDE_VERSION} and accepts {API_FLOOR} to {PRELUDE_VERSION}"
        )));
    };
    let api = if v.is_number() { v.number_value(scope).unwrap() } else { -1.0 };
    if !(API_FLOOR as f64..=PRELUDE_VERSION as f64).contains(&api) || api.fract() != 0.0 {
        return Err(Error::contract(format!(
            "source declares meta.api {} but this compiler provides api {PRELUDE_VERSION} and accepts {API_FLOOR} to {PRELUDE_VERSION}",
            v.to_rust_string_lossy(scope)
        )));
    }
    let name = match get(scope, obj, "name") {
        Some(v) if v.is_string() => Some(v.to_rust_string_lossy(scope)),
        Some(_) => return Err(Error::contract("meta.name must be a string")),
        None => None,
    };
    let duration = match get(scope, obj, "duration") {
        Some(v) if v.is_number() => v.number_value(scope).unwrap(),
        Some(_) => return Err(Error::contract("meta.duration must be a number of seconds")),
        None => return Err(Error::contract("meta.duration is required (seconds)")),
    };
    if !(duration.is_finite() && duration > 0.0 && duration <= 600.0) {
        return Err(Error::contract("meta.duration must be in (0, 600] seconds"));
    }
    let channels = match get(scope, obj, "channels") {
        Some(v) if v.is_number() => v.number_value(scope).unwrap(),
        Some(_) => return Err(Error::contract("meta.channels must be 1 or 2")),
        None => 1.0,
    };
    if channels != 1.0 && channels != 2.0 {
        return Err(Error::contract("meta.channels must be 1 or 2"));
    }
    let sample_rate = match get(scope, obj, "sampleRate") {
        Some(v) if v.is_number() => {
            let r = v.number_value(scope).unwrap();
            if !(8_000.0..=192_000.0).contains(&r) || r.fract() != 0.0 {
                return Err(Error::contract("meta.sampleRate must be an integer in [8000, 192000]"));
            }
            Some(r as u32)
        }
        Some(_) => return Err(Error::contract("meta.sampleRate must be a number")),
        None => None,
    };
    // An unsigned 32-bit integer, as written. Converting anything else would be a rule every host
    // had to reproduce exactly, for a seed nobody meant.
    let seed = match get(scope, obj, "seed") {
        Some(v) if v.is_number() => {
            let s = v.number_value(scope).unwrap();
            if !(0.0..=u32::MAX as f64).contains(&s) || s.fract() != 0.0 {
                return Err(Error::contract("meta.seed must be an integer in [0, 4294967295]"));
            }
            s as u32
        }
        Some(_) => return Err(Error::contract("meta.seed must be a number")),
        None => 0,
    };
    let looping = match get(scope, obj, "loop") {
        Some(v) if v.is_boolean() => v.is_true(),
        Some(_) => return Err(Error::contract("meta.loop must be a boolean")),
        None => false,
    };
    Ok(Meta { name, duration, channels: channels as u32, sample_rate, seed, looping })
}
