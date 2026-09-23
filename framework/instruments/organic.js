// syrinx-framework: instruments/organic -- the band: two pianos, bass guitar, acoustic guitar, a
// string ensemble, cello, and an acoustic kit. Built on the Karplus-Strong string in metal.js and
// dsp.js's oscillators.
//
// `piano` is the voice the Firmament album was played on. `grand` grew out of it, measured note
// by note against recordings of a real Steinway and changed where the numbers said; both stay as
// they are, and a better piano will be a new name beside them.

import { Random, hash } from "syrinx";
import {
  TAU, Osc, BlepOsc, Phasor, Noise, Biquad, OnePole,
  softclip, clamp, mtof,
} from "../dsp.js";
import { ksString } from "./metal.js";

function smooth(u) { return u <= 0 ? 0 : u >= 1 ? 1 : u * u * (3 - 2 * u); }
function buffer(sr, seconds) { return new Float32Array(Math.round(seconds * sr)); }
function tailFade(buf, sr, seconds) {
  const n = Math.min(buf.length, Math.round(seconds * sr));
  for (let i = 0; i < n; i++) buf[buf.length - 1 - i] *= i / n;
  return buf;
}

// ---------------------------------------------------------------- piano (mono)
//
// Three struck strings a cent or two apart per note, a hammer that is brighter the harder the
// key is hit, a knock from the action, a soundboard bump. Low notes ring longer than high ones.
// The damper is a 60 ms release at the gate.

export function ksPiano(ctx, ev, o) {
  const sr = ctx.sr;
  const p = o || {};
  const vel = ev.vel === undefined ? 0.8 : ev.vel;
  const reg = clamp((ev.midi - 36) / 60, 0, 1);
  const sustain = (3.8 - 2.8 * reg) * (p.sustain === undefined ? 1 : p.sustain);
  const damp = 2600 + 5500 * vel * (1 - 0.35 * reg);
  const rng = new Random(hash(ev.seed, "piano"));
  const cents = [-1.4 + rng.bipolar() * 0.4, 0, 1.2 + rng.bipolar() * 0.4];
  const strings = [];
  for (let k = 0; k < 3; k++) {
    strings.push(ksString(ctx, { midi: ev.midi, gate: ev.gate, seed: hash(ev.seed, k) },
      { damp, sustain, excCut: 900 + 4200 * vel, excLen: 0.0022 + 0.001 * (1 - reg), pick: 3.2 + 2 * vel, detune: cents[k], release: 0.06 }));
  }
  const knock = new Noise(hash(ev.seed, "knock"));
  const knockLp = Biquad.lowpass(sr, 500, 0.8);
  const body = Biquad.peak(sr, 240, 1.1, 2.5);
  const hp = Biquad.highpass(sr, 50, 0.7);
  const lp = Biquad.lowpass(sr, 9500, 0.6);
  const level = (p.level === undefined ? 0.5 : p.level) * (0.55 + 0.45 * vel);
  return function (t, i) {
    let x = 0;
    for (let k = 0; k < 3; k++) x += strings[k](t, i);
    x *= 0.4;
    if (t < 0.02) x += knockLp.process(knock.white()) * Math.exp(-t / 0.0035) * 0.35 * vel;
    return lp.process(hp.process(body.process(x))) * level;
  };
}

// ---------------------------------------------------------------- bass guitar (mono)

export function bassGuitar(ctx, ev, o) {
  const sr = ctx.sr;
  const p = o || {};
  const vel = ev.vel === undefined ? 0.9 : ev.vel;
  const str = ksString(ctx, ev, { damp: 750 + 300 * vel, sustain: p.sustain === undefined ? 0.9 : p.sustain, excCut: 700, excLen: 0.009, pick: 6.5 + 1.5 * vel, release: 0.03, glide: 0.08 });
  const body = Biquad.peak(sr, 260, 1.0, 3);
  const lp = Biquad.lowpass(sr, 2600, 0.7);
  const hp = Biquad.highpass(sr, 38, 0.7);
  const level = (p.level === undefined ? 0.55 : p.level) * (0.7 + 0.3 * vel);
  return function (t, i) {
    const s = str(t, i);
    return hp.process(lp.process(body.process(softclip(s * 1.6)))) * level;
  };
}

// ---------------------------------------------------------------- acoustic guitar string (mono)
//
// A bright string into a body with its two air/top resonances. Strum by offsetting the strings.

