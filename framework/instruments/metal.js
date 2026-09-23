// syrinx-framework: instruments/metal -- the guitars, the metal kit, the choir and the orchestral hit.
//
// The guitar is a Karplus-Strong string (loop length sr/f - 1 - tau(f), decay solved per pass),
// rendered CLEAN into a mono track, and the amp runs on the summed track -- because a real amp
// distorts the sum of the strings, and the intermodulation of a power chord through a gain stage
// is the metal sound. Two tracks with their own pick noise and a few milliseconds between them,
// hard left and right, are the double-tracked wall.

import { Random, hash } from "syrinx";
import {
  TAU, Osc, BlepOsc, Phasor, Noise, Biquad, OnePole, Delay,
  softclip, clamp, mtof,
} from "../dsp.js";
import { isLayer } from "../music.js";

function smooth(u) { return u <= 0 ? 0 : u >= 1 ? 1 : u * u * (3 - 2 * u); }

// ---------------------------------------------------------------- the string

/**
 * A plucked string. `o.damp` is the loop lowpass in Hz (a palm mute is a few hundred, an open
 * string a few thousand), `o.sustain` the decay time constant in seconds, `o.excCut` / `o.excLen`
 * the pick burst, `o.pick` its level. `ev.from` glides in over `o.glide`; `o.vibrato` (cents) at
 * `o.vibRate` arrives after `o.vibDelay`. The note is gated at `ev.gate`, as a re-picked string is.
 */
export function ksString(ctx, ev, o) {
  const sr = ctx.sr;
  const det = o.detune === undefined ? 0 : o.detune;
  const f = mtof(ev.midi) * Math.exp(det / 1200 * Math.LN2);
  const damp = o.damp;
  const pole = Math.exp(-TAU * damp / sr);
  const w = TAU * f / sr;
  const cw = Math.cos(w);
  const tau = (pole * cw - pole * pole) / (1 - 2 * pole * cw + pole * pole);
  const N = Math.max(2, sr / f - 1 - tau);
  const period = N / sr;
  const d = new Delay(sr, 0.06);
  const lp = new OnePole(sr);
  const dc = new OnePole(sr);
  const nz = new Noise(hash(ev.seed, "pick"));
  const exc = new Biquad(sr).set("lowpass", o.excCut, 0.7);
  const nExc = Math.max(2, Math.min(Math.round(o.excLen * sr), Math.round(N)));
  const pick = o.pick === undefined ? 1 : o.pick;
  const fb = Math.min(0.99995, Math.exp(-(1 / f) / o.sustain));
  const fFrom = ev.from === undefined ? f : mtof(ev.from) * Math.exp(det / 1200 * Math.LN2);
  const logR = Math.log(f / fFrom);
  const glide = o.glide === undefined ? 0.05 : o.glide;
  const vib = (o.vibrato === undefined ? 0 : o.vibrato) / 1200 * Math.LN2;
  const vibRate = o.vibRate === undefined ? 5.6 : o.vibRate;
  const vibDelay = o.vibDelay === undefined ? 0.3 : o.vibDelay;
  const moving = vib > 0 || logR !== 0;
  const gate = ev.gate === undefined ? 1e9 : ev.gate;
  const relTau = o.release === undefined ? 0.02 : o.release;
  return function (t, i) {
    let per = period;
    if (moving) {
      const g = t < glide ? t / glide : 1;
      const v = t > vibDelay ? smooth((t - vibDelay) / 0.35) * vib * Math.sin(TAU * vibRate * t) : 0;
      per = period * Math.exp(logR * (1 - g) - v);
    }
    const back = d.read(per);
    const y = lp.lp(back, damp);
    let x = 0;
    if (i < nExc) x = exc.process(nz.white()) * (1 - i / nExc) * pick;
    d.write(x + y * fb);
    let out = y - dc.lp(y, 8);
    if (t > gate) out *= Math.exp(-(t - gate) / relTau);
    return out;
  };
}

