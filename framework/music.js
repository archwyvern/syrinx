// syrinx-framework: music -- placing notes in time, and the stereo layer a track is built on.
//
// dsp.js gives you buffers and voices but no way to place many notes in time. `place` cannot do
// it: it allocates a track-length buffer per call, so 1000 notes become 1000 copies of the whole
// track. Everything here writes into ONE shared stereo pair instead, touching only the frames a
// note actually occupies. A layer (`layer(ctx)`, `mono(ctx)`) is the same pair as a stream: the
// same calls queue their work, and it runs on each block as the host pulls it; `each` is the loop
// over samples, and `input(mix, name)` reads a layer inside a streaming master. Also here: note
// names, grids, patterns, gate curves, arps, melodies from rows, gain curves and ducking.

import { Random, hash } from "syrinx";
import { TAU, clamp } from "./dsp.js";

const NONE = {};

/** A fresh [left, right] pair sized to the track: the whole-buffer form, rendered at setup. */
export function stereo(ctx) {
  return [new Float32Array(ctx.frames), new Float32Array(ctx.frames)];
}

// ---------------------------------------------------------------- layers (the streaming form)
//
// A layer is a pair that does not exist yet. The calls that would have written into a pair are
// recorded, and the layer computes itself one block at a time when the host pulls it, so a
// track starts playing before it has finished rendering. Every helper in this file, in fx.js
// and in album.js accepts a layer wherever it accepts a pair; a stem streams by asking for
// `layer(ctx)` instead of `stereo(ctx)` and returning it (a layer IS the block function the
// contract asks for). The arithmetic is the pair's, in the pair's order, index by index, so a
// layer renders the same bytes as the pair it replaces; the lockfile's per-layer digests are the
// proof, and the one thing that cannot be reproduced -- a look-ahead limiter -- is delayed by its
// look-ahead instead (see fx.js).
//
// Two things a pair let you do that a layer does not: read a sample back (`out[0][i]`), and
// normalise. Loops over samples become `each(...)`; `normalizeStereo` refuses a layer.

/** A lazy stereo pair; return it from a stem to stream. */
export function layer(ctx) { return makeLayer(ctx, 2); }

/** A lazy mono buffer, for a `sequenceMono` bus that goes through an amp before it is placed. */
export function mono(ctx) { return makeLayer(ctx, 1); }

export function isLayer(x) { return typeof x === "function" && x.__layer === true; }

/** A stem's block inside a mix stream: `addInto(mix, input(mix, "drums"), 0.8)`. */
export function input(mix, name) {
  if (!isLayer(mix)) throw new TypeError("input() needs the mix layer as its first argument");
  return { __input: true, name };
}

function makeLayer(ctx, channels) {
  const jobs = [];
  let cachedOffset = -1;
  let cachedFrames = -1;
  let cached = null;
  // The layer is a function so that a stem can return it as its stream; the recorded work
  // hangs off it as properties.
  const self = function (offset, frames, stems) {
    const planes = self.block(offset, frames, stems);
    return channels === 1 ? planes[0] : planes;
  };
  self.__layer = true;
  self.channels = channels;
  self.ctx = ctx;
  self.jobs = jobs;
  /** Runs every recorded job over [offset, offset + frames); memoised for the block being pulled. */
  self.block = function (offset, frames, stems) {
    if (offset === cachedOffset && frames === cachedFrames && cached !== null) return cached;
    const planes = [];
    for (let c = 0; c < channels; c++) planes.push(new Float32Array(frames));
    for (let j = 0; j < jobs.length; j++) jobs[j](planes, offset, frames, stems);
    cachedOffset = offset; cachedFrames = frames; cached = planes;
    return planes;
  };
  return self;
}

/**
 * The block of `x` for [offset, offset + frames): a layer computes it, a mix input is looked up
 * in the stems the host passed, a plain pair or buffer is viewed in place. Always an array of
 * planes.
 */
export function blockOf(x, offset, frames, stems) {
  if (isLayer(x)) return x.block(offset, frames, stems);
  if (x !== null && typeof x === "object" && x.__input === true) {
    if (stems === undefined || stems === null) throw new Error(`input("${x.name}") is only available inside a mix stream`);
    const planes = stems[x.name];
    if (planes === undefined) throw new Error(`no stem named "${x.name}" reaches this mix`);
    return planes;
  }
  if (x instanceof Float32Array) return [x.subarray(offset, offset + frames)];
  if (Array.isArray(x)) return x.map((p) => p.subarray(offset, offset + frames));
  throw new TypeError("not a layer, an input, a pair or a buffer");
}

