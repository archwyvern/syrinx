//! The mix stage, started: the probe that classifies the default export, then a mix stream
//! fed one block of every layer at a time (restartable on the block grid), or a whole-buffer
//! mix run in a fresh isolate over the whole layers. Dropping it terminates and joins.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::{BLOCK_FRAMES, Block, Error, ErrorKind, Meta, RenderOptions, Rendered};

use super::wrapper::{
    block_len, check_finite_planes, chop, deinterleave, drain, interleave, plane_arrays, planes_from, pull_block,
    read_stems, run_entry, run_object,
};
use super::{Deadline, caught_in, geometry, get, with_source};

pub(super) enum MixSetup {
    Stream,
    Whole,
    Planes(Vec<Vec<f32>>),
    Fail(Error),
}

pub(super) enum MixCommand {
    Restart { from: usize, reply: Sender<Result<(), Error>> },
    Mix { offset: usize, stems: Vec<Vec<f32>>, reply: Sender<Result<Vec<Vec<f32>>, Error>> },
    MixAll { stems: Vec<Vec<f32>>, budget: Duration, reply: Sender<Result<Rendered, Error>> },
}

pub(super) enum MixForm {
    Stream,
    Whole,
    Planes(Vec<Vec<f32>>),
}

/// What the probe found, for the forms that outlive the probe's isolate.
pub(super) struct Probed {
    form: MixForm,
    meta: Meta,
    sample_rate: u32,
    dependencies: Vec<PathBuf>,
    stem_names: Vec<String>,
}

/// The mix stage's inputs that stay the same across calls in one isolate: the run wrapper's
/// `mix` entry (evaluated once, as the wrapper insists) and the default export, if it runs.
#[derive(Clone, Copy)]
struct MixEntry<'s> {
    entry: v8::Local<'s, v8::Function>,
    default: Option<v8::Local<'s, v8::Function>>,
}

fn mix_entry<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    namespace: v8::Local<v8::Object>,
    use_default: bool,
) -> Result<MixEntry<'s>, Error> {
    let default = match (use_default, get(scope, namespace, "default")) {
        (true, Some(value)) => Some(
            v8::Local::<v8::Function>::try_from(value)
                .map_err(|_| Error::contract("the default export is not a function"))?,
        ),
        _ => None,
    };
    let run = run_object(scope)?;
    Ok(MixEntry { entry: run_entry(scope, run, "mix")?, default })
}

/// Calls `run.mix(fn | null, names, buffers | null, ..., from)`: the probe, a restart, or the
/// whole mix.
#[allow(clippy::too_many_arguments)]
fn call_mix<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    mix: &MixEntry<'s>,
    meta: &Meta,
    sample_rate: u32,
    frames: usize,
    names: &[String],
    buffers: Option<&[Vec<Vec<f32>>]>,
    from: usize,
) -> Result<v8::Local<'s, v8::Value>, Error> {
    let MixEntry { entry, default } = *mix;
    let name_array = v8::Array::new(scope, names.len() as i32);
    for (i, stem) in names.iter().enumerate() {
        let value = v8::String::new(scope, stem).ok_or_else(|| Error::internal("stem name too large"))?;
        name_array.set_index(scope, i as u32, value.into());
    }
    let buffers_value: v8::Local<v8::Value> = match buffers {
        Some(layers) => plane_arrays(scope, layers)?.into(),
        None => v8::null(scope).into(),
    };
    let fn_value: v8::Local<v8::Value> = match default {
        Some(f) => f.into(),
        None => v8::null(scope).into(),
    };
    let args: [v8::Local<v8::Value>; 9] = [
        fn_value,
        name_array.into(),
        buffers_value,
        v8::Number::new(scope, sample_rate as f64).into(),
        v8::Number::new(scope, frames as f64).into(),
        v8::Number::new(scope, meta.duration).into(),
        v8::Number::new(scope, meta.seed as f64).into(),
        v8::Number::new(scope, meta.channels as f64).into(),
        v8::Number::new(scope, from as f64).into(),
    ];
    v8::tc_scope!(let tc, scope);
    let undefined = v8::undefined(tc).into();
    match entry.call(tc, undefined, &args) {
        Some(r) => Ok(r),
        None => Err(caught_in(tc, ErrorKind::Runtime)),
    }
}

pub(super) fn mix_what(use_default: bool) -> &'static str {
    if use_default { "the default export" } else { "the mix" }
}

