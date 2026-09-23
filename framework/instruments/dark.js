// syrinx-framework: instruments/dark -- darksynth and EBM: arps, brass, growling and torn basses,
// a gated snare, an industrial kick, a pulse lead, a swarm of saws, and a soft tone. Same contract
// as synth.js: a voice is `(ctx, ev, o) -> (t, k) -> sample`, a hit is rendered once into a buffer
// and stamped. Nothing here is plucked: every envelope holds while the key is down and the
// filters move slowly.

import { Random, hash } from "syrinx";
import { TAU, Osc, BlepOsc, Phasor, Noise, Biquad, softclip, hardclip, fold, clamp, mtof } from "../dsp.js";
import { Resonator } from "../voice/vocal.js";

function smooth(u) { return u <= 0 ? 0 : u >= 1 ? 1 : u * u * (3 - 2 * u); }
function buffer(sr, seconds) { return new Float32Array(Math.round(seconds * sr)); }
function tailFade(buf, sr, seconds) {
  const n = Math.min(buf.length, Math.round(seconds * sr));
  for (let i = 0; i < n; i++) buf[buf.length - 1 - i] *= i / n;
  return buf;
}
/** Scale a buffer so its peak is `peak` (a silent buffer is left alone). */
function peakTo(buf, peak) {
  let mx = 0;
  for (let i = 0; i < buf.length; i++) { const v = buf[i] < 0 ? -buf[i] : buf[i]; if (v > mx) mx = v; }
  if (mx === 0) return buf;
  const g = peak / mx;
  for (let i = 0; i < buf.length; i++) buf[i] *= g;
  return buf;
}

// ---------------------------------------------------------------- arp voice (mono)
//
// A sequence voice, not a pluck: two saws a few cents apart and a square an octave down through
// a gentle two-pole whose cutoff opens a little on the note and settles over a third of a
// second, held flat until the gate, 30 ms release. The movement is the sequence, not the filter.

export function arpVoice(ctx, ev, o) {
  const sr = ctx.sr;
  const f = mtof(ev.midi);
  const vel = ev.vel === undefined ? 0.85 : ev.vel;
  const rng = new Random(hash(ev.seed, "arpv"));
  const det = (o.detune === undefined ? 6 : o.detune) / 1200 * Math.LN2;
  const s1 = BlepOsc.saw(sr, rng.next()), s2 = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, o.hp === undefined ? 150 : o.hp, 0.7);
  const base = o.cutoff === undefined ? 1200 : o.cutoff;
  const amt = o.envAmount === undefined ? 900 : o.envAmount;
  const tauF = o.filterDecay === undefined ? 0.3 : o.filterDecay;
  const q = o.q === undefined ? 0.75 : o.q;
  const open = ev.open === undefined ? (o.open === undefined ? 1 : o.open) : ev.open;
  const attack = o.attack === undefined ? 0.004 : o.attack;
  const release = o.release === undefined ? 0.03 : o.release;
  const subMix = o.sub === undefined ? 0.35 : o.sub;
  const level = (o.level === undefined ? 0.32 : o.level) * (0.7 + 0.3 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const fc = clamp((base + amt * vel * Math.exp(-t / tauF)) * open, 80, 16000);
      lp1.set("lowpass", fc, q); lp2.set("lowpass", fc * 1.3, 0.6);
    }
    let x = s1.next(f * Math.exp(-det * 0.5)) * 0.5 + s2.next(f * Math.exp(det * 0.5)) * 0.5 + sq.next(f * 0.5) * subMix;
    x = lp2.process(lp1.process(x));
    let a = t < attack ? smooth(t / attack) : 1;
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / release);
    return hp.process(softclip(x * 1.2)) * a * level;
  };
}

// ---------------------------------------------------------------- synth brass (mono)
//
// Three saws eight cents apart and a narrow pulse an octave down, a lowpass that opens over the
// first 40 ms and settles (the brass "wah"), a 25 ms attack, 80 ms release. No vibrato. For
// stabs and held chords.