/**
 * A loop over samples, as a layer job: `fn(planes, n, offset, ...inputBlocks)` is called once per
 * block with the layer's planes for the block, the block's length, its first frame, and the
 * matching block of each input (layers, mix inputs, or plain pairs and buffers). Index the planes
 * with k in [0, n); the absolute frame is offset + k. On a plain pair this runs once, over the whole
 * pair, so a stem written with `each` renders the same either way.
 */
export function each(out, inputs, fn) {
  if (isLayer(out)) {
    out.jobs.push(function (planes, offset, n, stems) {
      const blocks = inputs.map((x) => blockOf(x, offset, n, stems));
      fn(planes, n, offset, ...blocks);
    });
    return out;
  }
  const n = out[0].length;
  fn(out, n, 0, ...inputs.map((x) => blockOf(x, 0, n, null)));
  return out;
}

/** Constant-power pan gains for a position in [-1, 1]. */
export function panGains(p) {
  const a = (clamp(p, -1, 1) + 1) * TAU / 8;
  return [Math.cos(a), Math.sin(a)];
}

/**
 * Stamp one voice into a stereo pair.
 *
 * `make()` returns a fresh per-sample function (tLocal, iLocal) -> sample. It is called once,
 * here, so every note gets its own oscillator and filter state; sharing them would interleave.
 */
export function play(ctx, out, at, dur, make, opts) {
  const o = opts || NONE;
  const start = Math.round(at * ctx.sr);
  const i0 = Math.max(0, start);
  const i1 = Math.min(ctx.frames, start + Math.round(dur * ctx.sr));
  if (i1 <= i0) return;
  const g = o.gain === undefined ? 1 : o.gain;
  const pg = panGains(o.pan === undefined ? 0 : o.pan);
  const gl = pg[0] * g;
  const gr = pg[1] * g;
  const duck = o.duck;
  // Made now, not when its first block comes: a factory that draws from a shared generator must
  // draw in call order, as it did when the pair was rendered whole.
  const voice = make();
  const inv = 1 / ctx.sr;
  if (isLayer(out)) {
    out.jobs.push(function (planes, offset, n) {
      const from = Math.max(i0, offset), to = Math.min(i1, offset + n);
      if (from >= to) return;
      const L = planes[0], R = planes[1];
      for (let i = from; i < to; i++) {
        const k = i - start;
        let s = voice(k * inv, k);
        if (duck !== undefined) s *= duck[i];
        L[i - offset] += s * gl;
        R[i - offset] += s * gr;
      }
    });
    return;
  }
  const L = out[0];
  const R = out[1];
  for (let i = i0; i < i1; i++) {
    const k = i - start;
    let s = voice(k * inv, k);
    if (duck !== undefined) s *= duck[i];
    L[i] += s * gl;
    R[i] += s * gr;
  }
}

/**
 * Play a list of events through one voice factory.
 * Each event carries at least { t, dur }; `make(ev)` builds its voice.
 */
export function sequence(ctx, out, events, make, opts) {
  const o = opts || NONE;
  const base = o.gain === undefined ? 1 : o.gain;
  const basePan = o.pan === undefined ? 0 : o.pan;
  for (let e = 0; e < events.length; e++) {
    const ev = events[e];
    play(ctx, out, ev.t, ev.dur, function () { return make(ev); }, {
      gain: base * (ev.gain === undefined ? 1 : ev.gain),
      pan: ev.pan === undefined ? basePan : ev.pan,
      duck: o.duck,
    });
  }
}

/**
 * A sidechain gain curve built from the kick's own event times, so it can never drift out of
 * sync with the drums. Multiply it into whatever should duck.
 */
export function ducker(ctx, times, depth, attack, release) {
  const g = new Float32Array(ctx.frames);
  g.fill(1);
  const att = Math.max(1e-4, attack);
  const rel = Math.max(1e-4, release);
  const n = Math.round((att + rel * 5) * ctx.sr);
  const inv = 1 / ctx.sr;
  for (let k = 0; k < times.length; k++) {
    const s = Math.round(times[k] * ctx.sr);
    for (let i = 0; i < n; i++) {
      const idx = s + i;
      if (idx < 0) continue;
      if (idx >= ctx.frames) break;
      const t = i * inv;
      const v = t < att
        ? 1 - depth * (t / att)
        : 1 - depth * Math.exp(-(t - att) / rel);
      if (v < g[idx]) g[idx] = v;
    }
  }
  return g;
}

