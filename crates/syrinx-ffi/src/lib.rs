//! C ABI over syrinx-core. See `include/syrinx.h` for the contract as a header.
//!
//! Every call returns an opaque `SyrinxRender` that is either OK (carries meta + samples)
//! or an error (carries kind, message, position). Accessors are total: they tolerate null and
//! return zero/null for anything that is not present. The caller frees the result with
//! `syrinx_render_free`. No call blocks on another; each render runs in its own V8 isolate,
//! so calling from several threads at once is fine.
//!
//! A `SyrinxStream` hands the same sound out one block at a time: `syrinx_stream_open`, then
//! `syrinx_stream_next` until it returns null, each block a `SyrinxRender` of its own. A stream
//! is used from one thread at a time; freeing it stops whatever is still computing.

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;
use std::time::Duration;

use syrinx_core::{Block, Error, ErrorKind, Meta, RenderOptions, Rendered, Stream, Target};

pub struct SyrinxRender {
    ok: bool,
    kind: i32,
    message: CString,
    file: CString,
    line: i32,
    column: i32,
    dependencies: Vec<CString>,
    stems: Vec<CString>,
    /// The declared name; None when the source declares none.
    name: Option<CString>,
    duration: f64,
    seed: u32,
    looping: bool,
    sample_rate: u32,
    channels: u32,
    frames: u32,
    offset: u64,
    samples: Vec<f32>,
}

/// An open stream: the source's description (or the reason it would not open) and the blocks.
pub struct SyrinxStream {
    info: SyrinxRender,
    streaming: bool,
    stream: Option<Stream>,
}

const KIND_NONE: i32 = 0;
const KIND_CHECK: i32 = 1;
const KIND_COMPILE: i32 = 2;
const KIND_RUNTIME: i32 = 3;
const KIND_TIMEOUT: i32 = 4;
const KIND_CONTRACT: i32 = 5;
const KIND_INTERNAL: i32 = 6;

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "\u{fffd}")).unwrap()
}

impl SyrinxRender {
    fn failure(kind: i32, message: &str, line: Option<u32>, column: Option<u32>) -> Self {
        Self {
            ok: false,
            kind,
            message: cstring(message),
            file: CString::default(),
            line: line.map_or(0, |l| l as i32),
            column: column.map_or(0, |c| c as i32),
            dependencies: Vec::new(),
            stems: Vec::new(),
            name: None,
            duration: 0.0,
            seed: 0,
            looping: false,
            sample_rate: 0,
            channels: 0,
            frames: 0,
            offset: 0,
            samples: Vec::new(),
        }
    }

    fn from_error(e: &Error) -> Self {
        let kind = match e.kind {
            ErrorKind::Check => KIND_CHECK,
            ErrorKind::Compile => KIND_COMPILE,
            ErrorKind::Runtime => KIND_RUNTIME,
            ErrorKind::Timeout => KIND_TIMEOUT,
            ErrorKind::Contract => KIND_CONTRACT,
            ErrorKind::Internal => KIND_INTERNAL,
        };
        let message = if e.diagnostics.len() > 1 {
            e.diagnostics
                .iter()
                .map(|d| format!("{}:{}: {}", d.line, d.column, d.message))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            e.message.clone()
        };
        let mut out = Self::failure(kind, &message, e.line, e.column);
        if let Some(f) = &e.file {
            out.file = cstring(f);
        }
        out
    }

    fn from_meta(meta: &Meta) -> Self {
        Self {
            ok: true,
            kind: KIND_NONE,
            message: CString::default(),
            file: CString::default(),
            line: 0,
            column: 0,
            dependencies: Vec::new(),
            stems: Vec::new(),
            name: meta.name.as_deref().map(cstring),
            duration: meta.duration,
            seed: meta.seed,
            looping: meta.looping,
            sample_rate: meta.sample_rate.unwrap_or(0),
            channels: meta.channels,
            frames: 0,
            offset: 0,
            samples: Vec::new(),
        }
    }

