// syrinx-framework: voice/vocal -- a singing and speaking voice, by formant synthesis.
//
// Source: a Rosenberg glottal pulse (its derivative, so the closure spike is the excitation),
// with jitter, shimmer, breath noise, vibrato and a pitch contour. Tract: five two-pole
// resonators in cascade on the vowel's formants, targets gliding between phonemes. Unvoiced
// consonants are shaped noise; plosives are a closure, a burst and the formant transition out
// of it. Everything is a function of time and seeded noise: the same phrase renders the same.

import { Random, hash } from "syrinx";
import { TAU, Noise, Biquad, OnePole, Reverb, clamp, mtof } from "../dsp.js";

// ---------------------------------------------------------------- resonator (2-pole, unity at DC)

export class Resonator {
  constructor(sr) { this.sr = sr; this.a = 1; this.b = 0; this.c = 0; this.y1 = 0; this.y2 = 0; }
  set(f, bw) {
    const r = Math.exp(-Math.PI * bw / this.sr);
    this.c = -r * r;
    this.b = 2 * r * Math.cos(TAU * f / this.sr);
    this.a = 1 - this.b - this.c;
  }
  process(x) { const y = this.a * x + this.b * this.y1 + this.c * this.y2; this.y2 = this.y1; this.y1 = y; return y; }
}

// ---------------------------------------------------------------- phonemes
//
// Formants F1 F2 F3 in Hz (Peterson & Barney male averages for the vowels), a level, whether
// the source is voiced, and for the noisy ones a noise band. `locus` is where F2 starts the
// transition out of a plosive.

export const PH = {
  // vowels: Standard Southern British, male (Deterding 1997 / Hawkins & Midgley 2005 ballpark)
  a:  { f: [680, 1100, 2450], amp: 1, long: true },              // PALM, START
  ae: { f: [720, 1550, 2450], amp: 1 },                          // TRAP (modern, open)
  e:  { f: [520, 1880, 2500], amp: 1 },                          // DRESS
  i:  { f: [280, 2250, 2950], amp: 0.9, long: true },            // FLEECE
  ih: { f: [380, 1990, 2550], amp: 0.95 },                       // KIT
  o:  { f: [570, 880, 2450], amp: 1 },                           // LOT
  aw: { f: [450, 720, 2500], amp: 1, long: true },               // THOUGHT, NORTH
  u:  { f: [300, 870, 2240], amp: 0.9 },
  uf: { f: [340, 1600, 2400], amp: 0.9, long: true },            // GOOSE (fronted)
  uu: { f: [420, 1150, 2350], amp: 0.95 },                       // FOOT
  uh: { f: [650, 1250, 2400], amp: 1 },                          // STRUT
  er: { f: [520, 1450, 2450], amp: 0.95, long: true },           // NURSE (non-rhotic: no r-colouring)
  x:  { f: [500, 1500, 2500], amp: 0.9 },                        // schwa
  ai: { f: [750, 1300, 2450], f2: [450, 1950, 2600], amp: 1 },   // PRICE
  ei: { f: [530, 1850, 2500], f2: [400, 2050, 2700], amp: 1 },   // FACE
  ou: { f: [500, 1300, 2450], f2: [400, 1000, 2300], amp: 1 },   // GOAT (starts central)
  au: { f: [750, 1400, 2450], f2: [450, 1050, 2300], amp: 1 },   // MOUTH
  oi: { f: [450, 750, 2500], f2: [430, 1900, 2600], amp: 1 },    // CHOICE
  ia: { f: [390, 1990, 2550], f2: [500, 1500, 2500], amp: 1 },   // "here" (non-rhotic)
  ea: { f: [530, 1840, 2480], f2: [500, 1500, 2500], amp: 1 },   // "there"
  ua: { f: [440, 1020, 2240], f2: [500, 1500, 2500], amp: 0.95 }, // "sure"
  m:  { f: [250, 1000, 2200], bw: [120, 260, 300], amp: 0.38, nasal: true, zero: 900, locus: 800 },
  n:  { f: [250, 1550, 2500], bw: [120, 260, 300], amp: 0.38, nasal: true, zero: 1500, locus: 1700 },
  ng: { f: [250, 1200, 2300], bw: [120, 260, 300], amp: 0.38, nasal: true, zero: 2600, locus: 2100 },
  l:  { f: [360, 1300, 2700], amp: 0.55 },
  lx: { f: [420, 800, 2600], amp: 0.55 },                        // dark l, in codas ("wall")
  r:  { f: [350, 1100, 1500], amp: 0.55 },
  w:  { f: [300, 650, 2200], amp: 0.55 },
  y:  { f: [270, 2200, 3000], amp: 0.55 },
  v:  { f: [250, 1100, 2300], bw: [120, 200, 260], amp: 0.32, noise: { f: 3000, bw: 6000, level: 0.2 } },
  z:  { f: [250, 1500, 2500], bw: [120, 200, 260], amp: 0.3, noise: { f: 6500, bw: 2500, level: 0.5 } },
  zh: { f: [250, 1500, 2500], bw: [120, 200, 260], amp: 0.3, noise: { f: 3000, bw: 1600, level: 0.5 } },
  dh: { f: [300, 1400, 2500], bw: [120, 200, 260], amp: 0.32, noise: { f: 3000, bw: 6000, level: 0.15 } },
  s:  { voiced: false, noise: { f: 6500, bw: 2600, level: 1.4 } },
  sh: { voiced: false, noise: { f: 3000, bw: 1600, level: 1.3 } },
  f:  { voiced: false, noise: { f: 2500, bw: 6000, level: 0.2 } },
  th: { voiced: false, noise: { f: 3000, bw: 6000, level: 0.16 } },
  h:  { voiced: false, aspirate: 0.25 },
  "^": { voiced: false, inhale: true, f: [500, 1500, 2500], amp: 0 },   // a breath in
  p:  { plosive: true, voiced: false, burst: { f: 900, bw: 1400, level: 2.0, tau: 0.005 }, locus: 700 },
  b:  { plosive: true, voiced: true, burst: { f: 900, bw: 1400, level: 1.2, tau: 0.005 }, locus: 700 },
  t:  { plosive: true, voiced: false, burst: { f: 4500, bw: 3000, level: 3.0, tau: 0.004 }, locus: 1800 },
  d:  { plosive: true, voiced: true, burst: { f: 4000, bw: 3000, level: 1.3, tau: 0.004 }, locus: 1800 },
  k:  { plosive: true, voiced: false, burst: { f: 2200, bw: 1400, level: 2.8, tau: 0.008 }, locus: 2300 },
  g:  { plosive: true, voiced: true, burst: { f: 2000, bw: 1400, level: 1.3, tau: 0.008 }, locus: 2300 },
  "-": { silence: true },
};
const BW_DEFAULT = [80, 90, 120];
const F4 = 3400, F5 = 4300, BW4 = 140, BW5 = 180;

