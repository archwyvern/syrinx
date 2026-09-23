// syrinx-framework: instruments/synth -- the electronic voices: supersaws, pads, leads, synth
// basses, an FM piano, a theremin, and a drum machine (kick, clap, snare, hats, rim, crash, conga)
// with its risers, downlifters, impacts and swells.
//
// Tonal instruments are factories `(ctx, ev, opts) -> voice`, where a mono voice is
// `(t, k) -> sample` and a stereo one is `(t, k, out2)`; `ev` carries `midi`, `vel` (0..1),
// `gate` (seconds the key is held) and a per-note `seed`. Drum hits are rendered once into a
// buffer and stamped, like samples.
//
// Everything that moves is a function of `t`, never an oscillator polled at a lower rate; filter
// coefficients update every 16 or 32 samples, which is far above any modulation rate used here.

import { Random, hash } from "syrinx";
import {
  TAU, Osc, BlepOsc, Phasor, Noise, Biquad, Env,
  softclip, clamp, mtof,
} from "../dsp.js";

// JP-8000 supersaw detune curve, normalised so the outer voices sit at +-1.
const JP = [-1, -0.5715, -0.1774, 0, 0.181, 0.565, 0.9766];

function smooth(u) { return u <= 0 ? 0 : u >= 1 ? 1 : u * u * (3 - 2 * u); }

/** Attack (smoothstep), hold to gate, exponential release. */
function ar(attack, release, gate) {
  return (t) => t < attack ? smooth(t / attack) : t < gate ? 1 : Math.exp(-(t - gate) / release);
}

// ---------------------------------------------------------------- supersaw (stereo)
//
// Seven band-limited saws per side on the JP-8000 spread, each side its own phase set so the
// beating differs left and right: that difference is the width. Two-pole lowpass with an
// envelope, highpassed so it never fights the bass.

export function supersaw(ctx, ev, o) {
  const sr = ctx.sr;
  const f0 = mtof(ev.midi);
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const cents = o.spread === undefined ? 18 : o.spread;
  const side = o.side === undefined ? 0.62 : o.side;
  const rng = new Random(hash(ev.seed, "supersaw"));
  const fr = new Float64Array(7), g = new Float64Array(7);
  const oL = [], oR = [];
  for (let k = 0; k < 7; k++) {
    fr[k] = f0 * Math.exp(JP[k] * cents / 1200 * Math.LN2);
    g[k] = k === 3 ? 1 : side;
    oL.push(BlepOsc.saw(sr, rng.next()));
    oR.push(BlepOsc.saw(sr, rng.next()));
  }
  const base = o.cutoff === undefined ? 1400 : o.cutoff;
  const amt = o.envAmount === undefined ? 2600 : o.envAmount;
  const tauF = o.filterDecay === undefined ? 0.35 : o.filterDecay;
  const q = o.q === undefined ? 0.9 : o.q;
  const lpL1 = new Biquad(sr), lpL2 = new Biquad(sr), lpR1 = new Biquad(sr), lpR2 = new Biquad(sr);
  const hpL = Biquad.highpass(sr, o.hp === undefined ? 180 : o.hp, 0.7);
  const hpR = Biquad.highpass(sr, o.hp === undefined ? 180 : o.hp, 0.7);
  const amp = ar(o.attack === undefined ? 0.025 : o.attack, o.release === undefined ? 0.28 : o.release, ev.gate);
  const level = (o.level === undefined ? 0.12 : o.level) * (0.55 + 0.45 * vel);
  const open = o.open === undefined ? 1 : o.open;   // arrangement automation, 0..1
  return function (t, k, out) {
    if ((k & 31) === 0) {
      const fc = clamp((base + amt * vel * Math.exp(-t / tauF)) * open, 60, 18000);
      lpL1.set("lowpass", fc, q); lpL2.set("lowpass", fc, 0.6);
      lpR1.set("lowpass", fc, q); lpR2.set("lowpass", fc, 0.6);
    }
    // the centre saw is one oscillator shared by both sides, as on the JP-8000; the six detuned
    // ones have their own phases each side, and that difference is the width
    const c = oL[3].next(fr[3]);
    let l = c, r = c;
    for (let i = 0; i < 7; i++) { if (i === 3) continue; l += oL[i].next(fr[i]) * g[i]; r += oR[i].next(fr[i]) * g[i]; }
    const a = amp(t) * level;
    out[0] = hpL.process(lpL2.process(lpL1.process(l))) * a;
    out[1] = hpR.process(lpR2.process(lpR1.process(r))) * a;
  };
}

// ---------------------------------------------------------------- pad (stereo)
//
// Three detuned saws and a sub-octave sine per side, a slow lowpass that breathes on two
// unrelated rates, a long attack and a longer release.

