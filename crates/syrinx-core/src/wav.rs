//! Encoders for the rendered samples.

use std::io;
use std::path::Path;

use crate::Rendered;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// 16-bit PCM WAV. Plays everywhere.
    Wav16,
    /// 24-bit PCM WAV.
    Wav24,
    /// 32-bit float WAV. Lossless with respect to the render.
    WavFloat,
    /// Headerless interleaved little-endian f32. What an engine cache wants.
    RawF32,
}

impl Format {
    /// Picks a format from an output path's extension.
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "wav" => Ok(Format::Wav16),
            "f32" | "pcm" | "raw" => Ok(Format::RawF32),
            "mp3" | "ogg" | "opus" | "flac" | "aac" | "m4a" => Err(format!(
                "{ext} output is not supported: lossy encoders add delay that breaks seamless loops, \
                 and the cache artefact is PCM. Write .wav or .f32"
            )),
            "" => Err("output path has no extension; use .wav or .f32".into()),
            other => Err(format!("unknown output extension .{other}; use .wav or .f32")),
        }
    }
}

/// Writes `rendered` to `path` in `format`.
pub fn write(path: &Path, rendered: &Rendered, format: Format) -> io::Result<()> {
    match format {
        Format::RawF32 => {
            let mut bytes = Vec::with_capacity(rendered.samples.len() * 4);
            for s in &rendered.samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            std::fs::write(path, bytes)
        }
        Format::Wav16 | Format::Wav24 | Format::WavFloat => {
            let (bits, sample_format) = match format {
                Format::Wav16 => (16, hound::SampleFormat::Int),
                Format::Wav24 => (24, hound::SampleFormat::Int),
                _ => (32, hound::SampleFormat::Float),
            };
            let spec = hound::WavSpec {
                channels: rendered.channels as u16,
                sample_rate: rendered.sample_rate,
                bits_per_sample: bits,
                sample_format,
            };
            let mut w = hound::WavWriter::create(path, spec).map_err(to_io)?;
            match format {
                Format::Wav16 => {
                    for &s in &rendered.samples {
                        w.write_sample(quantise(s, 32767.0) as i16).map_err(to_io)?;
                    }
                }
                Format::Wav24 => {
                    for &s in &rendered.samples {
                        w.write_sample(quantise(s, 8_388_607.0)).map_err(to_io)?;
                    }
                }
                _ => {
                    for &s in &rendered.samples {
                        w.write_sample(s).map_err(to_io)?;
                    }
                }
            }
            w.finalize().map_err(to_io)
        }
    }
}

/// Clamp to [-1, 1] and round to the nearest integer code.
fn quantise(s: f32, full_scale: f32) -> i32 {
    (s.clamp(-1.0, 1.0) * full_scale).round() as i32
}

fn to_io(e: hound::Error) -> io::Error {
    match e {
        hound::Error::IoError(e) => e,
        other => io::Error::other(other),
    }
}
