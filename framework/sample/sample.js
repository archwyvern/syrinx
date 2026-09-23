// syrinx-framework: sample/sample -- a recorded (or rendered) voice as a buffer, and the ways of
// bending it that need a whole take rather than a sample at a time: pitch shifting by grains,
// time stretching, doubling, pre-echo, whisper doubles, a radio channel, a vocoder. Everything
// works on mono Float32Array at the take's sample rate; a take is a module `{ sr, seconds, pcm }`
// with 16-bit samples, read by `load`.

import { Random, hash } from "syrinx";
import { TAU, Noise, Biquad, OnePole, Reverb, clamp, softclip } from "../dsp.js";

/** The PCM module (`{ sr, seconds, pcm }`) as a float buffer, optionally padded to `seconds`. */
export function load(mod, seconds) {
  const n = seconds === undefined ? mod.pcm.length : Math.round(seconds * mod.sr);
  const out = new Float32Array(n);
  for (let i = 0; i < n && i < mod.pcm.length; i++) out[i] = mod.pcm[i] / 32768;
  return out;
}

/** Pitch shift by `semitones` with overlapping grains (formants move with the pitch: the cheap, machine kind). */
export function pitchShift(buf, sr, semitones, grainSec) {
  const ratio = Math.pow(2, semitones / 12);
  const grain = Math.round((grainSec === undefined ? 0.06 : grainSec) * sr), hop = grain >> 1;
  const out = new Float32Array(buf.length), win = new Float32Array(grain);
  for (let k = 0; k < grain; k++) win[k] = 0.5 - 0.5 * Math.cos(TAU * k / grain);
  for (let o = 0; o < buf.length; o += hop) {
    for (let k = 0; k < grain && o + k < buf.length; k++) {
      const p = o + k * ratio, j = Math.floor(p), f = p - j;
      if (j + 1 >= buf.length) break;
      out[o + k] += (buf[j] + (buf[j + 1] - buf[j]) * f) * win[k];
    }
  }
  return out;
}

/** Time-stretch by `factor` (2 = twice as long) at the same pitch, by grains. Returns a new, longer buffer. */
export function timeStretch(buf, sr, factor, grainSec) {
  const grain = Math.round((grainSec === undefined ? 0.06 : grainSec) * sr), hop = grain >> 1;
  const out = new Float32Array(Math.round(buf.length * factor) + grain), win = new Float32Array(grain);
  for (let k = 0; k < grain; k++) win[k] = 0.5 - 0.5 * Math.cos(TAU * k / grain);
  for (let o = 0; o + grain < out.length; o += hop) {
    const src = Math.round(o / factor);
    for (let k = 0; k < grain; k++) { const j = src + k; if (j >= buf.length) break; out[o + k] += buf[j] * win[k]; }
  }
  return out;
}

/** Reverse a copy. */
export function reverse(buf) {
  const out = new Float32Array(buf.length);
  for (let i = 0; i < buf.length; i++) out[i] = buf[buf.length - 1 - i];
  return out;
}

/** Amplitude envelope, `ms` follower, as a buffer 0..1. */
export function envelope(buf, sr, ms) {
  const k = 1 - Math.exp(-1 / ((ms === undefined ? 15 : ms) * 0.001 * sr));
  const out = new Float32Array(buf.length);
  let e = 0;
  for (let i = 0; i < buf.length; i++) { const a = Math.abs(buf[i]); e += ((a > e ? 1 : 0.25) * k) * (a - e); out[i] = e; }
  return out;
}

/** Layer copies: `parts` = [[buf, delaySeconds, gain], ...] into a buffer of `n` samples. */
export function layer(n, sr, parts) {
  const out = new Float32Array(n);
  for (const [b, d, g] of parts) {
    const a = Math.round(d * sr);
    for (let i = 0; i < b.length && a + i < n; i++) if (a + i >= 0) out[a + i] += b[i] * g;
  }
  return out;
}

