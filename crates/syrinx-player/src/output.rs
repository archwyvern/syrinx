//! The device end: a cpal output stream whose callback copies from a ring the mixer fills.
//! The callback is real-time: it reads atomics and the ring, allocates nothing, locks nothing
//! and never blocks. Pause is silence; the stream itself is never paused.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// How far ahead of the device the mixer may run: the ring's capacity.
pub const RING_SECONDS: u32 = 1;

/// State the callback reads (and `consumed`/`starved`, which it writes).
pub struct Shared {
    pub playing: AtomicBool,
    /// Mixer -> callback: throw away everything in the ring, then clear this.
    pub flush: AtomicBool,
    /// The ring ran dry while playing and the track was not over: the render is behind.
    pub starved: AtomicBool,
    /// Master volume, `f32` bits.
    pub volume: AtomicU32,
    /// Silence at the device without moving the volume: a screenshot run plays, unheard.
    pub muted: AtomicBool,
    /// Device frames the callback has taken from the ring since the stream opened.
    pub consumed: AtomicU64,
    /// The `consumed` count at which the current track ends; `u64::MAX` while it has not.
    pub finished_at: AtomicU64,
}

impl Shared {
    pub fn new(volume: f32) -> Arc<Shared> {
        Arc::new(Shared {
            playing: AtomicBool::new(false),
            flush: AtomicBool::new(false),
            starved: AtomicBool::new(false),
            volume: AtomicU32::new(volume.to_bits()),
            muted: AtomicBool::new(false),
            consumed: AtomicU64::new(0),
            finished_at: AtomicU64::new(u64::MAX),
        })
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// What is known of the open device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Hears a running stream fail: a device unplugged, a sound server gone.
pub type OnError = Box<dyn Fn(String) + Send + 'static>;

/// The session's way to the sound card: [`Cpal`] in the player; a test brings its own and plays
/// the callback's part itself.
pub trait Audio {
    /// Opens `name` (the default device when `None`) at its default configuration and returns
    /// the producer end of a fresh ring for the mixer. On failure whatever was open stays open.
    fn open(
        &mut self,
        name: Option<&str>,
        shared: &Arc<Shared>,
        on_error: OnError,
    ) -> Result<(OutputInfo, rtrb::Producer<f32>)>;

    /// Every output device, the default first.
    fn devices(&mut self) -> Vec<DeviceInfo>;
}

/// The real device through cpal's default host. Holds the open stream; opening another one
/// replaces it.
#[derive(Default)]
pub struct Cpal {
    stream: Option<cpal::Stream>,
}

impl Audio for Cpal {
    fn open(
        &mut self,
        name: Option<&str>,
        shared: &Arc<Shared>,
        on_error: OnError,
    ) -> Result<(OutputInfo, rtrb::Producer<f32>)> {
        let host = cpal::default_host();
        let device = match name {
            Some(wanted) => host
                .output_devices()
                .context("listing output devices")?
                .find(|d| d.description().map(|d| d.name() == wanted).unwrap_or(false))
                .ok_or_else(|| anyhow!("no output device named {wanted:?}"))?,
            None => host.default_output_device().ok_or_else(|| anyhow!("no default output device"))?,
        };
        let description = device.description().context("describing the output device")?;
        let default = device.default_output_config().context("the device's default output configuration")?;
        let config = cpal::StreamConfig {
            channels: default.channels(),
            sample_rate: default.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let channels = config.channels as usize;
        let capacity = (config.sample_rate as usize) * channels * RING_SECONDS as usize;
        let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(capacity);

        let callback_shared = Arc::clone(shared);
        let data = move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let shared = &callback_shared;
            if shared.flush.load(Ordering::Acquire) {
                let n = consumer.slots();
                if let Ok(chunk) = consumer.read_chunk(n) {
                    chunk.commit_all();
                }
                shared.flush.store(false, Ordering::Release);
            }
            if !shared.playing.load(Ordering::Relaxed) {
                out.fill(0.0);
                return;
            }
            let volume = if shared.muted.load(Ordering::Relaxed) {
                0.0
            } else {
                f32::from_bits(shared.volume.load(Ordering::Relaxed))
            };
            let want = out.len() - out.len() % channels;
            let mut have = consumer.slots().min(want);
            have -= have % channels;
            if let Ok(chunk) = consumer.read_chunk(have) {
                let (a, b) = chunk.as_slices();
                for (o, s) in out.iter_mut().zip(a.iter().chain(b.iter())) {
                    *o = (s * volume).clamp(-1.0, 1.0);
                }
                chunk.commit_all();
            } else {
                have = 0;
            }
            out[have..].fill(0.0);
            let frames = (have / channels) as u64;
            let consumed = shared.consumed.fetch_add(frames, Ordering::Relaxed) + frames;
            let finished = shared.finished_at.load(Ordering::Relaxed);
            shared.starved.store(have < want && consumed < finished, Ordering::Relaxed);
        };
        let error = move |e: cpal::Error| on_error(e.to_string());
        let stream = device
            .build_output_stream(config, data, error, Some(Duration::from_secs(2)))
            .context("opening the output stream")?;
        stream.play().context("starting the output stream")?;
        self.stream = Some(stream);
        let info = OutputInfo {
            device_name: description.name().to_string(),
            sample_rate: config.sample_rate,
            channels: config.channels,
        };
        Ok((info, producer))
    }

    fn devices(&mut self) -> Vec<DeviceInfo> {
        let host = cpal::default_host();
        let default_name =
            host.default_output_device().and_then(|d| d.description().ok()).map(|d| d.name().to_string());
        let mut out = Vec::new();
        if let Ok(devices) = host.output_devices() {
            for device in devices {
                if let Ok(description) = device.description() {
                    let name = description.name().to_string();
                    let is_default = default_name.as_deref() == Some(name.as_str());
                    out.push(DeviceInfo { name, is_default });
                }
            }
        }
        out.sort_by_key(|d| !d.is_default);
        out
    }
}