    fn from_rendered(r: Rendered) -> Self {
        let mut out = Self::from_meta(&r.meta);
        out.sample_rate = r.sample_rate;
        out.channels = r.channels;
        out.frames = r.frames as u32;
        out.samples = r.samples;
        out.dependencies = r.dependencies.iter().map(|p| cstring(&p.to_string_lossy())).collect();
        out.stems = r.stem_names.iter().map(|s| cstring(s)).collect();
        out
    }

    /// The open result of a stream: everything but samples.
    fn from_source(source: &syrinx_core::Source) -> Self {
        let mut out = Self::from_meta(source.meta());
        out.sample_rate = source.sample_rate();
        out.channels = source.channels();
        out.frames = source.frames() as u32;
        out.dependencies = source.dependencies().iter().map(|p| cstring(&p.to_string_lossy())).collect();
        out.stems = source.stem_names().iter().map(|s| cstring(s)).collect();
        out
    }

    /// One block: the stream's meta, the block's frames and samples, and where it starts.
    fn from_block(info: &SyrinxRender, block: Block) -> Self {
        Self {
            ok: true,
            kind: KIND_NONE,
            message: CString::default(),
            file: CString::default(),
            line: 0,
            column: 0,
            dependencies: Vec::new(),
            stems: Vec::new(),
            name: info.name.clone(),
            duration: info.duration,
            seed: info.seed,
            looping: info.looping,
            sample_rate: info.sample_rate,
            channels: info.channels,
            frames: block.frames as u32,
            offset: block.offset as u64,
            samples: block.samples,
        }
    }
}

/// # Safety
/// `source` must point to `source_len` readable bytes; `name` must be null or NUL-terminated.
unsafe fn read_inputs<'a>(source: *const u8, source_len: usize, name: *const c_char) -> Result<(&'a str, String), Box<SyrinxRender>> {
    if source.is_null() {
        return Err(Box::new(SyrinxRender::failure(KIND_INTERNAL, "source is null", None, None)));
    }
    let bytes = unsafe { std::slice::from_raw_parts(source, source_len) };
    let source = std::str::from_utf8(bytes)
        .map_err(|e| Box::new(SyrinxRender::failure(KIND_INTERNAL, &format!("source is not UTF-8: {e}"), None, None)))?;
    let name = if name.is_null() {
        "source.js".to_string()
    } else {
        unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned()
    };
    Ok((source, name))
}

fn timeout(timeout_ms: u32) -> Duration {
    if timeout_ms == 0 { RenderOptions::default().timeout } else { Duration::from_millis(timeout_ms as u64) }
}

fn guarded(f: impl FnOnce() -> SyrinxRender) -> *mut SyrinxRender {
    let result = catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|p| {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        SyrinxRender::failure(KIND_INTERNAL, &format!("internal panic: {msg}"), None, None)
    });
    Box::into_raw(Box::new(result))
}

/// Contract/prelude version. Cache artefacts keyed on this are invalidated when it changes.
#[unsafe(no_mangle)]
pub extern "C" fn syrinx_version() -> u32 {
    syrinx_core::PRELUDE_VERSION
}

/// Human-readable build description. Static; do not free.
#[unsafe(no_mangle)]
pub extern "C" fn syrinx_version_string() -> *const c_char {
    static S: OnceLock<CString> = OnceLock::new();
    S.get_or_init(|| {
        cstring(&format!(
            "syrinx {} (prelude {}, V8 {})",
            env!("CARGO_PKG_VERSION"),
            syrinx_core::PRELUDE_VERSION,
            syrinx_core::v8_version()
        ))
    })
    .as_ptr()
}

/// The prelude source, NUL-terminated. Static; do not free. For editors and tooling.
#[unsafe(no_mangle)]
pub extern "C" fn syrinx_prelude() -> *const c_char {
    static S: OnceLock<CString> = OnceLock::new();
    S.get_or_init(|| cstring(syrinx_core::PRELUDE)).as_ptr()
}