/** Several of them at once: detuned, delayed copies with a slow wobble, under the original. */
export function ghostChorus(buf, sr, seed, voices, spreadCents, spreadMs, gain) {
  const rng = new Random(hash(seed, "ghost"));
  const parts = [[buf, 0, 1]];
  for (let v = 0; v < voices; v++) {
    const cents = rng.range(-spreadCents, spreadCents), delay = rng.range(0.012, spreadMs * 0.001);
    const wob = rng.range(0.15, 0.4), wobPhase = rng.range(0, TAU);
    let pos = 0;
    const copy = new Float32Array(buf.length);
    for (let i = 0; i < buf.length; i++) {
      const rate = Math.pow(2, (cents + 6 * Math.sin(TAU * wob * i / sr + wobPhase)) / 1200);
      const j = Math.floor(pos), f = pos - j;
      if (j + 1 >= buf.length) break;
      copy[i] = buf[j] + (buf[j + 1] - buf[j]) * f;
      pos += rate;
    }
    parts.push([copy, delay, gain / Math.sqrt(voices)]);
  }
  return layer(buf.length + Math.round(spreadMs * 0.001 * sr), sr, parts);
}

/** A pre-echo: the reverb of the reversed take, reversed back, so every phrase is preceded by its own swell. */
export function preEcho(buf, sr, wet, size) {
  const rv = new Reverb(sr, { size: size === undefined ? 1.2 : size, damp: 0.3, feedback: 0.86 });
  const r = reverse(buf), tail = new Float32Array(buf.length);
  for (let i = 0; i < r.length; i++) tail[i] = rv.process(r[i]);
  const swell = reverse(tail);
  for (let i = 0; i < buf.length; i++) swell[i] = buf[i] + swell[i] * wet;
  return swell;
}

/** A whispered double: the take's envelope driving noise through a band, no pitch at all. */
export function whisperDouble(buf, sr, seed, gain, lo, hi) {
  const noise = new Noise(hash(seed, "whisper")), env = envelope(buf, sr, 8);
  const hp = Biquad.highpass(sr, lo === undefined ? 900 : lo, 0.7), lp = Biquad.lowpass(sr, hi === undefined ? 6000 : hi, 0.7);
  const out = new Float32Array(buf.length);
  for (let i = 0; i < buf.length; i++) out[i] = lp.process(hp.process(noise.white())) * env[i] * gain;
  return out;
}

/** A radio channel: a band, some drive, carrier hiss, and a squelch that opens with the voice. */
export function radio(buf, sr, seed, o) {
  const p = o || {};
  const lo = p.lo === undefined ? 300 : p.lo, hi = p.hi === undefined ? 3400 : p.hi, drv = p.drive === undefined ? 2.5 : p.drive;
  const hiss = p.hiss === undefined ? 0.02 : p.hiss, floor = p.floor === undefined ? 0.15 : p.floor;
  const hp = Biquad.highpass(sr, lo, 0.8), lp = Biquad.lowpass(sr, hi, 0.9), pk = Biquad.peak(sr, 2200, 1.2, 4);
  const noise = new Noise(hash(seed, "radio")), nbp = Biquad.bandpass(sr, 1800, 0.4);
  const env = envelope(buf, sr, 40);
  const out = new Float32Array(buf.length);
  for (let i = 0; i < buf.length; i++) {
    const v = Math.tanh(pk.process(lp.process(hp.process(buf[i]))) * drv) / Math.tanh(drv);
    const open = floor + (1 - floor) * clamp(env[i] * 12, 0, 1);   // squelch: the channel opens on the voice
    out[i] = v * open + nbp.process(noise.white()) * hiss * (0.6 + 0.4 * open);
  }
  return out;
}

/** Gain by a curve `g(t)`. */
export function shape(buf, sr, g) {
  for (let i = 0; i < buf.length; i++) buf[i] *= g(i / sr);
  return buf;
}

// ---------------------------------------------------------------- the worse kit

/** Granular scatter: the take re-said in grains -- some repeated, some backwards, the read head jumping back
 *  to re-say a fragment, each grain a little off pitch. `o`: grain (s), repeat, reverse, jump, drop (probabilities),
 *  back (s), cents. Returns a new buffer about `stretch` times as long. */