export function brass(ctx, ev, o) {
  const sr = ctx.sr;
  const f = mtof(ev.midi);
  const vel = ev.vel === undefined ? 0.9 : ev.vel;
  const rng = new Random(hash(ev.seed, "brass"));
  const det = (o.detune === undefined ? 8 : o.detune) / 1200 * Math.LN2;
  const s = [BlepOsc.saw(sr, rng.next()), BlepOsc.saw(sr, rng.next()), BlepOsc.saw(sr, rng.next())];
  const fr = [f * Math.exp(-det), f, f * Math.exp(det)];
  const pulse = Osc.pulse(sr, 0.3, rng.next());
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, o.hp === undefined ? 120 : o.hp, 0.7);
  const base = o.cutoff === undefined ? 700 : o.cutoff;
  const amt = o.envAmount === undefined ? 2600 : o.envAmount;
  const rise = o.filterAttack === undefined ? 0.04 : o.filterAttack;
  const tauF = o.filterDecay === undefined ? 0.25 : o.filterDecay;
  const settle = o.settle === undefined ? 0.45 : o.settle;
  const attack = o.attack === undefined ? 0.025 : o.attack;
  const release = o.release === undefined ? 0.08 : o.release;
  const drive = o.drive === undefined ? 1.6 : o.drive;
  const open = ev.open === undefined ? (o.open === undefined ? 1 : o.open) : ev.open;
  const level = (o.level === undefined ? 0.3 : o.level) * (0.6 + 0.4 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const env = (1 - Math.exp(-t / rise)) * (settle + (1 - settle) * Math.exp(-t / tauF));
      const fc = clamp((base + amt * (0.5 + 0.5 * vel) * env) * open, 100, 14000);
      lp1.set("lowpass", fc, 0.9); lp2.set("lowpass", fc * 1.4, 0.6);
    }
    let x = 0;
    for (let i = 0; i < 3; i++) x += s[i].next(fr[i]);
    x = x * 0.3 + pulse.next(f * 0.5) * 0.35;
    x = softclip(x * drive);
    x = lp2.process(lp1.process(x));
    let a = t < attack ? smooth(t / attack) : 1;
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / release);
    return hp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- growl bass (mono)
//
// A saw and a sub square driven hard, then through the three vowel formants of vocal.js, the
// vowel sliding from "a" to "o" over each note (the darksynth talkbox growl) or on a slow LFO
// when `o.lfoRate` is set. A third of the clipped signal bypasses the formants so the low end
// stays, and the sum is soft-clipped again because a formant peak on a saw is loud.

const GROWL_O = [570, 840, 2410];
const GROWL_A = [730, 1090, 2440];
const GROWL_BW = [90, 110, 160];
const GROWL_GAIN = [1, 0.6, 0.25];

export function growlBass(ctx, ev, o) {
  const sr = ctx.sr;
  const fTo = mtof(ev.midi);
  const fFrom = ev.from === undefined ? fTo : mtof(ev.from);
  const logR = Math.log(fTo / fFrom);
  const glide = o.glide === undefined ? 0.06 : o.glide;
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const rng = new Random(hash(ev.seed, "growl"));
  const saw = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const res = [new Resonator(sr), new Resonator(sr), new Resonator(sr)];
  const lp = Biquad.lowpass(sr, o.cutoff === undefined ? 3200 : o.cutoff, 0.7);
  const hp = Biquad.highpass(sr, 45, 0.7);
  const drive = o.drive === undefined ? 3.5 : o.drive;
  const lfoRate = o.lfoRate === undefined ? 0 : o.lfoRate;
  const vowelTau = o.vowelDecay === undefined ? 0.18 : o.vowelDecay;
  const tOff = ev.t;
  const level = (o.level === undefined ? 0.4 : o.level) * (0.75 + 0.25 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const u = lfoRate > 0 ? 0.5 + 0.5 * Math.sin(TAU * lfoRate * (t + tOff)) : Math.exp(-t / vowelTau);
      for (let j = 0; j < 3; j++) res[j].set(GROWL_O[j] + (GROWL_A[j] - GROWL_O[j]) * u, GROWL_BW[j]);
    }
    const f = logR === 0 ? fTo : fFrom * Math.exp(logR * (t < glide ? t / glide : 1));
    let x = saw.next(f) * 0.6 + sq.next(f * 0.5) * 0.5;
    x = Math.tanh(x * drive);
    let y = 0;
    for (let j = 0; j < 3; j++) y += res[j].process(x) * GROWL_GAIN[j];
    y = softclip(y * 0.5 + x * 0.35);
    let a = t < 0.003 ? t / 0.003 : 1;
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / 0.03);
    return hp.process(lp.process(y)) * a * level;
  };
}