export function acousticString(ctx, ev, o) {
  const sr = ctx.sr;
  const p = o || {};
  const vel = ev.vel === undefined ? 0.8 : ev.vel;
  const soft = p.soft === true;
  const str = ksString(ctx, ev, { damp: soft ? 2400 + 900 * vel : 3600 + 1400 * vel, sustain: p.sustain === undefined ? 1.3 : p.sustain, excCut: soft ? 1600 + 900 * vel : 3200 + 2000 * vel, excLen: soft ? 0.0045 : 0.002, pick: (soft ? 3.2 : 2.7) + 0.9 * vel, release: 0.03 });
  const air = Biquad.peak(sr, 105, 3, 5);
  const top = Biquad.peak(sr, 215, 2.5, 3);
  const pres = Biquad.peak(sr, 2600, 1, 2);
  const hp = Biquad.highpass(sr, 75, 0.7);
  const lp = Biquad.lowpass(sr, 7500, 0.6);
  const level = (p.level === undefined ? 0.5 : p.level) * (0.6 + 0.4 * vel);
  return function (t, i) {
    return lp.process(hp.process(pres.process(top.process(air.process(str(t, i)))))) * level;
  };
}

// ---------------------------------------------------------------- string ensemble (stereo)
//
// Three saws per side, each with its own vibrato rate so they never line up, a little rosin
// noise on the attack, a lowpass that opens with velocity, and a bowed attack.

export function strings(ctx, ev, o) {
  const sr = ctx.sr;
  const p = o || {};
  const f0 = mtof(ev.midi);
  const vel = ev.vel === undefined ? 0.8 : ev.vel;
  const rng = new Random(hash(ev.seed, "strings"));
  const det = [-9, 0, 8];
  const oL = [], oR = [], fr = [], vr = [], vp = [];
  for (let k = 0; k < 3; k++) {
    fr.push(f0 * Math.exp(det[k] / 1200 * Math.LN2));
    oL.push(BlepOsc.saw(sr, rng.next())); oR.push(BlepOsc.saw(sr, rng.next()));
    vr.push(4.9 + rng.next() * 1.4); vp.push(rng.next() * TAU);
  }
  const bow = new Noise(hash(ev.seed, "bow"));
  const bowBp = Biquad.bandpass(sr, 3800, 1.2);
  const lpL = new Biquad(sr), lpR = new Biquad(sr);
  const bodyL = Biquad.peak(sr, 320, 1, 2), bodyR = Biquad.peak(sr, 320, 1, 2);
  const hpL = Biquad.highpass(sr, 90, 0.7), hpR = Biquad.highpass(sr, 90, 0.7);
  const fc = 1800 + 2200 * vel;
  lpL.set("lowpass", fc, 0.7); lpR.set("lowpass", fc * 1.03, 0.7);
  const attack = p.attack === undefined ? 0.5 : p.attack;
  const release = p.release === undefined ? 0.7 : p.release;
  const vib = 8 / 1200 * Math.LN2;
  const level = (p.level === undefined ? 0.2 : p.level) * (0.6 + 0.4 * vel);
  return function (t, k, out) {
    const vAmt = t < 0.35 ? 0 : smooth((t - 0.35) / 0.6);
    let l = 0, r = 0;
    for (let i = 0; i < 3; i++) {
      const m = Math.exp(vAmt * vib * Math.sin(TAU * vr[i] * t + vp[i]));
      l += oL[i].next(fr[i] * m); r += oR[i].next(fr[i] * m);
    }
    const a = (t < attack ? smooth(t / attack) : t < ev.gate ? 1 : Math.exp(-(t - ev.gate) / release));
    const rosin = bowBp.process(bow.pink() * 2) * 0.05 * (t < attack ? 1 : 0.4);
    out[0] = hpL.process(bodyL.process(lpL.process(l * 0.33 + rosin))) * a * level;
    out[1] = hpR.process(bodyR.process(lpR.process(r * 0.33 + rosin))) * a * level;
  };
}

// ---------------------------------------------------------------- cello (mono)

