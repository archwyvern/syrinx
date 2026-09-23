// syrinx-framework: voice/tract -- an articulatory voice: a port of Pink Trombone's glottis and
// vocal tract.
//
// Pink Trombone, copyright 2017 Neil Thapen, MIT licence (https://dood.al/pinktrombone/). What is
// here is its DSP -- an LF-model glottal pulse into a 44-section Kelly-Lochbaum waveguide with
// a 28-section nasal branch, transients when a closure releases, turbulence noise injected at a
// constriction -- made deterministic (seeded noise in place of Math.random and simplex noise,
// fixed block size) and driven by a phoneme gesture table instead of a mouse. Consonants are
// not painted here: a stop is the tract closing and opening, a fricative is air through a
// narrow channel, a nasal is the velum opening. Coarticulation is the tongue moving.

import { Random, hash } from "syrinx";
import { TAU, Noise, Biquad, OnePole, clamp } from "../dsp.js";

// Pink Trombone's geometry is 44 sections at 44.1 kHz = 17.1 cm; here the section count comes
// from the wanted length: one section is c / (2 sr) metres of tract.
function geometry(n) {
  const k = n / 44;
  return { N: n, BLADE: Math.floor(10 * k), TIP: Math.floor(32 * k), LIPS: Math.floor(39 * k), NOSE_LEN: Math.floor(28 * k), NOSE_START: n - Math.floor(28 * k) + 1, K: k };
}

/** Smooth seeded noise in [-1, 1]: value noise with cosine interpolation, `f` lattice points a second. */
class Smooth {
  constructor(seed, f) { this.rng = new Random(hash(seed, "smooth")); this.f = f; this.a = this.rng.bipolar(); this.b = this.rng.bipolar(); this.k = 0; }
  at(t) {
    const x = t * this.f;
    const k = Math.floor(x);
    while (this.k < k) { this.a = this.b; this.b = this.rng.bipolar(); this.k++; }
    const u = x - k, w = 0.5 - 0.5 * Math.cos(Math.PI * u);
    return this.a + (this.b - this.a) * w;
  }
}

class Glottis {
  constructor(sr, seed) {
    this.sr = sr;
    this.timeInWaveform = 0; this.totalTime = 0;
    this.frequency = 140; this.tenseness = 0.6; this.intensity = 0; this.loudness = 1;
    this.wob1 = new Smooth(hash(seed, 1), 4.07); this.wob2 = new Smooth(hash(seed, 2), 2.15);
    this.ten1 = new Smooth(hash(seed, 3), 0.46); this.ten2 = new Smooth(hash(seed, 4), 0.36);
    this.asp1 = new Smooth(hash(seed, 5), 1.99);
    this.vibratoAmount = 0.003; this.vibratoFrequency = 5.6;
    this.setupWaveform();
  }
  setupWaveform() {
    const tenseness = clamp(this.tenseness + 0.1 * this.ten1.at(this.totalTime) + 0.05 * this.ten2.at(this.totalTime), 0.05, 0.95);
    let Rd = 3 * (1 - tenseness);
    this.waveformLength = 1 / this.frequency;
    if (Rd < 0.5) Rd = 0.5; if (Rd > 2.7) Rd = 2.7;
    const Ra = -0.01 + 0.048 * Rd, Rk = 0.224 + 0.118 * Rd;
    const Rg = (Rk / 4) * (0.5 + 1.2 * Rk) / (0.11 * Rd - Ra * (0.5 + 1.2 * Rk));
    const Ta = Ra, Tp = 1 / (2 * Rg), Te = Tp + Tp * Rk;
    const epsilon = 1 / Ta, shift = Math.exp(-epsilon * (1 - Te)), Delta = 1 - shift;
    let RHSIntegral = (1 / epsilon) * (shift - 1) + (1 - Te) * shift; RHSIntegral /= Delta;
    const totalLowerIntegral = -(Te - Tp) / 2 + RHSIntegral, totalUpperIntegral = -totalLowerIntegral;
    const omega = Math.PI / Tp, s = Math.sin(omega * Te);
    const y = -Math.PI * s * totalUpperIntegral / (Tp * 2), z = Math.log(y), alpha = z / (Tp / 2 - Te);
    this.E0 = -1 / (s * Math.exp(alpha * Te)); this.alpha = alpha; this.epsilon = epsilon; this.shift = shift; this.Delta = Delta; this.Te = Te; this.omega = omega;
    this.loudness = Math.pow(tenseness, 0.25);
    this.tensenessNow = tenseness;
  }
  /** One sample: `hz` and `tense` are the current targets, `aspNoise` the aspiration noise sample. */
  step(hz, tense, voiced, aspNoise) {
    const dt = 1 / this.sr;
    this.timeInWaveform += dt; this.totalTime += dt;
    this.intensity += ((voiced ? 1 : 0) - this.intensity) * (voiced ? 0.0022 : 0.0012);
    this.tenseness += (tense - this.tenseness) * 0.002;
    const vib = this.vibratoAmount * Math.sin(TAU * this.totalTime * this.vibratoFrequency) + 0.02 * this.wob1.at(this.totalTime) + 0.04 * this.wob2.at(this.totalTime);
    this.frequency = hz * (1 + vib);
    if (this.timeInWaveform > this.waveformLength) { this.timeInWaveform -= this.waveformLength; this.setupWaveform(); }
    const t = this.timeInWaveform / this.waveformLength;
    let out = t > this.Te ? (-Math.exp(-this.epsilon * (t - this.Te)) + this.shift) / this.Delta : this.E0 * Math.exp(this.alpha * t) * Math.sin(this.omega * t);
    out *= this.intensity * this.loudness;
    let asp = this.intensity * (1 - Math.sqrt(this.tensenessNow)) * this.noiseModulator() * aspNoise;
    asp *= 0.2 + 0.02 * this.asp1.at(this.totalTime);
    return out + asp;
  }
  noiseModulator() {
    const voiced = 0.1 + 0.2 * Math.max(0, Math.sin(TAU * this.timeInWaveform / this.waveformLength));
    return this.tensenessNow * this.intensity * voiced + (1 - this.tensenessNow * this.intensity) * 0.3;
  }
}