/** out[c][i] += src[c][i] * g. `src` may be a pair, a layer or a mix `input`. */
export function addInto(out, src, g) {
  if (isLayer(out)) {
    const channels = out.channels;
    out.jobs.push(function (planes, offset, n, stems) {
      const S = blockOf(src, offset, n, stems);
      for (let c = 0; c < channels; c++) {
        const a = planes[c];
        const b = S[c];
        for (let i = 0; i < n; i++) a[i] += b[i] * g;
      }
    });
    return;
  }
  for (let c = 0; c < 2; c++) {
    const a = out[c];
    const b = src[c];
    for (let i = 0; i < a.length; i++) a[i] += b[i] * g;
  }
}

/** Peak-normalise a stereo pair together, preserving the image. */
export function normalizeStereo(pair, peak) {
  if (isLayer(pair)) throw new Error("a layer cannot be normalised: the peak needs the whole render; use a limiter or a fixed gain");
  let mx = 0;
  for (let c = 0; c < 2; c++) {
    const a = pair[c];
    for (let i = 0; i < a.length; i++) {
      const v = a[i] < 0 ? -a[i] : a[i];
      if (v > mx) mx = v;
    }
  }
  if (mx === 0) return pair;
  const g = peak / mx;
  for (let c = 0; c < 2; c++) {
    const a = pair[c];
    for (let i = 0; i < a.length; i++) a[i] *= g;
  }
  return pair;
}

/** Linear fade in/out on a stereo pair, in place. */
export function fadeStereo(ctx, pair, fadeIn, fadeOut) {
  const nIn = Math.round(fadeIn * ctx.sr);
  const nOut = Math.round(fadeOut * ctx.sr);
  if (isLayer(pair)) {
    const len = ctx.frames;
    pair.jobs.push(function (planes, offset, n) {
      for (let c = 0; c < planes.length; c++) {
        const a = planes[c];
        for (let k = 0; k < n; k++) {
          const i = offset + k;
          if (i < nIn) a[k] *= i / nIn;
          const fromEnd = len - 1 - i;
          if (fromEnd < nOut) a[k] *= fromEnd / nOut;
        }
      }
    });
    return pair;
  }
  for (let c = 0; c < 2; c++) {
    const a = pair[c];
    for (let i = 0; i < nIn && i < a.length; i++) a[i] *= i / nIn;
    for (let i = 0; i < nOut && i < a.length; i++) a[a.length - 1 - i] *= i / nOut;
  }
  return pair;
}

const LETTER = { C: 0, D: 2, E: 4, F: 5, G: 7, A: 9, B: 11 };

/** Note name to MIDI number: n("A#1") === 34, n("Eb2") === 39. */
export function n(name) {
  let m = LETTER[name.charAt(0)];
  let i = 1;
  for (;;) {
    const c = name.charAt(i);
    if (c === "#") { m += 1; i += 1; }
    else if (c === "b") { m -= 1; i += 1; }
    else break;
  }
  let sign = 1;
  if (name.charAt(i) === "-") { sign = -1; i += 1; }
  let oct = 0;
  for (; i < name.length; i++) oct = oct * 10 + (name.charCodeAt(i) - 48);
  return m + (sign * oct + 1) * 12;
}

// ---------------------------------------------------------------- stereo voices, samples, curves
//
// A stereo voice writes into a two-slot scratch instead of returning a
// number, so a supersaw can carry a different phase set on each side without allocating.

/** Like `play`, for a voice of the form (tLocal, iLocal, out2) that fills out2[0], out2[1]. */
export function playStereo(ctx, out, at, dur, make, opts) {
  const o = opts || NONE;
  const start = Math.round(at * ctx.sr);
  const i0 = Math.max(0, start);
  const i1 = Math.min(ctx.frames, start + Math.round(dur * ctx.sr));
  if (i1 <= i0) return;
  const g = o.gain === undefined ? 1 : o.gain;
  const duck = o.duck;
  const voice = make();
  const sc = new Float64Array(2);
  const inv = 1 / ctx.sr;
  if (isLayer(out)) {
    out.jobs.push(function (planes, offset, n) {
      const from = Math.max(i0, offset), to = Math.min(i1, offset + n);
      if (from >= to) return;
      const L = planes[0], R = planes[1];
      for (let i = from; i < to; i++) {
        const k = i - start;
        voice(k * inv, k, sc);
        const d = duck === undefined ? g : g * duck[i];
        L[i - offset] += sc[0] * d;
        R[i - offset] += sc[1] * d;
      }
    });
    return;
  }
  const L = out[0];
  const R = out[1];
  for (let i = i0; i < i1; i++) {
    const k = i - start;
    voice(k * inv, k, sc);
    const d = duck === undefined ? g : g * duck[i];
    L[i] += sc[0] * d;
    R[i] += sc[1] * d;
  }
}