/// TypeScript declarations for the prelude and the source contract. Static; do not free.
#[unsafe(no_mangle)]
pub extern "C" fn syrinx_types() -> *const c_char {
    static S: OnceLock<CString> = OnceLock::new();
    S.get_or_init(|| cstring(syrinx_core::TYPES)).as_ptr()
}

/// Compiles `source` to samples. `name` is the source's path: relative imports resolve from
/// its directory. `root` (null = unrestricted) is the directory imports may not escape.
/// `sample_rate` 0 means "the source's, else 48000"; `timeout_ms` 0 means the default budget.
/// Never returns null.
///
/// # Safety
/// `source` must point to `source_len` readable bytes; `name` must be null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_render(
    source: *const u8,
    source_len: usize,
    name: *const c_char,
    root: *const c_char,
    sample_rate: u32,
    timeout_ms: u32,
) -> *mut SyrinxRender {
    guarded(|| {
        let (source, name) = match unsafe { read_inputs(source, source_len, name) } {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let opts = RenderOptions {
            sample_rate: if sample_rate == 0 { None } else { Some(sample_rate) },
            timeout: timeout(timeout_ms),
            root: unsafe { read_root(root) },
            // The C ABI renders the mix, which is all any consumer of it has ever wanted:
            // VLC and GStreamer play a sound, and the engine bakes one artefact per source.
            ..RenderOptions::default()
        };
        match syrinx_core::render(source, &name, &opts) {
            Ok(r) => SyrinxRender::from_rendered(r),
            Err(e) => SyrinxRender::from_error(&e),
        }
    })
}

/// Runs the static check and the module graph, returning meta and dependencies but no samples
/// (`syrinx_render_frames` is 0). Never returns null.
///
/// # Safety
/// As for [`syrinx_render`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_inspect(source: *const u8, source_len: usize, name: *const c_char, root: *const c_char) -> *mut SyrinxRender {
    guarded(|| {
        let (source, name) = match unsafe { read_inputs(source, source_len, name) } {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let opts = RenderOptions { root: unsafe { read_root(root) }, ..RenderOptions::default() };
        match syrinx_core::inspect(source, &name, &opts) {
            Ok(info) => {
                let mut out = SyrinxRender::from_meta(&info.meta);
                out.dependencies = info.dependencies.iter().map(|p| cstring(&p.to_string_lossy())).collect();
                out.stems = info.stems.iter().map(|s| cstring(s)).collect();
                out
            }
            Err(e) => SyrinxRender::from_error(&e),
        }
    })
}

/// Opens `source` as a stream. Arguments as for [`syrinx_render`]; `stems` is null or empty
/// for the mix, or a comma-separated list of layers to sum without the mix stage. Never returns
/// null: `syrinx_stream_info` says whether it opened.
///
/// # Safety
/// As for [`syrinx_render`]; `stems` must be null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_stream_open(
    source: *const u8,
    source_len: usize,
    name: *const c_char,
    root: *const c_char,
    sample_rate: u32,
    timeout_ms: u32,
    stems: *const c_char,
) -> *mut SyrinxStream {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let (source, name) = match unsafe { read_inputs(source, source_len, name) } {
            Ok(v) => v,
            Err(e) => return SyrinxStream { info: *e, streaming: false, stream: None },
        };
        let target = match unsafe { read_root(stems) } {
            None => Target::Mix,
            Some(list) => Target::Stems(
                list.to_string_lossy().split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
            ),
        };
        let opts = RenderOptions {
            sample_rate: if sample_rate == 0 { None } else { Some(sample_rate) },
            timeout: timeout(timeout_ms),
            root: unsafe { read_root(root) },
            target,
        };
        match Stream::open(source, &name, &opts) {
            Ok(s) => SyrinxStream { info: SyrinxRender::from_source(s.source()), streaming: s.streaming(), stream: Some(s) },
            Err(e) => SyrinxStream { info: SyrinxRender::from_error(&e), streaming: false, stream: None },
        }
    }))
    .unwrap_or_else(|p| {
        let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
        SyrinxStream { info: SyrinxRender::failure(KIND_INTERNAL, &format!("internal panic: {msg}"), None, None), streaming: false, stream: None }
    });
    Box::into_raw(Box::new(result))
}