export function pad(ctx, ev, o) {
  const sr = ctx.sr;
  const f0 = mtof(ev.midi);
  const rng = new Random(hash(ev.seed, "pad"));
  const det = o.detune === undefined ? 11 : o.detune;
  const fr = [f0 * Math.exp(-det / 1200 * Math.LN2), f0, f0 * Math.exp(det / 1200 * Math.LN2)];
  const oL = [], oR = [];
  for (let k = 0; k < 3; k++) { oL.push(BlepOsc.saw(sr, rng.next())); oR.push(BlepOsc.saw(sr, rng.next())); }
  const subL = new Phasor(sr, rng.next()), subR = new Phasor(sr, rng.next());
  const lpL1 = new Biquad(sr), lpL2 = new Biquad(sr), lpR1 = new Biquad(sr), lpR2 = new Biquad(sr);
  const hpL = Biquad.highpass(sr, 90, 0.7), hpR = Biquad.highpass(sr, 90, 0.7);
  const base = o.cutoff === undefined ? 900 : o.cutoff;
  const sweep = o.sweep === undefined ? 1100 : o.sweep;
  const ph1 = rng.next() * TAU, ph2 = rng.next() * TAU;
  const attack = o.attack === undefined ? 1.4 : o.attack;
  const release = o.release === undefined ? 2.4 : o.release;
  const level = o.level === undefined ? 0.22 : o.level;
  const open = o.open === undefined ? 1 : o.open;
  const tOff = ev.t;   // absolute start, so the breathing is continuous across chord changes
  return function (t, k, out) {
    if ((k & 63) === 0) {
      const ta = t + tOff;
      const fc = clamp((base + sweep * (0.5 + 0.5 * Math.sin(TAU * 0.053 * ta + ph1)) + 260 * Math.sin(TAU * 0.29 * ta + ph2)) * open, 120, 12000);
      lpL1.set("lowpass", fc, 0.8); lpL2.set("lowpass", fc * 1.3, 0.6);
      lpR1.set("lowpass", fc * 1.02, 0.8); lpR2.set("lowpass", fc * 1.33, 0.6);
    }
    let l = 0, r = 0;
    for (let i = 0; i < 3; i++) { l += oL[i].next(fr[i]); r += oR[i].next(fr[i]); }
    l = l * 0.33 + Math.sin(TAU * subL.next(f0 * 0.5)) * 0.35;
    r = r * 0.33 + Math.sin(TAU * subR.next(f0 * 0.5)) * 0.35;
    const a = (t < attack ? 1 - Math.exp(-3 * t / attack) : t < ev.gate ? 1 : Math.exp(-(t - ev.gate) / release)) * level;
    out[0] = hpL.process(lpL2.process(lpL1.process(l))) * a;
    out[1] = hpR.process(lpR2.process(lpR1.process(r))) * a;
  };
}

// ---------------------------------------------------------------- pluck (mono)
//
// Saw plus a sub-octave square through a resonant two-pole whose cutoff falls in ~60 ms, a
// 2 ms noise tick on the front. Velocity opens the filter more than it raises the level.

export function pluck(ctx, ev, o) {
  const sr = ctx.sr;
  const f = mtof(ev.midi);
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const rng = new Random(hash(ev.seed, "pluck"));
  const saw = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const noise = new Noise(hash(ev.seed, "pluck.tick"));
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, o.hp === undefined ? 140 : o.hp, 0.7);
  const base = o.cutoff === undefined ? 320 : o.cutoff;
  const amt = o.envAmount === undefined ? 6500 : o.envAmount;
  const tauF = o.filterDecay === undefined ? 0.06 : o.filterDecay;
  const tauA = o.decay === undefined ? 0.17 : o.decay;
  const q = o.q === undefined ? 1.6 : o.q;
  const open = o.open === undefined ? 1 : o.open;
  const level = (o.level === undefined ? 0.5 : o.level) * (0.6 + 0.4 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const fc = clamp((base + amt * (0.35 + 0.65 * vel) * Math.exp(-t / tauF)) * open, 80, 16000);
      lp1.set("lowpass", fc, q); lp2.set("lowpass", fc, 0.7);
    }
    let x = saw.next(f) * 0.8 + sq.next(f * 0.5) * 0.25;
    if (t < 0.004) x += noise.white() * 0.35 * Math.exp(-t / 0.0012);
    x = lp2.process(lp1.process(x));
    let a = (1 - Math.exp(-t / 0.0012)) * Math.exp(-t / tauA);
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / 0.04);
    return hp.process(softclip(x * 1.3)) * a * level;
  };
}

// ---------------------------------------------------------------- lead (mono)
//
// Two detuned saws, a sub-octave square and a sine for body, lowpass with a bite that settles,
// glide from the previous note when `ev.from` is set, vibrato that arrives after a quarter
// second the way a played note does. Driven into a soft clipper before the filter.

