// syrinx-framework: fx -- the effects a mix needs.
//
// dsp.js's Reverb is a four-comb Schroeder: fine behind a laser, thin under a pad. A track needs a
// real reverb (Freeverb), a tempo delay, chorus, a phaser, a bus compressor and a look-ahead
// limiter, EQ, saturation and filter sweeps. Everything here is a plain object with per-sample
// `process`, stereo ones leave the result on `.l` / `.r` so nothing allocates inside a loop; the
// `...Into` and `...Stereo` helpers run one over a pair or a layer.

import { TAU, Biquad, clamp, db } from "./dsp.js";
import { isLayer } from "./music.js";

// Every in-place helper below takes a pair or a layer (see music.js). On a layer the work is
// recorded as a block job that does the pair's arithmetic over the block, index by index, with
// whatever state the effect carries persisting from block to block -- the same samples in the
// same order, so the bytes match. The exception is `limitStereo`, whose look-ahead needs the
// future: on a layer it delays the signal by its look-ahead plus a margin instead (see there).

// ---------------------------------------------------------------- reverb (Freeverb)
//
// Jezar's Freeverb: eight parallel lowpass-feedback combs into four series allpasses per channel,
// the right channel's lines 23 samples longer so the two tails decorrelate. Tunings are given at
// 44.1 kHz and scaled. Wet only; the caller mixes.

const COMBS = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES = [556, 441, 341, 225];
const SPREAD = 23;

class Comb {
  constructor(n, feedback, damp) {
    this.buf = new Float32Array(n); this.i = 0; this.store = 0;
    this.feedback = feedback; this.damp1 = damp; this.damp2 = 1 - damp;
  }
  process(x) {
    const out = this.buf[this.i];
    this.store = out * this.damp2 + this.store * this.damp1;
    this.buf[this.i] = x + this.store * this.feedback;
    if (++this.i === this.buf.length) this.i = 0;
    return out;
  }
}

class Allpass {
  constructor(n) { this.buf = new Float32Array(n); this.i = 0; }
  process(x) {
    const b = this.buf[this.i];
    this.buf[this.i] = x + b * 0.5;
    if (++this.i === this.buf.length) this.i = 0;
    return b - x;
  }
}

export class Freeverb {
  /** room 0..1 (tail length), damp 0..1 (high loss), width 0..1, predelay seconds. */
  constructor(sr, opts) {
    const o = opts || {};
    const room = o.room === undefined ? 0.85 : o.room;
    const damp = o.damp === undefined ? 0.4 : o.damp;
    const width = o.width === undefined ? 1 : o.width;
    const pre = o.predelay === undefined ? 0 : o.predelay;
    const scale = sr / 44100;
    const fb = 0.7 + 0.28 * room;
    const dp = damp * 0.4;
    this.cL = []; this.cR = []; this.aL = []; this.aR = [];
    for (let k = 0; k < COMBS.length; k++) {
      const n = Math.round(COMBS[k] * scale);
      this.cL.push(new Comb(n, fb, dp));
      this.cR.push(new Comb(n + SPREAD, fb, dp));
    }
    for (let k = 0; k < ALLPASSES.length; k++) {
      const n = Math.round(ALLPASSES[k] * scale);
      this.aL.push(new Allpass(n));
      this.aR.push(new Allpass(n + SPREAD));
    }
    this.wet1 = width * 0.5 + 0.5;
    this.wet2 = (1 - width) * 0.5;
    this.pre = new Float32Array(Math.max(1, Math.round(pre * sr)));
    this.pi = 0;
    this.usePre = pre > 0;
    this.l = 0; this.r = 0;
  }
  process(l, r) {
    let x = (l + r) * 0.03;
    if (this.usePre) {
      const y = this.pre[this.pi];
      this.pre[this.pi] = x;
      if (++this.pi === this.pre.length) this.pi = 0;
      x = y;
    }
    let ol = 0, or = 0;
    for (let k = 0; k < 8; k++) { ol += this.cL[k].process(x); or += this.cR[k].process(x); }
    for (let k = 0; k < 4; k++) { ol = this.aL[k].process(ol); or = this.aR[k].process(or); }
    this.l = ol * this.wet1 + or * this.wet2;
    this.r = or * this.wet1 + ol * this.wet2;
  }
}

