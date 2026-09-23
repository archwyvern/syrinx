//! Render rate to device rate, per chunk. Bypassed when they agree, which is the normal case:
//! PipeWire and WASAPI both run at 48 kHz by default and every track in the catalogue renders at
//! 48 kHz. Rendering at the device rate instead was rejected: a 192 kHz device would make every
//! render four times slower for nothing the ear can hear.

use anyhow::{Context, Result};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub struct Resample {
    inner: Option<Async<f32>>,
    channels: usize,
    chunk_frames: usize,
    padded: Vec<f32>,
    out: Vec<f32>,
}

impl Resample {
    /// `chunk_frames` is the fixed input size every `process` call is sized for; a shorter
    /// input is legal only as the last chunk.
    pub fn new(from_rate: u32, to_rate: u32, channels: usize, chunk_frames: usize) -> Result<Resample> {
        let inner = if from_rate == to_rate {
            None
        } else {
            let params = SincInterpolationParameters {
                sinc_len: 128,
                f_cutoff: None,
                oversampling_factor: 128,
                interpolation: SincInterpolationType::Cubic,
                window: WindowFunction::BlackmanHarris2,
            };
            Some(
                Async::<f32>::new_sinc(
                    to_rate as f64 / from_rate as f64,
                    1.0,
                    &params,
                    chunk_frames,
                    channels,
                    FixedAsync::Input,
                )
                .with_context(|| format!("resampler {from_rate} -> {to_rate} Hz"))?,
            )
        };
        let out_frames = inner.as_ref().map_or(chunk_frames, |r| r.output_frames_max());
        Ok(Resample {
            inner,
            channels,
            chunk_frames,
            padded: vec![0.0; chunk_frames * channels],
            out: vec![0.0; out_frames * channels],
        })
    }

    #[cfg(test)]
    pub fn is_bypass(&self) -> bool {
        self.inner.is_none()
    }

    /// Interleaved `frames` frames in (at most `chunk_frames`), interleaved frames out. The
    /// output borrows this resampler and is valid until the next call.
    pub fn process<'a>(&'a mut self, input: &'a [f32], frames: usize) -> Result<&'a [f32]> {
        let Some(inner) = self.inner.as_mut() else {
            return Ok(&input[..frames * self.channels]);
        };
        // rubato reads exactly chunk_frames; a short last chunk is zero-padded and flagged.
        let partial = frames < self.chunk_frames;
        let source: &[f32] = if partial {
            self.padded.fill(0.0);
            self.padded[..frames * self.channels].copy_from_slice(&input[..frames * self.channels]);
            &self.padded
        } else {
            &input[..self.chunk_frames * self.channels]
        };
        let buffer_in = InterleavedSlice::new(source, self.channels, self.chunk_frames).context("input adapter")?;
        let capacity = self.out.len() / self.channels;
        let mut buffer_out =
            InterleavedSlice::new_mut(&mut self.out, self.channels, capacity).context("output adapter")?;
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: partial.then_some(frames),
            active_channels_mask: None,
        };
        let (_, out_frames) =
            inner.process_into_buffer(&buffer_in, &mut buffer_out, Some(&indexing)).context("resampling")?;
        Ok(&self.out[..out_frames * self.channels])
    }

    /// Forgets the filter history, for a seek.
    pub fn reset(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_rates_bypass_and_pass_the_input_through() {
        let mut r = Resample::new(48_000, 48_000, 2, 4).unwrap();
        assert!(r.is_bypass());
        let input = [0.5, -0.5, 0.25, -0.25, 1.0, -1.0, 0.0, 0.0];
        let out = r.process(&input, 4).unwrap();
        assert_eq!(out, &input);
        // A short last chunk comes back as exactly its frames.
        let out = r.process(&input, 2).unwrap();
        assert_eq!(out, &input[..4]);
    }

    #[test]
    fn a_ratio_produces_the_expected_frame_count() {
        let mut r = Resample::new(48_000, 44_100, 2, 4096).unwrap();
        assert!(!r.is_bypass());
        let input = vec![0.0f32; 4096 * 2];
        let mut total = 0;
        for _ in 0..10 {
            total += r.process(&input, 4096).unwrap().len() / 2;
        }
        // 10 chunks of 4096 at 48k are 40960 frames = 37632 at 44.1k, give or take the
        // resampler's start-up delay of one chunk.
        assert!((33536..=37700).contains(&total), "got {total} frames");
        let last = r.process(&input, 1000).unwrap().len() / 2;
        assert!(last > 0, "a partial last chunk still produces output");
    }
}