export function scatter(buf, sr, seed, o) {
  const p = o || {};
  const rng = new Random(hash(seed, "scatter"));
  const gmin = p.grainMin === undefined ? 0.04 : p.grainMin, gmax = p.grainMax === undefined ? 0.14 : p.grainMax;
  const repeat = p.repeat === undefined ? 0.3 : p.repeat, rev = p.reverse === undefined ? 0.25 : p.reverse;
  const jump = p.jump === undefined ? 0.2 : p.jump, drop = p.drop === undefined ? 0.1 : p.drop;
  const back = p.back === undefined ? 0.6 : p.back, cents = p.cents === undefined ? 60 : p.cents;
  const out = new Float32Array(Math.round(buf.length * (p.stretch === undefined ? 1.6 : p.stretch)));
  let read = 0, write = 0;
  while (read < buf.length && write < out.length) {
    const g = Math.round(rng.range(gmin, gmax) * sr);
    const times = rng.chance(repeat) ? rng.int(2, 5) : 1;
    const backwards = rng.chance(rev), skip = rng.chance(drop);
    if (!skip) for (let r = 0; r < times; r++) {
      const ratio = Math.pow(2, rng.range(-cents, cents) / 1200);
      for (let k = 0; k < g && write + k < out.length; k++) {
        const w = 0.5 - 0.5 * Math.cos(TAU * k / g);
        const src = backwards ? read + g - k * ratio : read + k * ratio;
        const j = Math.floor(src), f = src - j;
        if (j + 1 >= buf.length || j < 0) break;
        out[write + k] += (buf[j] + (buf[j + 1] - buf[j]) * f) * w;
      }
      write += g >> 1;
    }
    read += rng.chance(jump) ? -Math.round(rng.range(0.1, back) * sr) : g >> 1;
    if (read < 0) read = 0;
  }
  return out;
}

/** A channel vocoder: the take's spectral envelope in `bands` bands imposed on a carrier of a rough saw at `hz`
 *  (drifting) with noise. The robot that is not quite sure of its pitch. */
export function vocode(buf, sr, seed, hz, bands, noiseMix) {
  const nb = bands === undefined ? 14 : bands, nz = noiseMix === undefined ? 0.2 : noiseMix;
  const rng = new Random(hash(seed, "voc")), noise = new Noise(hash(seed, "vocn"));
  const out = new Float32Array(buf.length);
  // the carrier: a saw whose pitch wanders, plus noise
  const carrier = new Float32Array(buf.length);
  let phase = 0, drift = 0;
  for (let i = 0; i < buf.length; i++) {
    if ((i & 1023) === 0) drift = drift * 0.9 + rng.bipolar() * 0.03;
    phase += hz * (1 + drift) / sr; if (phase >= 1) phase -= 1;
    carrier[i] = (2 * phase - 1) * (1 - nz) + noise.white() * nz;
  }
  for (let b = 0; b < nb; b++) {
    const f = 180 * Math.pow(6500 / 180, b / (nb - 1));
    const an = Biquad.bandpass(sr, f, 5), ca = Biquad.bandpass(sr, f, 5);
    const k = 1 - Math.exp(-1 / (0.012 * sr));
    let env = 0;
    for (let i = 0; i < buf.length; i++) {
      const a = Math.abs(an.process(buf[i])); env += (a - env) * (a > env ? k : k * 0.3);
      out[i] += ca.process(carrier[i]) * env * 3;
    }
  }
  return out;
}

/** Stuck buffers: at `rate` per second the last `hold` (15-45 ms) repeats for 0.1-0.5 s, sometimes diving in pitch. */
export function glitch(buf, sr, seed, rate) {
  const rng = new Random(hash(seed, "glitch"));
  let i = Math.round(rng.range(0.1, 0.8) / rate * sr);
  while (i < buf.length) {
    const hold = Math.round(rng.range(0.015, 0.045) * sr), len = Math.round(rng.range(0.1, 0.5) * sr);
    const dive = rng.chance(0.4);
    const seg = buf.slice(Math.max(0, i - hold), i);
    if (seg.length > 0) {
      let pos = 0;
      for (let k = 0; k < len && i + k < buf.length; k++) {
        const u = k / len, ratio = dive ? 1 - 0.6 * u : 1;
        const j = Math.floor(pos), f = pos - j;
        buf[i + k] = seg[j % seg.length] + (seg[(j + 1) % seg.length] - seg[j % seg.length]) * f;
        pos += ratio; if (pos >= seg.length) pos -= seg.length;
      }
    }
    i += len + Math.round(rng.range(0.4, 1.6) / rate * sr);
  }
  return buf;
}

/** Shrieks: short screams of resonant noise and a rising tone, at the given times. */
export function shrieks(buf, sr, seed, times, level) {
  const noise = new Noise(hash(seed, "shriek"));
  for (let k = 0; k < times.length; k++) {
    const a = Math.round(times[k] * sr), n = Math.round((0.15 + (k % 3) * 0.1) * sr);
    const bp = new Biquad(sr);
    let phase = 0;
    for (let i = 0; i < n && a + i < buf.length; i++) {
      const u = i / n, env = Math.sin(Math.PI * Math.pow(u, 0.6));
      const f = 1200 * Math.pow(3.2, u);
      if ((i & 31) === 0) bp.set("bandpass", f, 2.5);
      buf[a + i] += bp.process(noise.white()) * 2.5 * env * level;
    }
  }
  return buf;
}