export function cello(ctx, ev, o) {
  const sr = ctx.sr;
  const p = o || {};
  const fTo = mtof(ev.midi);
  const fFrom = ev.from === undefined ? fTo : mtof(ev.from);
  const logR = Math.log(fTo / fFrom);
  const rng = new Random(hash(ev.seed, "cello"));
  const s1 = BlepOsc.saw(sr, rng.next()), s2 = BlepOsc.saw(sr, rng.next());
  const bow = new Noise(hash(ev.seed, "bow"));
  const bowBp = Biquad.bandpass(sr, 2800, 1.5);
  const f1 = Biquad.peak(sr, 210, 2, 4), f2 = Biquad.peak(sr, 420, 2, 3), f3 = Biquad.peak(sr, 1000, 1.5, 2);
  const lp = Biquad.lowpass(sr, 3200, 0.7);
  const hp = Biquad.highpass(sr, 60, 0.7);
  const attack = p.attack === undefined ? 0.12 : p.attack;
  const release = p.release === undefined ? 0.3 : p.release;
  const vib = 12 / 1200 * Math.LN2;
  const vibRate = 5.6 + rng.next() * 0.6;
  const level = p.level === undefined ? 0.4 : p.level;
  return function (t) {
    const g = t < 0.07 ? t / 0.07 : 1;
    const v = t < 0.3 ? 0 : smooth((t - 0.3) / 0.5) * vib * Math.sin(TAU * vibRate * t);
    const f = fFrom * Math.exp(logR * g + v);
    let x = s1.next(f) * 0.6 + s2.next(f * 1.0023) * 0.4;
    x += bowBp.process(bow.pink() * 2) * 0.08;
    x = lp.process(f3.process(f2.process(f1.process(x))));
    const a = t < attack ? smooth(t / attack) : t < ev.gate ? 1 : Math.exp(-(t - ev.gate) / release);
    return hp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- the kit

/** Kick drum: a 55 Hz shell falling from 140, a beater click, a short shell resonance. */
export function acKick(ctx, seed) {
  const sr = ctx.sr;
  const out = buffer(sr, 0.32);
  const noise = new Noise(seed);
  const beaterHp = Biquad.highpass(sr, 2500, 0.7);
  const shellLp = Biquad.lowpass(sr, 900, 0.8);
  const lp = Biquad.lowpass(sr, 8000, 0.7);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = 54 + 90 * Math.exp(-t / 0.012);
    phase += f / sr;
    const body = softclip(Math.sin(TAU * phase) * (1 + 0.6 * Math.exp(-t / 0.02))) * (0.9 * Math.exp(-t / 0.055) + 0.3 * Math.exp(-t / 0.13)) * (1 - Math.exp(-t / 0.0004));
    const n = noise.white();
    const beater = beaterHp.process(n) * Math.exp(-t / 0.0015) * 0.5 + Math.sin(TAU * 2400 * t) * Math.exp(-t / 0.001) * 0.2;
    const shell = shellLp.process(n) * Math.exp(-t / 0.008) * 0.3;
    out[i] = lp.process(body + beater + shell);
  }
  return tailFade(out, sr, 0.01);
}

/** Snare drum: a 185 Hz shell, a 330 Hz ring, the wires rattling in 1.2-4 kHz, a crack. */
export function acSnare(ctx, seed, o) {
  const sr = ctx.sr;
  const p = o || {};
  const pitch = p.pitch === undefined ? 1 : p.pitch;
  const out = buffer(sr, 0.36);
  const noise = new Noise(seed);
  const wiresBp = Biquad.bandpass(sr, 3000 * pitch, 0.6);
  const wiresHp = Biquad.highpass(sr, 1200 * pitch, 0.7);
  const crackHp = Biquad.highpass(sr, 5000, 0.7);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = pitch * (185 + 120 * Math.exp(-t / 0.015));
    phase += f / sr;
    const body = Math.sin(TAU * phase) * Math.exp(-t / 0.045) * 0.8 + Math.sin(TAU * 330 * pitch * t) * Math.exp(-t / 0.03) * 0.3;
    const n = noise.white();
    const wires = wiresHp.process(wiresBp.process(n)) * Math.exp(-t / 0.11) * (1 - Math.exp(-t / 0.003)) * 1.6;
    const crack = crackHp.process(n) * Math.exp(-t / 0.006) * 0.5;
    out[i] = softclip((body + wires + crack) * 1.5) * (1 - Math.exp(-t / 0.0004));
  }
  return tailFade(out, sr, 0.01);
}