class Tract {
  constructor(sr, n, throat) {
    this.sr = sr;
    const G = geometry(n);
    this.G = G;
    const { N, BLADE, TIP, LIPS, NOSE_LEN, NOSE_START } = G;
    this.N = N; this.NOSE_LEN = NOSE_LEN; this.NOSE_START = NOSE_START;
    this.diameter = new Float64Array(N); this.rest = new Float64Array(N); this.target = new Float64Array(N);
    for (let i = 0; i < N; i++) { const d = i < 7 * N / 44 - 0.5 ? throat : i < 12 * N / 44 ? 1.1 : 1.5; this.diameter[i] = this.rest[i] = this.target[i] = d; }
    this.R = new Float64Array(N); this.L = new Float64Array(N);
    this.reflection = new Float64Array(N + 1); this.newReflection = new Float64Array(N + 1);
    this.jR = new Float64Array(N + 1); this.jL = new Float64Array(N + 1);
    this.A = new Float64Array(N);
    this.noseR = new Float64Array(NOSE_LEN); this.noseL = new Float64Array(NOSE_LEN);
    this.noseJR = new Float64Array(NOSE_LEN + 1); this.noseJL = new Float64Array(NOSE_LEN + 1);
    this.noseReflection = new Float64Array(NOSE_LEN + 1); this.noseDiameter = new Float64Array(NOSE_LEN); this.noseA = new Float64Array(NOSE_LEN);
    for (let i = 0; i < NOSE_LEN; i++) { const d = 2 * (i / NOSE_LEN); let dia = d < 1 ? 0.4 + 1.6 * d : 0.5 + 1.5 * (2 - d); this.noseDiameter[i] = Math.min(dia, 1.9); }
    this.glottalReflection = 0.75; this.lipReflection = -0.85;
    this.velumTarget = 0.01; this.lastObstruction = -1; this.transients = [];
    this.movementSpeed = 15;
    this.newReflectionLeft = this.newReflectionRight = this.newReflectionNose = 0;
    this.reflectionLeft = this.reflectionRight = this.reflectionNose = 0;
    this.calculateReflections(); this.calculateNoseReflections();
    this.noseDiameter[0] = this.velumTarget;
    this.lipOutput = 0; this.noseOutput = 0;
  }
  /** The tongue's rest shape: Pink Trombone's TractUI.setRestDiameter. */
  setTongue(index, diameter) {
    const { BLADE, TIP, LIPS } = this.G;
    for (let i = BLADE; i < LIPS; i++) {
      const t = 1.1 * Math.PI * (index - i) / (TIP - BLADE);
      const fixed = 2 + (diameter - 2) / 1.5;
      let curve = (1.5 - fixed + 1.7) * Math.cos(t);
      if (i === BLADE - 2 || i === LIPS - 1) curve *= 0.8;
      if (i === BLADE || i === LIPS - 2) curve *= 0.94;
      this.rest[i] = 1.5 - curve;
    }
  }
  /** Targets = rest, then constrictions [{index, diameter}] carved in with Pink Trombone's widths. */
  setTargets(constrictions, velumOpen, jaw) {
    const { N, TIP, LIPS } = this.G;
    for (let i = 0; i < N; i++) this.target[i] = this.rest[i];
    // the jaw: open vowels flare the front of the tube
    if (jaw !== undefined) for (let i = LIPS - 8; i < N; i++) { const u = (i - (LIPS - 8)) / (N - (LIPS - 8)); this.target[i] = Math.max(this.target[i], this.rest[i] + (jaw - this.rest[i]) * (0.3 + 0.7 * u)); }
    this.velumTarget = velumOpen ? 0.4 : 0.01;
    for (let k = 0; k < constrictions.length; k++) {
      const index = constrictions[k].index;
      let diameter = constrictions[k].diameter - 0.3;
      if (diameter < 0) diameter = 0;
      let width = index < 25 ? 10 : index >= TIP ? 5 : 10 - 5 * (index - 25) / (TIP - 25);
      if (index < 2 || index >= N || diameter >= 3) continue;
      const intIndex = Math.round(index);
      for (let i = -Math.ceil(width) - 1; i < width + 1; i++) {
        if (intIndex + i < 0 || intIndex + i >= N) continue;
        let relpos = Math.abs((intIndex + i) - index) - 0.5;
        let shrink = relpos <= 0 ? 0 : relpos > width ? 1 : 0.5 * (1 - Math.cos(Math.PI * relpos / width));
        if (diameter < this.target[intIndex + i]) this.target[intIndex + i] = diameter + (this.target[intIndex + i] - diameter) * shrink;
      }
    }
  }
  reshape(deltaTime) {
    const { N, TIP, NOSE_START } = this.G;
    const amount = deltaTime * this.movementSpeed;
    let newLastObstruction = -1;
    for (let i = 0; i < N; i++) {
      const d = this.diameter[i], tgt = this.target[i];
      if (d <= 0) newLastObstruction = i;
      const slowReturn = i < NOSE_START ? 0.6 : i >= TIP ? 1 : 0.6 + 0.4 * (i - NOSE_START) / (TIP - NOSE_START);
      const up = slowReturn * amount, down = 2 * amount;
      this.diameter[i] = d < tgt ? Math.min(d + up, tgt) : Math.max(d - down, tgt);
    }
    if (this.lastObstruction > -1 && newLastObstruction === -1 && this.noseA[0] < 0.05) this.transients.push({ position: this.lastObstruction, timeAlive: 0, lifeTime: 0.2, strength: 0.5, exponent: 200 });
    this.lastObstruction = newLastObstruction;
    const v = this.noseDiameter[0], vt = this.velumTarget;
    this.noseDiameter[0] = v < vt ? Math.min(v + amount * 0.25, vt) : Math.max(v - amount * 0.1, vt);
    this.noseA[0] = this.noseDiameter[0] * this.noseDiameter[0];
  }
  calculateReflections() {
    const { N, NOSE_START } = this.G;
    for (let i = 0; i < N; i++) this.A[i] = this.diameter[i] * this.diameter[i];
    for (let i = 1; i < N; i++) { this.reflection[i] = this.newReflection[i]; this.newReflection[i] = this.A[i] === 0 ? 0.999 : (this.A[i - 1] - this.A[i]) / (this.A[i - 1] + this.A[i]); }
    this.reflectionLeft = this.newReflectionLeft; this.reflectionRight = this.newReflectionRight; this.reflectionNose = this.newReflectionNose;
    const sum = this.A[NOSE_START] + this.A[NOSE_START + 1] + this.noseA[0];
    this.newReflectionLeft = (2 * this.A[NOSE_START] - sum) / sum;
    this.newReflectionRight = (2 * this.A[NOSE_START + 1] - sum) / sum;
    this.newReflectionNose = (2 * this.noseA[0] - sum) / sum;
  }
  calculateNoseReflections() {
    const { NOSE_LEN } = this.G;
    for (let i = 0; i < NOSE_LEN; i++) this.noseA[i] = this.noseDiameter[i] * this.noseDiameter[i];
    for (let i = 1; i < NOSE_LEN; i++) this.noseReflection[i] = (this.noseA[i - 1] - this.noseA[i]) / (this.noseA[i - 1] + this.noseA[i]);
  }
  addTurbulence(noise, index, diameter, modulator) {
    const { N } = this.G;
    const i = Math.floor(index), delta = index - i;
    noise *= modulator;
    const thinness = clamp(8 * (0.7 - diameter), 0, 1), openness = clamp(30 * (diameter - 0.3), 0, 1);
    const n0 = noise * (1 - delta) * thinness * openness, n1 = noise * delta * thinness * openness;
    if (i + 1 < N) { this.R[i + 1] += n0 / 2; this.L[i + 1] += n0 / 2; }
    if (i + 2 < N) { this.R[i + 2] += n1 / 2; this.L[i + 2] += n1 / 2; }
  }
  step(glottal, lambda) {
    const { N, NOSE_LEN, NOSE_START } = this.G;
    for (let k = 0; k < this.transients.length; k++) {
      const tr = this.transients[k];
      const amp = tr.strength * Math.pow(2, -tr.exponent * tr.timeAlive);
      this.R[tr.position] += amp / 2; this.L[tr.position] += amp / 2;
      tr.timeAlive += 1 / (this.sr * 2);
    }
    for (let k = this.transients.length - 1; k >= 0; k--) if (this.transients[k].timeAlive > this.transients[k].lifeTime) this.transients.splice(k, 1);
    this.jR[0] = this.L[0] * this.glottalReflection + glottal;
    this.jL[N] = this.R[N - 1] * this.lipReflection;
    for (let i = 1; i < N; i++) {
      const r = this.reflection[i] * (1 - lambda) + this.newReflection[i] * lambda;
      const w = r * (this.R[i - 1] + this.L[i]);
      this.jR[i] = this.R[i - 1] - w; this.jL[i] = this.L[i] + w;
    }
    const i = NOSE_START;
    let r = this.newReflectionLeft * (1 - lambda) + this.reflectionLeft * lambda;
    this.jL[i] = r * this.R[i - 1] + (1 + r) * (this.noseL[0] + this.L[i]);
    r = this.newReflectionRight * (1 - lambda) + this.reflectionRight * lambda;
    this.jR[i] = r * this.L[i] + (1 + r) * (this.R[i - 1] + this.noseL[0]);
    r = this.newReflectionNose * (1 - lambda) + this.reflectionNose * lambda;
    this.noseJR[0] = r * this.noseL[0] + (1 + r) * (this.L[i] + this.R[i - 1]);
    for (let j = 0; j < N; j++) { this.R[j] = this.jR[j] * 0.999; this.L[j] = this.jL[j + 1] * 0.999; }
    this.lipOutput = this.R[N - 1];
    this.noseJL[NOSE_LEN] = this.noseR[NOSE_LEN - 1] * this.lipReflection;
    for (let j = 1; j < NOSE_LEN; j++) { const w = this.noseReflection[j] * (this.noseR[j - 1] + this.noseL[j]); this.noseJR[j] = this.noseR[j - 1] - w; this.noseJL[j] = this.noseL[j] + w; }
    for (let j = 0; j < NOSE_LEN; j++) { this.noseR[j] = this.noseJR[j]; this.noseL[j] = this.noseJL[j + 1]; }
    this.noseOutput = this.noseR[NOSE_LEN - 1];
  }
}

