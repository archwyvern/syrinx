// syrinx-framework: voice/convert -- turning a person's voice into a machine's, from the inside.
//
// A take is analysed into two things: the shape of the vocal tract, frame by frame (linear prediction:
// twenty reflection coefficients every 8 ms, plus a gain), and the pitch and voicing of what drove it.
// Then the person's excitation is thrown away and the tract is driven by something else -- a pulse
// train that cannot hold its pitch, inharmonic metal, breath -- and the tract itself can be warped,
// frozen, jittered or smoothed on the way. The words survive because the tract survives; the human
// does not, because nothing human is making the sound.

import { Random, hash } from "syrinx";
import { TAU, Noise, Biquad, OnePole, clamp } from "../dsp.js";
import { pitchShift } from "../sample/sample.js";

// ---------------------------------------------------------------- analysis

/** Reflection coefficients and gain per frame, by the autocorrelation method (Levinson-Durbin). */
function lpcFrame(x, order) {
  const n = x.length, r = new Float64Array(order + 1);
  for (let k = 0; k <= order; k++) { let s = 0; for (let i = k; i < n; i++) s += x[i] * x[i - k]; r[k] = s; }
  const k = new Float64Array(order + 1), a = new Float64Array(order + 1), tmp = new Float64Array(order + 1);
  let e = r[0] * (1 + 1e-6) + 1e-12;
  for (let i = 1; i <= order; i++) {
    let acc = r[i];
    for (let j = 1; j < i; j++) acc -= a[j] * r[i - j];
    let ki = acc / e;
    if (!(ki > -0.999 && ki < 0.999)) ki = clamp(ki, -0.999, 0.999);
    k[i] = ki;
    for (let j = 1; j < i; j++) tmp[j] = a[j] - ki * a[i - j];
    for (let j = 1; j < i; j++) a[j] = tmp[j];
    a[i] = ki;
    e *= 1 - ki * ki;
  }
  return { k, gain: Math.sqrt(Math.max(0, e) / n) };
}

/** Pitch of a frame by normalised autocorrelation, or 0 when unvoiced. */
function pitchFrame(x, sr, fmin, fmax) {
  const n = x.length;
  let e0 = 0; for (let i = 0; i < n; i++) e0 += x[i] * x[i];
  if (e0 < 1e-7) return { f0: 0, v: 0 };
  const lmin = Math.floor(sr / fmax), lmax = Math.min(n - 2, Math.ceil(sr / fmin));
  let best = 0, bestLag = 0;
  for (let lag = lmin; lag <= lmax; lag++) {
    let s = 0, e1 = 0;
    for (let i = 0; i + lag < n; i++) { s += x[i] * x[i + lag]; e1 += x[i + lag] * x[i + lag]; }
    const c = s / Math.sqrt(e0 * e1 + 1e-12);
    if (c > best) { best = c; bestLag = lag; }
  }
  return { f0: bestLag > 0 ? sr / bestLag : 0, v: best };
}

/** Analyse a take. Returns { hop, frames: [{ k, gain, f0, voiced, rms }], sr, order }. `formant` (semitones)
 *  warps the tract by analysing a pitch-shifted copy while the pitch is read from the original. */
