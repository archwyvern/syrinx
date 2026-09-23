//! One layer, started: its thread computes blocks ahead into a bounded channel, or handed its
//! whole planes over at setup and went away. Dropping a live layer terminates and joins.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::JoinHandle;

use crate::{Block, Error, RenderOptions, BLOCK_FRAMES};

use super::source::Slots;
use super::wrapper::{block_len, interleave, pull_block, stem_setup, StemForm};
use super::{geometry, with_source, Deadline};

pub(super) enum StemMessage {
    Whole(Vec<Vec<f32>>),
    Live(v8::IsolateHandle),
    Block(Vec<Vec<f32>>),
    End,
    Fail(Error),
}

/// A layer's thread: setup under a slot, then either its whole planes or its blocks, in order.
#[allow(clippy::too_many_arguments)]
pub(super) fn stem_thread(
    text: &str,
    name: &str,
    opts: &RenderOptions,
    deadline: Deadline,
    stem: &str,
    slots: &Slots,
    failed: &AtomicBool,
    tx: SyncSender<StemMessage>,
) {
    let slot = slots.acquire();
    if failed.load(Ordering::SeqCst) {
        return;
    }
    let what = format!("stem \"{stem}\"");
    let outcome = with_source(text, name, opts, deadline.remaining(), |scope, _name, meta, _deps, namespace, guard| {
        let (sample_rate, frames) = geometry(&meta, opts)?;
        match stem_setup(scope, namespace, &meta, sample_rate, frames, stem)? {
            StemForm::Whole(planes) => {
                let _ = tx.send(StemMessage::Whole(planes));
                Ok(())
            }
            StemForm::Live(driver) => {
                guard.watchdog.disarm();
                drop(slot);
                if tx.send(StemMessage::Live(guard.handle.clone())).is_err() {
                    return Ok(());
                }
                let mut offset = 0;
                while offset < frames {
                    let planes = pull_block(scope, guard, opts.timeout, driver, offset, frames, meta.channels, None, &what)?;
                    if tx.send(StemMessage::Block(planes)).is_err() {
                        // The consumer is gone; nothing to compute for.
                        return Ok(());
                    }
                    offset += BLOCK_FRAMES;
                }
                let _ = tx.send(StemMessage::End);
                Ok(())
            }
        }
    });
    if let Err(e) = outcome {
        failed.store(true, Ordering::SeqCst);
        let _ = tx.send(StemMessage::Fail(e));
    }
}

/// A live layer: its channel, its isolate and its thread. Dropping it terminates and joins.
pub(super) struct LiveStem {
    pub(super) rx: Receiver<StemMessage>,
    pub(super) handle: v8::IsolateHandle,
    pub(super) thread: Option<JoinHandle<()>>,
    pub(super) done: bool,
}

impl Drop for LiveStem {
    fn drop(&mut self) {
        self.handle.terminate_execution();
        // Closing the channel wakes a producer blocked on a full queue.
        let (_, dead) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.rx, dead));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub(super) enum StemHandle {
    Whole(Vec<Vec<f32>>),
    Live(LiveStem),
}

impl std::fmt::Debug for StemHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StemHandle::Whole(planes) => f.debug_tuple("Whole").field(&planes.first().map_or(0, Vec::len)).finish(),
            StemHandle::Live(_) => f.write_str("Live"),
        }
    }
}

/// One layer of a [`Source`], started. Its blocks come out in order; `streaming()` says whether
/// they are computed on demand or chopped from a whole render done at setup. Dropping it stops
/// its isolate and joins its thread.
#[derive(Debug)]
pub struct Stem {
    pub(super) name: String,
    pub(super) channels: u32,
    pub(super) frames: usize,
    pub(super) cursor: usize,
    pub(super) form: StemHandle,
    pub(super) error: Option<Error>,
}

impl Stem {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// False: the layer returned a buffer at setup; it is complete, `next_block` chops it and no
    /// thread is alive.
    pub fn streaming(&self) -> bool {
        matches!(self.form, StemHandle::Live(_))
    }

    /// The next block, interleaved, or `None` after the last. An error is sticky.
    pub fn next_block(&mut self) -> Result<Option<Block>, Error> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        if self.cursor >= self.frames {
            return Ok(None);
        }
        let planes = match &mut self.form {
            StemHandle::Whole(planes) => {
                let n = block_len(self.cursor, self.frames);
                planes.iter().map(|p| p[self.cursor..self.cursor + n].to_vec()).collect::<Vec<_>>()
            }
            StemHandle::Live(live) => {
                if live.done {
                    return Ok(None);
                }
                match live.rx.recv() {
                    Ok(StemMessage::Block(planes)) => planes,
                    Ok(StemMessage::End) => {
                        live.done = true;
                        return Ok(None);
                    }
                    Ok(StemMessage::Fail(e)) => {
                        self.error = Some(e.clone());
                        return Err(e);
                    }
                    Ok(_) | Err(_) => {
                        let e = Error::contract(format!("internal: layer \"{}\" ended without finishing", self.name));
                        self.error = Some(e.clone());
                        return Err(e);
                    }
                }
            }
        };
        let frames = planes.first().map_or(0, Vec::len);
        let block = Block { offset: self.cursor, frames, samples: interleave(&planes, frames, self.channels) };
        self.cursor += frames;
        Ok(Some(block))
    }
}
