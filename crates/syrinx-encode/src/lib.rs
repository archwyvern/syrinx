//! Encoders for rendered sounds.
//!
//! Native (no external tools): WAV, raw float, FLAC, MP3, Ogg Vorbis, Ogg Opus. Everything else
//! (AAC/M4A/MP4, AIFF, ALAC, WMA, ...) is handed to `ffmpeg` on PATH with the render piped in
//! as raw float, so any container/codec ffmpeg can write is reachable.
//!
//! Which to use for a game: the engine cache wants raw float or WAV; FLAC when a lossless file
//! must be small; Vorbis or Opus for shipped streams (Opus is the better codec, Vorbis has the
//! wider decoder support). MP3 and AAC add encoder delay and padding that break seamless loops,
//! so keep them for one-shots or exports for other people.

use std::fmt;
use std::io::{self, Write};
use std::num::{NonZeroU32, NonZeroU8};
use std::path::Path;
use std::process::{Command, Stdio};

use syrinx_core::Rendered;

pub use syrinx_core::wav::Format as WavFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// PCM WAV, 16-bit.
    Wav16,
    /// PCM WAV, 24-bit.
    Wav24,
    /// 32-bit float WAV.
    WavFloat,
    /// Headerless interleaved little-endian f32.
    RawF32,
    /// FLAC (16-bit, or 24-bit with `--bits 24`).
    Flac16,
    Flac24,
    /// MP3 via LAME, CBR at `bitrate`.
    Mp3,
    /// Ogg Vorbis, VBR targeting `bitrate`.
    Vorbis,
    /// Ogg Opus at `bitrate`. Needs a 48 kHz render.
    Opus,
    /// Anything ffmpeg can write; the extension decides the container and codec.
    Ffmpeg,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Lossy target in kbit/s.
    pub bitrate_kbps: u32,
    /// Bit depth for WAV/FLAC: 16, 24 or 32 (32 = float WAV; FLAC caps at 24).
    pub bits: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self { bitrate_kbps: 160, bits: 16 }
    }
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Unsupported(String),
    Encoder(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Unsupported(m) | Error::Encoder(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Picks the codec from the output path's extension and the bit-depth option.
pub fn codec_for(path: &Path, opts: &Options) -> Result<Codec, Error> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    Ok(match ext.as_str() {
        "wav" => match opts.bits {
            16 => Codec::Wav16,
            24 => Codec::Wav24,
            32 => Codec::WavFloat,
            b => return Err(Error::Unsupported(format!("wav bit depth {b}; use 16, 24 or 32"))),
        },
        "f32" | "pcm" | "raw" => Codec::RawF32,
        "flac" => match opts.bits {
            16 => Codec::Flac16,
            24 | 32 => Codec::Flac24,
            b => return Err(Error::Unsupported(format!("flac bit depth {b}; use 16 or 24"))),
        },
        "mp3" => Codec::Mp3,
        "ogg" | "oga" => Codec::Vorbis,
        "opus" => Codec::Opus,
        "" => return Err(Error::Unsupported("output path has no extension".into())),
        _ => Codec::Ffmpeg,
    })
}

/// Encodes `rendered` to `path`.
pub fn write(path: &Path, rendered: &Rendered, codec: Codec, opts: &Options) -> Result<(), Error> {
    match codec {
        Codec::Wav16 => Ok(syrinx_core::wav::write(path, rendered, WavFormat::Wav16)?),
        Codec::Wav24 => Ok(syrinx_core::wav::write(path, rendered, WavFormat::Wav24)?),
        Codec::WavFloat => Ok(syrinx_core::wav::write(path, rendered, WavFormat::WavFloat)?),
        Codec::RawF32 => Ok(syrinx_core::wav::write(path, rendered, WavFormat::RawF32)?),
        Codec::Flac16 => std::fs::write(path, flac(rendered, 16)?).map_err(Into::into),
        Codec::Flac24 => std::fs::write(path, flac(rendered, 24)?).map_err(Into::into),
        Codec::Mp3 => std::fs::write(path, mp3(rendered, opts)?).map_err(Into::into),
        Codec::Vorbis => std::fs::write(path, vorbis(rendered, opts)?).map_err(Into::into),
        Codec::Opus => std::fs::write(path, opus(rendered, opts)?).map_err(Into::into),
        Codec::Ffmpeg => ffmpeg(path, rendered, opts),
    }
}

/// Human-readable name of what a codec produces, for reports.
pub fn describe(codec: Codec, opts: &Options) -> String {
    match codec {
        Codec::Wav16 => "wav 16-bit".into(),
        Codec::Wav24 => "wav 24-bit".into(),
        Codec::WavFloat => "wav 32-bit float".into(),
        Codec::RawF32 => "raw f32".into(),
        Codec::Flac16 => "flac 16-bit".into(),
        Codec::Flac24 => "flac 24-bit".into(),
        Codec::Mp3 => format!("mp3 {} kbps", opts.bitrate_kbps),
        Codec::Vorbis => format!("ogg vorbis ~{} kbps", opts.bitrate_kbps),
        Codec::Opus => format!("opus {} kbps", opts.bitrate_kbps),
        Codec::Ffmpeg => format!("ffmpeg {} kbps", opts.bitrate_kbps),
    }
}

fn planes(r: &Rendered) -> Vec<Vec<f32>> {
    let ch = r.channels as usize;
    (0..ch).map(|c| r.samples.iter().skip(c).step_by(ch).copied().collect()).collect()
}

fn quantise(s: f32, full_scale: f32) -> i32 {
    (s.clamp(-1.0, 1.0) * full_scale).round() as i32
}

fn flac(r: &Rendered, bits: usize) -> Result<Vec<u8>, Error> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let full_scale = ((1i64 << (bits - 1)) - 1) as f32;
    let samples: Vec<i32> = r.samples.iter().map(|&s| quantise(s, full_scale)).collect();
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| Error::Encoder(format!("flac config: {e:?}")))?;
    let source = flacenc::source::MemSource::from_samples(&samples, r.channels as usize, bits, r.sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| Error::Encoder(format!("flac: {e:?}")))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| Error::Encoder(format!("flac write: {e:?}")))?;
    Ok(sink.as_slice().to_vec())
}