/** A palm-muted chug: short, dark, a hard pick. */
export function chug(ctx, ev, o) {
  return ksString(ctx, ev, { damp: 650, sustain: 0.085, excCut: 3200, excLen: 0.003, pick: 1.5, ...(o || {}) });
}

/** An open note or a power-chord string: long and bright. */
export function openString(ctx, ev, o) {
  return ksString(ctx, ev, { damp: 6500, sustain: 1.7, excCut: 5500, excLen: 0.0035, pick: 1, ...(o || {}) });
}

/** A lead string: sustain, vibrato, glide. */
export function leadString(ctx, ev, o) {
  return ksString(ctx, ev, { damp: 5200, sustain: 2.6, excCut: 5200, excLen: 0.003, pick: 1, vibrato: 35, vibRate: 5.8, vibDelay: 0.28, glide: 0.06, ...(o || {}) });
}

// ---------------------------------------------------------------- the amp
//
// Tight highpass, a mid push into the first gain stage (the pedal in front), a second stage,
// then the cabinet: a low bump, a scooped low-mid, presence at 2.4 kHz and four poles of
// rolloff above 5 kHz. Returns a per-sample function for one mono track.

export function makeAmp(sr, o) {
  const p = o || {};
  const g1 = p.gain === undefined ? 14 : p.gain;
  const g2 = p.gain2 === undefined ? 3 : p.gain2;
  const level = p.level === undefined ? 0.4 : p.level;
  const hpIn = Biquad.highpass(sr, p.tight === undefined ? 135 : p.tight, 0.8);
  const push = Biquad.peak(sr, 1100, 0.7, p.mid === undefined ? 7 : p.mid);
  const lpIn = Biquad.lowpass(sr, 5200, 0.7);
  const hpMid = Biquad.highpass(sr, 160, 0.7);
  const cabHp = Biquad.highpass(sr, 85, 0.8);
  const cabLow = Biquad.peak(sr, 115, 1.2, p.low === undefined ? 0 : p.low);
  const cabDip = Biquad.peak(sr, 480, 1.0, -3.5);
  const cabPres = Biquad.peak(sr, 2600, 1.0, 8);
  const cabLp1 = Biquad.lowpass(sr, 5300, 0.9);
  const cabLp2 = Biquad.lowpass(sr, 6200, 0.6);
  const cabLp3 = Biquad.lowpass(sr, 8000, 0.5);
  return function (x) {
    let y = lpIn.process(push.process(hpIn.process(x)));
    y = Math.tanh(y * g1);
    y = hpMid.process(y);
    y = Math.tanh(y * g2);
    y = cabLp3.process(cabLp2.process(cabLp1.process(cabPres.process(cabDip.process(cabLow.process(cabHp.process(y)))))));
    return y * level;
  };
}

/** Run an amp over a mono buffer in place. */
export function ampInto(buf, amp) {
  if (isLayer(buf)) {
    buf.jobs.push((planes, offset, n) => { const a = planes[0]; for (let k = 0; k < n; k++) a[k] = amp(a[k]); });
    return buf;
  }
  for (let i = 0; i < buf.length; i++) buf[i] = amp(buf[i]);
  return buf;
}

// ---------------------------------------------------------------- the kit

function buffer(sr, seconds) { return new Float32Array(Math.round(seconds * sr)); }
function tailFade(buf, sr, seconds) {
  const n = Math.min(buf.length, Math.round(seconds * sr));
  for (let i = 0; i < n; i++) buf[buf.length - 1 - i] *= i / n;
  return buf;
}

