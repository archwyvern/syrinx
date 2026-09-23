//! The bridge to the run wrapper: calling `run.stem` and a stream's driver, and moving planes
//! across the boundary. Arithmetic happens in `run.js`; what happens here is permutation
//! (interleaving, chopping, copying out) and the finite check.

use std::time::Duration;

use crate::{BLOCK_FRAMES, Error, ErrorKind, Meta, RUN};

use super::watchdog::Guard;
use super::{RUN_ORIGIN, caught_in, get, run_script};

/// The leading letter is load-bearing: an integer-like key sorts itself to the front of a
/// JavaScript object, which would silently change the order stems are summed in.
pub(super) fn valid_stem_name(name: &str) -> bool {
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    name.len() <= 64 && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// The source's layers, in declaration order.
pub(super) fn read_stems<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    namespace: v8::Local<v8::Object>,
) -> Result<Vec<(String, v8::Local<'s, v8::Function>)>, Error> {
    let Some(value) = get(scope, namespace, "stems") else {
        return Err(Error::contract(
            "source has no `stems` export; a sound is one or more named layers: export const stems = { name(ctx) { ... } }",
        ));
    };
    let obj = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| Error::contract("`stems` must be an object of functions"))?;
    let keys = obj
        .get_own_property_names(scope, v8::GetPropertyNamesArgsBuilder::new().build())
        .ok_or_else(|| Error::internal("cannot enumerate `stems`"))?;
    let mut out = Vec::with_capacity(keys.length() as usize);
    for i in 0..keys.length() {
        let Some(key) = keys.get_index(scope, i) else { continue };
        let stem = key.to_rust_string_lossy(scope);
        if !valid_stem_name(&stem) {
            return Err(Error::contract(format!(
                "stem name \"{stem}\" must start with a letter and contain only letters, digits, _ . -"
            )));
        }
        let Some(value) = obj.get(scope, key) else { continue };
        let f = v8::Local::<v8::Function>::try_from(value)
            .map_err(|_| Error::contract(format!("stem \"{stem}\" is not a function")))?;
        out.push((stem, f));
    }
    if out.is_empty() {
        return Err(Error::contract("`stems` is empty; declare at least one layer"));
    }
    Ok(out)
}

/// The `{ BLOCK_FRAMES, stem, mix }` object from `run.js`, evaluated once per isolate.
pub(super) fn run_object<'s>(scope: &mut v8::PinScope<'s, '_>) -> Result<v8::Local<'s, v8::Object>, Error> {
    let value = run_script(scope, RUN, RUN_ORIGIN)?;
    v8::Local::<v8::Object>::try_from(value).map_err(|_| Error::internal("the run wrapper is not an object"))
}

pub(super) fn run_entry<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    run: v8::Local<v8::Object>,
    entry: &str,
) -> Result<v8::Local<'s, v8::Function>, Error> {
    let value = get(scope, run, entry).ok_or_else(|| Error::internal(format!("the run wrapper has no `{entry}`")))?;
    v8::Local::<v8::Function>::try_from(value).map_err(|_| Error::internal(format!("run.{entry} is not a function")))
}

/// A Float32Array over a copy of `data`.
pub(super) fn f32_array<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    data: &[f32],
) -> Result<v8::Local<'s, v8::Float32Array>, Error> {
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for v in data {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let buffer = v8::ArrayBuffer::with_backing_store(scope, &store);
    v8::Float32Array::new(scope, buffer, 0, data.len()).ok_or_else(|| Error::internal("cannot build a Float32Array"))
}

/// An array of `channels` Float32Arrays per layer, from planes: what the run wrapper's mix
/// stage takes as `buffers` (whole) or `blocks` (one block).
pub(super) fn plane_arrays<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    layers: &[Vec<Vec<f32>>],
) -> Result<v8::Local<'s, v8::Array>, Error> {
    let outer = v8::Array::new(scope, layers.len() as i32);
    for (i, planes) in layers.iter().enumerate() {
        let inner = v8::Array::new(scope, planes.len() as i32);
        for (c, plane) in planes.iter().enumerate() {
            let array = f32_array(scope, plane)?;
            inner.set_index(scope, c as u32, array.into());
        }
        outer.set_index(scope, i as u32, inner.into());
    }
    Ok(outer)
}

