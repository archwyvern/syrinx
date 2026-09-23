// syrinx prelude — the standard library every sound source is compiled against.
//
// Everything here is deterministic: no wall clock, no Math.random, no I/O. Noise is seeded.
// A source is an ES module: `import { ... } from "syrinx"`, `export const meta = { ... }`, and
// `export const stems = { ... }` -- one or more named layers, each a function returning samples,
// whole or as a stream the host pulls one block at a time. An optional default export combines
// them; without one the mix is their sum.
//
// Contract (PRELUDE_VERSION 3; every api-2 source is unchanged):
//   meta   = { name, duration, channels?, sampleRate?, seed?, loop?, api? }
//   stems  = { <name>({ sr, frames, duration, seed, channels, stem }) -> Output | Stream }
//   default({ sr, frames, duration, seed, channels, stems }) -> Output | MixStream   (optional)
//   Output    = Float32Array | number[] | [left, right]
//   Stream    = (offset, frames) => Output              called once per block of BLOCK_FRAMES, in order
//   MixStream = (offset, frames, stems) => Output       stems[name] = that layer's block as planes
// A mix that reads ctx.stems is whole-buffer; a mix stream gets its layers' blocks as its third
// argument and never reads ctx.stems. Inside a block, normalize, fade and place refuse: a stream
// cannot see the whole render.
//
// Style: per-sample objects with .next()/.process(), envelopes as functions of time, and a
// `render` helper for the common "one loop over every frame" shape. Unit conventions: seconds,
// Hz, linear gain (use db() to convert), phase in [0, 1).

const PRELUDE_VERSION = 3;
// Frames per block of a stream: a constant of the standard, so a block boundary can never leak
// into the samples on one host and not another. 85 ms at 48 kHz.
const BLOCK_FRAMES = 4096;
const TAU = Math.PI * 2;

// ---------------------------------------------------------------- scalar helpers

function clamp(x, lo, hi) { return x < lo ? lo : x > hi ? hi : x; }
function lerp(a, b, t) { return a + (b - a) * t; }
function db(decibels) { return Math.pow(10, decibels / 20); }
function mtof(midi) { return 440 * Math.pow(2, (midi - 69) / 12); }
function softclip(x) { return Math.tanh(x); }
function hardclip(x) { return x < -1 ? -1 : x > 1 ? 1 : x; }
function fold(x) {
  // Wavefolder: reflects anything outside [-1, 1] back in.
  x = (x + 1) % 4;
  if (x < 0) x += 4;
  return x < 2 ? x - 1 : 3 - x;
}

// FNV-1a over the arguments, as a seed. Two layers each writing `new Noise(ctx.seed)` get the
// same noise, because every stem is handed the same meta.seed; `hash(ctx.seed, "kick")` gives
// each voice its own draw, stable when stems are reordered or renamed around it.
const HASH_SCRATCH = new DataView(new ArrayBuffer(8));
function hash(...parts) {
  let h = 0x811c9dc5;
  const byte = (b) => { h = Math.imul(h ^ (b & 0xff), 0x01000193) >>> 0; };
  for (const part of parts) {
    if (typeof part === "number") {
      HASH_SCRATCH.setFloat64(0, part, true);
      for (let i = 0; i < 8; i++) byte(HASH_SCRATCH.getUint8(i));
    } else {
      const text = String(part);
      for (let i = 0; i < text.length; i++) {
        const c = text.charCodeAt(i);
        byte(c);
        byte(c >>> 8);
      }
    }
    // Separator, so hash("ab", "c") and hash("a", "bc") differ.
    byte(0);
  }
  return h >>> 0;
}

// ---------------------------------------------------------------- randomness (seeded, mulberry32)

class Random {
  constructor(seed = 0) { this.state = (seed | 0) >>> 0; }
  // [0, 1)
  next() {
    let t = (this.state = (this.state + 0x6d2b79f5) >>> 0);
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }
  // [lo, hi)
  range(lo, hi) { return lo + (hi - lo) * this.next(); }
  // [-1, 1)
  bipolar() { return this.next() * 2 - 1; }
  int(lo, hi) { return lo + Math.floor(this.next() * (hi - lo)); }
  chance(p) { return this.next() < p; }
  pick(array) { return array[this.int(0, array.length)]; }
}

// ---------------------------------------------------------------- oscillators