export function sequenceStereo(ctx, out, events, make, opts) {
  const o = opts || NONE;
  const base = o.gain === undefined ? 1 : o.gain;
  for (let e = 0; e < events.length; e++) {
    const ev = events[e];
    playStereo(ctx, out, ev.t, ev.dur, function () { return make(ev); }, {
      gain: base * (ev.gain === undefined ? 1 : ev.gain),
      duck: o.duck,
    });
  }
}

/**
 * Stamp a pre-rendered hit (mono Float32Array, or [l, r]) at each event. Drum hits in this idiom
 * are identical every time, like a sample, so rendering the hit once and adding it is both
 * cheaper and truer than re-synthesising it per event.
 */
export function stamp(ctx, out, events, buf, opts) {
  const o = opts || NONE;
  const base = o.gain === undefined ? 1 : o.gain;
  const basePan = o.pan === undefined ? 0 : o.pan;
  const duck = o.duck;
  const isStereo = Array.isArray(buf);
  const bl = isStereo ? buf[0] : buf;
  const br = isStereo ? buf[1] : buf;
  if (isLayer(out)) {
    // The events' gains and windows once, at setup; each block adds the part of every hit that
    // overlaps it, in event order, as the pair did.
    const hits = [];
    for (let e = 0; e < events.length; e++) {
      const ev = events[e];
      const start = Math.round(ev.t * ctx.sr);
      const g = base * (ev.gain === undefined ? 1 : ev.gain);
      const pg = panGains(ev.pan === undefined ? basePan : ev.pan);
      hits.push({ start, gl: pg[0] * g * Math.SQRT2, gr: pg[1] * g * Math.SQRT2, i0: Math.max(0, start), i1: Math.min(ctx.frames, start + bl.length) });
    }
    out.jobs.push(function (planes, offset, n) {
      const L = planes[0], R = planes[1];
      for (let h = 0; h < hits.length; h++) {
        const hit = hits[h];
        const from = Math.max(hit.i0, offset), to = Math.min(hit.i1, offset + n);
        for (let i = from; i < to; i++) {
          const k = i - hit.start;
          const d = duck === undefined ? 1 : duck[i];
          L[i - offset] += bl[k] * hit.gl * d;
          R[i - offset] += br[k] * hit.gr * d;
        }
      }
    });
    return;
  }
  const L = out[0];
  const R = out[1];
  for (let e = 0; e < events.length; e++) {
    const ev = events[e];
    const start = Math.round(ev.t * ctx.sr);
    const g = base * (ev.gain === undefined ? 1 : ev.gain);
    const pg = panGains(ev.pan === undefined ? basePan : ev.pan);
    const gl = pg[0] * g * Math.SQRT2;
    const gr = pg[1] * g * Math.SQRT2;
    const i0 = Math.max(0, start);
    const i1 = Math.min(ctx.frames, start + bl.length);
    for (let i = i0; i < i1; i++) {
      const k = i - start;
      const d = duck === undefined ? 1 : duck[i];
      L[i] += bl[k] * gl * d;
      R[i] += br[k] * gr * d;
    }
  }
}

/**
 * A per-sample automation curve from [[seconds, value], ...] breakpoints, linear between them,
 * held flat outside. `log: true` interpolates geometrically (for cutoffs).
 */
export function curve(ctx, points, log) {
  const c = new Float32Array(ctx.frames);
  const inv = 1 / ctx.sr;
  let p = 0;
  for (let i = 0; i < ctx.frames; i++) {
    const t = i * inv;
    while (p + 1 < points.length && points[p + 1][0] <= t) p++;
    let v;
    if (p + 1 >= points.length || t <= points[0][0]) v = points[Math.min(p, points.length - 1)][1];
    else {
      const a = points[p], b = points[p + 1];
      const u = (t - a[0]) / (b[0] - a[0]);
      v = log ? a[1] * Math.exp(Math.log(b[1] / a[1]) * u) : a[1] + (b[1] - a[1]) * u;
    }
    c[i] = v;
  }
  return c;
}