export function lead(ctx, ev, o) {
  const sr = ctx.sr;
  const fTo = mtof(ev.midi);
  const fFrom = ev.from === undefined ? fTo : mtof(ev.from);
  const glide = o.glide === undefined ? 0.06 : o.glide;
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const rng = new Random(hash(ev.seed, "lead"));
  const det = o.detune === undefined ? 7 : o.detune;
  const d1 = Math.exp(-det / 1200 * Math.LN2), d2 = Math.exp(det / 1200 * Math.LN2);
  const s1 = BlepOsc.saw(sr, rng.next()), s2 = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const s5 = BlepOsc.saw(sr, rng.next());
  const fifth = o.fifth === undefined ? 0 : o.fifth;
  const drive = o.drive === undefined ? 1.5 : o.drive;
  const body = new Phasor(sr, rng.next());
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, 240, 0.7);
  const base = o.cutoff === undefined ? 1800 : o.cutoff;
  const amt = o.envAmount === undefined ? 4500 : o.envAmount;
  const attack = o.attack === undefined ? 0.008 : o.attack;
  const release = o.release === undefined ? 0.14 : o.release;
  const sus = o.sustain === undefined ? 0.8 : o.sustain;
  const vibDepth = (o.vibrato === undefined ? 8 : o.vibrato) / 1200 * Math.LN2;
  const vibRate = o.vibRate === undefined ? 5.4 : o.vibRate;
  const vibPhase = rng.next() * TAU;
  const logRatio = Math.log(fTo / fFrom);
  const level = (o.level === undefined ? 0.4 : o.level) * (0.65 + 0.35 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const fc = clamp(base + amt * (0.4 + 0.6 * vel) * (Math.exp(-t / 0.28) * 0.75 + 0.25) * (1 - Math.exp(-t / 0.006)), 100, 16000);
      lp1.set("lowpass", fc, 0.9); lp2.set("lowpass", fc * 1.4, 0.55);
    }
    const g = t < glide ? t / glide : 1;
    const vib = t < 0.22 ? 0 : smooth((t - 0.22) / 0.35) * vibDepth * Math.sin(TAU * vibRate * t + vibPhase);
    const f = fFrom * Math.exp(logRatio * g + vib);
    let x = s1.next(f * d1) * 0.5 + s2.next(f * d2) * 0.5 + sq.next(f * 0.5) * 0.15 + Math.sin(TAU * body.next(f)) * 0.22;
    if (fifth > 0) x += s5.next(f * 1.5) * fifth;
    x = softclip(x * drive);
    x = lp2.process(lp1.process(x));
    let a = t < attack ? smooth(t / attack) : sus + (1 - sus) * Math.exp(-(t - attack) / 0.16);
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / release);
    return hp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- drift lead (mono)
//
// The unstable one. A triangle and a saw whose pitch wanders on two slow, unrelated sines plus
// a seeded random walk, so no two notes are in tune with each other the same way twice. Slow
// attack, long release, driven hard into the clipper and darkened after. Meant to be phased,
// chorused and drowned by its layer.

export function driftLead(ctx, ev, o) {
  const sr = ctx.sr;
  const f0 = mtof(ev.midi);
  const rng = new Random(hash(ev.seed, "drift"));
  const tri = Osc.tri(sr, rng.next());
  const saw = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, 150, 0.7);
  const wob = (o.wobble === undefined ? 14 : o.wobble) / 1200 * Math.LN2;
  const p1 = rng.next() * TAU, p2 = rng.next() * TAU;
  const r1 = 0.31 + rng.next() * 0.2, r2 = 1.07 + rng.next() * 0.3;
  const attack = o.attack === undefined ? 0.18 : o.attack;
  const release = o.release === undefined ? 0.7 : o.release;
  const cutoff = o.cutoff === undefined ? 2400 : o.cutoff;
  const level = o.level === undefined ? 0.36 : o.level;
  const tOff = ev.t;
  let walk = 0;
  return function (t, k) {
    if ((k & 63) === 0) {
      walk = walk * 0.985 + rng.bipolar() * 0.09;
      const ta = t + tOff;
      const fc = clamp(cutoff * (0.75 + 0.35 * Math.sin(TAU * 0.11 * ta + p2)), 200, 9000);
      lp1.set("lowpass", fc, 1.1); lp2.set("lowpass", fc * 1.5, 0.6);
    }
    const drift = wob * (0.6 * Math.sin(TAU * r1 * t + p1) + 0.4 * Math.sin(TAU * r2 * t + p2) + walk);
    const f = f0 * Math.exp(drift);
    let x = tri.next(f) * 0.6 + saw.next(f * 1.003) * 0.35 + sq.next(f * 0.5) * 0.2;
    x = softclip(x * 2.4);
    x = lp2.process(lp1.process(x));
    const a = t < attack ? 1 - Math.exp(-3 * t / attack) : t < ev.gate ? 1 : Math.exp(-(t - ev.gate) / release);
    return hp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- FM piano (mono)
//
// Two-operator FM with a fast-decaying index: the bark of a struck tine, then a sine. A second
// carrier a fraction of a hertz away gives the slow beating of a real pair of strings, and a
// 1 ms noise burst is the hammer.

export function fmPiano(ctx, ev, o) {
  const sr = ctx.sr;
  const f = mtof(ev.midi);
  const vel = ev.vel === undefined ? 0.8 : ev.vel;
  const rng = new Random(hash(ev.seed, "piano"));
  const c1 = new Phasor(sr, 0), c2 = new Phasor(sr, 0.25);
  const m1 = new Phasor(sr, 0), m2 = new Phasor(sr, 0);
  const noise = new Noise(hash(ev.seed, "hammer"));
  const hp = Biquad.highpass(sr, 70, 0.7);
  const lp = Biquad.lowpass(sr, 7500, 0.6);
  const beat = 0.3 + rng.next() * 0.3;
  const bright = o.bright === undefined ? 1 : o.bright;
  const level = (o.level === undefined ? 0.34 : o.level) * (0.5 + 0.5 * vel);
  const release = o.release === undefined ? 0.16 : o.release;
  return function (t) {
    const i1 = (1.1 + 2.3 * vel) * bright * Math.exp(-t / 0.10) + 0.5 * bright * Math.exp(-t / 1.1);
    const i2 = 0.8 * vel * bright * Math.exp(-t / 0.028);
    const mod = i1 * Math.sin(TAU * m1.next(f)) + i2 * Math.sin(TAU * m2.next(f * 7));
    const y = Math.sin(TAU * c1.next(f) + mod) * 0.62 + Math.sin(TAU * c2.next(f + beat) + mod * 0.9) * 0.38;
    let a = (1 - Math.exp(-t / 0.0015)) * (0.7 * Math.exp(-t / 1.5) + 0.3 * Math.exp(-t / 0.22));
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / release);
    let x = y * a;
    if (t < 0.003) x += noise.white() * 0.25 * vel * Math.exp(-t / 0.0008);
    return lp.process(hp.process(x)) * level;
  };
}