// ---------------------------------------------------------------- tempo delay
//
// Ping-pong: the input enters the left line, the left line feeds the right, the right feeds the
// left. Highpass and lowpass in each feedback path so repeats thin out the way tape does.

export class PingPong {
  constructor(sr, opts) {
    const o = opts || {};
    const n = Math.max(2, Math.round((o.time === undefined ? 0.375 : o.time) * sr));
    this.bL = new Float32Array(n); this.bR = new Float32Array(n); this.i = 0;
    this.fb = o.feedback === undefined ? 0.4 : o.feedback;
    this.hpL = Biquad.highpass(sr, o.hp === undefined ? 250 : o.hp, 0.7);
    this.hpR = Biquad.highpass(sr, o.hp === undefined ? 250 : o.hp, 0.7);
    this.lpL = Biquad.lowpass(sr, o.lp === undefined ? 5000 : o.lp, 0.7);
    this.lpR = Biquad.lowpass(sr, o.lp === undefined ? 5000 : o.lp, 0.7);
    this.l = 0; this.r = 0;
  }
  process(l, r) {
    const x = (l + r) * 0.5;
    const oL = this.bL[this.i];
    const oR = this.bR[this.i];
    this.bL[this.i] = x + this.lpL.process(this.hpL.process(oR)) * this.fb;
    this.bR[this.i] = this.lpR.process(this.hpR.process(oL)) * this.fb;
    if (++this.i === this.bL.length) this.i = 0;
    this.l = oL; this.r = oR;
  }
}

// ---------------------------------------------------------------- chorus
//
// Two modulated taps per side, the sides moving in opposite directions, so a mono voice comes
// out wide and slowly shimmering. Mono in, stereo out.

export class Chorus {
  constructor(sr, opts) {
    const o = opts || {};
    this.sr = sr;
    this.rate = o.rate === undefined ? 0.55 : o.rate;
    this.depth = (o.depth === undefined ? 0.0022 : o.depth) * sr;
    this.base = (o.base === undefined ? 0.011 : o.base) * sr;
    this.mix = o.mix === undefined ? 0.45 : o.mix;
    this.phase = o.phase === undefined ? 0 : o.phase;
    const n = Math.ceil(this.base * 1.7 + this.depth * 2 + 4);
    this.buf = new Float32Array(n); this.i = 0; this.k = 0;
    this.l = 0; this.r = 0;
  }
  read(d) {
    const n = this.buf.length;
    const j = Math.floor(d), f = d - j;
    const a = this.buf[(this.i - j - 1 + 2 * n) % n];
    const b = this.buf[(this.i - j - 2 + 2 * n) % n];
    return a + (b - a) * f;
  }
  process(x) {
    this.buf[this.i] = x;
    const t = this.k / this.sr;
    const la = Math.sin(TAU * this.rate * t + this.phase);
    const lb = Math.sin(TAU * this.rate * 1.37 * t + this.phase + 2.1);
    const d = this.depth, b = this.base;
    const wl = this.read(b + d * la) + this.read(b * 1.6 - d * lb);
    const wr = this.read(b - d * la) + this.read(b * 1.6 + d * lb);
    if (++this.i === this.buf.length) this.i = 0;
    this.k++;
    const dry = x * (1 - this.mix), w = this.mix * 0.7;
    this.l = dry + wl * w;
    this.r = dry + wr * w;
  }
}

// ---------------------------------------------------------------- phaser
//
// A chain of first-order allpasses swept together by one slow LFO, with feedback from the end
// of the chain. Coefficients update every 32 samples; the sweep is far slower than that.