class Phasor {
  constructor(sr, phase = 0) { this.sr = sr; this.phase = phase; }
  // Advance by one sample at `freq` Hz; returns the phase BEFORE advancing, in [0, 1).
  next(freq) {
    const p = this.phase;
    this.phase += freq / this.sr;
    if (this.phase >= 1) this.phase -= Math.floor(this.phase);
    else if (this.phase < 0) this.phase -= Math.floor(this.phase);
    return p;
  }
}

class Osc {
  constructor(sr, shape, phase = 0) { this.phasor = new Phasor(sr, phase); this.shape = shape; this.width = 0.5; }
  static sine(sr, phase) { return new Osc(sr, Osc.shapes.sine, phase); }
  static saw(sr, phase) { return new Osc(sr, Osc.shapes.saw, phase); }
  static square(sr, phase) { return new Osc(sr, Osc.shapes.square, phase); }
  static tri(sr, phase) { return new Osc(sr, Osc.shapes.tri, phase); }
  static pulse(sr, width = 0.5, phase) { const o = new Osc(sr, Osc.shapes.pulse, phase); o.width = width; return o; }
  next(freq) { return this.shape(this.phasor.next(freq), this.width); }
}
Osc.shapes = {
  sine: (p) => Math.sin(p * TAU),
  saw: (p) => p * 2 - 1,
  square: (p) => (p < 0.5 ? 1 : -1),
  tri: (p) => (p < 0.5 ? p * 4 - 1 : 3 - p * 4),
  pulse: (p, w) => (p < w ? 1 : -1),
};

// Band-limited saw/square via PolyBLEP — use these when the naive ones alias audibly.
class BlepOsc {
  constructor(sr, shape, phase = 0) { this.phasor = new Phasor(sr, phase); this.shape = shape; }
  static saw(sr, phase) { return new BlepOsc(sr, "saw", phase); }
  static square(sr, phase) { return new BlepOsc(sr, "square", phase); }
  next(freq) {
    const dt = freq / this.phasor.sr;
    const p = this.phasor.next(freq);
    if (this.shape === "saw") return p * 2 - 1 - BlepOsc.blep(p, dt);
    let v = p < 0.5 ? 1 : -1;
    v += BlepOsc.blep(p, dt);
    v -= BlepOsc.blep((p + 0.5) % 1, dt);
    return v;
  }
  static blep(t, dt) {
    if (t < dt) { t /= dt; return t + t - t * t - 1; }
    if (t > 1 - dt) { t = (t - 1) / dt; return t * t + t + t + 1; }
    return 0;
  }
}

// ---------------------------------------------------------------- noise

class Noise {
  constructor(seed = 0) { this.rng = new Random(seed); this.b0 = 0; this.b1 = 0; this.b2 = 0; }
  // Uniform white noise in [-1, 1).
  white() { return this.rng.bipolar(); }
  // Pink noise (Paul Kellet's economy 3-pole approximation), roughly [-1, 1].
  pink() {
    const w = this.white();
    this.b0 = 0.99765 * this.b0 + w * 0.0990460;
    this.b1 = 0.96300 * this.b1 + w * 0.2965164;
    this.b2 = 0.57000 * this.b2 + w * 1.0526913;
    return (this.b0 + this.b1 + this.b2 + w * 0.1848) * 0.25;
  }
  // Brown noise: leaky-integrated white.
  brown() {
    this.b0 = clamp(this.b0 + this.white() * 0.02, -1, 1);
    return this.b0;
  }
}

// ---------------------------------------------------------------- envelopes (functions of time in seconds)

const Env = {
  // Linear attack, exponential decay to silence.
  ad(attack, decay) {
    return (t) => (t < 0 ? 0 : t < attack ? t / attack : Math.exp(-(t - attack) / decay));
  },
  // Linear attack, exponential decay to sustain, hold until `gate`, then exponential release.
  adsr(attack, decay, sustain, release, gate) {
    return (t) => {
      if (t < 0) return 0;
      if (t < attack) return t / attack;
      if (t < gate) return sustain + (1 - sustain) * Math.exp(-(t - attack) / decay);
      const atGate = sustain + (1 - sustain) * Math.exp(-(gate - attack) / decay);
      return atGate * Math.exp(-(t - gate) / release);
    };
  },
  // exp(-t / tau)
  exp(tau) { return (t) => (t < 0 ? 0 : Math.exp(-t / tau)); },
  // Straight line from `from` to `to` over `duration`, clamped at both ends.
  line(from, to, duration) { return (t) => lerp(from, to, clamp(t / duration, 0, 1)); },
  // Exponential sweep from `from` to `to` over `duration` (both must be > 0), clamped.
  sweep(from, to, duration) {
    const ratio = to / from;
    return (t) => from * Math.pow(ratio, clamp(t / duration, 0, 1));
  },
  // Hold 1 until `duration`, then linear fade to 0 over `fade`.
  gate(duration, fade) {
    return (t) => (t < 0 ? 0 : t < duration ? 1 : clamp(1 - (t - duration) / fade, 0, 1));
  },
  // Piecewise-linear through [[t0, v0], [t1, v1], ...] (times ascending).
  points(pts) {
    return (t) => {
      if (t <= pts[0][0]) return pts[0][1];
      for (let i = 1; i < pts.length; i++) {
        if (t < pts[i][0]) {
          const [t0, v0] = pts[i - 1];
          const [t1, v1] = pts[i];
          return lerp(v0, v1, (t - t0) / (t1 - t0));
        }
      }
      return pts[pts.length - 1][1];
    };
  },
};