/** Reverse a mono buffer into a fresh one. */
export function reverse(buf) {
  const out = new Float32Array(buf.length);
  for (let i = 0; i < buf.length; i++) out[i] = buf[buf.length - 1 - i];
  return out;
}

/** Like `sequence`, into ONE mono Float32Array: for a track that gets its own processing later. */
export function sequenceMono(ctx, buf, events, make, opts) {
  const o = opts || NONE;
  const base = o.gain === undefined ? 1 : o.gain;
  const inv = 1 / ctx.sr;
  if (isLayer(buf)) {
    const voices = [];
    for (let e = 0; e < events.length; e++) {
      const ev = events[e];
      const start = Math.round(ev.t * ctx.sr);
      const i0 = Math.max(0, start);
      const i1 = Math.min(ctx.frames, start + Math.round(ev.dur * ctx.sr));
      if (i1 <= i0) continue;
      voices.push({ start, i0, i1, g: base * (ev.gain === undefined ? 1 : ev.gain), voice: make(ev) });
    }
    buf.jobs.push(function (planes, offset, n) {
      const out = planes[0];
      for (let v = 0; v < voices.length; v++) {
        const it = voices[v];
        const from = Math.max(it.i0, offset), to = Math.min(it.i1, offset + n);
        for (let i = from; i < to; i++) {
          const k = i - it.start;
          out[i - offset] += it.voice(k * inv, k) * it.g;
        }
      }
    });
    return buf;
  }
  for (let e = 0; e < events.length; e++) {
    const ev = events[e];
    const start = Math.round(ev.t * ctx.sr);
    const i0 = Math.max(0, start);
    const i1 = Math.min(ctx.frames, start + Math.round(ev.dur * ctx.sr));
    if (i1 <= i0) continue;
    const g = base * (ev.gain === undefined ? 1 : ev.gain);
    const voice = make(ev);
    for (let i = i0; i < i1; i++) {
      const k = i - start;
      buf[i] += voice(k * inv, k) * g;
    }
  }
  return buf;
}

// ---------------------------------------------------------------- grids, patterns, gates, arps, melodies
//
// Added for the album. A grid turns (bar, step) into seconds for any metre; a pattern is a
// string of steps; the gate is the trance gate; arp() and melody() turn notes into events for
// `sequence`.

/**
 * Timing for a track: `beats` per bar (the metronome pulse), `div` steps per beat.
 * grid(132) is 4/4 in sixteenths; grid(68, 2, 3) is 6/8 in quavers (two dotted-crotchet beats).
 */
export function grid(bpm, beats, div) {
  const b = beats === undefined ? 4 : beats;
  const d = div === undefined ? 4 : div;
  const beat = 60 / bpm;
  const step = beat / d;
  const bar = beat * b;
  return {
    bpm, beat, bar, step, steps: b * d,
    at(barNo, stepNo) { return barNo * bar + (stepNo === undefined ? 0 : stepNo) * step; },
  };
}

/** "x.x.X..." one character a step: `x` a hit, `X` an accent, anything else a rest. */
export function pattern(str) {
  const steps = [];
  for (let i = 0; i < str.length; i++) {
    const c = str.charAt(i);
    if (c === "x") steps.push({ step: i, accent: false });
    else if (c === "X") steps.push({ step: i, accent: true });
  }
  return { steps, length: str.length };
}

/**
 * The trance gate: a gain curve that is `floor` between hits and 1 for `hold` steps after each
 * hit of `pat`, the pattern repeating every `pat.length` steps over bars [from, to), with linear
 * `attack` and `release` ramps in seconds. 1 outside the range. Multiply it into a pad with
 * `applyCurve`; call it once per range and apply each.
 */
export function gateCurve(ctx, g, pat, from, to, o) {
  const p = o || NONE;
  const floor = p.floor === undefined ? 0.2 : p.floor;
  const att = Math.max(1, Math.round((p.attack === undefined ? 0.005 : p.attack) * ctx.sr));
  const rel = Math.max(1, Math.round((p.release === undefined ? 0.04 : p.release) * ctx.sr));
  const hold = Math.round((p.hold === undefined ? 1 : p.hold) * g.step * ctx.sr);
  const c = new Float32Array(ctx.frames);
  c.fill(1);
  const i0 = Math.max(0, Math.round(g.at(from) * ctx.sr));
  const i1 = Math.min(ctx.frames, Math.round(g.at(to) * ctx.sr));
  for (let i = i0; i < i1; i++) c[i] = floor;
  const stepN = g.step * ctx.sr;
  const period = pat.length * stepN;
  for (let start = g.at(from) * ctx.sr; start < i1; start += period) {
    for (let h = 0; h < pat.steps.length; h++) {
      const on = Math.round(start + pat.steps[h].step * stepN);
      const off = on + hold;
      for (let i = on - att; i < off + rel; i++) {
        if (i < i0 || i >= i1) continue;
        let v;
        if (i < on) v = floor + (1 - floor) * (1 - (on - i) / att);
        else if (i < off) v = 1;
        else v = floor + (1 - floor) * (1 - (i - off) / rel);
        if (v > c[i]) c[i] = v;
      }
    }
  }
  return c;
}