/// The open result: ok or the error, the meta, the geometry (`frames` is the whole sound),
/// the dependencies and the layers. Owned by the stream; do not free.
///
/// # Safety
/// `s` must be null or a pointer returned by `syrinx_stream_open` that has not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_stream_info(s: *const SyrinxStream) -> *const SyrinxRender {
    match unsafe { s.as_ref() } {
        Some(s) => &s.info,
        None => std::ptr::null(),
    }
}

/// True when the blocks are computed on demand: at least one layer streams and the mix stage
/// streams. False when the whole sound was rendered at open and is handed out in blocks.
///
/// # Safety
/// As for [`syrinx_stream_info`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_stream_streaming(s: *const SyrinxStream) -> bool {
    unsafe { s.as_ref() }.is_some_and(|s| s.streaming)
}

/// The next block, or null after the last (or when the stream never opened). A block is a
/// `SyrinxRender` of its own: `syrinx_render_frames` is the block's length,
/// `syrinx_render_offset` its first frame, `syrinx_render_samples` its interleaved samples; an
/// error block reports the failure through the error accessors and ends the stream. Free every
/// block with `syrinx_render_free`.
///
/// # Safety
/// As for [`syrinx_stream_info`]; one thread at a time.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_stream_next(s: *mut SyrinxStream) -> *mut SyrinxRender {
    let Some(s) = (unsafe { s.as_mut() }) else {
        return std::ptr::null_mut();
    };
    let Some(stream) = s.stream.as_mut() else {
        return std::ptr::null_mut();
    };
    let result = catch_unwind(AssertUnwindSafe(|| stream.next_block()));
    let block = match result {
        Ok(Ok(Some(block))) => SyrinxRender::from_block(&s.info, block),
        Ok(Ok(None)) => return std::ptr::null_mut(),
        Ok(Err(e)) => SyrinxRender::from_error(&e),
        Err(p) => {
            let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
            SyrinxRender::failure(KIND_INTERNAL, &format!("internal panic: {msg}"), None, None)
        }
    };
    Box::into_raw(Box::new(block))
}

/// Frees a stream, stopping whatever it was still computing. Null is a no-op.
///
/// # Safety
/// `s` must be null or a pointer returned by `syrinx_stream_open`, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_stream_free(s: *mut SyrinxStream) {
    if !s.is_null() {
        drop(unsafe { Box::from_raw(s) });
    }
}

/// # Safety
/// `root` must be null or NUL-terminated.
unsafe fn read_root(root: *const c_char) -> Option<std::path::PathBuf> {
    if root.is_null() {
        None
    } else {
        let s = unsafe { CStr::from_ptr(root) }.to_string_lossy();
        if s.is_empty() { None } else { Some(std::path::PathBuf::from(s.into_owned())) }
    }
}

macro_rules! accessor {
    ($name:ident, $ty:ty, $default:expr, |$r:ident| $body:expr) => {
        /// # Safety
        /// `r` must be null or a pointer returned by `syrinx_render`/`syrinx_inspect` that has not been freed.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(r: *const SyrinxRender) -> $ty {
            match unsafe { r.as_ref() } {
                Some($r) => $body,
                None => $default,
            }
        }
    };
}