// ---------------------------------------------------------------- bass

/** Sub: a sine with a short octave drop at the onset so it thumps, slight second harmonic. */
export function sub(ctx, ev, o) {
  const sr = ctx.sr;
  const f = mtof(ev.midi);
  const ph = new Phasor(sr, 0);
  const level = o.level === undefined ? 0.5 : o.level;
  const release = o.release === undefined ? 0.03 : o.release;
  return function (t) {
    const fi = f * (1 + 0.6 * Math.exp(-t / 0.012));
    const x = softclip(Math.sin(TAU * ph.next(fi)) * 1.25);
    let a = t < 0.004 ? t / 0.004 : 1;
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / release);
    return x * a * level;
  };
}

/** Mid bass: saw and square through a resonant lowpass with a fast envelope, clipped, highpassed. */
export function midBass(ctx, ev, o) {
  const sr = ctx.sr;
  const f = mtof(ev.midi);
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const rng = new Random(hash(ev.seed, "bass"));
  const saw = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, 75, 0.7);
  const open = ev.open === undefined ? (o.open === undefined ? 1 : o.open) : ev.open;
  const base = o.cutoff === undefined ? 240 : o.cutoff;
  const amt = o.envAmount === undefined ? 2400 : o.envAmount;
  const tauF = o.filterDecay === undefined ? 0.09 : o.filterDecay;
  const level = (o.level === undefined ? 0.42 : o.level) * (0.7 + 0.3 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const fc = clamp(base + amt * open * (0.4 + 0.6 * vel) * (Math.exp(-t / tauF) + 0.12), 80, 12000);
      lp1.set("lowpass", fc, 1.7); lp2.set("lowpass", fc * 1.2, 0.6);
    }
    let x = saw.next(f) * 0.7 + sq.next(f) * 0.4;
    x = lp2.process(lp1.process(x));
    x = softclip(x * 2.2);
    let a = t < 0.002 ? t / 0.002 : 0.72 + 0.28 * Math.exp(-t / 0.12);
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / 0.018);
    return hp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- drum hits (rendered once)

function buffer(sr, seconds) { return new Float32Array(Math.round(seconds * sr)); }

function tailFade(buf, sr, seconds) {
  const n = Math.min(buf.length, Math.round(seconds * sr));
  for (let i = 0; i < n; i++) buf[buf.length - 1 - i] *= i / n;
  return buf;
}

/** Kick: phase-integrated sine falling 200 -> 46 Hz, hard early drive, a click and a knock on top. */
export function kickHit(ctx, seed, o) {
  const sr = ctx.sr;
  const p = o || {};
  const pitch = p.pitch === undefined ? 1 : p.pitch;
  const out = buffer(sr, 0.45);
  const noise = new Noise(seed);
  const clickHp = Biquad.highpass(sr, 2600, 0.7);
  const lp = Biquad.lowpass(sr, p.cutoff === undefined ? 9000 : p.cutoff, 0.7);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = pitch * (46 + 165 * Math.exp(-t / 0.019) + 45 * Math.exp(-t / 0.07));
    phase += f / sr;
    const drive = 1 + 1.6 * Math.exp(-t / 0.05);
    const ampB = (1 - Math.exp(-t / 0.0005)) * (0.75 * Math.exp(-t / 0.15) + 0.45 * Math.exp(-t / 0.035));
    const body = softclip(Math.sin(TAU * phase) * drive) * ampB;
    const click = clickHp.process(noise.white()) * Math.exp(-t / 0.0022) * 0.9;
    const knock = Math.sin(TAU * 1700 * t) * Math.exp(-t / 0.0016) * 0.45;
    out[i] = lp.process(body + click + knock);
  }
  return tailFade(out, sr, 0.01);
}