function rosenberg(p) {
  const tp = 0.42, tn = 0.16;
  if (p < tp) return 0.5 * (1 - Math.cos(Math.PI * p / tp));
  if (p < tp + tn) return Math.cos(Math.PI * (p - tp) / (2 * tn));
  return 0;
}

/**
 * Render a phrase into a mono Float32Array of `frames`.
 *
 * phrase: [{ ph, dur (s), midi?, hz?, gain? }, ...] -- a vowel on a note is a sung note; give
 * consonants no pitch and they take the neighbouring vowel's. Options: `scale` multiplies the
 * vowel formants (1.17 makes a higher, lighter voice), `vibrato` in cents, `vibRate`, `breath`
 * (0..1), `jitter` (0..1), `glide` seconds between pitches, `trans` seconds for formant
 * transitions, `sung` (true: vibrato and steady pitch; false: spoken, with a falling contour),
 * `singerFormant` dB of lift near 3 kHz, `seed`.
 */
export function voice(ctx, phrase, o) {
  const sr = ctx.sr;
  const p = o || {};
  const scale = p.scale === undefined ? 1 : p.scale;
  const vib = (p.vibrato === undefined ? 35 : p.vibrato) / 1200 * Math.LN2;
  const vibRate = p.vibRate === undefined ? 5.4 : p.vibRate;
  const breath = p.breath === undefined ? 0.06 : p.breath;
  const jitterAmt = p.jitter === undefined ? 0.4 : p.jitter;
  const glide = p.glide === undefined ? 0.06 : p.glide;
  const trans = p.trans === undefined ? 0.055 : p.trans;
  const sung = p.sung === undefined ? true : p.sung;
  const seed = p.seed === undefined ? ctx.seed : p.seed;
  const rng = new Random(hash(seed, "voice"));
  const noise = new Noise(hash(seed, "breath"));
  const fric = new Noise(hash(seed, "fric"));
  const res = [new Resonator(sr), new Resonator(sr), new Resonator(sr), new Resonator(sr), new Resonator(sr)];
  const fricBp = new Biquad(sr);
  const fricHp = new Biquad(sr);
  const burstBp = new Biquad(sr);
  const singer = Biquad.peak(sr, 2900, 1.6, p.singerFormant === undefined ? (sung ? 5 : 0) : p.singerFormant);
  const tilt = new OnePole(sr);
  const tiltHz = p.tilt === undefined ? 1400 : p.tilt;
  const flutter = p.flutter === undefined ? 0.006 : p.flutter;
  let uGlot = 0, pulseParity = 1;
  const contour = p.contour;   // [[t, hz], ...] -- when given, the pitch follows it instead of the segments
  let ci = 0;
  const lp = Biquad.lowpass(sr, 9000, 0.6);
  const antiRes = new Biquad(sr); antiRes.set("notch", 1000, 2.5);
  const aspLp = new OnePole(sr);
  const inhaleLp = new OnePole(sr);
  const inhaleLevel = p.inhale === undefined ? 0.03 : p.inhale;   // the breath in is nearly silent unless asked for
  const aspGain = p.aspiration === undefined ? 1 : p.aspiration;
  const nasalAmp = p.nasalAmp === undefined ? 1 : p.nasalAmp;
  const nasalDecay = p.nasalDecay === undefined ? 0.7 : p.nasalDecay;
  const stopTrans = p.stopTrans === undefined ? 0.03 : p.stopTrans;
  const dc = new OnePole(sr);
  const out = new Float32Array(ctx.frames);

  // lay the segments out in time: each has start, end, formant targets, pitch
  const segs = [];
  let t0 = 0;
  let lastHz = 220;
  for (let k = 0; k < phrase.length; k++) {
    const e = phrase[k];
    const def = PH[e.ph];
    if (def === undefined) throw new Error("unknown phoneme " + e.ph);
    const hz = e.hz !== undefined ? e.hz : e.midi !== undefined ? mtof(e.midi) : undefined;
    if (hz !== undefined) lastHz = hz;
    segs.push({ ph: e.ph, def, t0, t1: t0 + e.dur, hz, gain: e.gain === undefined ? 1 : e.gain, glottal: e.glottal === true,
      creak: e.creak === undefined ? 0 : e.creak, reduce: e.reduce === undefined ? 0 : e.reduce,
      press: e.press === undefined ? 0 : e.press, onset: e.onset === undefined ? 0.012 : e.onset, breathMul: e.breathMul === undefined ? 1 : e.breathMul });
    t0 += e.dur;
  }
  // consonants without a pitch take the next vowel's, else the previous
  for (let k = 0; k < segs.length; k++) {
    if (segs[k].hz !== undefined) continue;
    let j = k + 1; while (j < segs.length && segs[j].hz === undefined) j++;
    if (j < segs.length) segs[k].hz = segs[j].hz;
    else { j = k - 1; while (j >= 0 && segs[j].hz === undefined) j--; segs[k].hz = j >= 0 ? segs[j].hz : 220; }
  }
  const formantsOf = (s, next) => {
    const d = s.def;
    if (d.f !== undefined) {
      const r = s.reduce;
      return [(d.f[0] + (500 - d.f[0]) * r) * scale, (d.f[1] + (1500 - d.f[1]) * r) * scale, (d.f[2] + (2500 - d.f[2]) * r) * scale];
    }
    // noisy or silent segments carry the next vowel's tract shape, from the plosive locus when there is one
    const n = next && next.def.f ? next.def.f : [500, 1500, 2500];
    if (d.plosive) return [250 * scale, d.locus * scale, n[2] * scale];
    if (d.inhale) return [500 * scale, 1500 * scale, 2500 * scale];
    return [n[0] * scale, n[1] * scale, n[2] * scale];
  };
  for (let k = 0; k < segs.length; k++) {
    const s = segs[k];
    s.F = formantsOf(s, segs[k + 1]);
    s.F2 = s.def.f2 !== undefined ? [s.def.f2[0] * scale, s.def.f2[1] * scale, s.def.f2[2] * scale] : null;
    s.BW = s.def.bw !== undefined ? s.def.bw : BW_DEFAULT;
    // voice onset time: a voiceless stop releases into aspiration before the next vowel voices
    s.vot = s.def.plosive ? (s.def.voiced ? 0.012 : (segs[k + 1] && segs[k + 1].def.f !== undefined ? 0.06 : 0.035)) : 0;
    s.final = !segs[k + 1] || segs[k + 1].def.silence === true;
    s.afterStop = k > 0 && segs[k - 1].def.plosive === true;
    s.closure = s.def.plosive ? Math.max(0.025, Math.min(0.06, (s.t1 - s.t0) - s.vot)) : 0;
    s.asp = s.def.plosive && !s.def.voiced ? Math.max(0, (s.t1 - s.t0) - s.closure - 0.01) : 0;
    s.voiced = s.def.voiced === undefined ? !s.def.silence : s.def.voiced;
    s.amp = s.def.amp === undefined ? 0 : s.def.amp;
    s.Fnext = segs[k + 1] && segs[k + 1].def.f ? segs[k + 1].def.f.map((f) => f * scale) : null;
  }
  for (let k = 0; k < segs.length; k++) {
    const s = segs[k], pv = segs[k - 1], nx = segs[k + 1];
    s.zero = s.def.nasal ? s.def.zero : nx && nx.def.nasal ? nx.def.zero : pv && pv.def.nasal ? pv.def.zero : 0;
    s.nasalIn = pv && pv.def.nasal && s.def.f !== undefined ? 1 : 0;    // a vowel after a nasal starts nasalised
    s.nasalOut = nx && nx.def.nasal && s.def.f !== undefined ? 1 : 0;   // and one before a nasal ends nasalised
  }
  // where a vowel runs into a consonant with a shape of its own, it starts moving there early
  for (let k = 0; k < segs.length; k++) {
    const s = segs[k], n = segs[k + 1];
    s.Fanti = null;
    if (s.def.f === undefined || s.def.amp < 0.9 || !n) continue;
    if (n.def.plosive) s.Fanti = [250 * scale, n.def.locus * scale, s.F[2]];
    else if (n.def.nasal) s.Fanti = [250 * scale, n.def.locus * scale, s.F[2]];
    else if (n.def.f !== undefined && n.def.amp < 0.9) s.Fanti = n.F;
  }
  // the voiced source only fades where voicing actually stops: into a closure, a silence or a
  // voiceless segment. Between two voiced segments it runs on and the level glides.
  for (let k = 0; k < segs.length; k++) {
    const s = segs[k];
    const on = (x) => x !== undefined && x.voiced && x.amp > 0;
    s.fadeIn = !on(segs[k - 1]);
    s.fadeOut = !on(segs[k + 1]);
  }

  let phase = 0, uPrev = 0, jitter = 0, shimmer = 1, segIdx = 0;
  let hzCur = segs.length ? segs[0].hz : 220, hzFrom = hzCur, hzTo = hzCur, glideT0 = 0;
  const Fcur = [500, 1500, 2500], Fprev = [500, 1500, 2500];
  const inv = 1 / sr;
  const total = t0;
  let plosiveStart = 0;
  let ampCur = 0;
  const ampK = 1 - Math.exp(-1 / (0.004 * sr));
  const ampKslow = 1 - Math.exp(-1 / (0.015 * sr));
  for (let i = 0; i < ctx.frames; i++) {
    const t = i * inv;
    if (t >= total) break;
    while (segIdx < segs.length - 1 && t >= segs[segIdx].t1) {
      segIdx++;
      Fprev[0] = Fcur[0]; Fprev[1] = Fcur[1]; Fprev[2] = Fcur[2];
      hzFrom = hzCur; hzTo = segs[segIdx].hz; glideT0 = t;
      if (segs[segIdx].def.plosive) plosiveStart = t;
    }
    const s = segs[segIdx];
    const local = t - s.t0;
    const segLen = s.t1 - s.t0;
    if ((i & 15) === 0) {
      // formant glide into this segment; a plosive's vowel-side transition happens in the vowel after it
      const u = clamp(local / (s.afterStop ? stopTrans : trans), 0, 1);
      const uu = u * u * (3 - 2 * u);
      for (let j = 0; j < 3; j++) {
        let target = s.F[j];
        if (s.F2 !== null) { const w = clamp((local - trans) / Math.max(0.05, segLen - trans), 0, 1); target = s.F[j] + (s.F2[j] - s.F[j]) * w * w * (3 - 2 * w); }
        if (s.def.plosive && s.Fnext !== null && local > s.closure) { const w = clamp((local - s.closure) / Math.max(0.02, segLen - s.closure), 0, 1); target = s.F[j] + (s.Fnext[j] - s.F[j]) * 0.8 * w; }
        if (s.Fanti !== null && local > segLen - 0.04) { const w = clamp((local - (segLen - 0.04)) / 0.04, 0, 1); target += (s.Fanti[j] - target) * 0.5 * w * w * (3 - 2 * w); }
        Fcur[j] = Fprev[j] + (target - Fprev[j]) * uu;
      }
      res[1].set(Fcur[1], s.BW[1]); res[2].set(Fcur[2], s.BW[2]);
      res[3].set(F4 * scale, BW4); res[4].set(F5 * scale, BW5);
      if (s.def.noise) { fricBp.set("bandpass", s.def.noise.f, s.def.noise.f / s.def.noise.bw); fricHp.set("highpass", s.def.noise.f * 0.55, 0.7); }
      if (s.def.burst) burstBp.set("bandpass", s.def.burst.f, s.def.burst.f / s.def.burst.bw);
      if (s.zero > 0) antiRes.set("notch", s.zero * scale, 2.5);
      // pitch: glide between notes, a spoken contour falls through the phrase
      const g = clamp((t - glideT0) / glide, 0, 1);
      jitter = jitter * 0.9 + rng.bipolar() * 0.004 * jitterAmt;
      shimmer = 1 + rng.bipolar() * 0.03;
      let hz;
      if (contour !== undefined) {
        while (ci + 1 < contour.length && contour[ci + 1][0] <= t) ci++;
        if (ci + 1 >= contour.length) hz = contour[contour.length - 1][1];
        else { const a = contour[ci], b = contour[ci + 1]; const w = clamp((t - a[0]) / Math.max(1e-6, b[0] - a[0]), 0, 1); const ww = w * w * (3 - 2 * w); hz = Math.exp(Math.log(a[1]) + (Math.log(b[1]) - Math.log(a[1])) * ww); }
      } else {
        hz = hzFrom + (hzTo - hzFrom) * g;
        if (!sung) hz *= Math.exp(-0.18 * t / Math.max(0.5, total)) * (1 + 0.04 * Math.sin(TAU * 0.9 * t));
      }
      hz *= 1 + flutter * (Math.sin(TAU * 12.7 * t) + Math.sin(TAU * 7.1 * t) + Math.sin(TAU * 4.7 * t)) / 3;
      hzCur = hz;
    }
    // envelope of the segment: 12 ms in and out, plosive closure silent until the burst
    const att = s.fadeIn ? clamp(local / s.onset, 0, 1) : 1, rel = s.fadeOut ? clamp((segLen - local) / (s.def.nasal ? 0.04 : 0.015), 0, 1) : 1;
    const env = Math.min(att, rel) * (s.glottal && local < 0.03 ? 0.3 + 0.7 * local / 0.03 : 1);
    const nenv = Math.min(clamp(local / 0.012, 0, 1), clamp((segLen - local) / 0.015, 0, 1));   // the noise paths still fade at their edges
    const releaseVoicing = s.def.plosive && s.def.voiced && local > s.closure ? 0.9 * s.gain : 0;
    let ampTarget = s.voiced ? Math.max(s.amp * s.gain * (s.def.nasal ? nasalAmp : 1), releaseVoicing) : 0;
    if (s.def.nasal && s.fadeOut) ampTarget *= 1 - nasalDecay * clamp(local / segLen, 0, 1);   // a final nasal dies away
    ampCur += (ampTarget - ampCur) * (s.nasalIn && local < 0.04 ? ampKslow : ampK);
    let x = 0;
    if (s.voiced && ampTarget > 0) {
      const v = sung && t > 0.25 ? (1 - Math.exp(-(t - 0.25) / 0.4)) * vib * Math.sin(TAU * vibRate * t) : 0;
      // creak: the last part of a falling phrase drops toward 60 Hz with alternating strong and weak pulses
      const ck = s.creak > 0 ? s.creak * clamp((local - segLen * 0.45) / (segLen * 0.55), 0, 1) : 0;
      const f0 = hzCur * Math.exp(v) * (1 + jitter * (1 + 6 * ck)) * (1 - 0.45 * ck);
      phase += f0 * inv;
      if (phase >= 1) { phase -= 1; pulseParity = -pulseParity; }
      const u = rosenberg(phase);
      uGlot = u;
      const e = tilt.lp((u - uPrev) * sr / f0 * 0.12, tiltHz * (1 + 0.6 * s.press));
      uPrev = u;
      const asp = noise.white() * breath * s.breathMul * (0.25 + 0.75 * u);
      const doubling = 1 + 0.45 * ck * pulseParity;
      x = (e * shimmer * doubling + asp) * ampCur * env;
    } else if (s.def.aspirate) {
      x = noise.white() * s.def.aspirate * env * s.gain * 0.5;
    } else if (s.def.inhale) {
      x = inhaleLp.lp(noise.white(), 1200) * inhaleLevel * Math.sin(Math.PI * clamp(local / segLen, 0, 1)) * s.gain;
    }
    // a stop's aspiration is noise into the cascade, shaped by the tract as it moves toward the vowel
    const since = s.def.plosive ? local - s.closure : -1;
    if (s.asp > 0 && since >= 0.01 && since < 0.01 + s.asp) x += aspLp.lp(fric.white(), 4000) * (1 - (since - 0.01) / s.asp) * s.gain * 0.4 * aspGain;
    if ((i & 7) === 0) res[0].set(Fcur[0] * (1 + 0.07 * uGlot), s.BW[0] * (1 + 0.8 * uGlot));
    let y = res[4].process(res[3].process(res[2].process(res[1].process(res[0].process(x)))));
    // the noise paths bypass the tract
    if (s.def.noise) y += fricHp.process(fricBp.process(fric.white())) * s.def.noise.level * nenv * s.gain * 0.75 * (s.voiced ? 0.4 + 0.6 * uGlot : 1);
    if (s.def.plosive) {
      if (since >= 0 && since < 0.016) y += burstBp.process(fric.white()) * s.def.burst.level * (s.def.voiced && s.final ? 1.5 : 1) * Math.exp(-since / s.def.burst.tau) * s.gain;
      if (s.def.voiced && local < s.closure) y += Math.sin(TAU * hzCur * t) * 0.05 * s.gain;   // the voice bar
    }
    // nasal coupling: the anti-resonance in full through a nasal, fading in and out of its neighbours
    let nasalMix = s.def.nasal ? 1 : 0;
    if (s.nasalOut) nasalMix = Math.max(nasalMix, clamp((local - (segLen - 0.05)) / 0.05, 0, 1) * 0.8);
    if (s.nasalIn) nasalMix = Math.max(nasalMix, clamp(1 - local / 0.04, 0, 1) * 0.8);
    const yN = antiRes.process(y);
    y += (yN - y) * nasalMix;
    y = lp.process(singer.process(y));
    out[i] = y - dc.lp(y, 20);
  }
  return out;
}

