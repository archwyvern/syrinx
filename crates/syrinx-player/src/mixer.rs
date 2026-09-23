//! The mixer thread: reads the layers' files just ahead of the playhead, applies the faders,
//! runs the mix stage, resamples to the device and feeds the ring. Paced by the ring's capacity
//! (one second), so a fader move is audible within a chunk or two and nothing is baked ahead.
//!
//! Three ways a chunk is mixed, decided per chunk:
//! - a whole-buffer source with every fader at unity plays its canonical mix file, byte for byte
//!   what the CLI renders;
//! - any other whole-buffer case is the plain sum of the gained layers (a master over the sum
//!   cannot be re-run block by block, so it is bypassed and the status says so; a source with no
//!   default export loses nothing, the sum IS its mix);
//! - a source whose master is a mix stream runs it here, block by block, over the gained layers:
//!   the master hears the faders. A seek restarts the mix stage a second early and discards
//!   that second, so compressor and limiter state is warm at the first audible block.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::output::Shared;
use crate::resample::Resample;
use crate::track::{Master, Track};

/// Frames per mixer chunk: the standard's block, because a mix stream is fed exactly one block
/// at a time on the block grid. 85 ms at 48 kHz.
pub const MIX_FRAMES: usize = syrinx_core::BLOCK_FRAMES;

/// Blocks a restarted mix stream runs over before its output is used: a second at 48 kHz.
const PREROLL_BLOCKS: usize = 12;

/// How long the mixer waits for the callback to acknowledge a flush before assuming the stream
/// is not running (a dead device) and carrying on.
const FLUSH_WAIT: Duration = Duration::from_millis(250);

/// The faders: a gain per layer plus mute and solo, written by the window, read per chunk here.
pub struct Gains {
    gain: Vec<AtomicU32>,
    mute: Vec<AtomicBool>,
    solo: Vec<AtomicBool>,
}