// ---------------------------------------------------------------- filters

// One-pole low/high-pass. Cheap, 6 dB/oct, no resonance.
class OnePole {
  constructor(sr) { this.sr = sr; this.z = 0; }
  lp(x, cutoff) {
    const a = 1 - Math.exp(-TAU * cutoff / this.sr);
    this.z += a * (x - this.z);
    return this.z;
  }
  hp(x, cutoff) { return x - this.lp(x, cutoff); }
}

// RBJ cookbook biquad, transposed direct form II. Types: lowpass, highpass, bandpass, notch,
// allpass, peak, lowshelf, highshelf. Call set() again to modulate (it's cheap).
class Biquad {
  constructor(sr) {
    this.sr = sr;
    this.b0 = 1; this.b1 = 0; this.b2 = 0; this.a1 = 0; this.a2 = 0;
    this.z1 = 0; this.z2 = 0;
  }
  static lowpass(sr, freq, q = Math.SQRT1_2) { return new Biquad(sr).set("lowpass", freq, q); }
  static highpass(sr, freq, q = Math.SQRT1_2) { return new Biquad(sr).set("highpass", freq, q); }
  static bandpass(sr, freq, q = 1) { return new Biquad(sr).set("bandpass", freq, q); }
  static notch(sr, freq, q = 1) { return new Biquad(sr).set("notch", freq, q); }
  static peak(sr, freq, q = 1, gainDb = 0) { return new Biquad(sr).set("peak", freq, q, gainDb); }
  set(type, freq, q = Math.SQRT1_2, gainDb = 0) {
    freq = clamp(freq, 1, this.sr * 0.499);
    q = Math.max(q, 1e-4);
    const w0 = TAU * freq / this.sr;
    const cos = Math.cos(w0), sin = Math.sin(w0);
    const alpha = sin / (2 * q);
    const A = Math.pow(10, gainDb / 40);
    let b0, b1, b2, a0, a1, a2;
    switch (type) {
      case "lowpass":
        b0 = (1 - cos) / 2; b1 = 1 - cos; b2 = (1 - cos) / 2;
        a0 = 1 + alpha; a1 = -2 * cos; a2 = 1 - alpha; break;
      case "highpass":
        b0 = (1 + cos) / 2; b1 = -(1 + cos); b2 = (1 + cos) / 2;
        a0 = 1 + alpha; a1 = -2 * cos; a2 = 1 - alpha; break;
      case "bandpass":
        b0 = alpha; b1 = 0; b2 = -alpha;
        a0 = 1 + alpha; a1 = -2 * cos; a2 = 1 - alpha; break;
      case "notch":
        b0 = 1; b1 = -2 * cos; b2 = 1;
        a0 = 1 + alpha; a1 = -2 * cos; a2 = 1 - alpha; break;
      case "allpass":
        b0 = 1 - alpha; b1 = -2 * cos; b2 = 1 + alpha;
        a0 = 1 + alpha; a1 = -2 * cos; a2 = 1 - alpha; break;
      case "peak":
        b0 = 1 + alpha * A; b1 = -2 * cos; b2 = 1 - alpha * A;
        a0 = 1 + alpha / A; a1 = -2 * cos; a2 = 1 - alpha / A; break;
      case "lowshelf": {
        const s = 2 * Math.sqrt(A) * alpha;
        b0 = A * ((A + 1) - (A - 1) * cos + s); b1 = 2 * A * ((A - 1) - (A + 1) * cos); b2 = A * ((A + 1) - (A - 1) * cos - s);
        a0 = (A + 1) + (A - 1) * cos + s; a1 = -2 * ((A - 1) + (A + 1) * cos); a2 = (A + 1) + (A - 1) * cos - s; break;
      }
      case "highshelf": {
        const s = 2 * Math.sqrt(A) * alpha;
        b0 = A * ((A + 1) + (A - 1) * cos + s); b1 = -2 * A * ((A - 1) + (A + 1) * cos); b2 = A * ((A + 1) + (A - 1) * cos - s);
        a0 = (A + 1) - (A - 1) * cos + s; a1 = 2 * ((A - 1) - (A + 1) * cos); a2 = (A + 1) - (A - 1) * cos - s; break;
      }
      default: throw new Error(`Biquad: unknown type "${type}"`);
    }
    this.b0 = b0 / a0; this.b1 = b1 / a0; this.b2 = b2 / a0; this.a1 = a1 / a0; this.a2 = a2 / a0;
    return this;
  }
  process(x) {
    const y = this.b0 * x + this.z1;
    this.z1 = this.b1 * x - this.a1 * y + this.z2;
    this.z2 = this.b2 * x - this.a2 * y;
    return y;
  }
}