// ---------------------------------------------------------------- soft tone (mono)
//
// A sine with a little triangle, a slow attack and a long release, wobbling a few cents like a
// tape loop. `ev.gate` is the held length; the note rings on after it.

export function toneVoice(ctx, ev, o) {
  const sr = ctx.sr;
  const f = mtof(ev.midi);
  const rng = new Random(hash(ev.seed, "tone"));
  const ph = new Phasor(sr, rng.next());
  const tri = Osc.tri(sr, rng.next());
  const triMix = o.tri === undefined ? 0.3 : o.tri;
  const attack = o.attack === undefined ? 2.5 : o.attack;
  const release = o.release === undefined ? 7 : o.release;
  const lp = Biquad.lowpass(sr, o.cutoff === undefined ? 2500 : o.cutoff, 0.6);
  const level = o.level === undefined ? 0.2 : o.level;
  const wob = (o.wobble === undefined ? 4 : o.wobble) / 1200 * Math.LN2;
  const wr = 0.08 + rng.next() * 0.06, wp = rng.next() * TAU;
  return function (t) {
    const m = Math.exp(wob * Math.sin(TAU * wr * t + wp));
    const x = Math.sin(TAU * ph.next(f * m)) + tri.next(f * m) * triMix;
    const a = t < attack ? 1 - Math.exp(-3 * t / attack) : t < ev.gate ? 1 : Math.exp(-(t - ev.gate) / release);
    return lp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- the 80s kit and the noises

/**
 * Gated-reverb snare: a 200 Hz body falling from 320 and a wide noise burst with four early
 * reflections, the whole thing held for `hold` seconds and then cut in 6 ms.
 */
export function gatedSnareHit(ctx, seed, o) {
  const sr = ctx.sr;
  const p = o || {};
  const hold = p.hold === undefined ? 0.11 : p.hold;
  const pitch = p.pitch === undefined ? 1 : p.pitch;
  const out = buffer(sr, hold + 0.03);
  const noise = new Noise(seed);
  const bp = Biquad.bandpass(sr, 2400 * pitch, 0.5);
  const hp = Biquad.highpass(sr, 900 * pitch, 0.7);
  const pk = Biquad.peak(sr, 5000, 1, 4);
  const taps = [0.007, 0.013, 0.019, 0.029].map((s) => Math.round(s * sr));
  const room = new Float32Array(out.length);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = pitch * (200 + 120 * Math.exp(-t / 0.018));
    phase += f / sr;
    const body = Math.sin(TAU * phase) * Math.exp(-t / 0.05) * 0.7;
    const burst = pk.process(hp.process(bp.process(noise.white() * 2))) * (0.6 + 0.4 * Math.exp(-t / 0.04));
    room[i] = burst;
    let n = burst;
    for (let k = 0; k < 4; k++) if (i - taps[k] >= 0) n += room[i - taps[k]] * 0.45;
    const gate = t < hold ? 1 : Math.max(0, 1 - (t - hold) / 0.006);
    out[i] = softclip((body + n * 0.7) * 1.5) * gate * (1 - Math.exp(-t / 0.0004));
  }
  return out;
}

/** Industrial kick: a sine falling 190 -> 44 Hz driven hard into tanh, a click and a 4 ms crack, 0.32 s. */
export function industrialKick(ctx, seed, o) {
  const sr = ctx.sr;
  const p = o || {};
  const drive = p.drive === undefined ? 3.5 : p.drive;
  const pitch = p.pitch === undefined ? 1 : p.pitch;
  const out = buffer(sr, 0.32);
  const noise = new Noise(seed);
  const crackHp = Biquad.highpass(sr, 1800, 0.7);
  const lp = Biquad.lowpass(sr, p.cutoff === undefined ? 7000 : p.cutoff, 0.7);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = pitch * (44 + 146 * Math.exp(-t / 0.016) + 40 * Math.exp(-t / 0.06));
    phase += f / sr;
    const amp = (1 - Math.exp(-t / 0.0004)) * (0.8 * Math.exp(-t / 0.11) + 0.4 * Math.exp(-t / 0.03));
    const body = Math.tanh(Math.sin(TAU * phase) * drive * (1 + 2 * Math.exp(-t / 0.03))) * amp;
    const crack = crackHp.process(noise.white()) * Math.exp(-t / 0.004) * 0.7;
    const click = Math.sin(TAU * 2600 * t) * Math.exp(-t / 0.0012) * 0.4;
    out[i] = lp.process(body + crack + click);
  }
  return tailFade(peakTo(out, 1), sr, 0.01);
}