fn mp3(r: &Rendered, opts: &Options) -> Result<Vec<u8>, Error> {
    use mp3lame_encoder::{Bitrate, Builder, DualPcm, FlushGap, MonoPcm, Quality};

    let bitrate = match opts.bitrate_kbps {
        0..=8 => Bitrate::Kbps8,
        9..=16 => Bitrate::Kbps16,
        17..=24 => Bitrate::Kbps24,
        25..=32 => Bitrate::Kbps32,
        33..=40 => Bitrate::Kbps40,
        41..=48 => Bitrate::Kbps48,
        49..=64 => Bitrate::Kbps64,
        65..=80 => Bitrate::Kbps80,
        81..=96 => Bitrate::Kbps96,
        97..=112 => Bitrate::Kbps112,
        113..=128 => Bitrate::Kbps128,
        129..=160 => Bitrate::Kbps160,
        161..=192 => Bitrate::Kbps192,
        193..=224 => Bitrate::Kbps224,
        225..=256 => Bitrate::Kbps256,
        _ => Bitrate::Kbps320,
    };
    fn err<E: fmt::Debug>(what: &'static str) -> impl FnOnce(E) -> Error {
        move |e| Error::Encoder(format!("mp3 {what}: {e:?}"))
    }
    let mut enc = Builder::new().ok_or_else(|| Error::Encoder("mp3: cannot create LAME encoder".into()))?;
    enc.set_num_channels(r.channels as u8).map_err(err("channels"))?;
    enc.set_sample_rate(r.sample_rate).map_err(err("sample rate"))?;
    enc.set_brate(bitrate).map_err(err("bitrate"))?;
    enc.set_quality(Quality::Best).map_err(err("quality"))?;
    // The LAME tag carries encoder delay + padding so gapless-aware decoders return exactly
    // the samples that went in.
    enc.set_to_write_vbr_tag(true).map_err(err("vbr tag"))?;
    let mut enc = enc.build().map_err(err("init"))?;

    let mut out = Vec::new();
    let p = planes(r);
    let encoded = if r.channels == 2 {
        let input = DualPcm { left: p[0].as_slice(), right: p[1].as_slice() };
        out.reserve(mp3lame_encoder::max_required_buffer_size(p[0].len()));
        enc.encode(input, out.spare_capacity_mut()).map_err(err("encode"))?
    } else {
        out.reserve(mp3lame_encoder::max_required_buffer_size(p[0].len()));
        enc.encode(MonoPcm(p[0].as_slice()), out.spare_capacity_mut()).map_err(err("encode"))?
    };
    // SAFETY: the encoder wrote `encoded` bytes into the reserved spare capacity.
    unsafe { out.set_len(out.len() + encoded) };
    enc.flush_to_vec::<FlushGap>(&mut out).map_err(err("flush"))?;
    // LAME reserved a placeholder frame at the start; now that the delay and padding are known,
    // write the real LAME/Xing tag over it so gapless-aware decoders trim both.
    let tag_size = enc.lame_tag_size();
    if tag_size > 0 && tag_size <= out.len() {
        let mut tag = vec![std::mem::MaybeUninit::<u8>::uninit(); tag_size];
        if let Some(written) = enc.lame_tag_encode(&mut tag) {
            let written = written.get().min(tag_size);
            // SAFETY: LAME initialised the first `written` bytes.
            let tag: &[u8] = unsafe { std::slice::from_raw_parts(tag.as_ptr() as *const u8, written) };
            out[..written].copy_from_slice(tag);
        }
    }
    Ok(out)
}