export class Phaser {
  constructor(sr, opts) {
    const o = opts || {};
    this.sr = sr;
    this.stages = o.stages === undefined ? 6 : o.stages;
    this.rate = o.rate === undefined ? 0.12 : o.rate;
    this.min = o.min === undefined ? 300 : o.min;
    this.max = o.max === undefined ? 3000 : o.max;
    this.fb = o.feedback === undefined ? 0.35 : o.feedback;
    this.mix = o.mix === undefined ? 0.5 : o.mix;
    this.phase = o.phase === undefined ? 0 : o.phase;
    this.x1 = new Float64Array(this.stages);
    this.y1 = new Float64Array(this.stages);
    this.a = 0; this.k = 0; this.last = 0;
  }
  process(x) {
    if ((this.k & 31) === 0) {
      const t = this.k / this.sr;
      const u = 0.5 + 0.5 * Math.sin(TAU * this.rate * t + this.phase);
      const f = this.min * Math.pow(this.max / this.min, u);
      const w = Math.tan(Math.PI * f / this.sr);
      this.a = (w - 1) / (w + 1);
    }
    this.k++;
    let y = x + this.last * this.fb;
    const a = this.a;
    for (let s = 0; s < this.stages; s++) {
      const out = a * y + this.x1[s] - a * this.y1[s];
      this.x1[s] = y; this.y1[s] = out; y = out;
    }
    this.last = y;
    return x * (1 - this.mix) + y * this.mix;
  }
}

// ---------------------------------------------------------------- dynamics

/** Linked stereo compressor in place: threshold dBFS, soft knee, feed-forward peak detection. */
export function compressStereo(pair, sr, opts) {
  const o = opts || {};
  const thr = o.threshold === undefined ? -14 : o.threshold;
  const ratio = o.ratio === undefined ? 2.5 : o.ratio;
  const knee = o.knee === undefined ? 6 : o.knee;
  const aC = 1 - Math.exp(-1 / ((o.attack === undefined ? 0.01 : o.attack) * sr));
  const rC = 1 - Math.exp(-1 / ((o.release === undefined ? 0.15 : o.release) * sr));
  const makeup = db(o.makeup === undefined ? 0 : o.makeup);
  const slope = 1 - 1 / ratio;
  let env = 0;
  const run = (L, R, n) => {
    for (let i = 0; i < n; i++) {
      const l = L[i], r = R[i];
      const e = Math.max(l < 0 ? -l : l, r < 0 ? -r : r);
      env += (e - env) * (e > env ? aC : rC);
      const x = 20 * Math.log10(env + 1e-9) - thr;
      let gr = 0;
      if (x > knee * 0.5) gr = x * slope;
      else if (x > -knee * 0.5) { const d = x + knee * 0.5; gr = d * d / (2 * knee) * slope; }
      const g = (gr === 0 ? 1 : db(-gr)) * makeup;
      L[i] = l * g; R[i] = r * g;
    }
  };
  if (isLayer(pair)) { pair.jobs.push((planes, offset, n) => run(planes[0], planes[1], n)); return pair; }
  run(pair[0], pair[1], pair[0].length);
  return pair;
}

/**
 * Look-ahead peak limiter in place. Because the whole buffer is here, the look-ahead is a
 * backward pass: gain may fall no faster than one ramp per `lookahead` seconds ahead of a peak,
 * so it is already down when the peak arrives, and recovers exponentially after it.
 */
export function limitStereo(pair, sr, opts) {
  const o = opts || {};
  const ceil = o.ceiling === undefined ? db(-0.8) : o.ceiling;
  const la = Math.max(1, Math.round((o.lookahead === undefined ? 0.0015 : o.lookahead) * sr));
  const rC = 1 - Math.exp(-1 / ((o.release === undefined ? 0.06 : o.release) * sr));
  if (isLayer(pair)) { pair.jobs.push(limitStream(ceil, la, rC)); return pair; }
  const L = pair[0], R = pair[1];
  const n = L.length;
  const g = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    const l = L[i], r = R[i];
    const p = Math.max(l < 0 ? -l : l, r < 0 ? -r : r);
    g[i] = p > ceil ? ceil / p : 1;
  }
  const step = 1 / la;
  for (let i = n - 2; i >= 0; i--) { const v = g[i + 1] + step; if (v < g[i]) g[i] = v; }
  let prev = 1;
  for (let i = 0; i < n; i++) {
    const rec = prev + (1 - prev) * rC;
    const v = g[i] < rec ? g[i] : rec;
    L[i] = L[i] * v; R[i] = R[i] * v;
    prev = v;
  }
  return pair;
}