impl Gains {
    pub fn new(n: usize) -> Gains {
        Gains {
            gain: (0..n).map(|_| AtomicU32::new(1.0f32.to_bits())).collect(),
            mute: (0..n).map(|_| AtomicBool::new(false)).collect(),
            solo: (0..n).map(|_| AtomicBool::new(false)).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.gain.len()
    }

    pub fn gain(&self, i: usize) -> f32 {
        f32::from_bits(self.gain[i].load(Ordering::Relaxed))
    }

    pub fn set_gain(&self, i: usize, g: f32) {
        self.gain[i].store(g.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn muted(&self, i: usize) -> bool {
        self.mute[i].load(Ordering::Relaxed)
    }

    pub fn set_mute(&self, i: usize, on: bool) {
        self.mute[i].store(on, Ordering::Relaxed);
    }

    pub fn soloed(&self, i: usize) -> bool {
        self.solo[i].load(Ordering::Relaxed)
    }

    pub fn set_solo(&self, i: usize, on: bool) {
        self.solo[i].store(on, Ordering::Relaxed);
    }

    /// What each layer is multiplied by: its gain, or 0 when muted, or 0 when another layer is
    /// soloed and this one is not. A muted soloed layer is silent: mute wins.
    pub fn effective(&self) -> Vec<f32> {
        let any_solo = self.solo.iter().any(|s| s.load(Ordering::Relaxed));
        (0..self.len())
            .map(|i| if self.muted(i) || (any_solo && !self.soloed(i)) { 0.0 } else { self.gain(i) })
            .collect()
    }

    /// Every layer at exactly 1.0: the canonical balance.
    pub fn all_unity(&self) -> bool {
        self.effective().iter().all(|g| *g == 1.0)
    }

    pub fn reset(&self) {
        for i in 0..self.len() {
            self.set_gain(i, 1.0);
            self.set_mute(i, false);
            self.set_solo(i, false);
        }
    }
}

pub enum Command {
    Load {
        track: Arc<Track>,
        gains: Arc<Gains>,
        start_frame: usize,
    },
    Seek(usize),
    Unload,
    Loop(bool),
    /// The window opened a new device: a new ring to feed, at its rate and channel count.
    Output {
        producer: rtrb::Producer<f32>,
        sample_rate: u32,
        channels: u16,
    },
    Shutdown,
}

/// Where in the source each pushed chunk starts, so the window can turn the callback's frame
/// count into a position: `(device frames pushed before the chunk, source frame of the chunk)`.
pub struct Checkpoints {
    inner: Mutex<VecDeque<(u64, usize)>>,
}

impl Checkpoints {
    pub fn new() -> Checkpoints {
        Checkpoints { inner: Mutex::new(VecDeque::new()) }
    }

    pub fn push(&self, pushed: u64, source_frame: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.push_back((pushed, source_frame));
        // A second of device time per chunk at most; 1024 chunks is minutes of history.
        while inner.len() > 1024 {
            inner.pop_front();
        }
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    /// The source frame playing when the callback had consumed `consumed` device frames.
    pub fn position(&self, consumed: u64, device_rate: u32, source_rate: u32) -> Option<usize> {
        let inner = self.inner.lock().unwrap();
        let (pushed, frame) = inner.iter().rev().find(|(pushed, _)| *pushed <= consumed)?;
        let into = (consumed - pushed) as f64 * source_rate as f64 / device_rate as f64;
        Some(frame + into.round() as usize)
    }
}

/// What the window reads about the mixer.
pub struct Status {
    /// Source frame mixed up to.
    pub produced: AtomicUsize,
    /// The mixer is waiting for the render to pass the playhead.
    pub waiting: AtomicBool,
    /// A whole-buffer master is being skipped because a fader is off unity.
    pub master_bypassed: AtomicBool,
    /// The track ended (and loop is off), or failed.
    pub at_end: AtomicBool,
    pub error: Mutex<Option<String>>,
}

pub struct MixerHandle {
    tx: Sender<Command>,
    pub status: Arc<Status>,
    pub checkpoints: Arc<Checkpoints>,
    join: Option<JoinHandle<()>>,
}

impl MixerHandle {
    pub fn spawn(producer: rtrb::Producer<f32>, shared: Arc<Shared>, sample_rate: u32, channels: u16) -> MixerHandle {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Status {
            produced: AtomicUsize::new(0),
            waiting: AtomicBool::new(false),
            master_bypassed: AtomicBool::new(false),
            at_end: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let checkpoints = Arc::new(Checkpoints::new());
        let worker = Worker {
            rx,
            shared,
            status: Arc::clone(&status),
            checkpoints: Arc::clone(&checkpoints),
            producer,
            device_rate: sample_rate,
            device_channels: channels as usize,
            loop_on: false,
            loaded: None,
        };
        let join = std::thread::Builder::new()
            .name("syrinx-player-mixer".into())
            .spawn(move || worker.run())
            .expect("spawn the mixer thread");
        MixerHandle { tx, status, checkpoints, join: Some(join) }
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }
}

impl Drop for MixerHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The gained sum: `out = stems[0] * gains[0]`, then `+= stems[i] * gains[i]` in order. At unity
/// this is run.js's sum (a copy of the first layer, then the rest in declaration order), since
/// `x * 1.0 == x` and the additions round identically in f32.
pub fn sum_gained(stems: &[&[f32]], gains: &[f32], out: &mut Vec<f32>) {
    out.clear();
    let Some(first) = stems.first() else {
        return;
    };
    out.extend(first.iter().map(|s| s * gains[0]));
    for (stem, g) in stems.iter().zip(gains).skip(1) {
        for (o, s) in out.iter_mut().zip(stem.iter()) {
            *o += s * g;
        }
    }
}

/// Render channels to device channels: mono is duplicated to every channel, stereo goes to the
/// first two and any further channel is silent, stereo onto a mono device is the average.
pub fn fan_out(input: &[f32], in_channels: usize, out_channels: usize, frames: usize, out: &mut Vec<f32>) {
    out.clear();
    out.reserve(frames * out_channels);
    for f in 0..frames {
        let frame = &input[f * in_channels..(f + 1) * in_channels];
        match (in_channels, out_channels) {
            (a, b) if a == b => out.extend_from_slice(frame),
            (1, n) => out.extend(std::iter::repeat_n(frame[0], n)),
            (_, 1) => out.push(frame.iter().sum::<f32>() / in_channels as f32),
            (a, n) => {
                out.extend_from_slice(&frame[..a.min(n)]);
                out.extend(std::iter::repeat_n(0.0, n.saturating_sub(a)));
            }
        }
    }
}

struct Loaded {
    track: Arc<Track>,
    gains: Arc<Gains>,
    /// Next source frame to mix; always on the block grid.
    position: usize,
    /// Frames of the next chunk to drop before pushing: a seek inside a block.
    skip: usize,
    /// The track's mix stream, once taken from the track; `live_started` says whether it has
    /// been restarted for the current position.
    live: Option<syrinx_core::Mixer>,
    live_started: bool,
    resample: Resample,
    /// Device frames pushed to the ring since the last flush, counted from the callback's
    /// consumed count at that moment (the ring was empty then).
    pushed: u64,
    ended: bool,
    stems: Vec<Vec<f32>>,
    mixed: Vec<f32>,
    fanned: Vec<f32>,
}

struct Worker {
    rx: Receiver<Command>,
    shared: Arc<Shared>,
    status: Arc<Status>,
    checkpoints: Arc<Checkpoints>,
    producer: rtrb::Producer<f32>,
    device_rate: u32,
    device_channels: usize,
    loop_on: bool,
    loaded: Option<Loaded>,
}

enum Step {
    Continue,
    Shutdown,
}

impl Worker {
    fn run(mut self) {
        loop {
            // Idle (nothing loaded, or the track is over): block on the next command. Busy: take
            // whatever has arrived and get on with the next chunk.
            let idle = self.loaded.as_ref().is_none_or(|l| l.ended);
            let first = if idle {
                match self.rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                }
            } else {
                match self.rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            };
            if let Some(c) = first {
                if let Step::Shutdown = self.handle(c) {
                    return;
                }
                while let Ok(c) = self.rx.try_recv() {
                    if let Step::Shutdown = self.handle(c) {
                        return;
                    }
                }
            }
            if let Step::Shutdown = self.chunk() {
                return;
            }
        }
    }

    fn handle(&mut self, command: Command) -> Step {
        match command {
            Command::Load { track, gains, start_frame } => {
                self.flush_ring();
                let resample =
                    match Resample::new(track.sample_rate, self.device_rate, track.channels as usize, MIX_FRAMES) {
                        Ok(r) => r,
                        Err(e) => {
                            *self.status.error.lock().unwrap() = Some(format!("{e:#}"));
                            self.status.at_end.store(true, Ordering::Relaxed);
                            return Step::Continue;
                        }
                    };
                let n = track.stems.len();
                let start = start_frame.min(track.frames);
                self.loaded = Some(Loaded {
                    position: start - start % MIX_FRAMES,
                    skip: start % MIX_FRAMES,
                    live: None,
                    live_started: false,
                    track,
                    gains,
                    resample,
                    pushed: self.shared.consumed.load(Ordering::Relaxed),
                    ended: false,
                    stems: vec![Vec::new(); n],
                    mixed: Vec::new(),
                    fanned: Vec::new(),
                });
                self.begin();
            }
            Command::Seek(frame) => {
                if self.loaded.is_some() {
                    self.flush_ring();
                    let consumed = self.shared.consumed.load(Ordering::Relaxed);
                    let l = self.loaded.as_mut().expect("still loaded");
                    let frame = frame.min(l.track.frames);
                    l.position = frame - frame % MIX_FRAMES;
                    l.skip = frame % MIX_FRAMES;
                    l.live_started = false;
                    l.resample.reset();
                    l.pushed = consumed;
                    l.ended = false;
                    self.begin();
                }
            }
            Command::Unload => {
                self.flush_ring();
                self.loaded = None;
                self.checkpoints.clear();
                self.shared.finished_at.store(u64::MAX, Ordering::Relaxed);
                self.status.at_end.store(false, Ordering::Relaxed);
                self.status.waiting.store(false, Ordering::Relaxed);
                self.status.master_bypassed.store(false, Ordering::Relaxed);
            }
            Command::Loop(on) => {
                self.loop_on = on;
                if on {
                    if let Some(l) = self.loaded.as_mut() {
                        if l.ended {
                            // The track had ended; looping means it starts again.
                            l.position = 0;
                            l.skip = 0;
                            l.live_started = false;
                            l.ended = false;
                            l.pushed = self.shared.consumed.load(Ordering::Relaxed);
                            self.begin();
                        }
                    }
                }
            }
            Command::Output { producer, sample_rate, channels } => {
                self.producer = producer;
                self.device_rate = sample_rate;
                self.device_channels = channels as usize;
                if let Some(l) = self.loaded.as_mut() {
                    match Resample::new(l.track.sample_rate, sample_rate, l.track.channels as usize, MIX_FRAMES) {
                        Ok(r) => l.resample = r,
                        Err(e) => *self.status.error.lock().unwrap() = Some(format!("{e:#}")),
                    }
                    l.pushed = self.shared.consumed.load(Ordering::Relaxed);
                    if !l.ended {
                        self.begin();
                    }
                }
            }
            Command::Shutdown => return Step::Shutdown,
        }
        Step::Continue
    }

    /// Bookkeeping shared by load, seek and a device change: a clean slate for the checkpoints
    /// and the end marker.
    fn begin(&mut self) {
        self.checkpoints.clear();
        self.shared.finished_at.store(u64::MAX, Ordering::Relaxed);
        self.status.at_end.store(false, Ordering::Relaxed);
        *self.status.error.lock().unwrap() = None;
        if let Some(l) = &self.loaded {
            self.status.produced.store(l.position, Ordering::Relaxed);
        }
    }

    /// Asks the callback to drop everything in the ring and waits for it to say so.
    fn flush_ring(&mut self) {
        self.shared.flush.store(true, Ordering::Release);
        let started = Instant::now();
        while self.shared.flush.load(Ordering::Acquire) {
            if started.elapsed() > FLUSH_WAIT {
                // No callback is running (device gone). Whatever is in the ring stays; the
                // consumer count will not move either, so the checkpoints still line up.
                self.shared.flush.store(false, Ordering::Release);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// One chunk: read, gain, mix, resample, fan out, push.
    fn chunk(&mut self) -> Step {
        let Some(l) = self.loaded.as_mut() else {
            return Step::Continue;
        };
        if l.ended {
            return Step::Continue;
        }
        let track = Arc::clone(&l.track);
        if l.position >= track.frames {
            if self.loop_on {
                l.position = 0;
                l.skip = 0;
                l.live_started = false;
                l.resample.reset();
            } else {
                self.shared.finished_at.store(l.pushed, Ordering::Relaxed);
                l.ended = true;
                self.status.at_end.store(true, Ordering::Relaxed);
                self.status.waiting.store(false, Ordering::Relaxed);
            }
            return Step::Continue;
        }
        let n = MIX_FRAMES.min(track.frames - l.position);
        let gains = l.gains.effective();
        let unity = gains.iter().all(|g| *g == 1.0);

        // What the master is. Unknown until the render thread has probed it; a mix stream is
        // taken from the track the first time and kept for as long as the track is loaded.
        let form = {
            let mut master = track.master.lock().unwrap();
            match &mut *master {
                Master::Unknown => Form::Unknown,
                Master::Sum => Form::Sum,
                Master::Whole => Form::Whole,
                Master::Live(taken) => {
                    if l.live.is_none() {
                        l.live = taken.take();
                    }
                    Form::Live
                }
            }
        };
        // Wait for the render to get here; a failed render ends the track where it stopped.
        let ready = track.rendered_to(unity).is_some_and(|to| to >= l.position + n);
        if !ready {
            if let Some(err) = track.failed() {
                *self.status.error.lock().unwrap() = Some(err);
                self.shared.finished_at.store(l.pushed, Ordering::Relaxed);
                l.ended = true;
                self.status.at_end.store(true, Ordering::Relaxed);
                self.status.waiting.store(false, Ordering::Relaxed);
                return Step::Continue;
            }
            self.status.waiting.store(true, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(10));
            return Step::Continue;
        }
        self.status.waiting.store(false, Ordering::Relaxed);

        let position = l.position;
        let mixed = (|| -> anyhow::Result<()> {
            match form {
                Form::Whole if unity => {
                    track.mix.as_ref().expect("a whole master has a mix file").read_frames(
                        position,
                        n,
                        &mut l.mixed,
                    )?;
                }
                Form::Live => {
                    let Some(mixer) = l.live.as_mut() else {
                        anyhow::bail!("internal: the mix stream was taken by someone else");
                    };
                    if !l.live_started {
                        // A fresh processor knows nothing of the past; run it over the second
                        // before the playhead and throw that away, so its state is warm.
                        let preroll = (PREROLL_BLOCKS * MIX_FRAMES).min(position);
                        let from = position - preroll;
                        mixer.restart(from).map_err(|e| anyhow::anyhow!("{e}"))?;
                        let mut offset = from;
                        while offset < position {
                            read_gained(&track, offset, MIX_FRAMES, &gains, &mut l.stems)?;
                            let slices: Vec<&[f32]> = l.stems.iter().map(|s| s.as_slice()).collect();
                            mixer.mix(offset, &slices).map_err(|e| anyhow::anyhow!("{e}"))?;
                            offset += MIX_FRAMES;
                        }
                        l.live_started = true;
                    }
                    read_gained(&track, position, n, &gains, &mut l.stems)?;
                    let slices: Vec<&[f32]> = l.stems.iter().map(|s| s.as_slice()).collect();
                    let block = mixer.mix(position, &slices).map_err(|e| anyhow::anyhow!("{e}"))?;
                    l.mixed.clear();
                    l.mixed.extend_from_slice(&block.samples);
                }
                Form::Sum | Form::Whole => {
                    for (file, buf) in track.stems.iter().zip(l.stems.iter_mut()) {
                        file.read_frames(position, n, buf)?;
                    }
                    let stems: Vec<&[f32]> = l.stems.iter().map(|s| s.as_slice()).collect();
                    let mut mixed = std::mem::take(&mut l.mixed);
                    sum_gained(&stems, &gains, &mut mixed);
                    l.mixed = mixed;
                }
                Form::Unknown => unreachable!("waited above"),
            }
            Ok(())
        })();
        if let Err(e) = mixed {
            *self.status.error.lock().unwrap() = Some(format!("{e:#}"));
            self.shared.finished_at.store(l.pushed, Ordering::Relaxed);
            l.ended = true;
            self.status.at_end.store(true, Ordering::Relaxed);
            return Step::Continue;
        }
        self.status.master_bypassed.store(matches!(form, Form::Whole) && !unity, Ordering::Relaxed);

        // A seek inside a block: the chunk starts part-way through.
        let in_channels = track.channels as usize;
        let skip = l.skip.min(n);
        l.skip = 0;
        let audible = n - skip;
        if audible == 0 {
            l.position += n;
            self.status.produced.store(l.position, Ordering::Relaxed);
            return Step::Continue;
        }
        if skip > 0 {
            l.mixed.drain(..skip * in_channels);
        }
        let device_channels = self.device_channels;
        let resampled = match l.resample.process(&l.mixed, audible) {
            Ok(r) => r,
            Err(e) => {
                *self.status.error.lock().unwrap() = Some(format!("{e:#}"));
                l.ended = true;
                self.status.at_end.store(true, Ordering::Relaxed);
                return Step::Continue;
            }
        };
        let out_frames = resampled.len() / in_channels;
        let mut fanned = std::mem::take(&mut l.fanned);
        fan_out(resampled, in_channels, device_channels, out_frames, &mut fanned);
        l.fanned = fanned;
        let source_frame = position + skip;

        // Room in the ring: the pacing. A command arriving while we wait (a seek, a new device)
        // takes precedence over this chunk, which is then simply mixed again.
        let needed = l.fanned.len();
        loop {
            if self.producer.slots() >= needed {
                break;
            }
            match self.rx.try_recv() {
                Ok(c) => return self.handle(c),
                Err(TryRecvError::Disconnected) => return Step::Shutdown,
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        let l = self.loaded.as_mut().expect("still loaded");
        self.checkpoints.push(l.pushed, source_frame);
        if let Ok(chunk) = self.producer.write_chunk_uninit(needed) {
            chunk.fill_from_iter(l.fanned.iter().copied());
        }
        l.pushed += out_frames as u64;
        l.position += n;
        self.status.produced.store(l.position, Ordering::Relaxed);
        Step::Continue
    }
}

/// The form of the master, as the mixer thread sees it this chunk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    Unknown,
    Sum,
    Whole,
    Live,
}

/// Every layer's chunk at `offset`, scaled by its gain, into `bufs` (parallel to the layers).
fn read_gained(
    track: &Track,
    offset: usize,
    frames: usize,
    gains: &[f32],
    bufs: &mut [Vec<f32>],
) -> anyhow::Result<()> {
    for ((file, buf), g) in track.stems.iter().zip(bufs.iter_mut()).zip(gains) {
        file.read_frames(offset, frames, buf)?;
        if *g != 1.0 {
            for s in buf.iter_mut() {
                *s *= g;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_gained_is_the_literal_gained_sum() {
        let a = [1.0f32, 0.5];
        let b = [0.25f32, 0.25];
        let mut out = Vec::new();
        sum_gained(&[&a, &b], &[1.0, 0.5], &mut out);
        assert_eq!(out, vec![1.125, 0.625]);
        sum_gained(&[&a, &b], &[0.0, 1.0], &mut out);
        assert_eq!(out, vec![0.25, 0.25]);
        // One layer at unity is that layer, bit for bit, minus zero included.
        let z = [-0.0f32, 0.75];
        sum_gained(&[&z], &[1.0], &mut out);
        assert_eq!(out[0].to_bits(), (-0.0f32).to_bits());
        assert_eq!(out[1], 0.75);
    }

    #[test]
    fn fan_out_shapes() {
        let mut out = Vec::new();
        fan_out(&[0.5, -0.5], 1, 2, 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5, -0.5, -0.5]);
        fan_out(&[0.5, -0.5, 0.25, 0.75], 2, 4, 2, &mut out);
        assert_eq!(out, vec![0.5, -0.5, 0.0, 0.0, 0.25, 0.75, 0.0, 0.0]);
        fan_out(&[0.5, -0.5, 1.0, 0.0], 2, 1, 2, &mut out);
        assert_eq!(out, vec![0.0, 0.5]);
        fan_out(&[0.5, -0.5], 2, 2, 1, &mut out);
        assert_eq!(out, vec![0.5, -0.5]);
    }

    #[test]
    fn gains_effective_mute_and_solo() {
        let g = Gains::new(3);
        assert!(g.all_unity());
        g.set_mute(1, true);
        assert_eq!(g.effective(), vec![1.0, 0.0, 1.0]);
        assert!(!g.all_unity());
        g.set_mute(1, false);
        g.set_solo(2, true);
        assert_eq!(g.effective(), vec![0.0, 0.0, 1.0]);
        g.set_mute(2, true);
        assert_eq!(g.effective(), vec![0.0, 0.0, 0.0], "mute wins over solo on the same layer");
        g.reset();
        assert_eq!(g.effective(), vec![1.0, 1.0, 1.0]);
        g.set_gain(0, 5.0);
        assert_eq!(g.gain(0), 2.0, "gains clamp at +6 dB");
    }

    /// Plays a track through the real pipeline (files, mixer thread, ring) with this thread
    /// standing in for the audio callback, and returns what came out of the ring.
    fn play_through(
        source: &str,
        seek_to: Option<usize>,
        until_end: bool,
    ) -> (Vec<f32>, Arc<Shared>, MixerHandle, Arc<Track>) {
        use crate::cache::Cache;
        let dir = std::env::temp_dir().join(format!(
            "syrinx-player-mixer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let cache = Cache::at(dir.clone());
        let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/")).join(source);
        let shared = Shared::new(1.0);
        let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(48_000 * 2);
        let handle = MixerHandle::spawn(producer, Arc::clone(&shared), 48_000, 2);
        let track = Track::open(&cache, &path, || {}).unwrap();
        let gains = Arc::new(Gains::new(track.stem_names.len()));
        handle.send(Command::Load { track: Arc::clone(&track), gains, start_frame: 0 });
        shared.playing.store(true, Ordering::Relaxed);
        let mut out = Vec::new();
        let mut seek_sent = false;
        let started = Instant::now();
        loop {
            // The callback's duties: honour a flush, take what is there, count it.
            if shared.flush.load(Ordering::Acquire) {
                let n = consumer.slots();
                if let Ok(chunk) = consumer.read_chunk(n) {
                    chunk.commit_all();
                }
                shared.flush.store(false, Ordering::Release);
            }
            let n = consumer.slots();
            if n > 0 {
                let chunk = consumer.read_chunk(n).unwrap();
                let (a, b) = chunk.as_slices();
                out.extend_from_slice(a);
                out.extend_from_slice(b);
                chunk.commit_all();
                shared.consumed.fetch_add((n / 2) as u64, Ordering::Relaxed);
            }
            if let (Some(frame), false) = (seek_to, seek_sent) {
                if out.len() >= 4096 * 2 * 4 {
                    // A second of the start has played; now the seek, and forget what came before.
                    handle.send(Command::Seek(frame));
                    seek_sent = true;
                    out.clear();
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            }
            let finished = shared.finished_at.load(Ordering::Relaxed);
            if finished != u64::MAX && shared.consumed.load(Ordering::Relaxed) >= finished && consumer.slots() == 0 {
                if seek_to.is_none() || seek_sent {
                    break;
                }
            }
            if !until_end && seek_sent && out.len() >= 48_000 * 2 {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(60), "the pipeline did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = std::fs::remove_dir_all(dir);
        (out, shared, handle, track)
    }

    fn canonical(source: &str) -> syrinx_core::Rendered {
        let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/")).join(source);
        let text = std::fs::read_to_string(&path).unwrap();
        let opts = syrinx_core::RenderOptions { timeout: Duration::from_secs(120), ..Default::default() };
        syrinx_core::render(&text, &path.to_string_lossy(), &opts).unwrap()
    }

    /// The whole pipeline at unity, from the first frame, is the canonical render, bit for bit:
    /// a mix stream restarted at 0 and fed the layers' blocks in order is the definition of the
    /// render, and nothing on the way (files, gains at 1.0, a bypassed resampler, stereo onto a
    /// stereo device) may touch a sample.
    #[test]
    fn a_mix_stream_played_from_the_start_is_the_canonical_render() {
        let (out, _, _, _) = play_through("beacon.syr", None, true);
        let want = canonical("beacon.syr");
        assert_eq!(out.len(), want.samples.len(), "frame count");
        assert!(out == want.samples, "samples differ from the canonical render");
    }

    /// Same for a whole-buffer source: the canonical mix file plays.
    #[test]
    fn a_whole_buffer_source_played_from_the_start_is_the_canonical_render() {
        let (out, _, _, _) = play_through("explosion.syr", None, true);
        let want = canonical("explosion.syr");
        assert_eq!(out.len(), want.samples.len(), "frame count");
        assert!(out == want.samples, "samples differ from the canonical render");
    }

    /// A seek inside a block lands on the frame asked for (the checkpoint says so) and, after
    /// the pre-roll has warmed the master, the audio is the canonical render there.
    #[test]
    fn a_seek_lands_on_the_frame_and_plays_the_canonical_audio_from_there() {
        let target = 4096 * 20 + 1000;
        let (out, shared, handle, track) = play_through("beacon.syr", Some(target), false);
        let want = canonical("beacon.syr");
        assert!(out.len() >= 48_000 * 2, "at least a second after the seek");
        let consumed = shared.consumed.load(Ordering::Relaxed);
        let position = handle.checkpoints.position(consumed, 48_000, track.sample_rate).unwrap();
        let played = out.len() / 2;
        assert_eq!(
            position,
            target + played,
            "the checkpoints must place the playhead exactly where the seek landed plus what played"
        );
        let expected = &want.samples[target * 2..target * 2 + out.len()];
        let worst = out.iter().zip(expected).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "post-seek audio differs from the canonical render by {worst}");
    }

    #[test]
    fn checkpoints_map_a_consumed_count_to_a_source_frame() {
        let c = Checkpoints::new();
        c.push(0, 0);
        c.push(4096, 4096);
        assert_eq!(c.position(5000, 48_000, 48_000), Some(5000));
        // A different device rate scales the distance into the chunk.
        assert_eq!(c.position(4096 + 441, 44_100, 48_000), Some(4096 + 480));
        // A seek clears the history; the first chunk after it starts where the callback was.
        c.clear();
        assert_eq!(c.position(5000, 48_000, 48_000), None);
        c.push(8192, 100_000);
        assert_eq!(c.position(9000, 48_000, 48_000), Some(100_808));
        assert_eq!(c.position(8000, 48_000, 48_000), None, "before the first chunk nothing is known");
    }
}
