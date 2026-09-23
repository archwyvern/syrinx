//! A source, checked and inspected, and the two things started from it: its layers and its
//! mix stage. Setups are bounded by a slot semaphore so a source with more layers than cores
//! cannot swamp the machine; a layer that streams releases its slot once live.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::{Error, Inspected, Meta, RenderOptions, Target};

use super::mixer::{mixer_thread, MixCommand, MixForm, MixSetup, Mixer};
use super::stem::{stem_thread, LiveStem, Stem, StemHandle, StemMessage};
use super::whole::parallelism;
use super::{geometry, inspect_with, spawn_v8_thread, Deadline, PREFETCH_BLOCKS};

/// Bounds how many isolates are being set up at once, per call. A layer that streams releases
/// its slot once live, so a source with more streaming layers than cores cannot deadlock.
pub(super) struct Slots {
    free: Mutex<usize>,
    freed: Condvar,
}

impl Slots {
    fn new(n: usize) -> Self {
        Self { free: Mutex::new(n.max(1)), freed: Condvar::new() }
    }

    pub(super) fn acquire(&self) -> SlotGuard<'_> {
        let mut free = self.free.lock().unwrap();
        while *free == 0 {
            free = self.freed.wait(free).unwrap();
        }
        *free -= 1;
        SlotGuard(self)
    }
}

pub(super) struct SlotGuard<'a>(&'a Slots);

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap() += 1;
        self.0.freed.notify_one();
    }
}

/// A source, checked and inspected: its meta, layers and imports are known, and nothing has
/// been spawned. Cheap to hold and share; every layer and the mix stage are started from it.
#[derive(Debug, Clone)]
pub struct Source {
    pub(super) text: String,
    pub(super) name: String,
    pub(super) opts: RenderOptions,
    pub(super) info: Inspected,
    pub(super) sample_rate: u32,
    pub(super) frames: usize,
}

impl Source {
    /// Runs the static check and the module graph once.
    pub fn open(source: &str, name: &str, opts: &RenderOptions) -> Result<Source, Error> {
        Self::open_with(source, name, opts, Deadline::new(opts.timeout))
    }

    pub(super) fn open_with(source: &str, name: &str, opts: &RenderOptions, deadline: Deadline) -> Result<Source, Error> {
        let info = inspect_with(source, name, opts, deadline)?;
        let (sample_rate, frames) = geometry(&info.meta, opts)?;
        Ok(Source { text: source.to_string(), name: name.to_string(), opts: opts.clone(), info, sample_rate, frames })
    }