/** Power dips: `count` places where the playback rate dives to `floor` for a moment and recovers. Returns a new buffer. */
export function powerDips(buf, sr, seed, count, floor) {
  const rng = new Random(hash(seed, "dip"));
  const dips = [];
  for (let k = 0; k < count; k++) dips.push([rng.range(0.1, 0.9) * buf.length / sr, rng.range(0.15, 0.45)]);
  const fl = floor === undefined ? 0.3 : floor;
  const out = new Float32Array(buf.length);
  let pos = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    let rate = 1;
    for (const [at, len] of dips) if (t > at && t < at + len) { const u = (t - at) / len; rate = Math.min(rate, fl + (1 - fl) * Math.abs(2 * u - 1)); }
    const j = Math.floor(pos), f = pos - j;
    if (j + 1 >= buf.length) break;
    out[i] = buf[j] + (buf[j + 1] - buf[j]) * f;
    pos += rate;
  }
  return out;
}

/** Shimmer: a reverb whose tail is pitch-shifted up and fed back into itself; the cloud behind a psychic voice. */
export function shimmer(buf, sr, semitones, wet, passes) {
  const n = passes === undefined ? 3 : passes;
  let cloud = new Float32Array(buf.length);
  let src = buf;
  for (let p = 0; p < n; p++) {
    const rv = new Reverb(sr, { size: 1.8, damp: 0.4, feedback: 0.9 });
    const tail = new Float32Array(buf.length);
    for (let i = 0; i < buf.length; i++) tail[i] = rv.process(src[i]);
    src = pitchShift(tail, sr, semitones, 0.09);
    for (let i = 0; i < buf.length; i++) cloud[i] += src[i] * Math.pow(0.6, p);
  }
  const out = new Float32Array(buf.length);
  for (let i = 0; i < buf.length; i++) out[i] = buf[i] + cloud[i] * wet;
  return out;
}

/** Drowned: a slow lowpass that closes and opens on its own, with the level wobbling like something under water. */
export function drowned(buf, sr, seed, depth) {
  const rng = new Random(hash(seed, "drown")), lp = new OnePole(sr);
  const d = depth === undefined ? 0.7 : depth;
  let cut = 2000, target = 2000, wob = 1, wobT = 1;
  for (let i = 0; i < buf.length; i++) {
    if ((i & 2047) === 0) { target = 400 * Math.pow(12, rng.next()); wobT = 1 - d * rng.next(); }
    cut += (target - cut) * 0.002; wob += (wobT - wob) * 0.004;
    buf[i] = lp.lp(buf[i], cut) * wob;
  }
  return buf;
}

/** Only the loud bits: a hard gate that opens on peaks, so words surface out of the noise floor. */
export function surface(buf, sr, threshold, floor) {
  const env = envelope(buf, sr, 25);
  let peak = 0; for (let i = 0; i < env.length; i++) peak = Math.max(peak, env[i]);
  for (let i = 0; i < buf.length; i++) { const g = env[i] > threshold * peak ? 1 : floor; buf[i] *= g; }
  return buf;
}

/** Waves: the corruption comes and goes. Crossfades between `a` (the take, still audible) and `b` (the wreck)
 *  on an irregular schedule: `a` for about `aFrac` of the time in stretches near `period` seconds, 60 ms edges. */
export function waves(a, b, sr, seed, period, aFrac) {
  const rng = new Random(hash(seed, "waves"));
  const n = Math.max(a.length, b.length), out = new Float32Array(n);
  const edge = Math.round(0.06 * sr);
  let i = 0, clear = rng.chance(aFrac);
  const w = new Float32Array(n);
  while (i < n) {
    const len = Math.round(rng.range(0.4, 1.6) * period * (clear ? aFrac / (1 - aFrac + 1e-6) : 1) * sr);
    for (let k = 0; k < len && i + k < n; k++) w[i + k] = clear ? 1 : 0;
    i += len; clear = !clear;
  }
  // smooth the edges
  let s = w[0];
  const k = 1 - Math.exp(-1 / edge);
  for (let j = 0; j < n; j++) { s += (w[j] - s) * k; const va = j < a.length ? a[j] : 0, vb = j < b.length ? b[j] : 0; out[j] = va * s + vb * (1 - s); }
  return out;
}