/**
 * A siren: a sine sweeping between `lo` and `hi` Hz on a triangle at `rate` Hz for `seconds`,
 * a touch of second harmonic, fading in and out over `fade` seconds.
 */
export function sirenHit(ctx, seed, seconds, o) {
  const sr = ctx.sr;
  const p = o || {};
  const lo = p.lo === undefined ? 600 : p.lo, hi = p.hi === undefined ? 1200 : p.hi;
  const rate = p.rate === undefined ? 0.6 : p.rate;
  const fade = p.fade === undefined ? 0.5 : p.fade;
  const out = buffer(sr, seconds);
  const ph = new Phasor(sr, 0);
  const lp = Biquad.lowpass(sr, 4000, 0.7);
  const logR = Math.log(hi / lo);
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const u = 2 * Math.abs(((t * rate) % 1) - 0.5);
    const f = lo * Math.exp(logR * (1 - u));
    const pph = ph.next(f);
    const x = Math.sin(TAU * pph) + 0.15 * Math.sin(2 * TAU * pph);
    const a = Math.min(1, t / fade, (seconds - t) / fade);
    out[i] = lp.process(x) * a * 0.8;
  }
  return out;
}

// ---------------------------------------------------------------- torn bass (mono)
//
// The darksynth bass: two saws and a sub square into a screaming two-pole with a fast envelope,
// THEN torn -- a wavefolder and a hard clipper on the filtered signal, with a tanh copy under
// it for weight -- and a tone lowpass to keep the shreds below 3 kHz. Sidechain it to silence
// on the kick from the layer; the tearing is the instrument, the pumping is the rhythm.