/** Ride cymbal: a bell tone, a wash of noise and metallic partials, 0.9 s. */
export function rideHit(ctx, seed, o) {
  const sr = ctx.sr;
  const p = o || {};
  const bell = p.bell === undefined ? 0.25 : p.bell;
  const out = buffer(sr, 0.9);
  const noise = new Noise(seed);
  const ratios = [1, 1.34, 1.9, 2.72, 3.4];
  const oscs = ratios.map((r, k) => Osc.square(sr, k * 0.19));
  const washHp = Biquad.highpass(sr, 4000, 0.7);
  const metalHp = Biquad.highpass(sr, 3000, 0.7);
  const lp = Biquad.lowpass(sr, 12000, 0.7);
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    let m = 0;
    for (let k = 0; k < 5; k++) m += oscs[k].next(480 * ratios[k]);
    const x = washHp.process(noise.white()) * Math.exp(-t / 0.35) * 0.5
      + metalHp.process(m * 0.08) * Math.exp(-t / 0.3)
      + Math.sin(TAU * 2200 * t) * Math.exp(-t / 0.25) * bell;
    out[i] = lp.process(x) * (1 - Math.exp(-t / 0.0008));
  }
  return tailFade(out, sr, 0.02);
}

// ---------------------------------------------------------------- piano, modal (mono)
//
// Not a plucked string. Each partial is its own decaying resonator (y = 2 r cos w y1 - r^2 y2:
// three multiplies a sample, no sine call), tuned on the stretched series f_k = k f0 sqrt(1 + B k^2)
// -- the inharmonicity that makes a piano a piano and a harmonic string a zither -- with a
// strike-point comb in the amplitudes, high partials decaying much faster than low ones, and a
// second slower resonator on the first partials for the aftersound. A felt hammer (a lowpassed
// burst, darker the softer the key is hit), a key-bed thump, a soundboard, and a 90 ms damper at
// the gate. Unisons are not detuned: that beating is what a dulcimer has and a piano does not.

export function piano(ctx, ev, o) {
  const sr = ctx.sr;
  const p = o || {};
  const midi = ev.midi;
  const f0 = mtof(midi);
  const vel = ev.vel === undefined ? 0.8 : ev.vel;
  const rng = new Random(hash(ev.seed, "piano"));
  const B = 0.00022 * Math.exp((midi - 48) / 12 * Math.LN2);
  const N = Math.max(3, Math.min(midi < 60 ? 26 : 16, Math.floor(17000 / f0)));
  const tau1 = 4.5 * Math.exp(-(midi - 40) / 24 * Math.LN2) * (p.sustain === undefined ? 1 : p.sustain);
  // o.dark (0) steepens the partial roll-off; o.felt (1) scales the felt cutoff; o.hammer and o.thump (1)
  // scale the two attack noises. All default to the values the album was rendered with.
  const alpha = 0.85 + 0.7 * (1 - vel) + (p.dark === undefined ? 0 : p.dark);
  const strike = 0.118;
  // resonator state: c = 2 r cos w, rr = r^2, s1, s2 -- two banks: prompt and after
  const M = 2 * N;
  const c = new Float64Array(M), rr = new Float64Array(M), s1 = new Float64Array(M), s2 = new Float64Array(M);
  let norm = 0;
  for (let k = 1; k <= N; k++) {
    const fk = k * f0 * Math.sqrt(1 + B * k * k);
    if (fk > sr * 0.45) break;
    const w = TAU * fk / sr;
    let a = Math.abs(Math.sin(Math.PI * k * strike)) / Math.exp(alpha * Math.log(k));
    const tauK = tau1 / (1 + 0.15 * Math.exp(1.3 * Math.log(k - 1 + 1e-9)) );
    const phase = rng.next() * TAU;
    // prompt: most of the energy, decays at tauK/3; after: the rest, decays at tauK, only low partials
    const parts = k <= 8 ? [[0.62, tauK / 3.2], [0.38, tauK]] : [[1, tauK / 2.2]];
    for (let j = 0; j < parts.length; j++) {
      const idx = (k - 1) * 2 + j;
      const amp = a * parts[j][0];
      const r = Math.exp(-1 / (parts[j][1] * sr));
      c[idx] = 2 * r * Math.cos(w); rr[idx] = r * r;
      s1[idx] = amp * Math.sin(phase - w) / r;
      s2[idx] = amp * Math.sin(phase - 2 * w) / (r * r);
    }
    norm += a;
  }
  const hammer = new Noise(hash(ev.seed, "hammer"));
  const hammerLp = Biquad.lowpass(sr, 1200 + 3200 * vel, 0.7);
  const thumpLp = Biquad.lowpass(sr, 220, 0.8);
  const felt = Biquad.lowpass(sr, (2800 + 7000 * vel) * (p.felt === undefined ? 1 : p.felt), 0.6);
  const hammerK = p.hammer === undefined ? 1 : p.hammer, thumpK = p.thump === undefined ? 1 : p.thump;
  const body1 = Biquad.peak(sr, 190, 1.4, 2.5);
  const body2 = Biquad.peak(sr, 420, 1.2, 1.5);
  const hp = Biquad.highpass(sr, 45, 0.7);
  const level = (p.level === undefined ? 0.5 : p.level) * (0.45 + 0.55 * vel) / Math.max(0.3, norm);
  const gate = ev.gate === undefined ? 1e9 : ev.gate;
  return function (t) {
    let y = 0;
    for (let i = 0; i < M; i++) {
      const v = c[i] * s1[i] - rr[i] * s2[i];
      s2[i] = s1[i]; s1[i] = v; y += v;
    }
    let x = felt.process(y);
    if (t < 0.012) {
      const n = hammer.white();
      x += hammerLp.process(n) * Math.exp(-t / 0.0018) * 0.28 * vel * hammerK + thumpLp.process(n) * Math.exp(-t / 0.006) * 0.35 * vel * thumpK;
    }
    x = body2.process(body1.process(x));
    if (t > gate) x *= Math.exp(-(t - gate) / 0.09);
    return hp.process(x) * level;
  };
}