/** A metal snare: a 195 Hz body falling from 270, a shell ring at 330, and a wide crack. */
export function metalSnare(ctx, seed, o) {
  const sr = ctx.sr;
  const p = o || {};
  const pitch = p.pitch === undefined ? 1 : p.pitch;
  const out = buffer(sr, 0.42);
  const noise = new Noise(seed);
  const bp = Biquad.bandpass(sr, 3800 * pitch, 0.7);
  const hp = Biquad.highpass(sr, 1400 * pitch, 0.7);
  const pk = Biquad.peak(sr, 6500, 1.2, 3);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = pitch * (195 + 75 * Math.exp(-t / 0.03));
    phase += f / sr;
    const body = Math.sin(TAU * phase) * Math.exp(-t / 0.09) * 0.9;
    const ring = Math.sin(TAU * 330 * pitch * t) * Math.exp(-t / 0.07) * 0.3;
    const n = noise.white();
    const crack = (bp.process(n) * 1.6 + pk.process(hp.process(n)) * 0.7) * Math.exp(-t / 0.12);
    out[i] = softclip((body + ring + crack) * 1.8) * (1 - Math.exp(-t / 0.0004));
  }
  return tailFade(out, sr, 0.01);
}

/** A tom at `freq` Hz: a sine falling from 1.6x, a short noise attack, driven a little. */
export function tomHit(ctx, seed, freq) {
  const sr = ctx.sr;
  const out = buffer(sr, 0.5);
  const noise = new Noise(seed);
  const lp = Biquad.lowpass(sr, 2500, 0.7);
  let phase = 0;
  for (let i = 0; i < out.length; i++) {
    const t = i / sr;
    const f = freq * (1 + 0.6 * Math.exp(-t / 0.04));
    phase += f / sr;
    const body = Math.sin(TAU * phase) * Math.exp(-t / 0.22);
    const att = lp.process(noise.white()) * Math.exp(-t / 0.004) * 0.6;
    out[i] = softclip((body + att) * 1.7) * (1 - Math.exp(-t / 0.0006));
  }
  return tailFade(out, sr, 0.02);
}

/** A china: a crash with the mids pushed and a short decay, stereo. */
export function chinaHit(ctx, seed) {
  const sr = ctx.sr;
  const freqs = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];
  const pair = [];
  for (let c = 0; c < 2; c++) {
    const out = buffer(sr, 1.6);
    const noise = new Noise(hash(seed, c));
    const oscs = freqs.map((f, k) => Osc.square(sr, (k + c) * 0.21));
    const hp = Biquad.highpass(sr, 2600, 0.6);
    const pk = Biquad.peak(sr, 4200, 0.9, 7);
    const lp = Biquad.lowpass(sr, 12000, 0.7);
    for (let i = 0; i < out.length; i++) {
      const t = i / sr;
      let m = 0;
      for (let k = 0; k < 6; k++) m += oscs[k].next(freqs[k] * (c === 0 ? 4.7 : 4.9));
      const x = m * 0.15 + noise.white() * 0.6;
      const e = (1 - Math.exp(-t / 0.002)) * (Math.exp(-t / 0.45) * 0.8 + 0.2 * Math.exp(-t / 0.1));
      out[i] = lp.process(pk.process(hp.process(x))) * e;
    }
    pair.push(tailFade(out, sr, 0.05));
  }
  return pair;
}

// ---------------------------------------------------------------- choir (stereo)
//
// Four detuned saws with a slow vibrato, into three parallel formant bandpasses that morph
// between "oo" and "ah" on a slow LFO. It is the synthetic choir every futurepop record
// carries, on purpose.

const OO = [325, 700, 2530];
const AH = [700, 1220, 2600];
const FORMANT_GAIN = [1, 0.55, 0.22];