/// Checks whole layers handed to the mix stage: one per expected name, each `frames * channels`.
pub(super) fn check_whole_layers(
    stems: &[Vec<f32>],
    names: &[String],
    frames: usize,
    channels: u32,
) -> Result<(), Error> {
    if stems.len() != names.len() {
        return Err(Error::contract(format!(
            "the mix stage expects {} layer(s) ({}), got {}",
            names.len(),
            names.join(", "),
            stems.len()
        )));
    }
    for (name, samples) in names.iter().zip(stems) {
        if samples.len() != frames * channels as usize {
            return Err(Error::contract(format!(
                "stem \"{name}\" has {} samples, expected {} ({frames} frames x {channels} channels)",
                samples.len(),
                frames * channels as usize
            )));
        }
    }
    Ok(())
}

/// The whole mix over whole layers, in a fresh isolate: the api-2 mix stage. The isolate's
/// handle is published in `current` while it runs, so a drop can cancel it.
#[allow(clippy::too_many_arguments)]
pub(super) fn mix_whole_here(
    text: &str,
    name: &str,
    opts: &RenderOptions,
    budget: Duration,
    names: &[String],
    use_default: bool,
    stems: &[Vec<f32>],
    current: &Mutex<Option<v8::IsolateHandle>>,
) -> Result<Rendered, Error> {
    let result = with_source(text, name, opts, budget, |scope, _name, meta, dependencies, namespace, guard| {
        *current.lock().unwrap() = Some(guard.handle.clone());
        let (sample_rate, frames) = geometry(&meta, opts)?;
        let channels = meta.channels;
        check_whole_layers(stems, names, frames, channels)?;
        let layers: Vec<Vec<Vec<f32>>> = stems.iter().map(|s| deinterleave(s, frames, channels)).collect();
        let stem_names: Vec<String> = read_stems(scope, namespace)?.into_iter().map(|(n, _)| n).collect();
        let mix = mix_entry(scope, namespace, use_default)?;
        let result = call_mix(scope, &mix, &meta, sample_rate, frames, names, Some(&layers), 0)?;
        let planes = planes_from(scope, result, frames, channels)?;
        check_finite_planes(&planes, 0, mix_what(use_default))?;
        Ok(Rendered {
            meta,
            sample_rate,
            channels,
            frames,
            samples: interleave(&planes, frames, channels),
            dependencies,
            stem: None,
            stem_names,
        })
    });
    *current.lock().unwrap() = None;
    result
}