accessor!(syrinx_render_ok, bool, false, |r| r.ok);
accessor!(syrinx_render_error_kind, i32, KIND_INTERNAL, |r| r.kind);
accessor!(syrinx_render_error, *const c_char, std::ptr::null(), |r| if r.ok { std::ptr::null() } else { r.message.as_ptr() });
accessor!(syrinx_render_error_file, *const c_char, std::ptr::null(), |r| if r.ok || r.file.as_bytes().is_empty() { std::ptr::null() } else { r.file.as_ptr() });
accessor!(syrinx_render_error_line, i32, 0, |r| r.line);
accessor!(syrinx_render_error_column, i32, 0, |r| r.column);
accessor!(syrinx_render_name, *const c_char, std::ptr::null(), |r| match (&r.name, r.ok) {
    (Some(name), true) => name.as_ptr(),
    _ => std::ptr::null(),
});
accessor!(syrinx_render_duration, f64, 0.0, |r| r.duration);
accessor!(syrinx_render_seed, u32, 0, |r| r.seed);
accessor!(syrinx_render_loop, bool, false, |r| r.looping);
accessor!(syrinx_render_sample_rate, u32, 0, |r| r.sample_rate);
accessor!(syrinx_render_channels, u32, 0, |r| r.channels);
accessor!(syrinx_render_frames, u32, 0, |r| r.frames);
accessor!(syrinx_render_offset, u64, 0, |r| r.offset);
accessor!(syrinx_render_dependency_count, u32, 0, |r| r.dependencies.len() as u32);
accessor!(syrinx_render_stem_count, u32, 0, |r| r.stems.len() as u32);
accessor!(syrinx_render_samples, *const f32, std::ptr::null(), |r| if r.samples.is_empty() { std::ptr::null() } else { r.samples.as_ptr() });

/// The `i`th imported file (canonical path), or null when out of range. Owned by `r`.
///
/// # Safety
/// `r` must be null or a live result pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_render_dependency(r: *const SyrinxRender, i: u32) -> *const c_char {
    match unsafe { r.as_ref() } {
        Some(r) => r.dependencies.get(i as usize).map_or(std::ptr::null(), |d| d.as_ptr()),
        None => std::ptr::null(),
    }
}

/// The `i`th layer the source declares, in declaration order, or null when out of range. Owned
/// by `r`.
///
/// # Safety
/// `r` must be null or a live result pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_render_stem_name(r: *const SyrinxRender, i: u32) -> *const c_char {
    match unsafe { r.as_ref() } {
        Some(r) => r.stems.get(i as usize).map_or(std::ptr::null(), |d| d.as_ptr()),
        None => std::ptr::null(),
    }
}