// ---------------------------------------------------------------- gestures
//
// From Pink Trombone's map: vowels as (tongue index, tongue diameter), rounded ones with the
// lips narrowed; consonants as a constriction at a place on the tract (22 velar, 31.5
// post-alveolar, 36 alveolar, 38 dental, 41 labial), closed for stops and nasals, a narrow
// channel for fricatives, half open for approximants. `h` is turbulence just above the glottis.

const V = (i, d, lips) => ({ tongue: [i, d], lips });
export const GESTURE = {
  a: V(13, 2.4), ae: V(15, 2.9), aw: V(12, 2.0), o: V(17.7, 2.1, 1.0), ih: V(27, 2.98), i: V(27.4, 2.3), e: V(20, 3.5),
  uh: V(18.1, 2.55), u: V(23, 2.15, 0.8), uu: V(22, 2.5, 1.0), x: V(21, 2.9), er: { tongue: [21, 2.9], con: [{ index: 30, diameter: 1.15 }] },
  ai: { from: "a", to: "i" }, ei: { from: "e", to: "i" }, ou: { from: "o", to: "u" }, au: { from: "a", to: "u" }, oi: { from: "aw", to: "i" },
  // stops: `asp` seconds of aspiration after the release (voice onset delayed by it), the noise
  // placed at `noiseAt` and highpassed at `hp` -- sibilance is made at the teeth, not at the closure
  p: { close: 41, voiced: false, asp: 0.05, noiseAt: 41, hp: 700 }, b: { close: 41, voiced: true, noiseAt: 41, hp: 700 },
  t: { close: 36, voiced: false, asp: 0.07, noiseAt: 42, hp: 3500 }, d: { close: 36, voiced: true, noiseAt: 42, hp: 3000 },
  k: { close: 22, voiced: false, asp: 0.08, noiseAt: 24, hp: 1200 }, g: { close: 22, voiced: true, noiseAt: 24, hp: 1200 },
  m: { close: 41, nasal: true }, n: { close: 36, nasal: true }, ng: { close: 22, nasal: true },
  f: { fric: 41, voiced: false, noiseAt: 41, hp: 1200, gain: 1.3 }, v: { fric: 41, voiced: true, noiseAt: 41, hp: 1200, gain: 0.8 },
  s: { fric: 36, voiced: false, noiseAt: 42, hp: 3500, gain: 1.6 }, z: { fric: 36, voiced: true, noiseAt: 42, hp: 3500, gain: 1.1 },
  sh: { fric: 31.5, voiced: false, noiseAt: 38, hp: 1600, gain: 1.4 }, zh: { fric: 31.5, voiced: true, noiseAt: 38, hp: 1600, gain: 1 },
  th: { fric: 38, voiced: false, noiseAt: 42, hp: 2500, gain: 1.0 }, dh: { fric: 38, voiced: true, noiseAt: 42, hp: 2500, gain: 0.7 },
  h: { fric: 5, voiced: false, breathy: true, noiseAt: 5, hp: 300, gain: 1 },
  l: { approx: 38, tongue: [21, 2.9] }, r: { approx: 30, tongue: [18, 2.5] }, w: { approx: 41, tongue: [23, 2.15], lips: 0.7 }, y: { tongue: [27.4, 2.3] },
  "-": { silence: true }, "^": { inhale: true },
};


