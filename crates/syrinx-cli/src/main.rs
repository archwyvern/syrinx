//! syrinx: compile sound sources to PCM, or play them.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use std::collections::BTreeMap;
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rayon::prelude::*;
use syrinx_core::wav::Format;
use syrinx_core::{Error, ErrorKind, RenderOptions, Rendered, Target};
#[cfg(not(windows))]
use syrinx_core::Stream;
use syrinx_encode::Options as EncodeOptions;

#[derive(Parser)]
#[command(name = "syrinx", version, about = "A compiler for sounds: JavaScript in, PCM out.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile sources to audio files. Directories are searched for .syr files recursively.
    Compile {
        /// Source files (.syr) or directories.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Output file for a single input, or output directory for several. The extension
        /// picks the format: .wav, .f32 (raw float), .flac, .mp3, .ogg (vorbis), .opus
        /// natively; anything else (.m4a, .mp4, .aiff, ...) goes through ffmpeg if it is on
        /// PATH. Default: next to each input, with the --format extension.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Extension to use when --output is a directory or absent.
        #[arg(long, default_value = "wav")]
        format: String,
        /// Bit depth for .wav (16, 24, 32 = float) and .flac (16, 24) output.
        #[arg(long, default_value = "16")]
        bits: Bits,
        /// Target bitrate in kbit/s for lossy output (mp3, ogg, opus, aac).
        #[arg(long, default_value_t = 160)]
        bitrate: u32,
        /// Parallel renders. Default: one per CPU.
        #[arg(short, long)]
        jobs: Option<usize>,
        /// Write every layer to its own 32-bit float WAV in this directory, with a bounce.json
        /// describing them. Lossless on purpose: the mix stage must see what the layer produced.
        /// One source only.
        #[arg(long, conflicts_with_all = ["mix_from", "output"])]
        bounce: Option<PathBuf>,
        /// Run only the mix stage, over layers bounced into this directory earlier. One source
        /// only. Warns when the source has changed since the bounce.
        #[arg(long)]
        mix_from: Option<PathBuf>,
        #[command(flatten)]
        render: RenderArgs,
        /// Print nothing on success.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Print a hash of each source's rendered output, for a lockfile that catches unintended
    /// changes to sounds, shared modules or the prelude.
    Hash {
        /// Source files (.syr) or directories.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Write the hashes to this lockfile.
        #[arg(long)]
        write: Option<PathBuf>,
        /// Compare against this lockfile; exit non-zero on any change.
        #[arg(long)]
        check: Option<PathBuf>,
        #[arg(short, long)]
        jobs: Option<usize>,
        #[command(flatten)]
        render: RenderArgs,
    },
    /// Play a source through the system audio player. A streaming source starts at once: its
    /// blocks go to the player's stdin as raw float as they are computed.
    Play {
        /// Source file (.syr).
        input: PathBuf,
        /// Play it this many times.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        /// Player command to use instead of the first of pw-play, paplay, aplay, ffplay found on
        /// PATH. The sound is then rendered whole to a temporary wav, whose path is appended as
        /// the last argument.
        #[arg(long)]
        player: Option<String>,
        #[command(flatten)]
        render: RenderArgs,
    },
    /// Run the static determinism check and the module graph; print meta and imports.
    Check {
        /// Source files (.syr).
        inputs: Vec<PathBuf>,
        /// Project root: imports may not resolve outside it.
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Measure sounds: attack, decay, brightness, tonal/noisy balance, band energies, onsets.
    /// Inputs are .syr sources or .wav files.
    Analyze {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Print JSON (one object per input).
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        render: RenderArgs,
    },
    /// Draw a log-frequency spectrogram PNG of a sound (.syr or .wav).
    Spectrogram {
        input: PathBuf,
        /// Output PNG. Default: the input path with .png appended.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        render: RenderArgs,
    },
    /// Score a candidate against a reference, axis by axis, with a note per axis. Either may be
    /// a .syr source or a .wav file.
    Compare {
        candidate: PathBuf,
        reference: PathBuf,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        render: RenderArgs,
    },
    /// Print version and build information.
    Info,
    /// Print the prelude source (the library every sound is compiled against).
    Prelude,
    /// Print the TypeScript declarations for the prelude and the source contract.
    Types,
    /// Print the prelude's API reference as JSON, for a documentation site to render.
    Docs {
        /// Write to this file instead of standard output.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Write the syrinx framework into a directory of the project, to import by relative path.
    /// Files that differ are overwritten and named; nothing is deleted.
    Framework {
        /// Where to write it. Default: ./framework
        dir: Option<PathBuf>,
    },
}

#[derive(Args)]
struct RenderArgs {
    /// Render only these layers, summed, without the mix stage. Repeatable, or comma-separated.
    #[arg(long = "stem", value_delimiter = ',')]
    stems: Vec<String>,
    /// Sample rate override in Hz. Defaults to the source's meta.sampleRate, else 48000.
    #[arg(long)]
    sample_rate: Option<u32>,
    /// Wall-clock budget for running the source, in seconds.
    #[arg(long, default_value_t = 20.0)]
    timeout: f64,
    /// Project root: imports may not resolve outside it. Default: unrestricted.
    #[arg(long)]
    root: Option<PathBuf>,
}

impl RenderArgs {
    fn options(&self) -> RenderOptions {
        RenderOptions {
            sample_rate: self.sample_rate,
            timeout: Duration::from_secs_f64(self.timeout),
            root: self.root.clone(),
            target: if self.stems.is_empty() { Target::Mix } else { Target::Stems(self.stems.clone()) },
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Bits {
    #[value(name = "16")]
    B16,
    #[value(name = "24")]
    B24,
    #[value(name = "32")]
    B32,
}

fn main() -> ExitCode {
    // Rust ignores SIGPIPE, so a reader that stops early -- `syrinx prelude | head` -- turned every
    // println! into a panic on EPIPE. The default disposition ends the process quietly instead, as
    // any Unix tool does; `play` likewise stops when the player it feeds goes away.
    #[cfg(unix)]
    // SAFETY: called first thing on the main thread, before any other thread exists or any
    // output is written; it only restores the signal's default disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Compile { inputs, output, format, bits, bitrate, jobs, bounce, mix_from, render, quiet } => {
            if let Some(dir) = &bounce {
                return bounce_stems(&inputs, dir, &render.options(), quiet);
            }
            let sources = collect_sources(&inputs)?;
            let encode = EncodeOptions {
                bitrate_kbps: bitrate,
                bits: match bits {
                    Bits::B16 => 16,
                    Bits::B24 => 24,
                    Bits::B32 => 32,
                },
            };
            let single_file = sources.len() == 1 && inputs.len() == 1 && inputs[0].is_file();
            let out_dir = match &output {
                Some(o) if single_file && !o.is_dir() => None,
                Some(o) => Some(o.clone()),
                None => None,
            };
            // Output path per source: an explicit file for a single input, else under the output
            // directory (mirroring the directory structure when an input was a directory), else
            // next to the source.
            let plan: Vec<(PathBuf, PathBuf)> = sources
                .iter()
                .map(|(src, rel)| {
                    let name = src.with_extension(&format).file_name().unwrap().to_owned();
                    let out = match (&output, &out_dir) {
                        (Some(o), None) => o.clone(),
                        (_, Some(dir)) => dir.join(rel.parent().unwrap_or(Path::new(""))).join(name),
                        (None, None) => src.with_file_name(name),
                    };
                    (src.clone(), out)
                })
                .collect();
            for (_, out) in &plan {
                syrinx_encode::codec_for(out, &encode).map_err(|e| anyhow!("{}: {e}", out.display()))?;
            }
            let opts = render.options();
            if let Some(dir) = &mix_from {
                let (src, out) = plan.first().cloned().ok_or_else(|| anyhow!("no source"))?;
                if plan.len() > 1 {
                    bail!("--mix-from takes exactly one source, got {}", plan.len());
                }
                let codec = syrinx_encode::codec_for(&out, &encode).map_err(|e| anyhow!("{e}"))?;
                return mix_from_bounce(&src, dir, &out, codec, &encode, &opts, quiet);
            }
            configure_pool(jobs)?;
            let started = Instant::now();
            let failures = Mutex::new(Vec::<String>::new());
            plan.par_iter().for_each(|(src, out)| {
                let result = (|| -> Result<()> {
                    let codec = syrinx_encode::codec_for(out, &encode).map_err(|e| anyhow!("{e}"))?;
                    let (rendered, took) = compile(src, &opts)?;
                    if let Some(dir) = out.parent() {
                        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
                    }
                    syrinx_encode::write(out, &rendered, codec, &encode)
                        .map_err(|e| anyhow!("{e}"))
                        .with_context(|| format!("writing {}", out.display()))?;
                    if !quiet {
                        report(&rendered, Some(took), out, Some(&syrinx_encode::describe(codec, &encode)));
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    eprintln!("error: {e:#}");
                    failures.lock().unwrap().push(src.display().to_string());
                }
            });
            let failures = failures.into_inner().unwrap();
            if plan.len() > 1 && !quiet {
                eprintln!(
                    "{} compiled, {} failed in {:.0} ms",
                    plan.len() - failures.len(),
                    failures.len(),
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            if !failures.is_empty() {
                bail!("{} of {} failed", failures.len(), plan.len());
            }
            Ok(())
        }
        Command::Hash { inputs, write, check, jobs, render } => {
            let sources = collect_sources(&inputs)?;
            let opts = render.options();
            configure_pool(jobs)?;
            /// A source's digests: the mix's (`None`) and each layer's, by name.
            type Digests = Vec<(Option<String>, String)>;
            let results: Vec<(String, Result<Digests>)> = sources
                .par_iter()
                .map(|(src, rel)| (rel.to_string_lossy().replace('\\', "/"), hashes_of(src, &opts)))
                .collect();
            let mut failed = 0;
            let mut hashes = BTreeMap::new();
            for (rel, result) in results {
                match result {
                    Ok(entries) => {
                        for (stem, h) in entries {
                            // A layer's own digest is what makes an approval durable: it says the
                            // beat is byte-for-byte what it was, and only the master moved.
                            let key = match stem {
                                Some(stem) => format!("{rel}#{stem}"),
                                None => rel.clone(),
                            };
                            hashes.insert(key, h);
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        eprintln!("error: {e:#}");
                    }
                }
            }
            if failed > 0 {
                bail!("{failed} of {} failed to render", sources.len());
            }
            let text: String = hashes.iter().map(|(p, h)| format!("{h}  {p}\n")).collect();
            if let Some(path) = &check {
                let stored = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
                let stored: BTreeMap<String, String> = stored
                    .lines()
                    .filter_map(|l| l.split_once("  ").map(|(h, p)| (p.trim().to_string(), h.trim().to_string())))
                    .collect();
                let mut differences = 0;
                for (p, h) in &hashes {
                    match stored.get(p) {
                        Some(s) if s == h => {}
                        Some(_) => {
                            differences += 1;
                            println!("changed  {p}");
                        }
                        None => {
                            differences += 1;
                            println!("new      {p}");
                        }
                    }
                }
                for p in stored.keys() {
                    if !hashes.contains_key(p) {
                        differences += 1;
                        println!("missing  {p}");
                    }
                }
                if differences > 0 {
                    bail!("{differences} difference(s) against {}", path.display());
                }
                println!("{} digest(s) match {}", hashes.len(), path.display());
            } else if let Some(path) = &write {
                std::fs::write(path, &text).with_context(|| format!("writing {}", path.display()))?;
                println!("wrote {} digest(s) to {}", hashes.len(), path.display());
            } else {
                print!("{text}");
            }
            Ok(())
        }
        Command::Play { input, repeat, player, render } => {
            #[cfg(not(windows))]
            if player.is_none() {
                return (0..repeat.max(1)).try_for_each(|_| stream_to_player(&input, &render.options()));
            }
            let (rendered, took) = compile(&input, &render.options())?;
            report(&rendered, Some(took), Path::new("(player)"), None);
            let tmp = std::env::temp_dir().join(format!("syrinx-{}-{}.wav", std::process::id(), rendered.meta.name));
            syrinx_core::wav::write(&tmp, &rendered, Format::Wav16).context("writing temp wav")?;
            let result = (0..repeat.max(1)).try_for_each(|_| play(&tmp, player.as_deref()));
            let _ = std::fs::remove_file(&tmp);
            result
        }
        Command::Check { inputs, root } => {
            if inputs.is_empty() {
                bail!("no inputs");
            }
            let opts = RenderOptions { root, ..RenderOptions::default() };
            let mut failed = 0;
            for input in &inputs {
                let source = std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
                match syrinx_core::inspect(&source, &input.to_string_lossy(), &opts) {
                    Ok(info) => {
                        let m = &info.meta;
                        println!(
                            "{}: ok  name={} duration={}s channels={} sampleRate={} seed={} loop={} stems={} mix={}",
                            input.display(),
                            m.name,
                            m.duration,
                            m.channels,
                            m.sample_rate.map_or("default".to_string(), |r| r.to_string()),
                            m.seed,
                            m.looping,
                            info.stems.len(),
                            if info.has_mix { "declared" } else { "sum" }
                        );
                        for stem in &info.stems {
                            println!("    stem {stem}");
                        }
                        for d in &info.dependencies {
                            println!("    imports {}", d.display());
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        println!("{}", describe(input, &e));
                    }
                }
            }
            if failed > 0 {
                bail!("{failed} of {} failed", inputs.len());
            }
            Ok(())
        }
        Command::Analyze { inputs, json, render } => {
            let sources = collect_any(&inputs)?;
            let opts = render.options();
            let mut failed = 0;
            for path in &sources {
                match load_signal(path, &opts) {
                    Ok(sig) => {
                        let f = syrinx_core::analyze::features(&sig.as_signal());
                        if json {
                            let mut v = serde_json::to_value(&f)?;
                            v["path"] = serde_json::Value::String(path.display().to_string());
                            println!("{}", serde_json::to_string(&v)?);
                        } else {
                            print_features(path, &f);
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        eprintln!("error: {e:#}");
                    }
                }
            }
            if failed > 0 {
                bail!("{failed} of {} failed", sources.len());
            }
            Ok(())
        }
        Command::Spectrogram { input, output, render } => {
            let output = output.unwrap_or_else(|| {
                let mut p = input.clone().into_os_string();
                p.push(".png");
                PathBuf::from(p)
            });
            let sig = load_signal(&input, &render.options())?;
            let img = syrinx_core::analyze::spectrogram(&sig.as_signal());
            let file = std::fs::File::create(&output).with_context(|| format!("creating {}", output.display()))?;
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.width as u32, img.height as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().context("png header")?;
            w.write_image_data(&img.rgb).context("png data")?;
            w.finish().context("png finish")?;
            eprintln!("{}  {}x{}  -> {}", input.display(), img.width, img.height, output.display());
            Ok(())
        }
        Command::Compare { candidate, reference, json, render } => {
            let opts = render.options();
            let c = syrinx_core::analyze::features(&load_signal(&candidate, &opts)?.as_signal());
            let r = syrinx_core::analyze::features(&load_signal(&reference, &opts)?.as_signal());
            let cmp = syrinx_core::analyze::compare(&c, &r);
            if json {
                println!("{}", serde_json::to_string(&cmp)?);
            } else {
                println!("{}  vs  {}", candidate.display(), reference.display());
                for a in &cmp.axes {
                    let mark = if a.skipped { "-" } else if a.ok { "ok" } else { "!!" };
                    println!("  {mark:<3} {:<21} {:>7} {:<7} {}", a.axis, a.value, a.unit, a.note);
                }
                println!("  score {:.2}  {}", cmp.score, cmp.summary);
            }
            if cmp.score < 0.7 {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Info => {
            println!("syrinx {}", env!("CARGO_PKG_VERSION"));
            println!("prelude version {}", syrinx_core::PRELUDE_VERSION);
            println!("api {} to {}", syrinx_core::API_FLOOR, syrinx_core::PRELUDE_VERSION);
            println!("block frames {}", syrinx_core::BLOCK_FRAMES);
            println!("V8 {}", syrinx_core::v8_version());
            Ok(())
        }
        Command::Prelude => {
            print!("{}", syrinx_core::PRELUDE);
            Ok(())
        }
        Command::Types => {
            print!("{}", syrinx_core::TYPES);
            Ok(())
        }
        Command::Framework { dir } => {
            use syrinx_core::framework::Vendored;
            let dir = dir.unwrap_or_else(|| PathBuf::from("framework"));
            let report = syrinx_core::framework::vendor(&dir, env!("CARGO_PKG_VERSION"))
                .with_context(|| format!("writing the framework into {}", dir.display()))?;
            let (mut written, mut updated) = (0, 0);
            for (path, state) in &report {
                match state {
                    Vendored::Written => {
                        written += 1;
                        println!("wrote    {}", dir.join(path).display());
                    }
                    Vendored::Updated => {
                        updated += 1;
                        println!("updated  {}", dir.join(path).display());
                    }
                    Vendored::Unchanged => {}
                }
            }
            println!(
                "syrinx-framework {} in {}: {written} written, {updated} updated, {} unchanged",
                env!("CARGO_PKG_VERSION"),
                dir.display(),
                report.len() - written - updated
            );
            Ok(())
        }
        Command::Docs { out } => {
            // Checks the declarations against the prelude's real exports on the way, so a
            // drifted declaration fails here rather than shipping wrong documentation.
            let docs = syrinx_core::docs::docs().map_err(|e| anyhow!("{e}"))?;
            let json = serde_json::to_string_pretty(&docs)? + "\n";
            match out {
                Some(path) => {
                    std::fs::write(&path, &json).with_context(|| format!("writing {}", path.display()))?;
                    eprintln!(
                        "{} entr(ies) in {} group(s) -> {}",
                        docs.groups.iter().map(|g| g.entries.len()).sum::<usize>(),
                        docs.groups.len(),
                        path.display()
                    );
                }
                None => print!("{json}"),
            }
            Ok(())
        }
    }
}

/// A loaded signal: a rendered source or a decoded wav.
struct Loaded {
    samples: Vec<f32>,
    channels: u32,
    sample_rate: u32,
}

impl Loaded {
    fn as_signal(&self) -> syrinx_core::analyze::Signal<'_> {
        syrinx_core::analyze::Signal { samples: &self.samples, channels: self.channels, sample_rate: self.sample_rate }
    }
}

/// Renders a .syr or decodes a .wav.
fn load_signal(path: &Path, opts: &RenderOptions) -> Result<Loaded> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "wav" {
        let mut reader = hound::WavReader::open(path).with_context(|| format!("reading {}", path.display()))?;
        let spec = reader.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader.samples::<f32>().collect::<std::result::Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let scale = ((1i64 << (spec.bits_per_sample - 1)) - 1) as f32;
                reader.samples::<i32>().map(|s| s.map(|v| v as f32 / scale)).collect::<std::result::Result<_, _>>()?
            }
        };
        return Ok(Loaded { samples, channels: spec.channels as u32, sample_rate: spec.sample_rate });
    }
    let (r, _) = compile(path, opts)?;
    Ok(Loaded { samples: r.samples, channels: r.channels, sample_rate: r.sample_rate })
}

/// Like `collect_sources` but keeps .wav files too and drops the relative paths.
fn collect_any(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for input in inputs {
        if input.is_dir() {
            for entry in walkdir::WalkDir::new(input).sort_by_file_name() {
                let entry = entry?;
                let ext = entry.path().extension().and_then(|e| e.to_str()).unwrap_or("");
                if entry.file_type().is_file() && (ext == "syr" || ext == "wav") {
                    out.push(entry.path().to_path_buf());
                }
            }
        } else if input.is_file() {
            out.push(input.clone());
        } else {
            bail!("{}: no such file or directory", input.display());
        }
    }
    Ok(out)
}

fn print_features(path: &Path, f: &syrinx_core::analyze::Features) {
    println!("{}", path.display());
    println!(
        "  {:.3}s  peak {}  rms {}  crest {} dB  attack {} ms  decay {} ms  zcr {}",
        f.duration,
        dbfs(f.peak as f32),
        dbfs(f.rms as f32),
        f.crest_db,
        f.attack_ms,
        f.decay_ms,
        f.zcr
    );
    println!(
        "  centroid {} Hz (start {} / mid {} / end {})  rolloff {} Hz  tilt {} dB/oct  flatness {}",
        f.centroid_hz, f.brightness.start_hz, f.brightness.mid_hz, f.brightness.end_hz, f.rolloff_hz, f.tilt_db_per_oct, f.flatness
    );
    match f.f0_hz {
        Some(hz) => println!("  pitch {hz} Hz (clarity {})", f.f0_clarity),
        None => println!("  pitch none"),
    }
    let bands: Vec<String> = f.bands.iter().map(|b| format!("{} {}", b.name, b.db)).collect();
    println!("  bands  {}", bands.join("  "));
    let partials: Vec<String> = f.partials.iter().map(|p| format!("{}Hz {}dB", p.hz, p.db)).collect();
    println!("  partials  {}", if partials.is_empty() { "none".into() } else { partials.join("  ") });
    let onsets: Vec<String> = f.onsets.iter().map(|o| format!("{o}")).collect();
    println!("  onsets  {}", onsets.join(" "));
    let env: String = f.envelope.iter().map(|v| " .:-=+*#%@".chars().nth(((v * 9.0).round() as usize).min(9)).unwrap()).collect();
    println!("  envelope  |{env}|");
}

/// Expands files and directories into (absolute source path, path relative to the input it came
/// from) pairs. Directories are searched recursively for `.syr`.
fn collect_sources(inputs: &[PathBuf]) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut out = Vec::new();
    for input in inputs {
        if input.is_dir() {
            let mut found = Vec::new();
            for entry in walkdir::WalkDir::new(input).sort_by_file_name() {
                let entry = entry.with_context(|| format!("scanning {}", input.display()))?;
                if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "syr") {
                    let rel = entry.path().strip_prefix(input).unwrap().to_path_buf();
                    found.push((entry.path().to_path_buf(), rel));
                }
            }
            if found.is_empty() {
                bail!("no .syr files under {}", input.display());
            }
            out.extend(found);
        } else if input.is_file() {
            let rel = PathBuf::from(input.file_name().unwrap());
            out.push((input.clone(), rel));
        } else {
            bail!("{}: no such file or directory", input.display());
        }
    }
    Ok(out)
}

fn configure_pool(jobs: Option<usize>) -> Result<()> {
    if let Some(n) = jobs {
        rayon::ThreadPoolBuilder::new().num_threads(n.max(1)).build_global().context("configuring the thread pool")?;
    }
    Ok(())
}

/// BLAKE3 over the rendered signal: rate, channels, frames and every sample as little-endian
/// f32. Two renders hash equal iff they would produce identical PCM.
fn hash_of(r: &Rendered) -> String {
    let mut h = blake3::Hasher::new();
    h.update(&r.sample_rate.to_le_bytes());
    h.update(&r.channels.to_le_bytes());
    h.update(&(r.frames as u64).to_le_bytes());
    let mut bytes = Vec::with_capacity(r.samples.len() * 4);
    for s in &r.samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    h.update(&bytes);
    h.finalize().to_hex().to_string()
}

/// The one source a single-source flag was given, or a message naming the flag.
fn single_source(inputs: &[PathBuf], flag: &str) -> Result<PathBuf> {
    let sources = collect_sources(inputs)?;
    match sources.len() {
        1 => Ok(sources[0].0.clone()),
        n => bail!("{flag} takes exactly one source, got {n}"),
    }
}

/// BLAKE3 over the source and everything it imports, in load order. What a bounce records so it
/// can say the source has moved since -- not which layer moved, which nothing can know: a layer
/// depends on the module's shared constants as much as on its own body.
fn source_hash(source: &str, dependencies: &[String]) -> Result<String> {
    let mut h = blake3::Hasher::new();
    h.update(source.as_bytes());
    for dep in dependencies {
        h.update(&std::fs::read(dep).with_context(|| format!("reading {dep}"))?);
    }
    Ok(h.finalize().to_hex().to_string())
}

/// Writes every layer to its own lossless WAV, with a manifest describing the bounce.
fn bounce_stems(inputs: &[PathBuf], dir: &Path, opts: &RenderOptions, quiet: bool) -> Result<()> {
    let input = single_source(inputs, "--bounce")?;
    let source = std::fs::read_to_string(&input).with_context(|| format!("reading {}", input.display()))?;
    let started = Instant::now();
    let stems = syrinx_core::render_each(&source, &input.to_string_lossy(), opts, &[])
        .map_err(|e| anyhow!("{}", describe(&input, &e)))?;
    let elapsed = started.elapsed();

    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for r in &stems {
        let stem = r.stem.clone().unwrap_or_default();
        let out = dir.join(format!("{stem}.wav"));
        syrinx_core::wav::write(&out, r, Format::WavFloat).with_context(|| format!("writing {}", out.display()))?;
        if !quiet {
            report(r, None, &out, Some("32-bit float wav"));
        }
    }

    let first = stems.first().ok_or_else(|| anyhow!("no layers"))?;
    let dependencies: Vec<String> = first.dependencies.iter().map(|p| p.display().to_string()).collect();
    let manifest = serde_json::json!({
        "source": input.display().to_string(),
        "sourceHash": source_hash(&source, &dependencies)?,
        "sampleRate": first.sample_rate,
        "channels": first.channels,
        "frames": first.frames,
        "stems": stems.iter().map(|r| r.stem.clone().unwrap_or_default()).collect::<Vec<_>>(),
    });
    let path = dir.join("bounce.json");
    std::fs::write(&path, serde_json::to_string_pretty(&manifest)? + "\n")
        .with_context(|| format!("writing {}", path.display()))?;
    if !quiet {
        eprintln!(
            "{} layer(s) bounced in {:.0} ms -> {}",
            stems.len(),
            elapsed.as_secs_f64() * 1000.0,
            dir.display()
        );
    }
    Ok(())
}

/// Runs the mix stage over a bounce directory.
fn mix_from_bounce(
    input: &Path,
    dir: &Path,
    out: &Path,
    codec: syrinx_encode::Codec,
    encode: &EncodeOptions,
    opts: &RenderOptions,
    quiet: bool,
) -> Result<()> {
    let source = std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let manifest_path = dir.join("bounce.json");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path).with_context(|| format!("reading {}", manifest_path.display()))?,
    )
    .with_context(|| format!("parsing {}", manifest_path.display()))?;

    let names: Vec<String> = manifest["stems"]
        .as_array()
        .ok_or_else(|| anyhow!("{}: no `stems` array", manifest_path.display()))?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();

    let mut supplied = Vec::with_capacity(names.len());
    for stem in &names {
        let path = dir.join(format!("{stem}.wav"));
        let loaded = load_signal(&path, opts).with_context(|| format!("reading layer \"{stem}\""))?;
        supplied.push((stem.clone(), loaded.samples));
    }

    let started = Instant::now();
    let mixed = syrinx_core::mix_from(&source, &input.to_string_lossy(), opts, &supplied)
        .map_err(|e| anyhow!("{}", describe(input, &e)))?;

    // The bounce can only report that the source moved, never which layer did: nothing can key a
    // layer on its own text when it also depends on the module's shared constants.
    let recorded = manifest["sourceHash"].as_str().unwrap_or_default();
    let dependencies: Vec<String> = mixed.dependencies.iter().map(|p| p.display().to_string()).collect();
    if recorded != source_hash(&source, &dependencies)? {
        eprintln!(
            "warning: {} has changed since these layers were bounced; re-bounce to mix what it says now",
            input.display()
        );
    }

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    syrinx_encode::write(out, &mixed, codec, encode)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("writing {}", out.display()))?;
    if !quiet {
        report(&mixed, Some(started.elapsed()), out, Some(&syrinx_encode::describe(codec, encode)));
    }
    Ok(())
}

fn compile(input: &Path, opts: &RenderOptions) -> Result<(Rendered, Duration)> {
    let source = std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let started = Instant::now();
    let rendered = syrinx_core::render(&source, &input.to_string_lossy(), opts).map_err(|e| anyhow!("{}", describe(input, &e)))?;
    Ok((rendered, started.elapsed()))
}

/// The mix's digest and every layer's, rendering each layer exactly once. When layers were
/// selected with --stem, only those are hashed and there is no mix.
fn hashes_of(input: &Path, opts: &RenderOptions) -> Result<Vec<(Option<String>, String)>> {
    let source = std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let name = input.to_string_lossy();
    let selected: Vec<String> = match &opts.target {
        Target::Mix => Vec::new(),
        Target::Stems(names) => names.clone(),
    };
    let stems = syrinx_core::render_each(&source, &name, opts, &selected).map_err(|e| anyhow!("{}", describe(input, &e)))?;
    let mut out = Vec::with_capacity(stems.len() + 1);
    if selected.is_empty() {
        let supplied: Vec<(String, Vec<f32>)> =
            stems.iter().map(|r| (r.stem.clone().unwrap_or_default(), r.samples.clone())).collect();
        let mixed = syrinx_core::mix_from(&source, &name, opts, &supplied).map_err(|e| anyhow!("{}", describe(input, &e)))?;
        out.push((None, hash_of(&mixed)));
    }
    for r in &stems {
        out.push((r.stem.clone(), hash_of(r)));
    }
    Ok(out)
}

fn describe(input: &Path, e: &Error) -> String {
    let kind = match e.kind {
        ErrorKind::Check => "determinism check failed",
        ErrorKind::Compile => "syntax error",
        ErrorKind::Runtime => "runtime error",
        ErrorKind::Timeout => "timed out",
        ErrorKind::Contract => "contract error",
    };
    let file = e.file.clone().unwrap_or_else(|| input.display().to_string());
    if e.diagnostics.len() > 1 {
        let lines: Vec<String> = e
            .diagnostics
            .iter()
            .map(|d| format!("{file}:{}:{}: {}", d.line, d.column, d.message))
            .collect();
        return format!("{kind}\n{}", lines.join("\n"));
    }
    match (e.line, e.column) {
        (Some(l), Some(c)) => format!("{file}:{l}:{c}: {kind}: {}", e.message),
        (Some(l), None) => format!("{file}:{l}: {kind}: {}", e.message),
        _ => format!("{file}: {kind}: {}", e.message),
    }
}

fn dbfs(x: f32) -> String {
    if x <= 0.0 { "-inf dBFS".into() } else { format!("{:.1} dBFS", 20.0 * x.log10()) }
}

fn report(r: &Rendered, took: Option<Duration>, output: &Path, encoded_as: Option<&str>) {
    let peak = r.peak();
    let clip = if peak > 1.0 { "  CLIPPING" } else { "" };
    let encoded_as = encoded_as.map_or(String::new(), |e| format!(" ({e})"));
    // A layer is named for the sound it belongs to and the layer it is, so a bounce directory's
    // worth of lines cannot be mistaken for several sounds.
    let label = match &r.stem {
        Some(stem) => format!("{}#{stem}", r.meta.name),
        None => r.meta.name.clone(),
    };
    let timing = match took {
        Some(t) => format!("  rendered in {:.0} ms", t.as_secs_f64() * 1000.0),
        None => String::new(),
    };
    eprintln!(
        "{}  {:.3}s  {} Hz  {}ch  peak {}  rms {}{timing}  -> {}{encoded_as}{}",
        label,
        r.frames as f64 / r.sample_rate as f64,
        r.sample_rate,
        r.channels,
        dbfs(peak),
        dbfs(r.rms()),
        output.display(),
        clip
    );
}

/// Opens the source as a stream and writes its blocks to the first system player on PATH that
/// takes raw float on stdin, so a streaming source is heard as soon as its first block exists.
/// The geometry line is printed at open; peak and RMS follow at the end, when they are known.
#[cfg(not(windows))]
fn stream_to_player(input: &Path, opts: &RenderOptions) -> Result<()> {
    use std::io::Write;
    use std::process::{Command as Proc, Stdio};

    let source = std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let started = Instant::now();
    let mut stream = Stream::open(&source, &input.to_string_lossy(), opts).map_err(|e| anyhow!("{}", describe(input, &e)))?;
    let (rate, channels, frames) = (stream.source().sample_rate(), stream.source().channels(), stream.source().frames());
    eprintln!(
        "{}  {:.3}s  {} Hz  {}ch  {} in {:.0} ms  -> (player)",
        stream.source().meta().name,
        frames as f64 / rate as f64,
        rate,
        channels,
        if stream.streaming() { "streaming, opened" } else { "rendered" },
        started.elapsed().as_secs_f64() * 1000.0
    );

    // Raw little-endian float on stdin, in each player's own words.
    let (rate_s, ch_s) = (rate.to_string(), channels.to_string());
    let candidates: [&[&str]; 4] = [
        &["pw-play", "--raw", "--format=f32", &format!("--rate={rate_s}"), &format!("--channels={ch_s}"), "-"],
        &["paplay", "--raw", "--format=float32le", &format!("--rate={rate_s}"), &format!("--channels={ch_s}")],
        &["aplay", "-q", "-t", "raw", "-f", "FLOAT_LE", "-r", &rate_s, "-c", &ch_s, "-"],
        &["ffplay", "-nodisp", "-autoexit", "-loglevel", "error", "-f", "f32le", "-ar", &rate_s, "-ac", &ch_s, "-i", "-"],
    ];
    let mut child = None;
    for argv in candidates {
        match Proc::new(argv[0]).args(&argv[1..]).stdin(Stdio::piped()).spawn() {
            Ok(c) => {
                child = Some((argv[0], c));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("running {}", argv[0])),
        }
    }
    let Some((name, mut child)) = child else {
        bail!("no audio player found on PATH (tried pw-play, paplay, aplay, ffplay); pass --player");
    };

    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut peak = 0.0f32;
    let mut sum_squares = 0.0f64;
    let mut count = 0usize;
    let mut bytes = Vec::with_capacity(syrinx_core::BLOCK_FRAMES * channels as usize * 4);
    let outcome = (|| -> Result<()> {
        while let Some(block) = stream.next_block().map_err(|e| anyhow!("{}", describe(input, &e)))? {
            bytes.clear();
            for s in &block.samples {
                peak = peak.max(s.abs());
                sum_squares += (*s as f64) * (*s as f64);
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            count += block.samples.len();
            stdin.write_all(&bytes).with_context(|| format!("writing to {name}"))?;
        }
        Ok(())
    })();
    drop(stdin);
    let status = child.wait().with_context(|| format!("waiting for {name}"))?;
    outcome?;
    if !status.success() {
        bail!("{name} exited with {status}");
    }
    let rms = if count == 0 { 0.0 } else { (sum_squares / count as f64).sqrt() as f32 };
    eprintln!("peak {}  rms {}{}", dbfs(peak), dbfs(rms), if peak > 1.0 { "  CLIPPING" } else { "" });
    Ok(())
}

#[cfg(not(windows))]
fn play(wav: &Path, player: Option<&str>) -> Result<()> {
    use std::process::Command as Proc;

    let candidates: Vec<Vec<String>> = match player {
        Some(p) => vec![p.split_whitespace().map(String::from).collect()],
        None => vec![
            vec!["pw-play".into()],
            vec!["paplay".into()],
            vec!["aplay".into(), "-q".into()],
            vec!["ffplay".into(), "-nodisp".into(), "-autoexit".into(), "-loglevel".into(), "error".into()],
        ],
    };
    for argv in &candidates {
        let (cmd, args) = argv.split_first().ok_or_else(|| anyhow!("empty player command"))?;
        match Proc::new(cmd).args(args).arg(wav).status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => bail!("{cmd} exited with {status}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && player.is_none() => continue,
            Err(e) => return Err(e).with_context(|| format!("running {cmd}")),
        }
    }
    bail!("no audio player found on PATH (tried pw-play, paplay, aplay, ffplay); pass --player")
}

#[cfg(windows)]
fn play(wav: &Path, player: Option<&str>) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_FILENAME, SND_NODEFAULT, SND_SYNC};

    if let Some(p) = player {
        let mut parts = p.split_whitespace();
        let cmd = parts.next().ok_or_else(|| anyhow!("empty player command"))?;
        let status = std::process::Command::new(cmd).args(parts).arg(wav).status().with_context(|| format!("running {cmd}"))?;
        if !status.success() {
            bail!("{cmd} exited with {status}");
        }
        return Ok(());
    }
    let wide: Vec<u16> = wav.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the synchronous call.
    let ok = unsafe { PlaySoundW(wide.as_ptr(), std::ptr::null_mut(), SND_FILENAME | SND_SYNC | SND_NODEFAULT) };
    if ok == 0 {
        bail!("PlaySound failed");
    }
    Ok(())
}