/** Clap: four noise bursts 10 ms apart into a tail, bandpassed around 1.4 kHz with a 3 kHz lift. */
export function clapHit(ctx, seed) {
  const sr = ctx.sr;
  const out = buffer(sr, 0.34);
  const noise = new Noise(seed);
  const bp = Biquad.bandpass(sr, 1400, 0.8);
  const hp = Biquad.highpass(sr, 480, 0.7);
  const pk = Biquad.peak(sr, 3200, 1.2, 4);
  const bursts = [0, 0.010, 0.020, 0.031];
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    let e = 0;
    for (let b = 0; b < 4; b++) if (t >= bursts[b]) e += Math.exp(-(t - bursts[b]) / 0.006);
    if (t >= 0.031) e += 0.7 * Math.exp(-(t - 0.031) / 0.085);
    const x = pk.process(hp.process(bp.process(noise.white() * e)));
    const body = t < 0.12 ? Math.sin(TAU * 190 * t) * Math.exp(-t / 0.02) * 0.12 : 0;
    out[i] = softclip(x * 2.4) * 0.8 + body;
  }
  return tailFade(out, sr, 0.01);
}

/** Snare: a 190 Hz body falling from 300, plus highpassed noise. `pitch` scales both. */
export function snareHit(ctx, seed, o) {
  const sr = ctx.sr;
  const p = o || {};
  const pitch = p.pitch === undefined ? 1 : p.pitch;
  const out = buffer(sr, 0.3);
  const noise = new Noise(seed);
  const hp = Biquad.highpass(sr, 1500 * pitch, 0.7);
  const pk = Biquad.peak(sr, 4000, 1, 3);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = pitch * (190 + 110 * Math.exp(-t / 0.02));
    phase += f / sr;
    const body = Math.sin(TAU * phase) * Math.exp(-t / 0.06) * 0.6;
    const n = pk.process(hp.process(noise.white())) * Math.exp(-t / 0.075) * 0.8;
    out[i] = softclip((body + n) * 1.6) * (1 - Math.exp(-t / 0.0004));
  }
  return tailFade(out, sr, 0.01);
}

/** Hat: six squares at inharmonic ratios plus noise, twice highpassed. `open` lengthens the decay. */
export function hatHit(ctx, seed, o) {
  const sr = ctx.sr;
  const p = o || {};
  const tau = p.open ? 0.26 : (p.decay === undefined ? 0.022 : p.decay);
  const out = buffer(sr, p.open ? 1.1 : 0.14);
  const noise = new Noise(seed);
  const freqs = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];
  const oscs = freqs.map((f, k) => Osc.square(sr, k * 0.13));
  const hp1 = Biquad.highpass(sr, 6800, 0.8);
  const hp2 = Biquad.highpass(sr, 7800, 0.7);
  const pk = Biquad.peak(sr, 10500, 1.5, 3);
  const lp = Biquad.lowpass(sr, 15500, 0.7);
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    let m = 0;
    for (let k = 0; k < 6; k++) m += oscs[k].next(freqs[k] * 8.1);
    const x = m * 0.16 + noise.white() * 0.55;
    const e = (1 - Math.exp(-t / 0.0006)) * Math.exp(-t / tau);
    out[i] = lp.process(pk.process(hp2.process(hp1.process(x)))) * e;
  }
  return tailFade(out, sr, 0.01);
}

/** Shaker/ride tick: pink noise in a 7 kHz band, 35 ms. */
export function tickHit(ctx, seed) {
  const sr = ctx.sr;
  const out = buffer(sr, 0.2);
  const noise = new Noise(seed);
  const bp = Biquad.bandpass(sr, 7200, 1.4);
  const hp = Biquad.highpass(sr, 4500, 0.7);
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const e = (1 - Math.exp(-t / 0.002)) * Math.exp(-t / 0.035);
    out[i] = hp.process(bp.process(noise.pink() * 3)) * e;
  }
  return tailFade(out, sr, 0.01);
}

/** Rim: a short sine knock with a bandpassed noise edge. */
export function rimHit(ctx, seed) {
  const sr = ctx.sr;
  const out = buffer(sr, 0.08);
  const noise = new Noise(seed);
  const bp = Biquad.bandpass(sr, 2600, 2);
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const k = Math.sin(TAU * 880 * t) * Math.exp(-t / 0.005) * 0.7 + Math.sin(TAU * 1720 * t) * Math.exp(-t / 0.003) * 0.4;
    const e = bp.process(noise.white()) * Math.exp(-t / 0.004) * 0.9;
    out[i] = softclip((k + e) * 1.5);
  }
  return tailFade(out, sr, 0.005);
}

/** Crash: stereo, noise and the hat's metal partials, a long decay, each side its own noise. */
export function crashHit(ctx, seed, o) {
  const sr = ctx.sr;
  const p = o || {};
  const tau = p.decay === undefined ? 0.8 : p.decay;
  const len = p.length === undefined ? 3.2 : p.length;
  const freqs = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];
  const pair = [];
  for (let c = 0; c < 2; c++) {
    const out = buffer(sr, len);
    const noise = new Noise(hash(seed, c));
    const oscs = freqs.map((f, k) => Osc.square(sr, (k + c) * 0.17));
    const hp1 = Biquad.highpass(sr, 3800, 0.6), hp2 = Biquad.highpass(sr, 5200, 0.6);
    const lp = Biquad.lowpass(sr, 15000, 0.7);
    for (let i = 0; i < out.length; i++) {
      const t = i / sr;
      let m = 0;
      for (let k = 0; k < 6; k++) m += oscs[k].next(freqs[k] * (c === 0 ? 6.3 : 6.5));
      const x = m * 0.12 + noise.white() * 0.6;
      const e = (1 - Math.exp(-t / 0.003)) * (Math.exp(-t / tau) * 0.8 + 0.2 * Math.exp(-t / (tau * 0.25)));
      out[i] = lp.process(hp2.process(hp1.process(x))) * e;
    }
    pair.push(tailFade(out, sr, 0.05));
  }
  return pair;
}