/// The mix stage's thread: the probe, then commands until the [`Mixer`] is dropped.
#[allow(clippy::too_many_arguments)]
pub(super) fn mixer_thread(
    text: &str,
    name: &str,
    opts: &RenderOptions,
    deadline: Deadline,
    names: Vec<String>,
    use_default: bool,
    current: &Mutex<Option<v8::IsolateHandle>>,
    setup: Sender<MixSetup>,
    inbox: Receiver<MixCommand>,
) {
    let what = mix_what(use_default);
    let probed = with_source(
        text,
        name,
        opts,
        deadline.remaining(),
        |scope, _name, meta, dependencies, namespace, guard| {
            *current.lock().unwrap() = Some(guard.handle.clone());
            let (sample_rate, frames) = geometry(&meta, opts)?;
            let channels = meta.channels;
            let stem_names: Vec<String> = read_stems(scope, namespace)?.into_iter().map(|(n, _)| n).collect();
            let mix = mix_entry(scope, namespace, use_default)?;
            let result = call_mix(scope, &mix, &meta, sample_rate, frames, &names, None, 0)?;
            let found = |form| Probed {
                form,
                meta: meta.clone(),
                sample_rate,
                dependencies: dependencies.clone(),
                stem_names: stem_names.clone(),
            };
            if result.is_null() {
                return Ok(found(MixForm::Whole));
            }
            let Ok(mut driver) = v8::Local::<v8::Function>::try_from(result) else {
                let planes = planes_from(scope, result, frames, channels)?;
                check_finite_planes(&planes, 0, what)?;
                return Ok(found(MixForm::Planes(planes)));
            };
            guard.watchdog.disarm();
            if setup.send(MixSetup::Stream).is_err() {
                return Ok(found(MixForm::Stream));
            }
            while let Ok(command) = inbox.recv() {
                match command {
                    MixCommand::Restart { from, reply } => {
                        let outcome = call_mix(scope, &mix, &meta, sample_rate, frames, &names, None, from).and_then(|r| {
                        v8::Local::<v8::Function>::try_from(r).map_err(|_| {
                            Error::contract("the default export did not return a stream on restart, having returned one before")
                        })
                    });
                        let _ = reply.send(outcome.map(|d| driver = d));
                    }
                    MixCommand::Mix { offset, stems, reply } => {
                        let outcome = (|| {
                            if offset >= frames {
                                return Err(Error::contract(format!(
                                    "block at frame {offset} is past the end ({frames} frames)"
                                )));
                            }
                            let n = block_len(offset, frames);
                            if stems.len() != names.len() {
                                return Err(Error::contract(format!(
                                    "the mix stage expects {} layer(s) ({}), got {}",
                                    names.len(),
                                    names.join(", "),
                                    stems.len()
                                )));
                            }
                            for (stem, samples) in names.iter().zip(&stems) {
                                if samples.len() != n * channels as usize {
                                    return Err(Error::contract(format!(
                                        "stem \"{stem}\" has {} samples for the block at frame {offset}, expected {} ({n} frames x {channels} channels)",
                                        samples.len(),
                                        n * channels as usize
                                    )));
                                }
                            }
                            let blocks: Vec<Vec<Vec<f32>>> =
                                stems.iter().map(|s| deinterleave(s, n, channels)).collect();
                            pull_block(
                                scope,
                                guard,
                                opts.timeout,
                                driver,
                                offset,
                                frames,
                                channels,
                                Some(&blocks),
                                what,
                            )
                        })();
                        let _ = reply.send(outcome);
                    }
                    MixCommand::MixAll { stems, budget, reply } => {
                        let outcome = (|| {
                            check_whole_layers(&stems, &names, frames, channels)?;
                            driver = v8::Local::<v8::Function>::try_from(call_mix(
                                scope,
                                &mix,
                                &meta,
                                sample_rate,
                                frames,
                                &names,
                                None,
                                0,
                            )?)
                            .map_err(|_| {
                                Error::contract(
                                    "the default export did not return a stream on restart, having returned one before",
                                )
                            })?;
                            let layers: Vec<&[f32]> = stems.iter().map(Vec::as_slice).collect();
                            let blocks_at = |offset: usize| chop(&layers, offset, frames, channels);
                            let planes = drain(scope, guard, budget, driver, frames, channels, Some(&blocks_at), what)?;
                            Ok(Rendered {
                                meta: meta.clone(),
                                sample_rate,
                                channels,
                                frames,
                                samples: interleave(&planes, frames, channels),
                                dependencies: dependencies.clone(),
                                stem: None,
                                stem_names: stem_names.clone(),
                            })
                        })();
                        let _ = reply.send(outcome);
                    }
                }
            }
            Ok(found(MixForm::Stream))
        },
    );
    *current.lock().unwrap() = None;

    let probed = match probed {
        Ok(Probed { form: MixForm::Stream, .. }) => return,
        Ok(probed) => probed,
        Err(e) => {
            let _ = setup.send(MixSetup::Fail(e));
            return;
        }
    };
    let sent = match &probed.form {
        MixForm::Whole => setup.send(MixSetup::Whole),
        MixForm::Planes(planes) => setup.send(MixSetup::Planes(planes.clone())),
        MixForm::Stream => unreachable!(),
    };
    if sent.is_err() {
        return;
    }
    // Whole-form: every whole mix runs in a fresh isolate, so the probe's aborted call leaves
    // nothing behind; the isolate's handle is published for cancellation while it runs.
    let refused = || Error::contract("the default export reads ctx.stems, so this mix runs whole: use mix_all");
    while let Ok(command) = inbox.recv() {
        match command {
            MixCommand::Restart { reply, .. } => {
                let _ = reply.send(Err(refused()));
            }
            MixCommand::Mix { reply, .. } => {
                let _ = reply.send(Err(refused()));
            }
            MixCommand::MixAll { stems, budget, reply } => {
                let outcome = match &probed.form {
                    MixForm::Planes(planes) => {
                        let frames = planes.first().map_or(0, Vec::len);
                        let channels = probed.meta.channels;
                        // The mix ignored its layers: the probe's result is the mix, whatever is supplied.
                        check_whole_layers(&stems, &names, frames, channels).map(|_| Rendered {
                            meta: probed.meta.clone(),
                            sample_rate: probed.sample_rate,
                            channels,
                            frames,
                            samples: interleave(planes, frames, channels),
                            dependencies: probed.dependencies.clone(),
                            stem: None,
                            stem_names: probed.stem_names.clone(),
                        })
                    }
                    _ => mix_whole_here(text, name, opts, budget, &names, use_default, &stems, current),
                };
                let _ = reply.send(outcome);
            }
        }
    }
}