// ---------------------------------------------------------------- calibrated vowels
//
// Positions chosen by MEASUREMENT (work/cal/sweep.syr + the Praat-method tracker), for an 18 cm
// tract with throat 0.85: the position on the tongue/jaw/pharynx grid whose rendered F1 F2 F3
// come nearest the Peterson & Barney male vowels. `uf` is the fronted /u/ of modern English
// ("two"), taken from the male reference recording. These override the labelled map above.
const CAL = {
  a: { tongue: [26, 3.5], jaw: 3.0, pharynx: 0.6 },   // 730/1090/2440 -> 690/1025/2041
  ae: { tongue: [22, 3.5], jaw: 3.0, pharynx: undefined },   // 660/1720/2410 -> 630/1677/2251
  e: { tongue: [24, 3.2], jaw: 3.0, pharynx: undefined },   // 530/1840/2480 -> 556/1753/2149
  i: { tongue: [30, 2.6], jaw: undefined, pharynx: undefined },   // 270/2290/3010 -> 311/2317/3061
  ih: { tongue: [22, 2.0], jaw: 3.0, pharynx: undefined },   // 390/1990/2550 -> 297/1888/2801
  o: { tongue: [12, 2.6], jaw: undefined, pharynx: 0.6 },   // 570/840/2410 -> 585/923/2263
  aw: { tongue: [12, 2.6], jaw: undefined, pharynx: 0.6 },   // 590/880/2540 -> 585/923/2263
  u: { tongue: [30, 2.6], jaw: undefined, pharynx: 0.6 },   // 300/870/2240 -> 390/833/2335
  uf: { tongue: [22, 2.6], jaw: 2.2, pharynx: undefined },   // 425/1548/2400 -> 447/1565/2045
  uu: { tongue: [16, 2.0], jaw: 2.2, pharynx: 0.6 },   // 440/1020/2240 -> 448/1084/2303
  uh: { tongue: [12, 2.6], jaw: 3.0, pharynx: 0.6 },   // 640/1190/2390 -> 630/1105/2318
  er: { tongue: [22, 2.0], jaw: 2.2, pharynx: 0.6 },   // 490/1350/1690 -> 463/1333/1642
  x: { tongue: [20, 2.6], jaw: 2.2, pharynx: undefined },   // 500/1500/2500 -> 496/1397/2257
};
for (const k of Object.keys(CAL)) GESTURE[k] = CAL[k];
GESTURE.ai = { from: "a", to: "i" }; GESTURE.ei = { from: "e", to: "i" }; GESTURE.ou = { from: "o", to: "u" }; GESTURE.au = { from: "a", to: "u" }; GESTURE.oi = { from: "aw", to: "i" };