/// Calls a function and copies the planes it returned out of V8.
pub(super) fn call_for_planes<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    entry: v8::Local<'s, v8::Function>,
    args: &[v8::Local<'s, v8::Value>],
    frames: usize,
    channels: u32,
) -> Result<Vec<Vec<f32>>, Error> {
    let result = {
        v8::tc_scope!(let tc, scope);
        let undefined = v8::undefined(tc).into();
        match entry.call(tc, undefined, args) {
            Some(r) => r,
            None => return Err(caught_in(tc, ErrorKind::Runtime)),
        }
    };
    planes_from(scope, result, frames, channels)
}

/// Planes out of a run-wrapper return value: exactly `channels` typed arrays of `frames`.
pub(super) fn planes_from<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    frames: usize,
    channels: u32,
) -> Result<Vec<Vec<f32>>, Error> {
    let array = v8::Local::<v8::Array>::try_from(value)
        .map_err(|_| Error::internal("the run wrapper did not return planes"))?;
    if array.length() != channels {
        return Err(Error::internal(format!(
            "the run wrapper returned {} plane(s), expected {channels}",
            array.length()
        )));
    }
    let mut planes = Vec::with_capacity(channels as usize);
    for c in 0..channels {
        let plane = array.get_index(scope, c).ok_or_else(|| Error::internal("a plane is missing"))?;
        let view = v8::Local::<v8::ArrayBufferView>::try_from(plane)
            .map_err(|_| Error::internal("a plane is not a typed array"))?;
        let mut bytes = vec![0u8; frames * 4];
        let copied = view.copy_contents(&mut bytes);
        if copied != bytes.len() {
            return Err(Error::internal(format!("expected {} bytes in a plane, got {copied}", bytes.len())));
        }
        planes.push(bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect());
    }
    Ok(planes)
}

/// Planes to interleaved frames. A permutation of values already decided, so a host may do it.
pub(super) fn interleave(planes: &[Vec<f32>], frames: usize, channels: u32) -> Vec<f32> {
    let channels = channels as usize;
    let mut out = vec![0.0f32; frames * channels];
    for (c, plane) in planes.iter().enumerate() {
        for i in 0..frames {
            out[i * channels + c] = plane[i];
        }
    }
    out
}

/// Interleaved frames back to planes.
pub(super) fn deinterleave(samples: &[f32], frames: usize, channels: u32) -> Vec<Vec<f32>> {
    (0..channels as usize).map(|c| (0..frames).map(|i| samples[i * channels as usize + c]).collect()).collect()
}

/// Frame-major, so the frame reported is the earliest one, as the JavaScript host reports it.
/// `base` is the absolute frame of the planes' first sample.
pub(super) fn check_finite_planes(planes: &[Vec<f32>], base: usize, what: &str) -> Result<(), Error> {
    let frames = planes.first().map_or(0, Vec::len);
    for i in 0..frames {
        for (c, plane) in planes.iter().enumerate() {
            if !plane[i].is_finite() {
                return Err(Error::contract(format!(
                    "{what} produced a non-finite sample at frame {} channel {c}",
                    base + i
                )));
            }
        }
    }
    Ok(())
}

/// The length of the block at `offset`.
pub(super) fn block_len(offset: usize, frames: usize) -> usize {
    BLOCK_FRAMES.min(frames - offset)
}

/// Pulls one block from a run-wrapper driver: `driver(offset[, blocks])`, checked finite. The
/// watchdog is armed around the call only, with the whole budget for this block.
#[allow(clippy::too_many_arguments)]
pub(super) fn pull_block<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    guard: &Guard,
    budget: Duration,
    driver: v8::Local<'s, v8::Function>,
    offset: usize,
    frames: usize,
    channels: u32,
    blocks: Option<&[Vec<Vec<f32>>]>,
    what: &str,
) -> Result<Vec<Vec<f32>>, Error> {
    v8::scope!(let inner, scope);
    let n = block_len(offset, frames);
    let mut args: Vec<v8::Local<v8::Value>> = vec![v8::Number::new(inner, offset as f64).into()];
    if let Some(blocks) = blocks {
        args.push(plane_arrays(inner, blocks)?.into());
    }
    guard.watchdog.arm(budget);
    let result = call_for_planes(inner, driver, &args, n, channels);
    let fired = guard.watchdog.disarm();
    let planes = result?;
    if fired {
        return Err(Error::timeout());
    }
    check_finite_planes(&planes, offset, what)?;
    Ok(planes)
}

