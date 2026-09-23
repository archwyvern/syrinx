// syrinx-framework: sample/machine -- ways to break a voice: ring modulation, bitcrushing,
// stutters, dropouts, tape stops, a speaker, static, clicks, a metal room. Every function works on
// a mono Float32Array.

import { Random, hash } from "syrinx";
import { TAU, Noise, Biquad, Reverb, softclip, clamp } from "../dsp.js";

/** Ring-modulate against a sine: the metallic robot tone. */
export function ringMod(buf, sr, hz, mix) {
  for (let i = 0; i < buf.length; i++) { const m = Math.sin(TAU * hz * i / sr); buf[i] = buf[i] * (1 - mix) + buf[i] * m * mix; }
  return buf;
}

/** Bit-crush: quantise to `bits` and hold each value for `hold` samples. `curve` (0..1 per sample) scales the damage. */
export function bitcrush(buf, bits, hold, curve) {
  let held = 0;
  for (let i = 0; i < buf.length; i++) {
    const c = curve === undefined ? 1 : curve[i];
    const b = Math.max(2, Math.round(16 - (16 - bits) * c));
    const h = Math.max(1, Math.round(1 + (hold - 1) * c));
    const levels = Math.pow(2, b - 1);
    if (i % h === 0) held = Math.round(buf[i] * levels) / levels;
    buf[i] = held;
  }
  return buf;
}

/** Stutter: from `at` seconds, repeat the next `len` seconds `times` times, overwriting what follows. */
export function stutter(buf, sr, at, len, times) {
  const a = Math.round(at * sr), n = Math.round(len * sr);
  const seg = buf.slice(a, a + n);
  const fadeN = Math.min(48, n >> 2);
  for (let r = 1; r < times; r++) {
    const base = a + r * n;
    for (let i = 0; i < n && base + i < buf.length; i++) {
      let g = 1;
      if (i < fadeN) g = i / fadeN; else if (i > n - fadeN) g = (n - i) / fadeN;
      buf[base + i] = seg[i] * g;
    }
  }
  return buf;
}

/** Dropouts: random mutes, `rate` per second, each `len` seconds, with 2 ms edges. */
export function dropouts(buf, sr, seed, rate, len) {
  const rng = new Random(hash(seed, "drop"));
  const n = Math.round(len * sr), edge = Math.round(0.002 * sr);
  let i = 0;
  while (i < buf.length) {
    i += Math.round(rng.range(0.3, 1.7) / rate * sr);
    for (let k = 0; k < n && i + k < buf.length; k++) {
      const g = k < edge ? 1 - k / edge : k > n - edge ? 1 - (n - k) / edge : 0;
      buf[i + k] *= g;
    }
    i += n;
  }
  return buf;
}

/** Tape stop: from `from` seconds the playback rate falls to zero over `seconds`; the tail is silence. */
export function tapeStop(buf, sr, from, seconds) {
  const a = Math.round(from * sr);
  const src = buf.slice(a);
  let pos = 0;
  const n = Math.round(seconds * sr);
  for (let i = 0; i < n && a + i < buf.length; i++) {
    const u = i / n;
    const rate = (1 - u) * (1 - u);
    const j = Math.floor(pos), f = pos - j;
    buf[a + i] = j + 1 < src.length ? (src[j] + (src[j + 1] - src[j]) * f) * (1 - u * 0.3) : 0;
    pos += rate;
  }
  for (let i = a + n; i < buf.length; i++) buf[i] = 0;
  return buf;
}

/** Pitch warp: read the buffer at a rate given by `rateAt(t)` (1 = normal). Returns a new buffer. */
export function warp(buf, sr, rateAt) {
  const out = new Float32Array(buf.length);
  let pos = 0;
  for (let i = 0; i < out.length; i++) {
    const j = Math.floor(pos), f = pos - j;
    if (j + 1 >= buf.length) break;
    out[i] = buf[j] + (buf[j + 1] - buf[j]) * f;
    pos += rateAt(i / sr);
  }
  return out;
}

/** Tremolo / mechanical buzz: amplitude modulation at `hz`, `depth` 0..1, square-ish when `hard`. */
export function tremolo(buf, sr, hz, depth, hard) {
  for (let i = 0; i < buf.length; i++) {
    let m = Math.sin(TAU * hz * i / sr);
    if (hard) m = softclip(m * 4);
    buf[i] *= 1 - depth * 0.5 * (1 - m);
  }
  return buf;
}