/**
 * Arpeggio events over bars [from, to): the hits of `pat` (repeating every `pat.length` steps)
 * take the notes of `notes` in `mode` order, "up", "down", "updown" or "random" (seeded);
 * `octaves` extends the list upward. Events: { t, gate, dur, midi, vel, seed }.
 */
export function arp(ctx, g, notes, pat, from, to, o) {
  const p = o || NONE;
  const mode = p.mode === undefined ? "up" : p.mode;
  const gateFrac = p.gate === undefined ? 0.8 : p.gate;
  const vel = p.vel === undefined ? 0.85 : p.vel;
  const accent = p.accent === undefined ? 1 : p.accent;
  const octaves = p.octaves === undefined ? 1 : p.octaves;
  const tag = p.tag === undefined ? "arp" : p.tag;
  const tail = p.tail === undefined ? 0.15 : p.tail;
  const seed = p.seed === undefined ? ctx.seed : p.seed;
  let list = [];
  for (let oc = 0; oc < octaves; oc++) for (let k = 0; k < notes.length; k++) list.push(notes[k] + 12 * oc);
  if (mode === "down") list = list.slice().reverse();
  else if (mode === "updown") list = list.concat(list.slice(1, -1).reverse());
  const rng = new Random(hash(seed, tag));
  const out = [];
  let n = 0;
  for (let bar = from; bar < to; bar++) {
    for (let s = 0; s < g.steps; s++) {
      const idx = ((bar - from) * g.steps + s) % pat.length;
      let hit = null;
      for (let h = 0; h < pat.steps.length; h++) if (pat.steps[h].step === idx) { hit = pat.steps[h]; break; }
      if (hit === null) continue;
      const midi = mode === "random" ? list[Math.floor(rng.next() * list.length)] : list[n % list.length];
      n++;
      const gate = g.step * gateFrac;
      out.push({ t: g.at(bar, s), gate, dur: gate + tail, midi, vel: hit.accent ? accent : vel, seed: hash(seed, tag, bar, s) });
    }
  }
  return out;
}

/**
 * Melody rows [bar, step, midi, length] on grid `g` to events. `vel` (0.85) gains up to 0.12
 * for a note of half a bar or more and 0.03 on a downbeat; `tail` seconds past the gate (0.6);
 * `gateFrac` (0.88) for a note that does not run into the next; `transpose`; `bars(rowBar)`
 * filters rows; rows at least `throw` long get `ev.throw = true`; with `link` (default true) a
 * note starting where the last ended, within four semitones, gets `ev.from` for a glide.
 */
export function melody(ctx, g, tag, startBar, rows, o) {
  const p = o || NONE;
  const out = [];
  let prev = null;
  const tail = p.tail === undefined ? 0.6 : p.tail;
  const link = p.link === undefined ? true : p.link;
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i];
    if (p.bars !== undefined && !p.bars(r[0])) continue;
    const bar = startBar + r[0];
    const t = g.at(bar, r[1]);
    const end = g.at(bar, r[1] + r[3]);
    const midi = r[2] + (p.transpose === undefined ? 0 : p.transpose);
    const connected = prev !== null && Math.abs(prev.end - t) < 1e-6;
    const gate = connected ? end - t : (end - t) * (p.gateFrac === undefined ? 0.88 : p.gateFrac);
    const vel = clamp((p.vel === undefined ? 0.85 : p.vel) + 0.12 * Math.min(1, r[3] / (g.steps / 2)) + (r[1] === 0 ? 0.03 : 0), 0, 1);
    const ev = { t, gate, dur: gate + tail, midi, vel, seed: hash(ctx.seed, ctx.stem, tag, bar, r[1]) };
    if (link && connected && Math.abs(midi - prev.midi) <= 4) ev.from = prev.midi;
    if (p.throw !== undefined && r[3] >= p.throw) ev.throw = true;
    out.push(ev);
    prev = { end, midi };
  }
  return out;
}