/** Riser: noise through a bandpass climbing 200 -> 8 kHz over `seconds`, a sine two octaves up under it. */
export function riserHit(ctx, seed, seconds, o) {
  const sr = ctx.sr;
  const p = o || {};
  const pair = [];
  for (let c = 0; c < 2; c++) {
    const out = buffer(sr, seconds);
    const noise = new Noise(hash(seed, c));
    const bp = new Biquad(sr);
    const ph = new Phasor(sr, 0);
    const skew = c === 0 ? 1 : 1.04;
    let fc = 200, q = 3, comp = 1;
    for (let i = 0; i < out.length; i++) {
      const t = i / sr;
      const u = t / seconds;
      if ((i & 63) === 0) {
        fc = clamp(200 * Math.exp(Math.log(40) * u) * skew, 100, 12000);
        q = 3 - 2.2 * u;
        bp.set("bandpass", fc, q);
        // a bandpass passes noise power in proportion to fc / q: hold the level to the curve
        comp = Math.sqrt((8000 / 0.8) / (fc / q));
      }
      const amp = (0.06 + 0.94 * u * Math.sqrt(u)) * comp * (p.gain === undefined ? 1 : p.gain);
      const tone = p.tone === false ? 0 : Math.sin(TAU * ph.next(110 * Math.exp(Math.LN2 * 2 * u))) * 0.12 * u;
      out[i] = bp.process(noise.white()) * 1.6 * amp + tone * amp;
    }
    pair.push(tailFade(out, sr, 0.004));
  }
  return pair;
}

/** Downlifter: the riser in reverse spirit, a band falling 6 kHz -> 200 Hz and fading. */
export function downlifterHit(ctx, seed, seconds) {
  const sr = ctx.sr;
  const pair = [];
  for (let c = 0; c < 2; c++) {
    const out = buffer(sr, seconds);
    const noise = new Noise(hash(seed, c));
    const bp = new Biquad(sr);
    for (let i = 0; i < out.length; i++) {
      const t = i / sr;
      const u = t / seconds;
      if ((i & 63) === 0) bp.set("bandpass", clamp(6000 * Math.exp(-Math.log(30) * u) * (c === 0 ? 1 : 1.05), 100, 12000), 2.2);
      out[i] = bp.process(noise.white()) * 1.3 * (1 - u) * (1 - u) * (1 - Math.exp(-t / 0.02));
    }
    pair.push(tailFade(out, sr, 0.05));
  }
  return pair;
}

/** Impact: a sub sine falling 60 -> 32 Hz over a second with a dark noise burst on top. */
export function impactHit(ctx, seed) {
  const sr = ctx.sr;
  const out = buffer(sr, 1.6);
  const noise = new Noise(seed);
  const lp = Biquad.lowpass(sr, 1400, 0.7);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = 32 + 30 * Math.exp(-t / 0.12);
    phase += f / sr;
    const body = softclip(Math.sin(TAU * phase) * 1.4) * Math.exp(-t / 0.45) * (1 - Math.exp(-t / 0.003));
    const n = lp.process(noise.white()) * Math.exp(-t / 0.12) * 0.5;
    out[i] = body * 0.9 + n;
  }
  return tailFade(out, sr, 0.05);
}

/**
 * A swell: noise and a detuned saw chord rising through an opening lowpass over `seconds`, at
 * full strength when it ends, so the stamp's end is the cut. `top` is the final cutoff, `notes`
 * the chord. Each side its own noise and phases.
 */
export function swellHit(ctx, seed, seconds, o) {
  const sr = ctx.sr;
  const p = o || {};
  const top = p.top === undefined ? 6000 : p.top;
  const notes = p.notes === undefined ? [57, 60, 64, 69] : p.notes;
  const pair = [];
  for (let c = 0; c < 2; c++) {
    const out = buffer(sr, seconds);
    const noise = new Noise(hash(seed, "swell", c));
    const rng = new Random(hash(seed, "swellph", c));
    const oscs = [], fr = [];
    for (let n = 0; n < notes.length; n++) {
      const f = mtof(notes[n]);
      for (let k = 0; k < 3; k++) { oscs.push(BlepOsc.saw(sr, rng.next())); fr.push(f * Math.exp((k - 1) * 8 / 1200 * Math.LN2)); }
    }
    const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
    const hp = Biquad.highpass(sr, 120, 0.7);
    const logTop = Math.log(top / 250);
    for (let i = 0; i < out.length; i++) {
      const t = i / sr;
      const u = t / seconds;
      if ((i & 63) === 0) { const fc = 250 * Math.exp(logTop * u); lp1.set("lowpass", fc, 0.9); lp2.set("lowpass", fc * 1.4, 0.6); }
      const amp = u <= 0 ? 0 : Math.exp(2.2 * Math.log(u));
      let s = 0;
      for (let k = 0; k < oscs.length; k++) s += oscs[k].next(fr[k]);
      s = s / oscs.length * 1.6;
      out[i] = hp.process(lp2.process(lp1.process(s + noise.white() * 0.45))) * amp;
    }
    pair.push(tailFade(out, sr, 0.003));
  }
  return pair;
}