// State-variable filter (Chamberlin). Resonance 0..1; outputs all three responses per sample.
class Svf {
  constructor(sr) { this.sr = sr; this.low = 0; this.band = 0; this.high = 0; this.f = 0; this.q = 1; }
  set(cutoff, resonance = 0) {
    this.f = 2 * Math.sin(Math.PI * clamp(cutoff, 1, this.sr * 0.25) / this.sr);
    this.q = 1 - clamp(resonance, 0, 0.995);
    return this;
  }
  process(x) {
    this.low += this.f * this.band;
    this.high = x - this.low - this.q * this.band;
    this.band += this.f * this.high;
    return this.low;
  }
}

// ---------------------------------------------------------------- delay lines and reverb parts

class Delay {
  constructor(sr, maxSeconds) {
    this.sr = sr;
    this.buffer = new Float32Array(Math.max(2, Math.ceil(maxSeconds * sr) + 1));
    this.pos = 0;
  }
  // Read `seconds` back from the write head with linear interpolation.
  read(seconds) {
    const n = this.buffer.length;
    let d = seconds * this.sr;
    if (d < 0) d = 0; else if (d > n - 2) d = n - 2;
    const i = Math.floor(d), frac = d - i;
    const a = this.buffer[(this.pos - i - 1 + n * 2) % n];
    const b = this.buffer[(this.pos - i - 2 + n * 2) % n];
    return a + (b - a) * frac;
  }
  write(x) {
    this.buffer[this.pos] = x;
    this.pos = (this.pos + 1) % this.buffer.length;
  }
  // Convenience: write then read `seconds` back.
  process(x, seconds) { this.write(x); return this.read(seconds); }
}

// Feedback comb with a one-pole damping filter in the loop (Schroeder/Freeverb style).
class Comb {
  constructor(sr, seconds, feedback = 0.8, damp = 0.2) {
    this.delay = new Delay(sr, seconds);
    this.seconds = seconds; this.feedback = feedback; this.damp = damp; this.store = 0;
  }
  process(x) {
    const out = this.delay.read(this.seconds);
    this.store = out * (1 - this.damp) + this.store * this.damp;
    this.delay.write(x + this.store * this.feedback);
    return out;
  }
}

class Allpass {
  constructor(sr, seconds, gain = 0.5) {
    this.delay = new Delay(sr, seconds);
    this.seconds = seconds; this.gain = gain;
  }
  process(x) {
    const d = this.delay.read(this.seconds);
    const out = -x + d;
    this.delay.write(x + d * this.gain);
    return out;
  }
}

// Small Schroeder reverb assembled from the parts above: 4 combs in parallel into 2 allpasses.
class Reverb {
  constructor(sr, { size = 1, damp = 0.3, feedback = 0.84 } = {}) {
    const combTimes = [0.0297, 0.0371, 0.0411, 0.0437].map((t) => t * size);
    this.combs = combTimes.map((t) => new Comb(sr, t, feedback, damp));
    this.allpasses = [new Allpass(sr, 0.005 * size, 0.5), new Allpass(sr, 0.0017 * size, 0.5)];
  }
  process(x) {
    let y = 0;
    for (const c of this.combs) y += c.process(x);
    y *= 0.25;
    for (const a of this.allpasses) y = a.process(y);
    return y;
  }
}