/// The mix stage of a [`Source`], started: the default export, or the sum of the layers. Fed
/// one block of every layer at a time (`mix`) or the whole layers at once (`mix_all`). Dropping
/// it stops its isolate and joins its thread.
pub struct Mixer {
    pub(super) names: Vec<String>,
    pub(super) channels: u32,
    pub(super) sample_rate: u32,
    pub(super) frames: usize,
    pub(super) budget: Duration,
    pub(super) form: MixForm,
    pub(super) commands: Option<Sender<MixCommand>>,
    pub(super) current: Arc<Mutex<Option<v8::IsolateHandle>>>,
    pub(super) thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Mixer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mixer").field("names", &self.names).field("streaming", &self.streaming()).finish()
    }
}

impl Mixer {
    /// The layers `mix` and `mix_all` expect, in this order: every layer the source declares, or
    /// the subset the mixer was made for.
    pub fn stem_names(&self) -> &[String] {
        &self.names
    }

    pub fn channels(&self) -> u32 {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// False: the default export read `ctx.stems`, so it is a whole-buffer mix and only
    /// `mix_all` works.
    pub fn streaming(&self) -> bool {
        matches!(self.form, MixForm::Stream)
    }

    fn send(&self, command: MixCommand) -> Result<(), Error> {
        self.commands
            .as_ref()
            .and_then(|c| c.send(command).ok())
            .ok_or_else(|| Error::internal("the mix stage's thread is gone"))
    }

    /// A fresh processor instance in the same isolate, its first block at `offset`, which must
    /// be a multiple of [`BLOCK_FRAMES`] below `frames()`. What it computes from there differs
    /// from the canonical render until its state warms; the canonical render starts at 0.
    pub fn restart(&mut self, offset: usize) -> Result<(), Error> {
        if !offset.is_multiple_of(BLOCK_FRAMES) || offset >= self.frames {
            return Err(Error::contract(format!(
                "cannot restart at frame {offset}: a restart lands on a block boundary (a multiple of {BLOCK_FRAMES}) before the end ({} frames)",
                self.frames
            )));
        }
        if !self.streaming() {
            return Err(Error::contract("the default export reads ctx.stems, so this mix runs whole: use mix_all"));
        }
        let (reply, answer) = mpsc::channel();
        self.send(MixCommand::Restart { from: offset, reply })?;
        answer.recv().unwrap_or_else(|_| Err(Error::internal("the mix stage's thread is gone")))
    }

    /// One block: `stems` are interleaved, parallel to `stem_names()`, each the length of the
    /// block at `offset`; offsets must be consecutive since the last restart. Scaled inputs are
    /// legal; the result is then not the canonical render.
    pub fn mix(&mut self, offset: usize, stems: &[&[f32]]) -> Result<Block, Error> {
        if !self.streaming() {
            return Err(Error::contract("the default export reads ctx.stems, so this mix runs whole: use mix_all"));
        }
        let (reply, answer) = mpsc::channel();
        self.send(MixCommand::Mix { offset, stems: stems.iter().map(|s| s.to_vec()).collect(), reply })?;
        let planes = answer.recv().unwrap_or_else(|_| Err(Error::internal("the mix stage's thread is gone")))?;
        let frames = planes.first().map_or(0, Vec::len);
        Ok(Block { offset, frames, samples: interleave(&planes, frames, self.channels) })
    }

    /// The whole mix over whole layers (what `mix_from` does): a mix stream is restarted at 0
    /// and driven over the layers chopped into blocks, leaving it at the end; a whole-buffer mix
    /// runs in a fresh isolate. The whole call, or each block, gets the options' budget.
    pub fn mix_all(&mut self, stems: &[&[f32]]) -> Result<Rendered, Error> {
        self.mix_all_with(stems, self.budget)
    }

    pub(super) fn mix_all_with(&mut self, stems: &[&[f32]], budget: Duration) -> Result<Rendered, Error> {
        let (reply, answer) = mpsc::channel();
        self.send(MixCommand::MixAll { stems: stems.iter().map(|s| s.to_vec()).collect(), budget, reply })?;
        answer.recv().unwrap_or_else(|_| Err(Error::internal("the mix stage's thread is gone")))
    }
}

impl Drop for Mixer {
    fn drop(&mut self) {
        if let Some(handle) = self.current.lock().unwrap().as_ref() {
            handle.terminate_execution();
        }
        drop(self.commands.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