    pub fn meta(&self) -> &Meta {
        &self.info.meta
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u32 {
        self.info.meta.channels
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Every file the source imports, transitively, as canonical paths.
    pub fn dependencies(&self) -> &[PathBuf] {
        &self.info.dependencies
    }

    /// Every layer the source declares, in declaration order.
    pub fn stem_names(&self) -> &[String] {
        &self.info.stems
    }

    /// Whether it has a default export. Without one the mix is the sum of the layers.
    pub fn has_mix(&self) -> bool {
        self.info.has_mix
    }

    /// The options it was opened with.
    pub fn options(&self) -> &RenderOptions {
        &self.opts
    }

    /// Starts the named layers, one isolate and one owned thread each. Setups run concurrently,
    /// bounded by the machine's parallelism, and this returns when every one is done: a layer's
    /// `streaming()` is then known, a whole-buffer layer has rendered, and a setup error surfaces
    /// here. The result is parallel to `names`.
    pub fn stems(&self, names: &[&str]) -> Result<Vec<Stem>, Error> {
        self.stems_with(names, Deadline::new(self.opts.timeout))
    }

    pub(super) fn stems_with(&self, names: &[&str], deadline: Deadline) -> Result<Vec<Stem>, Error> {
        for (i, name) in names.iter().enumerate() {
            if !self.info.stems.iter().any(|s| s == name) {
                return Err(Error::contract(format!(
                    "no stem named \"{name}\"; this source declares {}",
                    self.info.stems.join(", ")
                )));
            }
            if names[..i].contains(name) {
                return Err(Error::contract(format!("stem \"{name}\" selected twice")));
            }
        }
        let slots = Arc::new(Slots::new(parallelism()));
        let failed = Arc::new(AtomicBool::new(false));
        let lanes: Vec<(Receiver<StemMessage>, JoinHandle<()>)> = names
            .iter()
            .map(|stem| {
                let (tx, rx) = mpsc::sync_channel(PREFETCH_BLOCKS);
                let text = self.text.clone();
                let name = self.name.clone();
                let opts = self.opts.clone();
                let stem = stem.to_string();
                let slots = slots.clone();
                let failed = failed.clone();
                let thread = spawn_v8_thread(move || stem_thread(&text, &name, &opts, deadline, &stem, &slots, &failed, tx));
                (rx, thread)
            })
            .collect();

        let mut stems = Vec::with_capacity(names.len());
        let mut failure: Option<Error> = None;
        for (name, (rx, thread)) in names.iter().zip(lanes) {
            let stem = Stem {
                name: name.to_string(),
                channels: self.channels(),
                frames: self.frames,
                cursor: 0,
                form: StemHandle::Whole(Vec::new()),
                error: None,
            };
            match rx.recv() {
                Ok(StemMessage::Whole(planes)) => {
                    let _ = thread.join();
                    stems.push(Stem { form: StemHandle::Whole(planes), ..stem });
                }
                Ok(StemMessage::Live(handle)) => {
                    stems.push(Stem { form: StemHandle::Live(LiveStem { rx, handle, thread: Some(thread), done: false }), ..stem });
                }
                Ok(StemMessage::Fail(e)) => {
                    let _ = thread.join();
                    failure.get_or_insert(e);
                }
                Ok(_) => {
                    let _ = thread.join();
                    failure.get_or_insert(Error::contract(format!("internal: layer \"{name}\" reported a block before its setup")));
                }
                // The thread saw another layer fail and never started, or died.
                Err(_) => {
                    let _ = thread.join();
                    if failure.is_none() && !failed.load(Ordering::SeqCst) {
                        failure = Some(Error::contract(format!("internal: layer \"{name}\" ended without a result")));
                    }
                }
            }
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(stems),
        }
    }

    /// Starts the mix stage in its own isolate: the default export, or the sum, over every layer
    /// for [`Target::Mix`], or over the named subset (in declaration order) for [`Target::Stems`].
    /// Returns once the stage is classified, so `streaming()` is known.
    pub fn mixer(&self, target: &Target) -> Result<Mixer, Error> {
        self.mixer_with(target, Deadline::new(self.opts.timeout))
    }

    pub(super) fn mixer_with(&self, target: &Target, deadline: Deadline) -> Result<Mixer, Error> {
        let (names, use_default) = plan(&self.info, target)?;
        let (commands, inbox) = mpsc::channel::<MixCommand>();
        let (setup_tx, setup_rx) = mpsc::channel::<MixSetup>();
        let current = Arc::new(Mutex::new(None::<v8::IsolateHandle>));
        let thread = {
            let text = self.text.clone();
            let name = self.name.clone();
            let opts = self.opts.clone();
            let names = names.clone();
            let current = current.clone();
            spawn_v8_thread(move || mixer_thread(&text, &name, &opts, deadline, names, use_default, &current, setup_tx, inbox))
        };
        let form = match setup_rx.recv() {
            Ok(MixSetup::Stream) => MixForm::Stream,
            Ok(MixSetup::Whole) => MixForm::Whole,
            Ok(MixSetup::Planes(planes)) => MixForm::Planes(planes),
            Ok(MixSetup::Fail(e)) => {
                drop(commands);
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(Error::contract("internal: the mix stage ended without a result"));
            }
        };
        Ok(Mixer {
            names,
            channels: self.channels(),
            sample_rate: self.sample_rate,
            frames: self.frames,
            budget: self.opts.timeout,
            form,
            commands: Some(commands),
            current,
            thread: Some(thread),
        })
    }
}

/// Every name is a declared layer and none repeats.
pub(super) fn validate_selection(info: &Inspected, names: &[String]) -> Result<(), Error> {
    if names.is_empty() {
        return Err(Error::contract("no stems selected"));
    }
    for (i, name) in names.iter().enumerate() {
        if !info.stems.contains(name) {
            return Err(Error::contract(format!(
                "no stem named \"{name}\"; this source declares {}",
                info.stems.join(", ")
            )));
        }
        if names[..i].contains(name) {
            return Err(Error::contract(format!("stem \"{name}\" selected twice")));
        }
    }
    Ok(())
}

/// Which layers a target asks for, in declaration order, and whether the default export runs.
pub(super) fn plan(info: &Inspected, target: &Target) -> Result<(Vec<String>, bool), Error> {
    match target {
        Target::Mix => Ok((info.stems.clone(), info.has_mix)),
        Target::Stems(names) => {
            validate_selection(info, names)?;
            // Declaration order whatever order was asked: f32 addition is not associative, so
            // `--stem a,b` and `--stem b,a` would otherwise differ by an ulp. A subset never runs
            // the default export: it reads layers by name and cannot be handed fewer.
            let selected = info.stems.iter().filter(|s| names.contains(s)).cloned().collect();
            Ok((selected, false))
        }
    }
}