/**
 * The limiter as a block job: the same three passes, over a window of the most recent input,
 * with the output DELAYED by `la + 8` frames. The backward pass over a whole buffer lets a gain
 * fall before a peak that is still up to `la` frames away; in a block that future is not here
 * yet, so the job holds every frame back until the frames that could lower its gain have
 * arrived. Below the top `la` frames (plus eight for rounding) the window's backward pass reads
 * exactly what the whole-buffer pass would have, so what comes out is the whole-buffer limiter
 * shifted by the delay; the final `la + 8` frames of the input never come out.
 */
function limitStream(ceil, la, rC) {
  const step = 1 / la;
  const D = la + 8;
  const CAP = 3 * 4096 + D;
  const inL = new Float32Array(CAP), inR = new Float32Array(CAP), raw = new Float32Array(CAP), g = new Float32Array(CAP);
  let winStart = -1; // absolute frame of inL[0]: the first block's, which is not 0 after a restart
  let have = 0;      // frames held: [winStart, winStart + have)
  let prev = 1;      // the forward pass's release state, across blocks
  return function (planes, offset, n) {
    const L = planes[0], R = planes[1];
    if (winStart < 0) winStart = offset;
    if (have + n > CAP) {
      // Drop what the outputs of this block can no longer reach: everything before the first
      // frame this block emits, minus nothing (the forward pass has consumed it).
      const keepFrom = Math.max(0, offset - D) - winStart;
      const drop = Math.min(have, Math.max(0, keepFrom));
      inL.copyWithin(0, drop, have); inR.copyWithin(0, drop, have); raw.copyWithin(0, drop, have);
      winStart += drop; have -= drop;
    }
    for (let k = 0; k < n; k++) {
      const l = L[k], r = R[k];
      const p = Math.max(l < 0 ? -l : l, r < 0 ? -r : r);
      inL[have + k] = l; inR[have + k] = r;
      raw[have + k] = p > ceil ? ceil / p : 1;
    }
    have += n;
    // The backward pass over the window, from its top, as the whole pass would from the end.
    g[have - 1] = raw[have - 1];
    for (let i = have - 2; i >= 0; i--) {
      g[i] = raw[i];
      const v = g[i + 1] + step;
      if (v < g[i]) g[i] = v;
    }
    // The forward pass, in order, over the frames this block emits: input frame j - D for
    // output frame j. Before the window (the first D frames of a start or a restart) the delay
    // line is empty: silence.
    for (let k = 0; k < n; k++) {
      const src = offset + k - D;
      if (src < winStart) { L[k] = 0; R[k] = 0; continue; }
      const idx = src - winStart;
      const rec = prev + (1 - prev) * rC;
      const v = g[idx] < rec ? g[idx] : rec;
      L[k] = inL[idx] * v; R[k] = inR[idx] * v;
      prev = v;
    }
  };
}

// ---------------------------------------------------------------- stereo helpers

/** Run a fresh biquad per channel over a pair, in place. */
export function eqStereo(pair, sr, type, freq, q, gainDb) {
  const bl = new Biquad(sr).set(type, freq, q, gainDb);
  const br = new Biquad(sr).set(type, freq, q, gainDb);
  const run = (L, R, n) => { for (let i = 0; i < n; i++) { L[i] = bl.process(L[i]); R[i] = br.process(R[i]); } };
  if (isLayer(pair)) { pair.jobs.push((planes, offset, n) => run(planes[0], planes[1], n)); return pair; }
  run(pair[0], pair[1], pair[0].length);
  return pair;
}

/** pair *= curve (Float32Array of the same length), in place. */
export function applyCurve(pair, curve) {
  if (isLayer(pair)) {
    pair.jobs.push((planes, offset, n) => {
      for (let c = 0; c < planes.length; c++) { const a = planes[c]; for (let k = 0; k < n; k++) a[k] *= curve[offset + k]; }
    });
    return pair;
  }
  const L = pair[0], R = pair[1];
  for (let i = 0; i < L.length; i++) { const g = curve[i]; L[i] *= g; R[i] *= g; }
  return pair;
}