export function analyze(buf, sr, o) {
  const p = o || {};
  const order = p.order === undefined ? 20 : p.order;
  const hop = Math.round((p.hop === undefined ? 0.008 : p.hop) * sr), win = Math.round((p.window === undefined ? 0.025 : p.window) * sr);
  const src = p.formant ? pitchShift(buf, sr, p.formant, 0.05) : buf;
  // pre-emphasis for the tract analysis; a lowpassed copy for the pitch
  const pre = new Float32Array(src.length);
  for (let i = 0; i < src.length; i++) pre[i] = src[i] - 0.97 * (i > 0 ? src[i - 1] : 0);
  const lp1 = new OnePole(sr), lp2 = new OnePole(sr), low = new Float32Array(buf.length);
  for (let i = 0; i < buf.length; i++) low[i] = lp2.lp(lp1.lp(buf[i], 900), 900);
  const hann = new Float32Array(win); for (let i = 0; i < win; i++) hann[i] = 0.5 - 0.5 * Math.cos(TAU * i / win);
  const pwin = Math.round(0.035 * sr);
  const frames = [];
  let peak = 0; for (let i = 0; i < buf.length; i++) peak = Math.max(peak, Math.abs(buf[i]));
  for (let c = 0; c < buf.length; c += hop) {
    const x = new Float64Array(win);
    for (let i = 0; i < win; i++) { const j = c - (win >> 1) + i; x[i] = j >= 0 && j < pre.length ? pre[j] * hann[i] : 0; }
    const { k, gain } = lpcFrame(x, order);
    const y = new Float64Array(pwin);
    for (let i = 0; i < pwin; i++) { const j = c - (pwin >> 1) + i; y[i] = j >= 0 && j < low.length ? low[j] : 0; }
    let rms = 0; for (let i = 0; i < win; i++) { const j = c - (win >> 1) + i; if (j >= 0 && j < buf.length) rms += buf[j] * buf[j]; } rms = Math.sqrt(rms / win);
    const { f0, v } = pitchFrame(y, sr, p.fmin === undefined ? 60 : p.fmin, p.fmax === undefined ? 500 : p.fmax);
    frames.push({ k, gain, f0, voiced: v > 0.45 && rms > 0.02 * peak ? 1 : 0, rms });
  }
  // a little median smoothing on the pitch, and fill unvoiced frames with the nearest voiced pitch so the excitation glides
  for (let m = 1; m < frames.length - 1; m++) {
    const a = frames[m - 1].f0, b = frames[m].f0, c = frames[m + 1].f0;
    if (frames[m].voiced && frames[m - 1].voiced && frames[m + 1].voiced) frames[m].f0 = [a, b, c].sort((x, y) => x - y)[1];
  }
  let last = 0; for (const f of frames) { if (f.voiced && f.f0 > 0) last = f.f0; else f.f0 = last; }
  last = 0; for (let m = frames.length - 1; m >= 0; m--) { if (frames[m].voiced) last = frames[m].f0; else if (frames[m].f0 === 0) frames[m].f0 = last; }
  return { hop, frames, sr, order };
}

// ---------------------------------------------------------------- tract surgery

/** Walk the frames and change them in place: freeze (hold a frame for a while), jitter the tract, sharpen or
 *  dull it. `o`: freeze (per second), freezeLen (s), jitter (0..1), sharpen (k scale, 1 = as is). */
export function surgery(an, seed, o) {
  const p = o || {}, rng = new Random(hash(seed, "surgery"));
  const F = an.frames, order = an.order, perSec = an.sr / an.hop;
  if (p.freeze) {
    let m = Math.round(rng.range(0.2, 0.8) * perSec / p.freeze);
    while (m < F.length) {
      const len = Math.round(rng.range(0.5, 1.5) * (p.freezeLen === undefined ? 0.18 : p.freezeLen) * perSec);
      for (let j = 1; j < len && m + j < F.length; j++) F[m + j].k = F[m].k;
      m += len + Math.round(rng.range(0.4, 1.6) * perSec / p.freeze);
    }
  }
  if (p.jitter) for (const f of F) { const k = new Float64Array(f.k); for (let i = 1; i <= order; i++) k[i] = clamp(k[i] + rng.bipolar() * 0.12 * p.jitter, -0.99, 0.99); f.k = k; }
  if (p.sharpen !== undefined && p.sharpen !== 1) for (const f of F) { const k = new Float64Array(f.k); for (let i = 1; i <= order; i++) k[i] = clamp(k[i] * p.sharpen, -0.995, 0.995); f.k = k; }
  return an;
}

// ---------------------------------------------------------------- excitation

/** The machine's larynx. Per sample it is given the frame's pitch, voicing and time, and returns the source.
 *  `o`: pitch (multiplier), quantize (semitones, 0 = off), mono (0..1: collapse toward one pitch), flutter (0..1),
 *  jitter (0..1), sub (0..1: alternate periods weakened, a growl), metal (0..1: inharmonic ring on each pulse),
 *  noise (0..1: breath mixed into voiced frames), double (cents: a second pulse train beating against the first),
 *  seed. */