export function choir(ctx, ev, o) {
  const sr = ctx.sr;
  const f0 = mtof(ev.midi);
  const rng = new Random(hash(ev.seed, "choir"));
  const det = o.detune === undefined ? 14 : o.detune;
  const cents = [-det, -det * 0.35, det * 0.4, det];
  const oL = [], oR = [], fr = [];
  for (let k = 0; k < 4; k++) {
    fr.push(f0 * Math.exp(cents[k] / 1200 * Math.LN2));
    oL.push(BlepOsc.saw(sr, rng.next())); oR.push(BlepOsc.saw(sr, rng.next()));
  }
  const bL = [], bR = [];
  for (let k = 0; k < 3; k++) { bL.push(new Biquad(sr)); bR.push(new Biquad(sr)); }
  const hpL = Biquad.highpass(sr, 160, 0.7), hpR = Biquad.highpass(sr, 160, 0.7);
  const morphRate = o.morphRate === undefined ? 0.07 : o.morphRate;
  const ph = rng.next() * TAU;
  const vibPh = rng.next() * TAU;
  const attack = o.attack === undefined ? 0.6 : o.attack;
  const release = o.release === undefined ? 1.5 : o.release;
  const level = o.level === undefined ? 0.25 : o.level;
  const tOff = ev.t;
  return function (t, k, out) {
    if ((k & 63) === 0) {
      const u = 0.5 + 0.5 * Math.sin(TAU * morphRate * (t + tOff) + ph);
      for (let j = 0; j < 3; j++) {
        const fc = OO[j] * Math.exp(Math.log(AH[j] / OO[j]) * u);
        bL[j].set("bandpass", fc, 9); bR[j].set("bandpass", fc * 1.015, 9);
      }
    }
    const v = t > 0.4 ? smooth((t - 0.4) / 0.5) * 0.006 * Math.sin(TAU * 5.3 * t + vibPh) : 0;
    const m = Math.exp(v);
    let l = 0, r = 0;
    for (let i = 0; i < 4; i++) { l += oL[i].next(fr[i] * m); r += oR[i].next(fr[i] * m); }
    let fl = 0, frr = 0;
    for (let j = 0; j < 3; j++) { fl += bL[j].process(l) * FORMANT_GAIN[j]; frr += bR[j].process(r) * FORMANT_GAIN[j]; }
    const a = (t < attack ? smooth(t / attack) : t < ev.gate ? 1 : Math.exp(-(t - ev.gate) / release)) * level;
    out[0] = hpL.process(fl) * a;
    out[1] = hpR.process(frr) * a;
  };
}

// ---------------------------------------------------------------- orchestral hit (stereo, rendered once)
//
// A stack of detuned saws on the chord plus its octave, a sub thump and a noise crack, all
// decaying together in a third of a second. Stamp it on the accents.

export function orchHit(ctx, seed, notes) {
  const sr = ctx.sr;
  const pair = [];
  for (let c = 0; c < 2; c++) {
    const out = buffer(sr, 0.9);
    const rng = new Random(hash(seed, "orch", c));
    const noise = new Noise(hash(seed, "crack", c));
    const oscs = [], fr = [];
    for (let n = 0; n < notes.length; n++) {
      const f = mtof(notes[n]);
      for (let k = 0; k < 3; k++) { oscs.push(BlepOsc.saw(sr, rng.next())); fr.push(f * Math.exp((k - 1) * 9 / 1200 * Math.LN2)); }
      oscs.push(BlepOsc.saw(sr, rng.next())); fr.push(f * 2 * Math.exp(6 / 1200 * Math.LN2));
    }
    const lp = Biquad.lowpass(sr, 3800, 0.8);
    const hp = Biquad.highpass(sr, 90, 0.7);
    const crackHp = Biquad.highpass(sr, 2800, 0.7);
    let phase = 0;
    for (let i = 0; i < out.length; i++) {
      const t = i / sr;
      let s = 0;
      for (let k = 0; k < oscs.length; k++) s += oscs[k].next(fr[k]);
      s = s / oscs.length * 2.2;
      const env = (1 - Math.exp(-t / 0.004)) * Math.exp(-t / 0.32);
      phase += (48 + 30 * Math.exp(-t / 0.05)) / sr;
      const thump = Math.sin(TAU * phase) * Math.exp(-t / 0.18) * 0.3;
      const crack = crackHp.process(noise.white()) * Math.exp(-t / 0.012) * 0.5;
      out[i] = hp.process(lp.process(softclip(s * 1.3))) * env + thump * (1 - Math.exp(-t / 0.002)) + crack;
    }
    pair.push(tailFade(out, sr, 0.02));
  }
  return pair;
}