/** pair *= g, in place. */
export function scale(pair, g) {
  if (isLayer(pair)) {
    pair.jobs.push((planes, offset, n) => { for (let c = 0; c < planes.length; c++) { const a = planes[c]; for (let k = 0; k < n; k++) a[k] *= g; } });
    return pair;
  }
  const L = pair[0], R = pair[1];
  for (let i = 0; i < L.length; i++) { L[i] *= g; R[i] *= g; }
  return pair;
}

/** Add a reverb to a pair in place: out = dry + wet * reverb(dry * send). `sendCurve` optional. */
export function reverbInto(pair, rv, send, wet, sendCurve) {
  const run = (L, R, n, offset) => {
    for (let k = 0; k < n; k++) {
      const l = L[k], r = R[k];
      const s = sendCurve === undefined ? send : send * sendCurve[offset + k];
      rv.process(l * s, r * s);
      L[k] = l + rv.l * wet; R[k] = r + rv.r * wet;
    }
  };
  if (isLayer(pair)) { pair.jobs.push((planes, offset, n) => run(planes[0], planes[1], n, offset)); return pair; }
  run(pair[0], pair[1], pair[0].length, 0);
  return pair;
}

/** Add a stereo delay to a pair in place: out = dry + wet * delay(dry * send). */
export function delayInto(pair, dl, send, wet) {
  const run = (L, R, n) => {
    for (let i = 0; i < n; i++) {
      const l = L[i], r = R[i];
      dl.process(l * send, r * send);
      L[i] = l + dl.l * wet; R[i] = r + dl.r * wet;
    }
  };
  if (isLayer(pair)) { pair.jobs.push((planes, offset, n) => run(planes[0], planes[1], n)); return pair; }
  run(pair[0], pair[1], pair[0].length);
  return pair;
}

/** Chorus a pair in place: each channel's mono sum goes through the chorus, which is stereo out. */
export function chorusInto(pair, ch) {
  const run = (L, R, n) => {
    for (let i = 0; i < n; i++) {
      ch.process((L[i] + R[i]) * 0.5);
      L[i] = ch.l; R[i] = ch.r;
    }
  };
  if (isLayer(pair)) { pair.jobs.push((planes, offset, n) => run(planes[0], planes[1], n)); return pair; }
  run(pair[0], pair[1], pair[0].length);
  return pair;
}

/** Phaser both channels through separate instances, in place. */
export function phaserInto(pair, pl, pr) {
  const run = (L, R, n) => { for (let i = 0; i < n; i++) { L[i] = pl.process(L[i]); R[i] = pr.process(R[i]); } };
  if (isLayer(pair)) { pair.jobs.push((planes, offset, n) => run(planes[0], planes[1], n)); return pair; }
  run(pair[0], pair[1], pair[0].length);
  return pair;
}

/** Soft saturation in place: unity gain for small signals, peaks bent down to tanh(drive)/drive. */
export function saturate(pair, drive) {
  const inv = 1 / drive;
  if (isLayer(pair)) {
    pair.jobs.push((planes, offset, n) => { for (let c = 0; c < planes.length; c++) { const a = planes[c]; for (let k = 0; k < n; k++) a[k] = Math.tanh(a[k] * drive) * inv; } });
    return pair;
  }
  const L = pair[0], R = pair[1];
  for (let i = 0; i < L.length; i++) { L[i] = Math.tanh(L[i] * drive) * inv; R[i] = Math.tanh(R[i] * drive) * inv; }
  return pair;
}

/** A variable lowpass over a pair: cutoff from a curve (Hz per sample), coefficients every 32 samples. */
export function sweepLowpass(pair, sr, cutoffCurve, q) {
  const bl = new Biquad(sr), br = new Biquad(sr);
  const qq = q === undefined ? 0.75 : q;
  const run = (L, R, n, offset) => {
    for (let k = 0; k < n; k++) {
      const i = offset + k;
      if ((i & 31) === 0) {
        const f = clamp(cutoffCurve[i], 40, 20000);
        bl.set("lowpass", f, qq); br.set("lowpass", f, qq);
      }
      L[k] = bl.process(L[k]); R[k] = br.process(R[k]);
    }
  };
  if (isLayer(pair)) { pair.jobs.push((planes, offset, n) => run(planes[0], planes[1], n, offset)); return pair; }
  run(pair[0], pair[1], pair[0].length, 0);
  return pair;
}