/** Sung syllables: [[text, midi, beats], ...] with `text` a space-separated phoneme string
 *  whose vowel gets the note; consonants take `cdur` seconds each. */
export function sing(ctx, syllables, bpm, o) {
  const p = o || {};
  const cdur = p.cdur === undefined ? 0.07 : p.cdur;
  const beat = 60 / bpm;
  const phrase = [];
  for (let k = 0; k < syllables.length; k++) {
    const [text, midi, beats] = syllables[k];
    const phs = text.split(" ").filter((x) => x.length > 0);
    const vowels = phs.filter((x) => PH[x] && PH[x].f !== undefined && PH[x].amp >= 0.9);
    const consonantTime = (phs.length - vowels.length) * cdur;
    const vowelDur = Math.max(0.05, beats * beat - consonantTime) / Math.max(1, vowels.length);
    for (let j = 0; j < phs.length; j++) {
      const isV = vowels.indexOf(phs[j]) >= 0;
      phrase.push({ ph: phs[j], dur: isV ? vowelDur : (PH[phs[j]].plosive ? cdur * 1.4 : cdur), midi: isV ? midi : undefined });
    }
  }
  return voice(ctx, phrase, p);
}

/** Spoken: [[text, hz, seconds], ...]. */
export function speak(ctx, words, o) {
  const p = o || {};
  const cdur = p.cdur === undefined ? 0.06 : p.cdur;
  const phrase = [];
  for (let k = 0; k < words.length; k++) {
    const [text, hz, secs] = words[k];
    const phs = text.split(" ").filter((x) => x.length > 0);
    const vowels = phs.filter((x) => PH[x] && PH[x].f !== undefined && PH[x].amp >= 0.9);
    const vowelDur = Math.max(0.05, secs - (phs.length - vowels.length) * cdur) / Math.max(1, vowels.length);
    for (let j = 0; j < phs.length; j++) {
      const isV = vowels.indexOf(phs[j]) >= 0;
      phrase.push({ ph: phs[j], dur: isV ? vowelDur : cdur, hz: isV ? hz : undefined });
    }
    phrase.push({ ph: "-", dur: 0.08 });
  }
  return voice(ctx, phrase, { sung: false, vibrato: 0, ...p });
}

