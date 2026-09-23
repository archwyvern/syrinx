//! Whole layers: one isolate each, a stream drained inside it, bounded concurrency. What
//! `render_each`, `--bounce` and `hash` use.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::{Error, Meta, RenderOptions, Rendered};

use super::wrapper::{drain, interleave, read_stems, stem_setup, StemForm};
use super::{geometry, on_own_thread, with_source, Deadline, V8_THREAD_STACK_BYTES};

/// One layer's audio, whole.
pub(super) struct StemAudio {
    pub(super) planes: Vec<Vec<f32>>,
    pub(super) meta: Meta,
    pub(super) sample_rate: u32,
    pub(super) frames: usize,
    pub(super) dependencies: Vec<PathBuf>,
    pub(super) stem_names: Vec<String>,
}

impl StemAudio {
    pub(super) fn into_rendered(self, stem: Option<String>) -> Rendered {
        let channels = self.meta.channels;
        let samples = interleave(&self.planes, self.frames, channels);
        Rendered {
            meta: self.meta,
            sample_rate: self.sample_rate,
            channels,
            frames: self.frames,
            samples,
            dependencies: self.dependencies,
            stem,
            stem_names: self.stem_names,
        }
    }
}

/// Renders one layer whole in its own isolate, draining it block by block when it streams.
pub(super) fn render_stem_here(source: &str, name: &str, opts: &RenderOptions, deadline: Deadline, stem: &str) -> Result<StemAudio, Error> {
    with_source(source, name, opts, deadline.remaining(), |scope, _name, meta, dependencies, namespace, guard| {
        let (sample_rate, frames) = geometry(&meta, opts)?;
        let stem_names: Vec<String> = read_stems(scope, namespace)?.into_iter().map(|(n, _)| n).collect();
        let planes = match stem_setup(scope, namespace, &meta, sample_rate, frames, stem)? {
            StemForm::Whole(planes) => planes,
            StemForm::Live(driver) => {
                guard.watchdog.disarm();
                drain(scope, guard, opts.timeout, driver, frames, meta.channels, None, &format!("stem \"{stem}\""))?
            }
        };
        Ok(StemAudio { planes, meta, sample_rate, frames, dependencies, stem_names })
    })
}

/// Renders `stems` whole, concurrently, one isolate and one owned thread each, bounded by the
/// machine's parallelism. Results come back in the order asked for.
pub(super) fn render_stems(source: &str, name: &str, opts: &RenderOptions, deadline: Deadline, stems: &[String]) -> Result<Vec<StemAudio>, Error> {
    if stems.len() == 1 {
        return Ok(vec![on_own_thread(|| render_stem_here(source, name, opts, deadline, &stems[0]))?]);
    }
    let next = Mutex::new(0usize);
    let failed = AtomicBool::new(false);
    let slots: Vec<Mutex<Option<Result<StemAudio, Error>>>> = stems.iter().map(|_| Mutex::new(None)).collect();
    let workers = parallelism().clamp(1, stems.len());
    std::thread::scope(|s| {
        for _ in 0..workers {
            let next = &next;
            let slots = &slots;
            let failed = &failed;
            std::thread::Builder::new()
                .name("syrinx-v8".into())
                .stack_size(V8_THREAD_STACK_BYTES)
                .spawn_scoped(s, move || loop {
                    // One layer failing condemns the whole render, so there is no reason to
                    // compile and run the layers that have not started yet.
                    if failed.load(Ordering::Relaxed) {
                        break;
                    }
                    let i = {
                        let mut cursor = next.lock().unwrap();
                        let i = *cursor;
                        *cursor += 1;
                        i
                    };
                    if i >= stems.len() {
                        break;
                    }
                    let rendered = render_stem_here(source, name, opts, deadline, &stems[i]);
                    if rendered.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    *slots[i].lock().unwrap() = Some(rendered);
                })
                .expect("spawn syrinx-v8 thread");
        }
    });

    // Lowest index wins, so which layer is blamed does not depend on which thread got there
    // first. Slots left empty belong to layers that were never started.
    let mut done = Vec::with_capacity(stems.len());
    let mut failure: Option<Error> = None;
    for slot in slots {
        match slot.into_inner().unwrap() {
            Some(Ok(audio)) => done.push(audio),
            Some(Err(e)) => {
                failure.get_or_insert(e);
            }
            None => {}
        }
    }
    match failure {
        Some(e) => Err(e),
        None => {
            debug_assert_eq!(done.len(), stems.len());
            Ok(done)
        }
    }
}

pub(super) fn parallelism() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}