export function tornBass(ctx, ev, o) {
  const sr = ctx.sr;
  const fTo = mtof(ev.midi);
  const fFrom = ev.from === undefined ? fTo : mtof(ev.from);
  const logR = Math.log(fTo / fFrom);
  const glide = o.glide === undefined ? 0.05 : o.glide;
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const rng = new Random(hash(ev.seed, "torn"));
  const det = (o.detune === undefined ? 9 : o.detune) / 1200 * Math.LN2;
  const s1 = BlepOsc.saw(sr, rng.next()), s2 = BlepOsc.saw(sr, rng.next());
  const sq = BlepOsc.square(sr, rng.next());
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, 45, 0.7);
  const tone = Biquad.lowpass(sr, o.tone === undefined ? 2800 : o.tone, 0.7);
  const base = o.cutoff === undefined ? 150 : o.cutoff;
  const amt = o.envAmount === undefined ? 2600 : o.envAmount;
  const tauF = o.filterDecay === undefined ? 0.12 : o.filterDecay;
  const q = o.q === undefined ? 2.6 : o.q;
  const drive = o.drive === undefined ? 6 : o.drive;
  const tear = o.tear === undefined ? 0.6 : o.tear;          // share of the folded/clipped copy
  const open = ev.open === undefined ? (o.open === undefined ? 1 : o.open) : ev.open;
  const sus = o.sustain === undefined ? 0.85 : o.sustain;
  const release = o.release === undefined ? 0.03 : o.release;
  const level = (o.level === undefined ? 0.45 : o.level) * (0.75 + 0.25 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const fc = clamp((base + amt * (0.4 + 0.6 * vel) * Math.exp(-t / tauF) + 80) * open, 60, 9000);
      lp1.set("lowpass", fc, q); lp2.set("lowpass", fc * 1.3, 0.7);
    }
    const f = logR === 0 ? fTo : fFrom * Math.exp(logR * (t < glide ? t / glide : 1));
    let x = s1.next(f * Math.exp(-det * 0.5)) * 0.5 + s2.next(f * Math.exp(det * 0.5)) * 0.5 + sq.next(f * 0.5) * 0.45;
    x = lp2.process(lp1.process(x * 1.5));
    const torn = hardclip(fold(x * drive * 0.5) * 1.3);
    const warm = Math.tanh(x * drive);
    let y = torn * tear + warm * (1 - tear);
    let a = t < 0.002 ? t / 0.002 : sus + (1 - sus) * Math.exp(-t / 0.1);
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / release);
    return hp.process(tone.process(y)) * a * level;
  };
}

// ---------------------------------------------------------------- pulse lead (mono)
//
// Two pulses a few cents apart whose width sweeps on a slow LFO (the PWM that Carpenter's
// leads are made of), driven, lowpassed, glide on `ev.from`, vibrato after a fifth of a
// second. Naive pulses: they alias above a few kHz, which under the lowpass is grit, not a bug.

export function pulseLead(ctx, ev, o) {
  const sr = ctx.sr;
  const fTo = mtof(ev.midi);
  const fFrom = ev.from === undefined ? fTo : mtof(ev.from);
  const logR = Math.log(fTo / fFrom);
  const glide = o.glide === undefined ? 0.08 : o.glide;
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const rng = new Random(hash(ev.seed, "pulse"));
  const ph1 = new Phasor(sr, rng.next()), ph2 = new Phasor(sr, rng.next());
  const det = (o.detune === undefined ? 8 : o.detune) / 1200 * Math.LN2;
  const pwmRate = o.pwmRate === undefined ? 0.7 : o.pwmRate;
  const pwmDepth = o.pwmDepth === undefined ? 0.3 : o.pwmDepth;
  const pwmPh = rng.next() * TAU;
  const lp1 = new Biquad(sr), lp2 = new Biquad(sr);
  const hp = Biquad.highpass(sr, 200, 0.7);
  const base = o.cutoff === undefined ? 2200 : o.cutoff;
  const amt = o.envAmount === undefined ? 2500 : o.envAmount;
  const drive = o.drive === undefined ? 2.5 : o.drive;
  const attack = o.attack === undefined ? 0.01 : o.attack;
  const release = o.release === undefined ? 0.15 : o.release;
  const sus = o.sustain === undefined ? 0.85 : o.sustain;
  const vibDepth = (o.vibrato === undefined ? 10 : o.vibrato) / 1200 * Math.LN2;
  const vibRate = o.vibRate === undefined ? 5.2 : o.vibRate;
  const vibPh = rng.next() * TAU;
  const level = (o.level === undefined ? 0.36 : o.level) * (0.65 + 0.35 * vel);
  return function (t, k) {
    if ((k & 15) === 0) {
      const fc = clamp(base + amt * (0.4 + 0.6 * vel) * (Math.exp(-t / 0.3) * 0.7 + 0.3), 150, 15000);
      lp1.set("lowpass", fc, 0.9); lp2.set("lowpass", fc * 1.4, 0.6);
    }
    const g = t < glide ? t / glide : 1;
    const vib = t < 0.2 ? 0 : smooth((t - 0.2) / 0.3) * vibDepth * Math.sin(TAU * vibRate * t + vibPh);
    const f = fFrom * Math.exp(logR * g + vib);
    const w = 0.5 + pwmDepth * Math.sin(TAU * pwmRate * t + pwmPh);
    const p1 = ph1.next(f * Math.exp(-det * 0.5)), p2 = ph2.next(f * Math.exp(det * 0.5));
    let x = (p1 < w ? 1 : -1) * 0.5 + (p2 < w ? 1 : -1) * 0.5;
    x = softclip(x * drive);
    x = lp2.process(lp1.process(x));
    let a = t < attack ? smooth(t / attack) : sus + (1 - sus) * Math.exp(-(t - attack) / 0.2);
    if (t > ev.gate) a *= Math.exp(-(t - ev.gate) / release);
    return hp.process(x) * a * level;
  };
}