// ---------------------------------------------------------------- techno lead (mono)
//
// Two saws twelve cents apart and a sub square, driven into the clipper first, then a resonant
// two-pole whose cutoff snaps open on every note and closes in ~110 ms. No vibrato, no glide:
// the movement is the filter and the delay. `ev.open` (or o.open) scales the cutoff for
// arrangement automation.

export function techLead(ctx, ev, o) {
  const sr = ctx.sr;
  const fTo = mtof(ev.midi);
  const fFrom = ev.from === undefined ? fTo : mtof(ev.from);
  const slideLog = Math.log(fTo / fFrom);
  const slide = o.slide === undefined ? 0.07 : o.slide;
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const rng = new Random(hash(ev.seed, "tech"));
  const det = (o.detune === undefined ? 12 : o.detune) / 1200 * Math.LN2;
  const s1 = BlepOsc.saw(sr, rng.next()), s2 = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, 160, 0.7);
  const base = o.cutoff === undefined ? 520 : o.cutoff;
  const amt = o.envAmount === undefined ? 6500 : o.envAmount;
  const tauF = o.filterDecay === undefined ? 0.11 : o.filterDecay;
  const q = o.q === undefined ? 2.4 : o.q;
  const drive = o.drive === undefined ? 2.6 : o.drive;
  const open = ev.open === undefined ? (o.open === undefined ? 1 : o.open) : ev.open;
  const sus = o.sustain === undefined ? 0.5 : o.sustain;
  const release = o.release === undefined ? 0.07 : o.release;
  const level = (o.level === undefined ? 0.42 : o.level) * (0.7 + 0.3 * vel);
  return function (t, k) {
    if ((k & 7) === 0) {
      const fc = clamp((base + amt * (0.35 + 0.65 * vel) * Math.exp(-t / tauF)) * open + 120, 90, 17000);
      lp1.set("lowpass", fc, q); lp2.set("lowpass", fc * 1.25, 0.7);
    }
    const f = slideLog === 0 ? fTo : fFrom * Math.exp(slideLog * (t < slide ? t / slide : 1));
    let x = s1.next(f * Math.exp(-det * 0.5)) * 0.5 + s2.next(f * Math.exp(det * 0.5)) * 0.5 + sq.next(f * 0.5) * 0.2;
    x = softclip(x * drive);
    x = lp2.process(lp1.process(x));
    let a = t < 0.002 ? t / 0.002 : sus + (1 - sus) * Math.exp(-(t - 0.002) / 0.22);
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / release);
    return hp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- theremin (mono)
//
// A near-sine that glides into every note over `glide` seconds on a smooth curve, with a deep
// slow vibrato that arrives almost at once. Ask for `ev.from` on every event and it wails.