// ---------------------------------------------------------------- buffers

// Run `fn(t, i)` once per frame and collect the result. The workhorse for simple sources.
function render(ctx, fn) {
  const out = new Float32Array(ctx.frames);
  const sr = ctx.sr;
  for (let i = 0; i < ctx.frames; i++) out[i] = fn(i / sr, i);
  return out;
}

// render(), one block at a time: returns a stream the host calls once per block, in order, and
// fn sees the same t and i in the same order as under render(), so the samples are identical.
function stream(ctx, fn) {
  const sr = ctx.sr;
  return (offset, frames) => {
    const out = new Float32Array(frames);
    for (let k = 0; k < frames; k++) {
      const i = offset + k;
      out[k] = fn(i / sr, i);
    }
    return out;
  };
}

// The helpers below need the whole render. run.js raises `__syrinx.block` around every block
// call of a stream; inside one they refuse, naming what to do instead.
function wholeRenderOnly(name, fix) {
  if (typeof __syrinx !== "undefined" && __syrinx.block) {
    throw new Error(`${name} needs the whole render and cannot run inside a stream: ${fix}`);
  }
}

function mix(...buffers) {
  const n = Math.max(...buffers.map((b) => b.length));
  const out = new Float32Array(n);
  for (const b of buffers) for (let i = 0; i < b.length; i++) out[i] += b[i];
  return out;
}

function gain(buffer, g) {
  const out = new Float32Array(buffer.length);
  for (let i = 0; i < buffer.length; i++) out[i] = buffer[i] * g;
  return out;
}

// Scale so the absolute peak hits `peak` (default -1 dBFS). Silence is left alone.
function normalize(buffer, peak = db(-1)) {
  wholeRenderOnly("normalize", "a stream cannot see its peak; use a limiter, or a fixed gain (gain(block, g))");
  let max = 0;
  for (let i = 0; i < buffer.length; i++) { const a = Math.abs(buffer[i]); if (a > max) max = a; }
  return max > 0 ? gain(buffer, peak / max) : buffer;
}

// Linear fade-in over `fadeIn` seconds and fade-out over `fadeOut` seconds.
function fade(ctx, buffer, fadeIn, fadeOut) {
  wholeRenderOnly("fade", "shape the level from the absolute time instead: multiply by Env.line(0, 1, fadeIn)(t) and Env.gate(duration - fadeOut, fadeOut)(t)");
  const out = new Float32Array(buffer.length);
  const inN = Math.floor(fadeIn * ctx.sr), outN = Math.floor(fadeOut * ctx.sr);
  for (let i = 0; i < buffer.length; i++) {
    let g = 1;
    if (i < inN) g *= i / inN;
    const fromEnd = buffer.length - 1 - i;
    if (fromEnd < outN) g *= fromEnd / outN;
    out[i] = buffer[i] * g;
  }
  return out;
}

// Constant-power pan of a mono buffer. pan in [-1, 1]. Returns [left, right].
function pan(buffer, position = 0) {
  const angle = (clamp(position, -1, 1) + 1) * Math.PI / 4;
  const l = Math.cos(angle), r = Math.sin(angle);
  return [gain(buffer, l), gain(buffer, r)];
}

// Place `buffer` into a new buffer of `ctx.frames` starting at `at` seconds.
function place(ctx, buffer, at) {
  wholeRenderOnly("place", "write the buffer into the block from index Math.round(at * sr) - offset, clipped to the block");
  const out = new Float32Array(ctx.frames);
  const start = Math.round(at * ctx.sr);
  for (let i = 0; i < buffer.length; i++) {
    const j = start + i;
    if (j >= 0 && j < out.length) out[j] = buffer[i];
  }
  return out;
}

// Apply a per-sample processor (anything with .process(x)) over a buffer.
function filter(buffer, processor) {
  const out = new Float32Array(buffer.length);
  for (let i = 0; i < buffer.length; i++) out[i] = processor.process(buffer[i]);
  return out;
}

// ---------------------------------------------------------------- exports

export {
  PRELUDE_VERSION,
  BLOCK_FRAMES,
  TAU,
  clamp, lerp, db, mtof, softclip, hardclip, fold, hash,
  Random,
  Phasor, Osc, BlepOsc,
  Noise,
  Env,
  OnePole, Biquad, Svf,
  Delay, Comb, Allpass, Reverb,
  render, stream, mix, gain, normalize, fade, pan, place, filter,
};