// ---------------------------------------------------------------- swarm (stereo)
//
// A cloud of bees: `voices` saws a note spread over +-`spread` cents, half left and half right,
// each with its own phase, driven into a bandpass on the note's third harmonic (the bzz) with the
// raw saws under it for body; `ring` mixes in a ring-modulated copy (the machine in the bee).
// Fast attack, a short release after the gate; level is keyed to the voice count so a chord of
// bees is no louder than one bee.

export function swarm(ctx, ev, o) {
  const sr = ctx.sr;
  const f0 = mtof(ev.midi);
  const rng = new Random(hash(ev.seed, "swarm"));
  const n = o.voices === undefined ? 12 : o.voices;
  const spread = o.spread === undefined ? 22 : o.spread;
  const oscs = [], det = [];
  for (let v = 0; v < n; v++) { oscs.push(BlepOsc.saw(sr, rng.next())); det.push(Math.exp(rng.bipolar() * spread / 1200 * Math.LN2)); }
  const bz = [Biquad.bandpass(sr, Math.min(16000, f0 * 3), 2.2), Biquad.bandpass(sr, Math.min(16000, f0 * 3), 2.2)];
  const cut = o.cutoff === undefined ? 9000 : o.cutoff;
  const lp = [Biquad.lowpass(sr, cut, 0.7), Biquad.lowpass(sr, cut, 0.7)];
  const hp = [Biquad.highpass(sr, o.hp === undefined ? 180 : o.hp, 0.7), Biquad.highpass(sr, o.hp === undefined ? 180 : o.hp, 0.7)];
  const ring = o.ring === undefined ? 0 : o.ring;
  const gate = ev.gate, att = o.attack === undefined ? 0.003 : o.attack, rel = o.release === undefined ? 0.04 : o.release;
  const drive = o.drive === undefined ? 2.4 : o.drive;
  const vel = ev.vel === undefined ? 1 : ev.vel;
  const level = (o.level === undefined ? 0.3 : o.level) * (0.6 + 0.4 * vel) / Math.sqrt(n);
  const half = n >> 1;
  return function (t, k, sc) {
    let l = 0, r = 0;
    for (let v = 0; v < n; v++) { const s = oscs[v].next(f0 * det[v]); if (v < half) l += s; else r += s; }
    let a = t < att ? t / att : 1;
    if (t > gate) a *= Math.exp(-(t - gate) / rel);
    if (ring > 0) { const m = Math.sin(TAU * 61 * t); l = l * (1 - ring) + l * m * ring; r = r * (1 - ring) + r * m * ring; }
    const xl = softclip((l * 0.45 + bz[0].process(l) * 1.1) * drive), xr = softclip((r * 0.45 + bz[1].process(r) * 1.1) * drive);
    sc[0] = hp[0].process(lp[0].process(xl)) * a * level;
    sc[1] = hp[1].process(lp[1].process(xr)) * a * level;
  };
}