/**
 * Render a phrase (the same [{ph, dur, hz?, gain?, ...}] as vocal.js) through the tract, the
 * pitch from `o.contour` when given. Options: `base` Hz fallback, `tenseness` (0.6), `speed`
 * (tract movement, 15), `seed`, `scale` (1: tract as is; not used yet).
 */
export function articulate(ctx, phrase, o) {
  const sr = ctx.sr;
  const p = o || {};
  const seed = p.seed === undefined ? ctx.seed : p.seed;
  const glottis = new Glottis(sr, hash(seed, "glottis"));
  const lengthCm = p.length === undefined ? 17.5 : p.length;
  const n = Math.max(20, Math.round(lengthCm / 100 / (343 / (2 * sr))));
  const tract = new Tract(sr, n, p.throat === undefined ? 0.85 : p.throat);
  const K = tract.G.K;                                     // gesture indices are on the 44-section map
  tract.movementSpeed = p.speed === undefined ? 18 : p.speed;
  const lungs = p.lungs === undefined ? 1 : p.lungs;      // glottal drive
  const fricGlobal = p.fricGain === undefined ? 1 : p.fricGain;
  // spectral tilt of the source: a one-pole lowpass on the glottal pulse (not the turbulence)
  const tiltHz = p.sourceTilt === undefined ? 0 : p.sourceTilt;
  const tiltLp = new OnePole(sr);
  const reduceScale = p.reduce === undefined ? 0.55 : p.reduce;
  const aspNoise = new Noise(hash(seed, "asp")), fricNoise = new Noise(hash(seed, "fric"));
  const aspBp = Biquad.bandpass(sr, 500, 0.5), fricBp = Biquad.bandpass(sr, 1000, 0.5);
  const fricHp = new Biquad(sr); fricHp.set("highpass", 300, 0.7);
  let fricHpHz = 300, fricGain = 1;
  let aspLeft = 0, aspDur = 0.05, aspIndex = 42, aspHp = 3000;
  const contour = p.contour;
  let ci = 0;
  const baseTense = p.tenseness === undefined ? 0.6 : p.tenseness;
  const out = new Float32Array(ctx.frames);
  // segments
  const segs = [];
  let t0 = 0, lastHz = p.base === undefined ? 120 : p.base;
  for (let k = 0; k < phrase.length; k++) {
    const e = phrase[k];
    const g = GESTURE[e.ph];
    if (g === undefined) throw new Error("no gesture for " + e.ph);
    if (e.hz !== undefined) lastHz = e.hz;
    segs.push({ ph: e.ph, g, t0, t1: t0 + e.dur, hz: e.hz, gain: e.gain === undefined ? 1 : e.gain, press: e.press === undefined ? 0 : e.press, reduce: e.reduce === undefined ? 0 : e.reduce });
    t0 += e.dur;
  }
  for (let k = 0; k < segs.length; k++) if (segs[k].hz === undefined) { let j = k + 1; while (j < segs.length && segs[j].hz === undefined) j++; segs[k].hz = j < segs.length ? segs[j].hz : (k > 0 ? segs[k - 1].hz : lastHz); }
  // a vowel's tongue target, possibly reduced toward schwa
  const tongueOf = (g, reduce, u) => {
    let tg = g.tongue;
    if (g.from) { const a = GESTURE[g.from].tongue, b = GESTURE[g.to].tongue; tg = [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]; }
    if (!tg) return null;
    const r = reduce * reduceScale;
    return [(tg[0] + (21 - tg[0]) * r) * K, tg[1] + (2.9 - tg[1]) * r];
  };
  const BLOCK = 128;
  let tongue = [21, 2.9], tongueTarget = [21, 2.9];
  let si = 0;
  let fricAmt = 0, fricIndex = 0, fricDia = 0.5;
  let prevSi = -1;
  const inv = 1 / sr;
  const total = t0;
  for (let i = 0; i < ctx.frames; i += BLOCK) {
    const tb = i * inv;
    if (tb >= total) break;
    while (si < segs.length - 1 && tb >= segs[si].t1) si++;
    const s = segs[si], g = s.g;
    const local = tb - s.t0, segLen = s.t1 - s.t0;
    if (si !== prevSi) {
      // a voiceless stop has just released: aspirate for a while, voice onset delayed
      const prev = prevSi >= 0 ? segs[prevSi].g : null;
      if (prev && prev.close !== undefined && prev.voiced === false && prev.asp) { aspLeft = prev.asp; aspDur = prev.asp; aspIndex = (prev.noiseAt === undefined ? prev.close : prev.noiseAt) * K; aspHp = prev.hp === undefined ? 1000 : prev.hp; }
      prevSi = si;
    }
    // where the tongue should be for this segment: a vowel's target, a consonant keeps the
    // neighbouring vowel's (coarticulation) unless it has its own
    let tg = tongueOf(g, s.reduce, g.from ? clamp((local - 0.04) / Math.max(0.05, segLen - 0.06), 0, 1) : 0);
    if (tg === null) {
      let j = si + 1; while (j < segs.length && !(segs[j].g.tongue || segs[j].g.from)) j++;
      if (j < segs.length) tg = tongueOf(segs[j].g, segs[j].reduce, 0);
      else { j = si - 1; while (j >= 0 && !(segs[j].g.tongue || segs[j].g.from)) j--; tg = j >= 0 ? tongueOf(segs[j].g, segs[j].reduce, 1) : [21, 2.9]; }
    }
    tongueTarget = tg;
    const rate = BLOCK * inv / 0.045;
    tongue = [tongue[0] + (tongueTarget[0] - tongue[0]) * Math.min(1, rate), tongue[1] + (tongueTarget[1] - tongue[1]) * Math.min(1, rate)];
    tract.setTongue(tongue[0], tongue[1]);
    const cons = [];
    if (g.lips !== undefined) cons.push({ index: 42 * K, diameter: g.lips });

    if (g.close !== undefined) cons.push({ index: g.close * K, diameter: -0.4 });
    if (g.fric !== undefined) cons.push({ index: g.fric * K, diameter: g.fric === 5 ? 0.45 : 0.48 });
    if (g.approx !== undefined) cons.push({ index: g.approx * K, diameter: 1.1 });
    // a diphthong's jaw and pharynx follow its endpoints
    let jaw = g.jaw, phar = g.pharynx;
    if (g.from) { const A = GESTURE[g.from], B = GESTURE[g.to]; const u = clamp((local - 0.04) / Math.max(0.05, segLen - 0.06), 0, 1); const ja = A.jaw === undefined ? 1.5 : A.jaw, jb = B.jaw === undefined ? 1.5 : B.jaw; jaw = ja + (jb - ja) * u; if (jaw <= 1.5) jaw = undefined; const pa = A.pharynx === undefined ? 1.1 : A.pharynx, pb = B.pharynx === undefined ? 1.1 : B.pharynx; phar = pa + (pb - pa) * u; if (phar >= 1.05) phar = undefined; }
    if (phar !== undefined) cons.push({ index: 7 * K, diameter: phar });
    tract.setTargets(cons, g.nasal === true, jaw);
    tract.reshape(BLOCK * inv);
    tract.calculateReflections();
    tract.calculateNoseReflections();
    // fricative intensity ramps in and out over 60 ms
    const wantFric = g.fric !== undefined ? 1 : 0;
    fricAmt += (wantFric - fricAmt) * Math.min(1, BLOCK * inv / 0.03);
    if (g.fric !== undefined) { fricIndex = (g.noiseAt === undefined ? g.fric : g.noiseAt) * K; fricDia = g.fric === 5 ? 0.45 : 0.48; fricGain = g.gain === undefined ? 1 : g.gain; if (g.hp !== fricHpHz && g.hp !== undefined) { fricHpHz = g.hp; fricHp.set("highpass", g.hp, 0.7); } }
    let aspNow = 0;
    if (aspLeft > 0) { aspNow = aspLeft / aspDur; aspLeft -= BLOCK * inv; if (fricHpHz !== aspHp) { fricHpHz = aspHp; fricHp.set("highpass", aspHp, 0.7); } }
    const voiced = g.silence || g.inhale ? false : aspNow > 0.35 ? false : g.voiced === undefined ? true : g.voiced;
    const tense = clamp(baseTense + 0.25 * s.press + (g.breathy ? -0.2 : 0), 0.15, 0.9);
    for (let j = 0; j < BLOCK && i + j < ctx.frames; j++) {
      const t = (i + j) * inv;
      const lambda1 = j / BLOCK, lambda2 = (j + 0.5) / BLOCK;
      let hz;
      if (contour !== undefined) {
        while (ci + 1 < contour.length && contour[ci + 1][0] <= t) ci++;
        if (ci + 1 >= contour.length) hz = contour[contour.length - 1][1];
        else { const a = contour[ci], b = contour[ci + 1]; const w = clamp((t - a[0]) / Math.max(1e-6, b[0] - a[0]), 0, 1); const ww = w * w * (3 - 2 * w); hz = Math.exp(Math.log(a[1]) + (Math.log(b[1]) - Math.log(a[1])) * ww); }
      } else hz = s.hz;
      let glot = glottis.step(hz, tense, voiced, aspBp.process(aspNoise.white()) * lungs) * lungs;
      if (tiltHz > 0) glot = tiltLp.lp(glot, tiltHz) * (1 + 800 / tiltHz);
      const fn = fricHp.process(fricBp.process(fricNoise.white()) * 2.2);
      let y = 0;
      const turb = fricAmt > 0.001 ? 0.66 * fn * fricAmt * fricGain * fricGlobal : 0;
      const aspT = aspNow > 0 ? 0.9 * fn * aspNow * fricGlobal : 0;
      if (turb !== 0) tract.addTurbulence(turb, fricIndex, fricDia, glottis.noiseModulator());
      if (aspT !== 0) tract.addTurbulence(aspT, aspIndex, 0.5, 0.5);
      tract.step(glot, lambda1); y += tract.lipOutput + tract.noseOutput;
      if (turb !== 0) tract.addTurbulence(turb, fricIndex, fricDia, glottis.noiseModulator());
      if (aspT !== 0) tract.addTurbulence(aspT, aspIndex, 0.5, 0.5);
      tract.step(glot, lambda2); y += tract.lipOutput + tract.noseOutput;
      if (g.inhale) y += aspBp.process(aspNoise.white()) * 0.6 * Math.sin(Math.PI * clamp((t - s.t0) / segLen, 0, 1));
      out[i + j] = y * 0.125 * s.gain;
    }
  }
  return out;
}