fn vorbis(r: &Rendered, opts: &Options) -> Result<Vec<u8>, Error> {
    use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

    let err = |e: vorbis_rs::VorbisError| Error::Encoder(format!("vorbis: {e}"));
    let rate = NonZeroU32::new(r.sample_rate).ok_or_else(|| Error::Encoder("vorbis: zero sample rate".into()))?;
    let channels = NonZeroU8::new(r.channels as u8).ok_or_else(|| Error::Encoder("vorbis: zero channels".into()))?;
    let bitrate = NonZeroU32::new(opts.bitrate_kbps.max(32) * 1000).unwrap();
    let mut builder = VorbisEncoderBuilder::new(rate, channels, Vec::<u8>::new()).map_err(err)?;
    builder.bitrate_management_strategy(VorbisBitrateManagementStrategy::Vbr { target_bitrate: bitrate });
    // A title only when the source declares a name: the tag carries what the sound says it is.
    if let Some(name) = &r.meta.name {
        builder.comment_tag("TITLE", name.as_str()).map_err(err)?;
    }
    let mut encoder = builder.build().map_err(err)?;
    let p = planes(r);
    let block: Vec<&[f32]> = p.iter().map(|v| v.as_slice()).collect();
    encoder.encode_audio_block(&block).map_err(err)?;
    encoder.finish().map_err(err)
}

fn opus(r: &Rendered, opts: &Options) -> Result<Vec<u8>, Error> {
    use ogg::writing::{PacketWriteEndInfo, PacketWriter};
    use opus::{Application, Bitrate, Channels, Encoder};

    const RATE: u32 = 48_000;
    const FRAME: usize = 960; // 20 ms at 48 kHz

    if r.sample_rate != RATE {
        return Err(Error::Unsupported(format!(
            "opus needs a 48000 Hz render (got {}); pass --sample-rate 48000",
            r.sample_rate
        )));
    }
    let err = |e: opus::Error| Error::Encoder(format!("opus: {e}"));
    let channels = if r.channels == 2 { Channels::Stereo } else { Channels::Mono };
    let mut enc = Encoder::new(RATE, channels, Application::Audio).map_err(err)?;
    enc.set_bitrate(Bitrate::Bits((opts.bitrate_kbps.max(6) * 1000) as i32)).map_err(err)?;
    let pre_skip = enc.get_lookahead().map_err(err)? as u16;
    let ch = r.channels as usize;

    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(ch as u8);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&RATE.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // mapping family 0: mono/stereo

    let vendor = b"syrinx";
    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    // A title only when the source declares a name, as for vorbis.
    let comments: Vec<String> = r.meta.name.iter().map(|name| format!("TITLE={name}")).collect();
    tags.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for comment in &comments {
        tags.extend_from_slice(&(comment.len() as u32).to_le_bytes());
        tags.extend_from_slice(comment.as_bytes());
    }

    let serial = 0x5359_5258; // "SYRX"
    let mut writer = PacketWriter::new(Vec::<u8>::new());
    writer.write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)?;
    writer.write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)?;

    // The encoder delays its output by `pre_skip` samples, so the audio has to be followed by
    // at least that much silence for the tail to come out. Granule positions count decoded
    // samples including the pre-skip; the final one is the true end (pre_skip + frames), so
    // decoders trim both the lookahead at the start and the padding at the end.
    let total = r.frames as u64;
    let needed = total + pre_skip as u64;
    let mut buf = vec![0u8; 4000];
    let mut frame = vec![0f32; FRAME * ch];
    let mut sent: u64 = 0;
    let mut pos = 0usize;
    while sent < needed {
        let n = (r.frames - pos.min(r.frames)).min(FRAME);
        frame.fill(0.0);
        if n > 0 {
            frame[..n * ch].copy_from_slice(&r.samples[pos * ch..(pos + n) * ch]);
        }
        let len = enc.encode_float(&frame, &mut buf).map_err(err)?;
        pos += n;
        sent += FRAME as u64;
        let last = sent >= needed;
        let granule = if last { needed } else { pre_skip as u64 + sent };
        let end = if last { PacketWriteEndInfo::EndStream } else { PacketWriteEndInfo::NormalPacket };
        writer.write_packet(buf[..len].to_vec(), serial, end, granule)?;
    }
    Ok(writer.into_inner())
}