export function larynx(sr, o) {
  const p = o || {}, rng = new Random(hash(p.seed === undefined ? 1 : p.seed, "larynx")), noise = new Noise(hash(p.seed === undefined ? 1 : p.seed, "breath"));
  const pitch = p.pitch === undefined ? 1 : p.pitch, quant = p.quantize === undefined ? 0 : p.quantize, mono = p.mono === undefined ? 0 : p.mono;
  const flutter = p.flutter === undefined ? 0 : p.flutter, jit = p.jitter === undefined ? 0 : p.jitter, sub = p.sub === undefined ? 0 : p.sub;
  const metal = p.metal === undefined ? 0 : p.metal, nz = p.noise === undefined ? 0.05 : p.noise, dbl = p.double === undefined ? 0 : p.double;
  const rings = [[1.71, 0.004], [2.93, 0.003], [4.6, 0.002]].map(([r, tau]) => ({ r, tau, ph: 0, env: 0 }));
  let phase = 0, phase2 = 0, parity = 1, wander = 0, hzPrev = 0, ringBase = 200, jitNow = 0;
  const glide = 1 - Math.exp(-1 / (0.012 * sr));
  return (f0, voiced, t, rmsRef) => {
    let hz = f0 * pitch;
    if (mono > 0 && rmsRef > 0) hz = Math.exp(Math.log(hz) * (1 - mono) + Math.log(rmsRef * pitch) * mono);
    if (quant > 0 && hz > 0) { const st = 12 * Math.log(hz / 55) / Math.LN2; hz = 55 * Math.pow(2, Math.round(st / quant) * quant / 12); }
    if ((Math.floor(t * sr) & 511) === 0) { wander = wander * 0.92 + rng.bipolar() * 0.05 * flutter; jitNow = rng.bipolar() * 0.06 * jit; }
    hz *= 1 + wander + jitNow;
    hzPrev += (hz - hzPrev) * glide; hz = hzPrev;
    let x = 0;
    if (voiced && hz > 0) {
      phase += hz / sr;
      let pulse = 0;
      if (phase >= 1) { phase -= 1; parity = -parity; pulse = 1 - (sub * (parity < 0 ? 0.75 : 0)); ringBase = hz; for (const r of rings) r.env = 1; }
      x += pulse * 4;
      if (dbl > 0) { phase2 += hz * Math.pow(2, dbl / 1200) / sr; if (phase2 >= 1) { phase2 -= 1; x += 2.5; } }
      if (metal > 0) for (const r of rings) { r.ph += ringBase * r.r / sr; r.env *= Math.exp(-1 / (r.tau * sr)); x += Math.sin(TAU * r.ph) * r.env * metal * 3; }
      x += noise.white() * nz * 2;
    } else {
      x += noise.white() * (voiced ? nz * 2 : 1.2);
    }
    return x;
  };
}

// ---------------------------------------------------------------- synthesis

/** Drive the analysed tract with a larynx. Returns a new buffer the length of the take. `o`: gainFollow
 *  (0..1: how much the original loudness contour is kept), out (frames to render, default all). */
export function synthesize(an, ex, o) {
  const p = o || {}, sr = an.sr, F = an.frames, order = an.order, hop = an.hop;
  const n = F.length * hop, out = new Float32Array(n);
  const follow = p.gainFollow === undefined ? 1 : p.gainFollow;
  const b = new Float64Array(order + 1);
  const kCur = new Float64Array(order + 1);
  let ref = 0, cnt = 0; for (const f of F) if (f.voiced) { ref += f.f0; cnt++; } ref = cnt ? ref / cnt : 120;
  const fvals = new Float64Array(order + 1);
  let yPrev = 0;
  for (let i = 0; i < n; i++) {
    const m = Math.min(F.length - 1, Math.floor(i / hop)), w = (i - m * hop) / hop, f = F[m], g = F[Math.min(F.length - 1, m + 1)];
    for (let j = 1; j <= order; j++) kCur[j] = f.k[j] + (g.k[j] - f.k[j]) * w;
    const gain = Math.exp(Math.log(f.gain + 1e-9) * (1 - w) + Math.log(g.gain + 1e-9) * w);
    const voiced = w < 0.5 ? f.voiced : g.voiced, f0 = f.f0 + (g.f0 - f.f0) * w;
    const e = ex(f0, voiced, i / sr, ref) * (follow > 0 ? Math.pow(gain, follow) * Math.pow(0.02, 1 - follow) : 0.02);
    // lattice synthesis: forward path from the excitation down through the tract, backward path updates the state
    const fv = fvals; fv[order] = e;
    for (let j = order; j >= 1; j--) fv[j - 1] = fv[j] + kCur[j] * b[j - 1];
    for (let j = order; j >= 1; j--) b[j] = b[j - 1] - kCur[j] * fv[j - 1];
    b[0] = fv[0];
    const y = fv[0];
    const d = y + 0.97 * yPrev; yPrev = d;   // de-emphasis
    out[i] = d;
  }
  return out;
}

/** Convenience: analyse, operate, drive. */
export function convert(buf, sr, o) {
  const p = o || {};
  const an = analyze(buf, sr, { order: p.order, formant: p.formant, hop: p.hop });
  if (p.surgery) surgery(an, p.seed === undefined ? 1 : p.seed, p.surgery);
  const ex = larynx(sr, { seed: p.seed, ...(p.larynx || {}) });
  return synthesize(an, ex, { gainFollow: p.gainFollow });
}
