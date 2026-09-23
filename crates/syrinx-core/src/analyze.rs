//! Measurements of a rendered sound: the author's substitute for ears.
//!
//! [`features`] reduces a signal to numbers an agent can reason about (attack, decay, brightness,
//! tonal/noisy balance, band energies, onsets...). [`compare`] scores a candidate against a
//! reference axis by axis with a plain-language note per axis. [`spectrogram`] draws a
//! log-frequency STFT. All of it is deterministic and dependency-free (own radix-2 FFT).
//!
//! ANALYSIS_VERSION 4 of the analyzer, kept so the numbers stay commensurable with
//! that body of experience.

use serde::Serialize;

pub const ANALYSIS_VERSION: u32 = 4;

const FFT_SIZE: usize = 1024;
const HOP: usize = 256;
const BINS: usize = FFT_SIZE / 2;
const ENVELOPE_POINTS: usize = 32;
/// Minimum autocorrelation peak for a pitch to count.
const PITCH_CLARITY: f64 = 0.5;

/// A signal to analyse: interleaved samples.
pub struct Signal<'a> {
    pub samples: &'a [f32],
    pub channels: u32,
    pub sample_rate: u32,
}

impl<'a> From<&'a crate::Rendered> for Signal<'a> {
    fn from(r: &'a crate::Rendered) -> Self {
        Signal { samples: &r.samples, channels: r.channels, sample_rate: r.sample_rate }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Partial {
    pub hz: u32,
    /// dB relative to the loudest partial.
    pub db: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Band {
    pub name: &'static str,
    pub lo_hz: u32,
    pub hi_hz: u32,
    /// dB relative to the loudest band (0 = loudest, floored at -60).
    pub db: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Brightness {
    pub start_hz: f64,
    pub mid_hz: f64,
    pub end_hz: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Features {
    pub version: u32,
    /// Seconds.
    pub duration: f64,
    /// Absolute peak, 0..1+.
    pub peak: f64,
    pub rms: f64,
    /// peak/rms in dB: punchiness.
    pub crest_db: f64,
    /// Time to reach 90% of peak.
    pub attack_ms: f64,
    /// Time from the peak level to fall 20 dB: tail length.
    pub decay_ms: f64,
    /// Zero-crossing rate 0..1: a noisiness/brightness proxy.
    pub zcr: f64,
    /// Transient times in seconds.
    pub onsets: Vec<f64>,
    /// Spectral centroid: overall brightness.
    pub centroid_hz: f64,
    /// Spectral slope in dB/octave: bright (less negative) .. dark (steeply negative).
    pub tilt_db_per_oct: f64,
    pub brightness: Brightness,
    /// 85%-energy frequency.
    pub rolloff_hz: f64,
    /// 0 tonal .. 1 noisy.
    pub flatness: f64,
    /// Estimated pitch, when the sound is tonal enough to have one.
    pub f0_hz: Option<u32>,
    /// 0..1 confidence in f0.
    pub f0_clarity: f64,
    /// Dominant spectral peaks.
    pub partials: Vec<Partial>,
    /// 32-point normalised loudness contour.
    pub envelope: Vec<f64>,
    /// Six-band energy profile.
    pub bands: Vec<Band>,
}

const BAND_EDGES: [(&str, f64, f64); 6] = [
    ("sub", 0.0, 60.0),
    ("low", 60.0, 250.0),
    ("lowmid", 250.0, 1000.0),
    ("highmid", 1000.0, 4000.0),
    ("high", 4000.0, 10000.0),
    ("air", 10000.0, f64::INFINITY),
];

fn round(x: f64, digits: i32) -> f64 {
    let m = 10f64.powi(digits);
    (x * m).round() / m
}

/// In-place radix-2 complex FFT. `re.len()` must be a power of two.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut i = 0;
        while i < n {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (ar, ai) = (re[i + k], im[i + k]);
                let (br, bi) = (re[i + k + len / 2], im[i + k + len / 2]);
                let (tr, ti) = (br * cr - bi * ci, br * ci + bi * cr);
                re[i + k] = ar + tr;
                im[i + k] = ai + ti;
                re[i + k + len / 2] = ar - tr;
                im[i + k + len / 2] = ai - ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
            i += len;
        }
        len <<= 1;
    }
}

fn mono_of(s: &Signal) -> Vec<f64> {
    let ch = s.channels.max(1) as usize;
    let n = s.samples.len() / ch;
    let mut mono = vec![0.0f64; n.max(FFT_SIZE)];
    for (i, frame) in s.samples.chunks_exact(ch).take(n).enumerate() {
        mono[i] = frame.iter().map(|v| *v as f64).sum::<f64>() / ch as f64;
    }
    mono
}

/// STFT magnitudes: (frames, averaged magnitude per bin, per-frame centroid, per-frame energy,
/// per-frame dB magnitudes for drawing).
struct Stft {
    frames: usize,
    avg_mag: Vec<f64>,
    centroid: Vec<f64>,
    energy: Vec<f64>,
    db: Vec<f64>,
}

fn stft(mono: &[f64], n: usize, sr: f64, keep_db: bool) -> Stft {
    let frames = ((n.max(FFT_SIZE) - FFT_SIZE) / HOP + 1).max(1);
    let mut re = vec![0.0; FFT_SIZE];
    let mut im = vec![0.0; FFT_SIZE];
    let mut avg_mag = vec![0.0; BINS];
    let mut centroid = vec![0.0; frames];
    let mut energy = vec![0.0; frames];
    let mut db = if keep_db { vec![0.0; frames * BINS] } else { Vec::new() };
    let window: Vec<f64> = (0..FFT_SIZE)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (FFT_SIZE - 1) as f64).cos())
        .collect();
    for fr in 0..frames {
        let off = fr * HOP;
        for i in 0..FFT_SIZE {
            re[i] = mono.get(off + i).copied().unwrap_or(0.0) * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        let (mut cs, mut ms, mut e) = (0.0, 0.0, 0.0);
        for b in 0..BINS {
            let mag = re[b].hypot(im[b]);
            avg_mag[b] += mag;
            cs += (b as f64 * sr / FFT_SIZE as f64) * mag;
            ms += mag;
            e += mag * mag;
            if keep_db {
                db[fr * BINS + b] = 20.0 * (mag + 1e-9).log10();
            }
        }
        centroid[fr] = if ms > 1e-9 { cs / ms } else { 0.0 };
        energy[fr] = e;
    }
    for m in &mut avg_mag {
        *m /= frames as f64;
    }
    Stft { frames, avg_mag, centroid, energy, db }
}

fn decay_time(mono: &[f64], n: usize, sr: f64) -> f64 {
    let hop = ((sr * 0.001).floor() as usize).max(1);
    let win = hop * 4;
    let mut levels = Vec::new();
    let (mut peak_lvl, mut peak_idx) = (0.0, 0);
    let mut i = 0;
    while i + win <= n {
        let s: f64 = mono[i..i + win].iter().map(|x| x * x).sum();
        let lvl = (s / win as f64).sqrt();
        if lvl > peak_lvl {
            peak_lvl = lvl;
            peak_idx = levels.len();
        }
        levels.push(lvl);
        i += hop;
    }
    if peak_lvl < 1e-9 || levels.len() < 2 {
        return 0.0;
    }
    let decay_idx = (peak_idx..levels.len()).find(|&k| levels[k] <= 0.1 * peak_lvl).unwrap_or(levels.len() - 1);
    ((decay_idx - peak_idx) * hop) as f64 / sr * 1000.0
}

fn spectral_tilt(avg_mag: &[f64], max_mag: f64, sr: f64) -> f64 {
    let (mut sx, mut sy, mut sxy, mut sxx, mut cnt) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (b, mag) in avg_mag.iter().enumerate().take(BINS).skip(1) {
        let f = b as f64 * sr / FFT_SIZE as f64;
        if !(50.0..=16000.0).contains(&f) {
            continue;
        }
        let x = f.log2();
        let y = (20.0 * ((mag + 1e-9) / max_mag).log10()).max(-80.0);
        sx += x;
        sy += y;
        sxy += x * y;
        sxx += x * x;
        cnt += 1.0;
    }
    let denom = cnt * sxx - sx * sx;
    if denom.abs() < 1e-9 { 0.0 } else { (cnt * sxy - sx * sy) / denom }
}

/// Onsets via spectral-energy flux with an adaptive local-mean threshold.
fn detect_onsets(energy: &[f64], sr: f64) -> Vec<f64> {
    let frames = energy.len();
    if frames < 2 {
        return vec![0.0];
    }
    let mut flux = vec![0.0; frames];
    let mut max_flux: f64 = 0.0;
    for f in 0..frames {
        let d = if f == 0 { energy[0] } else { energy[f] - energy[f - 1] };
        flux[f] = d.max(0.0);
        max_flux = max_flux.max(flux[f]);
    }
    if max_flux < 1e-12 {
        return vec![0.0];
    }
    let floor = max_flux * 0.04;
    let w = ((0.05 * sr / HOP as f64).floor() as usize).max(2);
    let min_gap = ((0.025 * sr / HOP as f64).floor() as isize).max(1);
    let mut onsets = Vec::new();
    let mut last: isize = -min_gap - 1;
    for f in 0..frames {
        let lo = f.saturating_sub(w);
        let hi = (f + w).min(frames - 1);
        let mean = flux[lo..=hi].iter().sum::<f64>() / (hi - lo + 1) as f64;
        let thr = floor.max(mean * 2.5);
        let prev = if f == 0 { 0.0 } else { flux[f - 1] };
        let peak = flux[f] > thr && flux[f] >= prev && (f == frames - 1 || flux[f] > flux[f + 1]);
        if peak && f as isize - last >= min_gap {
            onsets.push(round(f as f64 * HOP as f64 / sr, 3));
            last = f as isize;
        }
    }
    if onsets.is_empty() { vec![0.0] } else { onsets }
}

/// The loudest ~100 ms window, for pitch estimation.
fn loudest_window(mono: &[f64], n: usize, sr: f64) -> Option<Vec<f64>> {
    let win = ((sr * 0.1) as usize).clamp(256, n.max(256));
    if n < win {
        return if n > 0 { Some(mono[..n].to_vec()) } else { None };
    }
    let hop = (win / 4).max(1);
    let (mut best, mut best_at) = (-1.0, 0);
    let mut i = 0;
    while i + win <= n {
        let e: f64 = mono[i..i + win].iter().map(|x| x * x).sum();
        if e > best {
            best = e;
            best_at = i;
        }
        i += hop;
    }
    Some(mono[best_at..best_at + win].to_vec())
}

fn pitch_autocorr(x: &[f64], sr: f64) -> Option<(f64, f64)> {
    let n = x.len();
    let min_lag = (sr / 2000.0).floor() as usize;
    let max_lag = ((sr / 40.0).floor() as usize).min(n >> 1);
    if max_lag <= min_lag + 2 {
        return None;
    }
    let r0: f64 = x.iter().map(|v| v * v).sum();
    if r0 < 1e-9 {
        return None;
    }
    let mut r = vec![0.0; max_lag + 1];
    for (lag, slot) in r.iter_mut().enumerate().take(max_lag + 1).skip(min_lag) {
        let s: f64 = (0..n - lag).map(|i| x[i] * x[i + lag]).sum();
        *slot = s / r0;
    }
    let (mut peak_lag, mut clarity) = (None, 0.0);
    for lag in min_lag + 1..max_lag {
        if r[lag] >= r[lag - 1] && r[lag] >= r[lag + 1] && r[lag] > clarity {
            clarity = r[lag];
            peak_lag = Some(lag);
        }
    }
    let peak_lag = peak_lag?;
    if clarity < PITCH_CLARITY {
        return None;
    }
    let mut best = peak_lag;
    for lag in min_lag + 1..peak_lag {
        if r[lag] >= r[lag - 1] && r[lag] >= r[lag + 1] && r[lag] >= 0.85 * clarity {
            best = lag;
            break;
        }
    }
    let clarity = round(clarity, 2);
    if best > min_lag && best < max_lag {
        let (a, b, c) = (r[best - 1], r[best], r[best + 1]);
        let denom = a - 2.0 * b + c;
        if denom.abs() > 1e-12 {
            let shift = 0.5 * (a - c) / denom;
            if shift.abs() < 1.0 {
                return Some((sr / (best as f64 + shift), clarity));
            }
        }
    }
    Some((sr / best as f64, clarity))
}

/// Measures a signal.
pub fn features(s: &Signal) -> Features {
    let sr = s.sample_rate as f64;
    let ch = s.channels.max(1) as usize;
    let n = s.samples.len() / ch;
    let mono = mono_of(s);

    let mut peak: f64 = 0.0;
    let mut sum_sq = 0.0;
    for &v in &mono[..n] {
        peak = peak.max(v.abs());
        sum_sq += v * v;
    }
    let rms = (sum_sq / n.max(1) as f64).sqrt();
    let crest_db = if rms > 1e-9 { 20.0 * (peak / rms).log10() } else { 0.0 };
    let zc = (1..n).filter(|&i| (mono[i - 1] < 0.0) != (mono[i] < 0.0)).count();
    let zcr = if n > 1 { zc as f64 / (n - 1) as f64 } else { 0.0 };

    let step = (n / ENVELOPE_POINTS).max(1);
    let mut env = vec![0.0; ENVELOPE_POINTS];
    for (k, slot) in env.iter_mut().enumerate() {
        let a = k * step;
        let b = (a + step).min(n);
        let sum: f64 = if a < b { mono[a..b].iter().map(|x| x * x).sum() } else { 0.0 };
        *slot = (sum / (b.saturating_sub(a)).max(1) as f64).sqrt();
    }
    let env_max = env.iter().cloned().fold(1e-9, f64::max);
    for v in &mut env {
        *v = round(*v / env_max, 3);
    }
    let thr = 0.9 * peak;
    let attack_samples = (0..n).find(|&i| mono[i].abs() >= thr).unwrap_or(0);

    let st = stft(&mono, n, sr, false);
    let (mut cw, mut ce) = (0.0, 0.0);
    for fr in 0..st.frames {
        cw += st.centroid[fr] * st.energy[fr];
        ce += st.energy[fr];
    }
    let centroid_hz = if ce > 1e-9 { cw / ce } else { 0.0 };

    let total: f64 = st.avg_mag.iter().sum();
    let mut acc = 0.0;
    let mut roll_bin = BINS - 1;
    for b in 0..BINS {
        acc += st.avg_mag[b];
        if acc >= 0.85 * total {
            roll_bin = b;
            break;
        }
    }

    let (mut log_sum, mut arith) = (0.0, 0.0);
    for b in 1..BINS {
        let m = st.avg_mag[b] + 1e-12;
        log_sum += m.ln();
        arith += m;
    }
    let geo = (log_sum / (BINS - 1) as f64).exp();
    let ar = arith / (BINS - 1) as f64;
    let flatness = if ar > 1e-12 { (geo / ar).min(1.0) } else { 0.0 };

    let max_mag = st.avg_mag.iter().cloned().fold(1e-9, f64::max);
    let mut found: Vec<(usize, f64)> = (2..BINS - 1)
        .filter(|&b| {
            st.avg_mag[b] > st.avg_mag[b - 1] && st.avg_mag[b] >= st.avg_mag[b + 1] && st.avg_mag[b] > max_mag * 0.05
        })
        .map(|b| (b, st.avg_mag[b]))
        .collect();
    found.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let partials: Vec<Partial> = found
        .iter()
        .take(6)
        .map(|&(bin, mag)| Partial {
            hz: (bin as f64 * sr / FFT_SIZE as f64).round() as u32,
            db: round(20.0 * (mag / max_mag).log10(), 1),
        })
        .collect();

    let pitch = if flatness < 0.4 { loudest_window(&mono, n, sr).and_then(|w| pitch_autocorr(&w, sr)) } else { None };
    let onsets = detect_onsets(&st.energy, sr);
    let decay_ms = decay_time(&mono, n, sr);
    let tilt = spectral_tilt(&st.avg_mag, max_mag, sr);

    let mut band_energy = [0.0f64; 6];
    for b in 1..BINS {
        let f = b as f64 * sr / FFT_SIZE as f64;
        if let Some(i) = BAND_EDGES.iter().position(|&(_, lo, hi)| f >= lo && f < hi) {
            band_energy[i] += st.avg_mag[b] * st.avg_mag[b];
        }
    }
    let max_band = band_energy.iter().cloned().fold(1e-18, f64::max);
    let bands: Vec<Band> = BAND_EDGES
        .iter()
        .enumerate()
        .map(|(i, &(name, lo, hi))| Band {
            name,
            lo_hz: lo as u32,
            hi_hz: if hi.is_finite() { hi as u32 } else { (sr / 2.0).round() as u32 },
            db: round((10.0 * ((band_energy[i] + 1e-18) / max_band).log10()).max(-60.0), 1),
        })
        .collect();

    Features {
        version: ANALYSIS_VERSION,
        duration: round(n as f64 / sr, 3),
        peak: round(peak, 3),
        rms: round(rms, 3),
        crest_db: round(crest_db, 1),
        attack_ms: round(attack_samples as f64 / sr * 1000.0, 1),
        decay_ms: round(decay_ms, 1),
        zcr: round(zcr, 3),
        onsets,
        centroid_hz: round(centroid_hz, 0),
        tilt_db_per_oct: round(tilt, 1),
        brightness: Brightness {
            start_hz: round(st.centroid[0], 0),
            mid_hz: round(st.centroid[st.frames / 2], 0),
            end_hz: round(st.centroid[st.frames - 1], 0),
        },
        rolloff_hz: round(roll_bin as f64 * sr / FFT_SIZE as f64, 0),
        flatness: round(flatness, 3),
        f0_hz: pitch.map(|(hz, _)| hz.round() as u32),
        f0_clarity: pitch.map_or(0.0, |(_, c)| c),
        partials,
        envelope: env,
        bands,
    }
}

// ---------------------------------------------------------------------------------------------
// Compare

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Axis {
    pub axis: &'static str,
    pub value: f64,
    pub unit: &'static str,
    pub ok: bool,
    pub note: String,
    /// The reference was degenerate for this axis; it is reported but not scored.
    pub skipped: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Comparison {
    pub axes: Vec<Axis>,
    /// Fraction of scored axes within tolerance, 0..1.
    pub score: f64,
    pub summary: String,
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let ma = a[..n].iter().sum::<f64>() / n as f64;
    let mb = b[..n].iter().sum::<f64>() / n as f64;
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (da, db) = (a[i] - ma, b[i] - mb);
        cov += da * db;
        va += da * da;
        vb += db * db;
    }
    if va < 1e-12 || vb < 1e-12 { 0.0 } else { cov / (va * vb).sqrt() }
}

fn octaves(candidate: f64, reference: f64, floor: f64) -> f64 {
    (candidate.max(floor) / reference.max(floor)).log2()
}

/// Scores `candidate` against `reference`, axis by axis.
pub fn compare(candidate: &Features, reference: &Features) -> Comparison {
    let r1 = |x: f64| round(x, 1);
    let r2 = |x: f64| round(x, 2);
    let mut axes = Vec::new();
    let axis = |axis, value, unit, ok, note: String| Axis { axis, value, unit, ok, note, skipped: false };
    let skipped = |axis, unit| Axis {
        axis,
        value: 0.0,
        unit,
        ok: true,
        note: "reference degenerate; skipped".into(),
        skipped: true,
    };

    let corr = pearson(&candidate.envelope, &reference.envelope);
    axes.push(axis(
        "envelope",
        r2(corr),
        "corr",
        corr >= 0.75,
        if corr >= 0.75 {
            "envelope shape matches".into()
        } else {
            format!("envelope shape diverges (corr {})", r2(corr))
        },
    ));

    if reference.centroid_hz < 20.0 {
        axes.push(skipped("centroid", "oct"));
    } else {
        let oct = octaves(candidate.centroid_hz, reference.centroid_hz, 20.0);
        let ok = oct.abs() <= 0.5;
        axes.push(axis(
            "centroid",
            r2(oct),
            "oct",
            ok,
            if ok {
                "brightness matches".into()
            } else {
                format!("centroid {} oct too {}", r2(oct.abs()), if oct > 0.0 { "bright" } else { "dark" })
            },
        ));
    }

    {
        let pairs = [
            (candidate.brightness.start_hz, reference.brightness.start_hz),
            (candidate.brightness.mid_hz, reference.brightness.mid_hz),
            (candidate.brightness.end_hz, reference.brightness.end_hz),
        ];
        let valid: Vec<_> = pairs.iter().filter(|(_, r)| *r >= 20.0).collect();
        if valid.is_empty() {
            axes.push(skipped("brightnessTrajectory", "oct"));
        } else {
            let mean = valid.iter().map(|(c, r)| octaves(*c, *r, 20.0).abs()).sum::<f64>() / valid.len() as f64;
            let ok = mean <= 0.6;
            axes.push(axis(
                "brightnessTrajectory",
                r2(mean),
                "oct",
                ok,
                if ok {
                    "brightness motion matches".into()
                } else {
                    format!("brightness trajectory off by {} oct on average", r2(mean))
                },
            ));
        }
    }

    if reference.attack_ms <= 0.0 && candidate.attack_ms <= 0.0 {
        axes.push(skipped("attack", "oct"));
    } else {
        let oct = octaves(candidate.attack_ms, reference.attack_ms, 1.0);
        let ok = oct.abs() <= 1.0;
        axes.push(axis(
            "attack",
            r2(oct),
            "oct",
            ok,
            if ok {
                "attack speed matches".into()
            } else {
                format!("attack {}x too {}", r1(2f64.powf(oct.abs())), if oct > 0.0 { "slow" } else { "fast" })
            },
        ));
    }

    if reference.decay_ms <= 0.0 {
        axes.push(skipped("decay", "oct"));
    } else {
        let oct = octaves(candidate.decay_ms, reference.decay_ms, 1.0);
        let ok = oct.abs() <= 0.7;
        axes.push(axis(
            "decay",
            r2(oct),
            "oct",
            ok,
            if ok {
                "tail length matches".into()
            } else {
                format!("decay {:.1}x too {}", r1(2f64.powf(oct.abs())), if oct > 0.0 { "long" } else { "short" })
            },
        ));
    }

    {
        let d = candidate.crest_db - reference.crest_db;
        let ok = d.abs() <= 6.0;
        axes.push(axis(
            "crest",
            r1(d),
            "dB",
            ok,
            if ok {
                "dynamics match".into()
            } else {
                format!("crest {} dB too {}", r1(d.abs()), if d > 0.0 { "spiky" } else { "flat" })
            },
        ));
    }
    {
        let d = candidate.flatness - reference.flatness;
        let ok = d.abs() <= 0.25;
        axes.push(axis(
            "flatness",
            r2(d),
            "",
            ok,
            if ok {
                "tonal/noisy balance matches".into()
            } else {
                format!("{} too {}", r2(d.abs()), if d > 0.0 { "noisy" } else { "tonal" })
            },
        ));
    }
    {
        let d = candidate.tilt_db_per_oct - reference.tilt_db_per_oct;
        let ok = d.abs() <= 3.0;
        axes.push(axis(
            "tilt",
            r1(d),
            "dB/oct",
            ok,
            if ok {
                "spectral slope matches".into()
            } else {
                format!("tilt {} dB/oct too {}", r1(d.abs()), if d > 0.0 { "bright" } else { "dark" })
            },
        ));
    }
    {
        let d_count = candidate.onsets.len() as i64 - reference.onsets.len() as i64;
        let d_first =
            candidate.onsets.first().copied().unwrap_or(0.0) - reference.onsets.first().copied().unwrap_or(0.0);
        let ok = d_count.abs() <= 1;
        axes.push(axis(
            "onsets",
            d_count as f64,
            "count",
            ok,
            if ok {
                format!(
                    "onset count matches (first onset {}s {})",
                    r2(d_first.abs()),
                    if d_first >= 0.0 { "late" } else { "early" }
                )
            } else {
                format!("{} {} onsets vs the reference", d_count.abs(), if d_count > 0 { "extra" } else { "missing" })
            },
        ));
    }
    {
        let deltas: Vec<(&str, f64)> = candidate
            .bands
            .iter()
            .filter_map(|b| reference.bands.iter().find(|r| r.name == b.name).map(|r| (b.name, b.db - r.db)))
            .collect();
        if deltas.is_empty() {
            axes.push(skipped("bands", "dB"));
        } else {
            let mean = deltas.iter().map(|(_, d)| d.abs()).sum::<f64>() / deltas.len() as f64;
            let worst = deltas.iter().cloned().fold(deltas[0], |w, x| if x.1.abs() > w.1.abs() { x } else { w });
            let ok = mean <= 8.0 && worst.1.abs() <= 12.0;
            axes.push(axis(
                "bands",
                r1(mean),
                "dB",
                ok,
                if ok {
                    "frequency balance matches".into()
                } else {
                    format!(
                        "{} {} dB {} the reference",
                        worst.0,
                        r1(worst.1.abs()),
                        if worst.1 > 0.0 { "over" } else { "under" }
                    )
                },
            ));
        }
    }

    let scored: Vec<&Axis> = axes.iter().filter(|a| !a.skipped).collect();
    let ok_count = scored.iter().filter(|a| a.ok).count();
    let score = if scored.is_empty() { 1.0 } else { ok_count as f64 / scored.len() as f64 };
    const SEVERITY: [&str; 10] = [
        "envelope",
        "onsets",
        "decay",
        "attack",
        "bands",
        "centroid",
        "brightnessTrajectory",
        "tilt",
        "flatness",
        "crest",
    ];
    let mut failing: Vec<&Axis> = scored.iter().copied().filter(|a| !a.ok).collect();
    failing.sort_by_key(|a| SEVERITY.iter().position(|s| *s == a.axis).unwrap_or(99));
    let summary = match failing.first() {
        None => format!("{ok_count}/{} axes within tolerance", scored.len()),
        Some(f) => format!("{ok_count}/{} axes within tolerance; worst: {}", scored.len(), f.note),
    };
    Comparison { axes, score, summary }
}

// ---------------------------------------------------------------------------------------------
// Spectrogram

pub struct Image {
    pub width: usize,
    pub height: usize,
    /// RGB, row-major, top row first.
    pub rgb: Vec<u8>,
}

const SPECTRO_HEIGHT: usize = 256;
const SPECTRO_MAX_WIDTH: usize = 600;
const SPECTRO_F_MIN: f64 = 30.0;
const SPECTRO_DB_RANGE: f64 = 80.0;

fn magma(t: f64) -> [u8; 3] {
    const STOPS: [(f64, [f64; 3]); 6] = [
        (0.0, [0.0, 0.0, 4.0]),
        (0.25, [60.0, 16.0, 110.0]),
        (0.5, [150.0, 38.0, 110.0]),
        (0.7, [221.0, 81.0, 86.0]),
        (0.85, [251.0, 150.0, 70.0]),
        (1.0, [252.0, 253.0, 191.0]),
    ];
    for i in 1..STOPS.len() {
        if t <= STOPS[i].0 {
            let (t0, c0) = STOPS[i - 1];
            let (t1, c1) = STOPS[i];
            let f = (t - t0) / (t1 - t0).max(1e-9);
            return [0, 1, 2].map(|k| (c0[k] + (c1[k] - c0[k]) * f).round() as u8);
        }
    }
    [252, 253, 191]
}

/// Log-frequency spectrogram, time on X, 30 Hz .. Nyquist on Y, 80 dB below the peak.
pub fn spectrogram(s: &Signal) -> Image {
    let sr = s.sample_rate as f64;
    let ch = s.channels.max(1) as usize;
    let n = s.samples.len() / ch;
    let mono = mono_of(s);
    let st = stft(&mono, n, sr, true);
    let max_db = st.db.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let floor_db = max_db - SPECTRO_DB_RANGE;
    let width = st.frames.clamp(240, SPECTRO_MAX_WIDTH);
    let height = SPECTRO_HEIGHT;
    let f_max = sr / 2.0;
    let last_sample = n.saturating_sub(1) as f64;
    let mut rgb = vec![0u8; width * height * 3];
    for x in 0..width {
        let sample = if width > 1 { x as f64 / (width - 1) as f64 * last_sample } else { 0.0 };
        let fp = ((sample - FFT_SIZE as f64 / 2.0) / HOP as f64).clamp(0.0, (st.frames - 1) as f64);
        let f0 = fp.floor() as usize;
        let f1 = (f0 + 1).min(st.frames - 1);
        let af = fp - f0 as f64;
        for y in 0..height {
            let frac = (height - 1 - y) as f64 / (height - 1) as f64;
            let freq = SPECTRO_F_MIN * (f_max / SPECTRO_F_MIN).powf(frac);
            let bin = ((freq * FFT_SIZE as f64 / sr).round() as usize).min(BINS - 1);
            let db = (1.0 - af) * st.db[f0 * BINS + bin] + af * st.db[f1 * BINS + bin];
            let t = ((db - floor_db) / SPECTRO_DB_RANGE).clamp(0.0, 1.0);
            let o = (y * width + x) * 3;
            rgb[o..o + 3].copy_from_slice(&magma(t));
        }
    }
    Image { width, height, rgb }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f64, secs: f64, sr: u32) -> Vec<f32> {
        (0..(secs * sr as f64) as usize)
            .map(|i| ((i as f64 * hz * std::f64::consts::TAU / sr as f64).sin() * 0.5) as f32)
            .collect()
    }

    #[test]
    fn fft_matches_dft_on_a_tone() {
        let n = 1024;
        let mut re: Vec<f64> = (0..n).map(|i| (i as f64 * 8.0 * std::f64::consts::TAU / n as f64).sin()).collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);
        let mag: Vec<f64> = (0..n / 2).map(|b| re[b].hypot(im[b])).collect();
        let peak = mag.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
        assert_eq!(peak, 8);
        assert!((mag[8] - n as f64 / 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_pure_tone_is_pitched_and_tonal() {
        let samples = tone(440.0, 0.5, 48_000);
        let f = features(&Signal { samples: &samples, channels: 1, sample_rate: 48_000 });
        assert_eq!(f.f0_hz, Some(440));
        assert!(f.flatness < 0.1, "{}", f.flatness);
        assert!((f.centroid_hz - 440.0).abs() < 60.0, "{}", f.centroid_hz);
        assert_eq!(f.onsets, vec![0.0]);
        assert!((f.peak - 0.5).abs() < 0.01);
    }

    #[test]
    fn noise_is_unpitched_and_flat() {
        let mut x: u32 = 7;
        let samples: Vec<f32> = (0..24_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect();
        let f = features(&Signal { samples: &samples, channels: 1, sample_rate: 48_000 });
        assert_eq!(f.f0_hz, None);
        assert!(f.flatness > 0.5, "{}", f.flatness);
    }

    #[test]
    fn compare_is_identity_on_itself_and_notes_a_darker_candidate() {
        let a = features(&Signal { samples: &tone(440.0, 0.3, 48_000), channels: 1, sample_rate: 48_000 });
        let same = compare(&a, &a);
        assert_eq!(same.score, 1.0);
        let dark = features(&Signal { samples: &tone(110.0, 0.3, 48_000), channels: 1, sample_rate: 48_000 });
        let c = compare(&dark, &a);
        let centroid = c.axes.iter().find(|x| x.axis == "centroid").unwrap();
        assert!(!centroid.ok);
        assert!(centroid.note.contains("dark"), "{}", centroid.note);
    }

    #[test]
    fn spectrogram_has_the_right_shape() {
        let samples = tone(1000.0, 0.2, 48_000);
        let img = spectrogram(&Signal { samples: &samples, channels: 1, sample_rate: 48_000 });
        assert_eq!(img.height, 256);
        assert!(img.width >= 240);
        assert_eq!(img.rgb.len(), img.width * img.height * 3);
    }
}