fn ffmpeg(path: &Path, r: &Rendered, opts: &Options) -> Result<(), Error> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-f", "f32le"])
        .args(["-ar", &r.sample_rate.to_string(), "-ac", &r.channels.to_string(), "-i", "pipe:0"]);
    match ext.as_str() {
        "m4a" | "mp4" | "aac" => {
            cmd.args(["-c:a", "aac", "-b:a", &format!("{}k", opts.bitrate_kbps)]);
        }
        "wma" | "ac3" | "mp2" | "webm" | "mka" => {
            cmd.args(["-b:a", &format!("{}k", opts.bitrate_kbps)]);
        }
        _ => {}
    }
    cmd.arg(path).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Error::Unsupported(format!(".{ext} needs ffmpeg on PATH (native formats: wav, f32, flac, mp3, ogg, opus)"))
        } else {
            Error::Io(e)
        }
    })?;
    {
        let mut stdin = child.stdin.take().unwrap();
        let mut bytes = Vec::with_capacity(r.samples.len() * 4);
        for s in &r.samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        stdin.write_all(&bytes)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(Error::Encoder(format!("ffmpeg failed for .{ext}: {msg}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use syrinx_core::Meta;

    fn tone(channels: u32, rate: u32) -> Rendered {
        let frames = rate as usize / 2;
        let mut samples = Vec::with_capacity(frames * channels as usize);
        for i in 0..frames {
            let s = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
            for _ in 0..channels {
                samples.push(s);
            }
        }
        Rendered {
            meta: Meta { name: Some("tone".into()), duration: 0.5, channels, sample_rate: None, seed: 0, looping: false },
            sample_rate: rate,
            channels,
            frames,
            samples,
            dependencies: Vec::new(),
            stem: None,
            stem_names: vec!["tone".into()],
        }
    }

    #[test]
    fn flac_has_magic() {
        let out = flac(&tone(2, 48_000), 16).unwrap();
        assert_eq!(&out[..4], b"fLaC");
        let out24 = flac(&tone(1, 44_100), 24).unwrap();
        assert_eq!(&out24[..4], b"fLaC");
    }

    #[test]
    fn mp3_has_frames() {
        let out = mp3(&tone(2, 48_000), &Options::default()).unwrap();
        assert!(out.len() > 1000);
        let out = mp3(&tone(1, 44_100), &Options { bitrate_kbps: 64, bits: 16 }).unwrap();
        assert!(out.len() > 1000);
    }

    #[test]
    fn vorbis_is_ogg() {
        let out = vorbis(&tone(2, 48_000), &Options::default()).unwrap();
        assert_eq!(&out[..4], b"OggS");
    }

    #[test]
    fn opus_is_ogg_with_head() {
        let out = opus(&tone(2, 48_000), &Options::default()).unwrap();
        assert_eq!(&out[..4], b"OggS");
        assert!(out.windows(8).any(|w| w == b"OpusHead"));
        assert!(matches!(opus(&tone(1, 44_100), &Options::default()), Err(Error::Unsupported(_))));
    }

    #[test]
    fn codec_from_extension() {
        let o = Options::default();
        assert_eq!(codec_for(Path::new("a.wav"), &o).unwrap(), Codec::Wav16);
        assert_eq!(codec_for(Path::new("a.flac"), &Options { bits: 24, ..o.clone() }).unwrap(), Codec::Flac24);
        assert_eq!(codec_for(Path::new("a.m4a"), &o).unwrap(), Codec::Ffmpeg);
        assert!(codec_for(Path::new("a"), &o).is_err());
    }
}