/// What one layer's function returned, normalised by the run wrapper.
pub(super) enum StemForm<'s> {
    Whole(Vec<Vec<f32>>),
    Live(v8::Local<'s, v8::Function>),
}

/// Calls one layer through the run wrapper. Which other layers the source declares, and whether
/// they were rendered, cannot affect the result: the module graph is evaluated fresh per isolate.
pub(super) fn stem_setup<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    namespace: v8::Local<v8::Object>,
    meta: &Meta,
    sample_rate: u32,
    frames: usize,
    stem: &str,
) -> Result<StemForm<'s>, Error> {
    let stems = read_stems(scope, namespace)?;
    let Some(f) = stems.iter().find(|(n, _)| n == stem).map(|(_, f)| *f) else {
        let names: Vec<&str> = stems.iter().map(|(n, _)| n.as_str()).collect();
        return Err(Error::contract(format!("no stem named \"{stem}\"; this source declares {}", names.join(", "))));
    };
    let run = run_object(scope)?;
    let entry = run_entry(scope, run, "stem")?;
    let stem_name = v8::String::new(scope, stem).ok_or_else(|| Error::internal("stem name too large"))?;
    let args: [v8::Local<v8::Value>; 7] = [
        f.into(),
        v8::Number::new(scope, sample_rate as f64).into(),
        v8::Number::new(scope, frames as f64).into(),
        v8::Number::new(scope, meta.duration).into(),
        v8::Number::new(scope, meta.seed as f64).into(),
        v8::Number::new(scope, meta.channels as f64).into(),
        stem_name.into(),
    ];
    let result = {
        v8::tc_scope!(let tc, scope);
        let undefined = v8::undefined(tc).into();
        match entry.call(tc, undefined, &args) {
            Some(r) => r,
            None => return Err(caught_in(tc, ErrorKind::Runtime)),
        }
    };
    if let Ok(driver) = v8::Local::<v8::Function>::try_from(result) {
        return Ok(StemForm::Live(driver));
    }
    let planes = planes_from(scope, result, frames, meta.channels)?;
    check_finite_planes(&planes, 0, &format!("stem \"{stem}\""))?;
    Ok(StemForm::Whole(planes))
}

/// Drives a stream to the end inside its own isolate: whole planes, block by block.
#[allow(clippy::too_many_arguments)]
pub(super) fn drain<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    guard: &Guard,
    budget: Duration,
    driver: v8::Local<'s, v8::Function>,
    frames: usize,
    channels: u32,
    blocks_at: Option<&dyn Fn(usize) -> Vec<Vec<Vec<f32>>>>,
    what: &str,
) -> Result<Vec<Vec<f32>>, Error> {
    let mut out: Vec<Vec<f32>> = (0..channels).map(|_| Vec::with_capacity(frames)).collect();
    let mut offset = 0;
    while offset < frames {
        let blocks = blocks_at.map(|f| f(offset));
        let planes = pull_block(scope, guard, budget, driver, offset, frames, channels, blocks.as_deref(), what)?;
        for (c, plane) in planes.into_iter().enumerate() {
            out[c].extend_from_slice(&plane);
        }
        offset += BLOCK_FRAMES;
    }
    Ok(out)
}

/// The blocks at `offset` of whole interleaved layers, as planes per layer.
pub(super) fn chop(layers: &[&[f32]], offset: usize, frames: usize, channels: u32) -> Vec<Vec<Vec<f32>>> {
    let n = block_len(offset, frames);
    let ch = channels as usize;
    layers.iter().map(|samples| deinterleave(&samples[offset * ch..(offset + n) * ch], n, channels)).collect()
}