// ---------------------------------------------------------------- grand (mono)
//
// The concert grand, iterated against a Steinway B (reference/piano/iowa, mf) with the FAD
// judge (tools/judge/fad.py, PANNs embeddings): `piano` scores 58.3, this scores 38.7, where the
// Steinway's own mf-vs-pp is 15.8 and real-vs-real 3.7 (2026-09-19). Same modal engine as
// `piano`; what the judge and the partial measurements agreed on: the bass keeps far more
// partials and they ring (a real B0 has 35 above -40 dB at 1.5 s), the treble is a cliff after
// the second partial (real E5 at 0.5 s: octave -14 dB, third partial -40 dB), the prompt sound
// decays much harder (real C4: -20 dB in 0.75 s) and a small aftersound rings long on ALL the
// partials. What it refused: damping the upper partials faster, thinning the mid register,
// confining the aftersound to the low partials. `piano` stays as the album heard it; this is the
// voice to keep iterating. Knobs, defaults = the best setting found:
//   nBass (90) partials below C3, nMid (24) to C5, nTop (3) above; alphaBass (0.55), alphaMid
//   (0.95), alphaTop (2.3) roll-off per register (+0.7 (1 - vel)); promptDiv (4) prompt decay =
//   tauK / promptDiv, promptDivTop for the treble alone; afterMix (0.12) share of energy in the
//   aftersound; afterK (40) partials that carry it; afterMul (3) aftersound length = tauK *
//   afterMul; dampK (0.08) how much faster partial k decays than the fundamental; unison (8)
//   cents between the three strings of a note above C3 -- the beating was the single biggest
//   step, 46.3 -> 38.7, and the judge's plateau is 8-12; rise (0) a bloom it rejected; knockTop
//   and knockLp scale the treble's hammer knock and its cutoff; plus sustain, felt, hammer,
//   thump, level as `piano`.