// ---------------------------------------------------------------- say: speech with prosody
//
// phrases: [{ syl: [["h e l p", 1], ["m i", 0]], end: "fall" | "rise" | "level", pause?: s }, ...]
// Each syllable is a phoneme string and a stress (0, 1, 2). Pitch declines through a phrase from
// 1.12x to 0.9x of `base`, stressed syllables bump it up and stretch, the last syllable falls
// (with a creak) or rises; a breath is taken before each phrase after the first. Returns the
// rendered buffer.

const VOWELS = ["a", "ae", "e", "i", "ih", "o", "aw", "u", "uf", "uu", "uh", "er", "x", "ai", "ei", "ou", "au", "oi", "ia", "ea", "ua"];

export function say(ctx, phrases, o) {
  const pl = plan(phrases, o);
  return voice(ctx, pl.phrase, { sung: false, vibrato: 0, contour: pl.contour, ...(o || {}) });
}

/** The phrase segments and pitch contour that `say` renders: reusable by other engines. */
export function plan(phrases, o) {
  const p = o || {};
  const base = p.base === undefined ? 120 : p.base;
  const rate = p.rate === undefined ? 1 : p.rate;
  const range = p.range === undefined ? 1 : p.range;      // widens or narrows the pitch movement
  const phrase = [];
  const contour = [];
  let t = 0;
  const mark = (hz) => contour.push([t, hz]);
  const bump = (x) => Math.exp(Math.log(x) * range);
  // consonants barely lengthen in slow speech: a fifth of the vowels' stretch
  const crate = Math.exp(Math.log(rate) * 0.3);
  const cdurOf = (ph, def) => {
    if (def.plosive) return (def.voiced ? 0.07 : 0.105) / crate;
    if (def.noise && !def.voiced) return 0.1 / crate;
    if (def.noise) return 0.065 / crate;
    if (ph === "m" || ph === "n" || ph === "ng") return 0.09 / crate;
    if (ph === "h") return 0.06 / crate;
    if (ph === "r" || ph === "l" || ph === "lx" || ph === "w" || ph === "y") return 0.085 / crate;
    return 0.055 / crate;
  };
  for (let pi = 0; pi < phrases.length; pi++) {
    const ph = phrases[pi];
    const n = ph.syl.length;
    if (pi > 0 || ph.breath) { const d = ph.breath === undefined ? 0.22 : ph.breath; if (d > 0) { phrase.push({ ph: "^", dur: d, gain: 0.5 }); t += d; } }
    let prevHz = base * bump(1.1);
    let afterPause = true;
    let prevEndedInVowel = false;
    for (let k = 0; k < n; k++) {
      const [text, stress, wordStart] = ph.syl[k];
      const last = k === n - 1;
      const first = text.split(" ").filter((x) => x.length > 0)[0];
      const glottal = wordStart && k > 0 && prevEndedInVowel && VOWELS.indexOf(first) >= 0;
      const prog = n <= 1 ? 1 : k / (n - 1);
      const decl = Math.exp(Math.log(bump(1.1)) + (Math.log(bump(0.88)) - Math.log(bump(1.1))) * prog);
      const peak = base * decl * (stress === 2 ? bump(1.25) : stress === 1 ? bump(1.12) : 1);
      const low = base * decl * bump(0.95);
      const phs = text.split(" ").filter((x) => x.length > 0);
      const stretch = (stress === 2 ? 1.45 : stress === 1 ? 1.25 : 0.85) * (last ? (n === 1 ? 1.45 : 1.3) : 1) / rate;
      const gain = stress === 2 ? 1.2 : stress === 1 ? 1.08 : 0.85;
      let prevPh = null;
      for (let j = 0; j < phs.length; j++) {
        const isV = VOWELS.indexOf(phs[j]) >= 0;
        const def = PH[phs[j]];
        if (isV) {
          // the coda's voicing sets the vowel's length: short before a voiceless consonant, long before a voiced one
          let coda = 1;
          for (let q = j + 1; q < phs.length; q++) { const cd = PH[phs[q]]; if (!cd || cd.f !== undefined && !cd.nasal && cd.amp >= 0.9) break; coda = cd.voiced === false ? 0.8 : (cd.plosive || cd.noise) ? 1.25 : 1.1; break; }
          const dur = (def.f2 !== undefined ? 0.14 : def.long ? 0.145 : 0.1) * stretch * coda;
          const prevDef = prevPh ? PH[prevPh] : null;
          const voicelessBefore = prevDef && prevDef.voiced === false;
          const voicedObstruentBefore = prevDef && prevDef.voiced !== false && (prevDef.plosive || prevDef.noise);
          // the pitch through the vowel: a hump on accented syllables, a sag on the rest
          mark(prevHz * (voicelessBefore ? bump(1.06) : voicedObstruentBefore ? bump(0.96) : 1));
          const endHz = last ? (ph.end === "rise" ? base * decl * bump(1.6) : ph.end === "level" ? low : base * decl * bump(0.7)) : low;
          if (stress > 0) { t += dur * 0.35; mark(peak); t += dur * 0.65; mark(endHz); }
          else { t += dur * 0.5; mark(Math.sqrt(prevHz * endHz)); t += dur * 0.5; mark(endHz); }
          const creak = last && ph.end !== "rise" && ph.end !== "level" ? 0.8 : 0;
          phrase.push({ ph: phs[j], dur, gain, reduce: stress === 0 ? 0.15 : 0, creak, press: stress === 2 ? 0.6 : stress === 1 ? 0.3 : 0, glottal: glottal && j === 0,
            onset: afterPause ? 0.03 : prevDef && prevDef.plosive ? 0.006 : 0.012, breathMul: 0.8 + 0.9 * prog });
          prevHz = endHz;
          afterPause = false;
        } else {
          const dur = cdurOf(phs[j], def) * (last && j === phs.length - 1 && def.nasal ? 1.5 : 1);
          phrase.push({ ph: phs[j], dur, gain: gain * 0.9, breathMul: 0.8 + 0.9 * prog });
          t += dur;
        }
        prevPh = phs[j];
      }
      prevEndedInVowel = VOWELS.indexOf(prevPh) >= 0;
    }
    const pause = ph.pause === undefined ? 0.28 : ph.pause;
    phrase.push({ ph: "-", dur: pause }); t += pause;
    afterPause = true;
  }
  return { phrase, contour };
}

/** A small room: early reflections and a touch of dsp.js's reverb. Returns a new buffer. */
export function room(buf, sr, wet) {
  const taps = [[0.007, 0.5], [0.013, 0.35], [0.021, 0.3], [0.029, 0.22], [0.041, 0.16], [0.053, 0.1]];
  const out = new Float32Array(buf.length);
  const w = wet === undefined ? 0.5 : wet;
  for (let i = 0; i < buf.length; i++) {
    let y = buf[i];
    for (let k = 0; k < taps.length; k++) { const d = Math.round(taps[k][0] * sr); if (i >= d) y += buf[i - d] * taps[k][1] * w; }
    out[i] = y;
  }
  const rv = new Reverb(sr, { size: 0.35, damp: 0.6, feedback: 0.6 });
  for (let i = 0; i < out.length; i++) out[i] += rv.process(out[i]) * 0.12 * w;
  return out;
}