/** Hard drive into a clipper. */
export function drive(buf, amount) {
  for (let i = 0; i < buf.length; i++) buf[i] = Math.tanh(buf[i] * amount) / Math.tanh(amount);
  return buf;
}

/** A band, like a bad speaker: `lo`..`hi` Hz with a resonant peak at the top. */
export function speaker(buf, sr, lo, hi, peakDb) {
  const hp = Biquad.highpass(sr, lo, 0.9), lp = Biquad.lowpass(sr, hi, 1.2), pk = Biquad.peak(sr, hi * 0.8, 2, peakDb === undefined ? 6 : peakDb);
  for (let i = 0; i < buf.length; i++) buf[i] = lp.process(pk.process(hp.process(buf[i])));
  return buf;
}

/** Static: bursts of filtered noise, `density` per second, mixed in at `level`. */
export function staticBursts(buf, sr, seed, density, level) {
  const rng = new Random(hash(seed, "static")), noise = new Noise(hash(seed, "sn"));
  const bp = Biquad.bandpass(sr, 3000, 0.7);
  let next = Math.round(rng.range(0, 1) / density * sr), len = 0, i = 0;
  while (i < buf.length) {
    if (i >= next) { len = Math.round(rng.range(0.01, 0.12) * sr); next = i + len + Math.round(rng.range(0.2, 1.8) / density * sr); }
    if (len > 0) { buf[i] += bp.process(noise.white()) * level * rng.range(0.5, 1); len--; }
    i++;
  }
  return buf;
}

/** Servo whine: a sine sliding from `f0` to `f1` Hz over `seconds` with a little noise, into `buf` at `at`. */
export function servo(buf, sr, seed, at, seconds, f0, f1, level) {
  const noise = new Noise(hash(seed, "servo"));
  const a = Math.round(at * sr), n = Math.round(seconds * sr);
  let phase = 0;
  for (let i = 0; i < n && a + i < buf.length; i++) {
    const u = i / n;
    const f = f0 * Math.exp(Math.log(f1 / f0) * u);
    phase += f / sr;
    const env = Math.sin(Math.PI * u);
    buf[a + i] += (Math.sin(TAU * phase) * 0.7 + Math.sin(TAU * phase * 2.01) * 0.2 + noise.white() * 0.1) * env * level;
  }
  return buf;
}

/** Relay clicks: short resonant impulses at the given times. */
export function clicks(buf, sr, seed, times, level) {
  const noise = new Noise(hash(seed, "click"));
  for (let k = 0; k < times.length; k++) {
    const bp = Biquad.bandpass(sr, 1800 + (k % 3) * 900, 4);
    const a = Math.round(times[k] * sr);
    for (let i = 0; i < Math.round(0.02 * sr) && a + i < buf.length; i++) buf[a + i] += bp.process(noise.white()) * Math.exp(-i / (0.003 * sr)) * level;
  }
  return buf;
}

/** A metal room: dsp.js's Schroeder, bright and short, mixed in. */
export function metalRoom(buf, sr, wet, size) {
  const rv = new Reverb(sr, { size: size === undefined ? 0.6 : size, damp: 0.15, feedback: 0.78 });
  const out = new Float32Array(buf.length);
  for (let i = 0; i < buf.length; i++) out[i] = buf[i] + rv.process(buf[i]) * wet;
  return out;
}

/** Fade the ends. */
export function fadeEnds(buf, sr, secIn, secOut) {
  const a = Math.round(secIn * sr), b = Math.round(secOut * sr);
  for (let i = 0; i < a && i < buf.length; i++) buf[i] *= i / a;
  for (let i = 0; i < b && i < buf.length; i++) buf[buf.length - 1 - i] *= i / b;
  return buf;
}

/** Sum buffers into a new one (the longest wins). */
export function sum(...bufs) {
  let n = 0; for (const b of bufs) n = Math.max(n, b.length);
  const out = new Float32Array(n);
  for (const b of bufs) for (let i = 0; i < b.length; i++) out[i] += b[i];
  return out;
}