export function grand(ctx, ev, o) {
  const sr = ctx.sr;
  const p = o || {};
  const midi = ev.midi;
  const f0 = mtof(midi);
  const vel = ev.vel === undefined ? 0.8 : ev.vel;
  const rng = new Random(hash(ev.seed, "piano"));
  const B = 0.00022 * Math.exp((midi - 48) / 12 * Math.LN2);
  const g = (k, d) => (p[k] === undefined ? d : p[k]);
  const N = Math.max(3, Math.min(midi < 48 ? g("nBass", 90) : midi < 72 ? g("nMid", 24) : g("nTop", 3), Math.floor(21000 / f0)));
  const tau1 = 4.5 * Math.exp(-(midi - 40) / 24 * Math.LN2) * g("sustain", 1);
  const alphaBase = midi < 48 ? g("alphaBass", 0.55) : midi < 72 ? g("alphaMid", 0.95) : g("alphaTop", 2.3);
  const alpha = alphaBase + 0.7 * (1 - vel);
  const strike = 0.118;
  const promptDiv = midi >= 72 ? g("promptDivTop", g("promptDiv", 4)) : g("promptDiv", 4);
  const afterMix = g("afterMix", 0.12), afterK = g("afterK", 40), afterMul = g("afterMul", 3), dampK = g("dampK", 0.08);
  // unison: above C3 the strings are doubled and tripled; detuned by `unison` cents they beat
  const uniCents = midi >= 72 ? g("unison", 8) : midi >= 48 ? g("unisonMid", g("unison", 8)) : 0;
  const offs = uniCents > 0 ? [-uniCents, 0, uniCents] : [0];
  const M = 2 * N * offs.length;
  const c = new Float64Array(M), rr = new Float64Array(M), s1 = new Float64Array(M), s2 = new Float64Array(M);
  let norm = 0;
  for (let k = 1; k <= N; k++) {
    const fk = k * f0 * Math.sqrt(1 + B * k * k);
    if (fk > sr * 0.45) break;
    const w = TAU * fk / sr;
    const a = Math.abs(Math.sin(Math.PI * k * strike)) / Math.exp(alpha * Math.log(k));
    const tauK = tau1 / (1 + dampK * Math.exp(1.3 * Math.log(k - 1 + 1e-9)));
    const phase = rng.next() * TAU;
    const parts = k <= afterK ? [[1 - afterMix, tauK / promptDiv], [afterMix, tauK * afterMul]] : [[1, tauK / promptDiv]];
    for (let j = 0; j < parts.length; j++) {
      for (let u = 0; u < offs.length; u++) {
        const idx = ((k - 1) * 2 + j) * offs.length + u;
        const wu = w * Math.exp(offs[u] / 1200 * Math.LN2);
        const ph = offs.length > 1 ? rng.next() * TAU : phase;
        const amp = a * parts[j][0] / offs.length;
        const r = Math.exp(-1 / (parts[j][1] * sr));
        c[idx] = 2 * r * Math.cos(wu); rr[idx] = r * r;
        s1[idx] = amp * Math.sin(ph - wu) / r;
        s2[idx] = amp * Math.sin(ph - 2 * wu) / (r * r);
      }
    }
    norm += a;
  }
  // the note blooms: the string and the soundboard take `rise` seconds to come up after the hammer
  const riseTau = g("rise", 0);
  const hammer = new Noise(hash(ev.seed, "hammer"));
  const knock = midi >= 72 ? g("knockTop", 1) : 1;
  const hammerLp = Biquad.lowpass(sr, (1200 + 3200 * vel) * g("knockLp", 1), 0.7);
  const thumpLp = Biquad.lowpass(sr, 220, 0.8);
  const felt = Biquad.lowpass(sr, (2800 + 7000 * vel) * g("felt", 1), 0.6);
  const hammerK = g("hammer", 1), thumpK = g("thump", 1);
  const body1 = Biquad.peak(sr, 190, 1.4, 2.5);
  const body2 = Biquad.peak(sr, 420, 1.2, 1.5);
  const hp = Biquad.highpass(sr, 45, 0.7);
  const level = g("level", 0.5) * (0.45 + 0.55 * vel) / Math.max(0.3, norm);
  const gate = ev.gate === undefined ? 1e9 : ev.gate;
  return function (t) {
    let y = 0;
    for (let i = 0; i < M; i++) {
      const v = c[i] * s1[i] - rr[i] * s2[i];
      s2[i] = s1[i]; s1[i] = v; y += v;
    }
    let x = felt.process(y);
    if (riseTau > 0) x *= 1 - Math.exp(-t / riseTau);
    if (t < 0.012) {
      const n = hammer.white();
      x += hammerLp.process(n) * Math.exp(-t / 0.0018) * 0.28 * vel * hammerK * knock + thumpLp.process(n) * Math.exp(-t / 0.006) * 0.35 * vel * thumpK * knock;
    }
    x = body2.process(body1.process(x));
    if (t > gate) x *= Math.exp(-(t - gate) / 0.09);
    return hp.process(x) * level;
  };
}