/// Frees a result. Null is a no-op.
///
/// # Safety
/// `r` must be null or a pointer returned by `syrinx_render`/`syrinx_inspect`, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syrinx_render_free(r: *mut SyrinxRender) {
    if !r.is_null() {
        drop(unsafe { Box::from_raw(r) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLICK: &str = r#"
import { Osc, Env, render, stream } from "./framework/dsp.js";
export const meta = { api: 4, name: "click", duration: 0.06, channels: 1, seed: 3 };
export const stems = {
  tick(ctx) { const e = Env.exp(0.002); return stream(ctx, (t) => e(t) * 0.5); },
  body(ctx) { const body = Osc.sine(ctx.sr); const tone = Env.ad(0.0005, 0.012); return render(ctx, (t) => body.next(1400) * tone(t) * 0.5); },
};
export default function (ctx) { return (offset, frames, { tick, body }) => tick[0].map((v, i) => v + body[0][i]); }
"#;

    fn text(p: *const c_char) -> String {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    /// click.syr in a project with the framework vendored beside it, as its path for the C ABI.
    fn click(test: &str) -> CString {
        let dir = std::env::temp_dir().join(format!("syrinx-ffi-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        syrinx_core::framework::vendor(&dir.join("framework"), "test").unwrap();
        let path = dir.join("click.syr");
        std::fs::write(&path, CLICK).unwrap();
        CString::new(path.to_str().unwrap()).unwrap()
    }

    #[test]
    fn a_stream_concatenates_to_the_render() {
        let name = click("stream");
        let rendered = unsafe { syrinx_render(CLICK.as_ptr(), CLICK.len(), name.as_ptr(), std::ptr::null(), 0, 0) };
        assert!(unsafe { syrinx_render_ok(rendered) });
        let frames = unsafe { syrinx_render_frames(rendered) } as usize;
        let whole = unsafe { std::slice::from_raw_parts(syrinx_render_samples(rendered), frames) }.to_vec();
        assert_eq!(unsafe { syrinx_render_stem_count(rendered) }, 2);
        assert_eq!(text(unsafe { syrinx_render_stem_name(rendered, 1) }), "body");
        assert!(unsafe { syrinx_render_stem_name(rendered, 2) }.is_null());

        let stream = unsafe { syrinx_stream_open(CLICK.as_ptr(), CLICK.len(), name.as_ptr(), std::ptr::null(), 0, 0, std::ptr::null()) };
        let info = unsafe { syrinx_stream_info(stream) };
        assert!(unsafe { syrinx_render_ok(info) });
        assert_eq!(unsafe { syrinx_render_frames(info) } as usize, frames);
        assert_eq!(unsafe { syrinx_render_channels(info) }, 1);
        assert_eq!(text(unsafe { syrinx_render_name(info) }), "click");
        assert_eq!(unsafe { syrinx_render_stem_count(info) }, 2);
        assert!(unsafe { syrinx_stream_streaming(stream) });
        let mut concatenated = Vec::new();
        let mut offsets = Vec::new();
        loop {
            let block = unsafe { syrinx_stream_next(stream) };
            if block.is_null() {
                break;
            }
            assert!(unsafe { syrinx_render_ok(block) });
            let n = unsafe { syrinx_render_frames(block) } as usize;
            offsets.push(unsafe { syrinx_render_offset(block) });
            concatenated.extend_from_slice(unsafe { std::slice::from_raw_parts(syrinx_render_samples(block), n) });
            unsafe { syrinx_render_free(block) };
        }
        assert_eq!(concatenated, whole);
        assert_eq!(offsets, vec![0]);
        assert!(unsafe { syrinx_stream_next(stream) }.is_null(), "the end stays the end");
        unsafe { syrinx_stream_free(stream) };
        unsafe { syrinx_render_free(rendered) };
    }

    #[test]
    fn a_subset_and_a_bad_source_report_through_info() {
        let name = click("subset");
        let subset = CString::new("body").unwrap();
        let stream = unsafe { syrinx_stream_open(CLICK.as_ptr(), CLICK.len(), name.as_ptr(), std::ptr::null(), 0, 0, subset.as_ptr()) };
        assert!(unsafe { syrinx_render_ok(syrinx_stream_info(stream)) });
        assert!(!unsafe { syrinx_stream_streaming(stream) }, "one whole layer and no mix stage");
        let block = unsafe { syrinx_stream_next(stream) };
        assert!(!block.is_null());
        assert_eq!(unsafe { syrinx_render_frames(block) }, 2880);
        unsafe { syrinx_render_free(block) };
        unsafe { syrinx_stream_free(stream) };

        let bad = "export const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: (ctx) => Math.random() };";
        let stream = unsafe { syrinx_stream_open(bad.as_ptr(), bad.len(), name.as_ptr(), std::ptr::null(), 0, 0, std::ptr::null()) };
        let info = unsafe { syrinx_stream_info(stream) };
        assert!(!unsafe { syrinx_render_ok(info) });
        assert_eq!(unsafe { syrinx_render_error_kind(info) }, KIND_CHECK);
        assert!(text(unsafe { syrinx_render_error(info) }).contains("Math.random"));
        assert!(unsafe { syrinx_stream_next(stream) }.is_null());
        unsafe { syrinx_stream_free(stream) };
        unsafe { syrinx_stream_free(std::ptr::null_mut()) };
    }

    #[test]
    fn an_undeclared_name_is_null() {
        let source = "export const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: (ctx) => new Float32Array(ctx.frames) };";
        let name = CString::new("nameless.syr").unwrap();
        let info = unsafe { syrinx_inspect(source.as_ptr(), source.len(), name.as_ptr(), std::ptr::null()) };
        assert!(unsafe { syrinx_render_ok(info) });
        assert!(unsafe { syrinx_render_name(info) }.is_null(), "no default name: absent is NULL");
        unsafe { syrinx_render_free(info) };
    }
}