export function theremin(ctx, ev, o) {
  const sr = ctx.sr;
  const fTo = mtof(ev.midi);
  const fFrom = ev.from === undefined ? fTo : mtof(ev.from);
  const glide = ev.glide === undefined ? (o.glide === undefined ? 0.25 : o.glide) : ev.glide;
  const logR = Math.log(fTo / fFrom);
  const rng = new Random(hash(ev.seed, "theremin"));
  const ph = new Phasor(sr, rng.next());
  const vib = (o.vibrato === undefined ? 45 : o.vibrato) / 1200 * Math.LN2;
  const vibRate = o.vibRate === undefined ? 6.2 : o.vibRate;
  const vibPh = rng.next() * TAU;
  const attack = o.attack === undefined ? 0.12 : o.attack;
  const release = o.release === undefined ? 0.45 : o.release;
  const level = o.level === undefined ? 0.4 : o.level;
  const lp = Biquad.lowpass(sr, 6000, 0.6);
  return function (t) {
    const u = t < glide ? t / glide : 1;
    const g = u * u * (3 - 2 * u);
    const v = (t < 0.08 ? 0 : smooth((t - 0.08) / 0.3)) * vib * Math.sin(TAU * vibRate * t + vibPh);
    const f = fFrom * Math.exp(logR * g + v);
    const p = ph.next(f);
    const x = Math.sin(TAU * p) + 0.12 * Math.sin(2 * TAU * p);
    const a = t < attack ? smooth(t / attack) : t < ev.gate ? 1 : Math.exp(-(t - ev.gate) / release);
    return lp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- dark bass (mono)
//
// Two saws nine cents apart and a sub square, driven hard into the clipper, then a resonant
// two-pole with a fast envelope and an optional slow LFO on the cutoff (`o.lfoRate` Hz, keyed to
// absolute time so it stays in phase across notes). Highpassed at 55: the sub owns the bottom.

export function darkBass(ctx, ev, o) {
  const sr = ctx.sr;
  const f = mtof(ev.midi);
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const rng = new Random(hash(ev.seed, "dark"));
  const det = 9 / 1200 * Math.LN2;
  const s1 = BlepOsc.saw(sr, rng.next()), s2 = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, 55, 0.7);
  const base = o.cutoff === undefined ? 180 : o.cutoff;
  const amt = o.envAmount === undefined ? 2600 : o.envAmount;
  const tauF = o.filterDecay === undefined ? 0.1 : o.filterDecay;
  const q = o.q === undefined ? 1.9 : o.q;
  const drive = o.drive === undefined ? 3.2 : o.drive;
  const open = ev.open === undefined ? (o.open === undefined ? 1 : o.open) : ev.open;
  const lfoRate = ev.lfoRate === undefined ? (o.lfoRate === undefined ? 0 : o.lfoRate) : ev.lfoRate;
  const lfoDepth = o.lfoDepth === undefined ? 0.6 : o.lfoDepth;
  const tOff = ev.t;
  const level = (o.level === undefined ? 0.45 : o.level) * (0.75 + 0.25 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const lfo = lfoRate > 0 ? 1 + lfoDepth * 0.5 * (1 + Math.sin(TAU * lfoRate * (t + tOff) - Math.PI / 2)) : 1;
      const fc = clamp((base + amt * (0.4 + 0.6 * vel) * Math.exp(-t / tauF) + 100) * open * lfo, 70, 12000);
      lp1.set("lowpass", fc, q); lp2.set("lowpass", fc * 1.3, 0.6);
    }
    let x = s1.next(f * Math.exp(-det * 0.5)) * 0.5 + s2.next(f * Math.exp(det * 0.5)) * 0.5 + sq.next(f * 0.5) * 0.5;
    x = softclip(x * drive);
    x = lp2.process(lp1.process(x));
    let a = t < 0.002 ? t / 0.002 : 0.85 + 0.15 * Math.exp(-t / 0.15);
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / 0.02);
    return hp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- phat bass (mono)
//
// The riff voice. Two saws six cents apart and a sub square through a screaming two-pole
// (resonance 2.4, envelope snapping down in 90 ms) with a four-bar LFO on the cutoff, THEN
// distortion -- the grit is on the filtered signal -- glide on `ev.from`, highpassed at 40.

export function phatBass(ctx, ev, o) {
  const sr = ctx.sr;
  const fTo = mtof(ev.midi);
  const fFrom = ev.from === undefined ? fTo : mtof(ev.from);
  const slideLog = Math.log(fTo / fFrom);
  const glide = o.glide === undefined ? 0.06 : o.glide;
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const rng = new Random(hash(ev.seed, "phat"));
  const det = 6 / 1200 * Math.LN2;
  const s1 = BlepOsc.saw(sr, rng.next()), s2 = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, 40, 0.7);
  const base = o.cutoff === undefined ? 120 : o.cutoff;
  const amt = o.envAmount === undefined ? 1800 : o.envAmount;
  const tauF = o.filterDecay === undefined ? 0.09 : o.filterDecay;
  const q = o.q === undefined ? 2.4 : o.q;
  const post = o.drive === undefined ? 2.5 : o.drive;
  const open = ev.open === undefined ? (o.open === undefined ? 1 : o.open) : ev.open;
  const lfoRate = o.lfoRate === undefined ? 0 : o.lfoRate;
  const tOff = ev.t;
  const level = (o.level === undefined ? 0.45 : o.level) * (0.75 + 0.25 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const lfo = lfoRate > 0 ? 1 + 0.35 * Math.sin(TAU * lfoRate * (t + tOff)) : 1;
      const fc = clamp((base + amt * (0.4 + 0.6 * vel) * Math.exp(-t / tauF) + 60) * open * lfo, 60, 9000);
      lp1.set("lowpass", fc, q); lp2.set("lowpass", fc * 1.4, 0.6);
    }
    const f = slideLog === 0 ? fTo : fFrom * Math.exp(slideLog * (t < glide ? t / glide : 1));
    let x = s1.next(f * Math.exp(-det * 0.5)) * 0.5 + s2.next(f * Math.exp(det * 0.5)) * 0.5 + sq.next(f * 0.5) * 0.45;
    x = lp2.process(lp1.process(x * 1.4));
    x = Math.tanh(x * post) / Math.tanh(post) * 0.9;
    let a = t < 0.003 ? t / 0.003 : 0.88 + 0.12 * Math.exp(-t / 0.2);
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / 0.025);
    return hp.process(x) * a * level;
  };
}

/** Conga / bongo: a sine dropping from 1.3x with a slap of bandpassed noise on the front. */
export function congaHit(ctx, seed, freq, o) {
  const sr = ctx.sr;
  const p = o || {};
  const slap = p.slap === undefined ? 0.5 : p.slap;
  const tau = p.decay === undefined ? 0.13 : p.decay;
  const out = buffer(sr, 0.4);
  const noise = new Noise(seed);
  const bp = Biquad.bandpass(sr, 2400, 1.2);
  const lp = Biquad.lowpass(sr, 5000, 0.7);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = freq * (1 + 0.3 * Math.exp(-t / 0.018));
    phase += f / sr;
    const body = softclip(Math.sin(TAU * phase) * 1.3) * Math.exp(-t / tau);
    const s = bp.process(noise.white()) * Math.exp(-t / 0.005) * slap;
    out[i] = lp.process(body + s) * (1 - Math.exp(-t / 0.0005));
  }
  return tailFade(out, sr, 0.01);
}
