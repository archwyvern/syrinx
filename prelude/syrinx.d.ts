// The syrinx module: everything a sound source can import.
//
// A source is an ES module that imports from "syrinx", exports `meta` and `stems`, and may
// default-export a function combining them. A layer, and the mix, return their samples whole or
// as a stream the host pulls one block at a time. `syrinx types` emits these declarations; point
// an editor at them (a jsconfig.json, or Monaco's addExtraLib) for autocomplete and hover. Sources
// are plain JavaScript -- the types are for tooling, not because anything is compiled from
// TypeScript.

// ---------------------------------------------------------------- The source contract

/**
 * What a source declares about itself.
 * @core
 */
export interface Meta {
  /** Display name; defaults to the file stem. */
  name?: string;
  /** Length in seconds. Required. */
  duration: number;
  /** 1 (default) or 2. A mono return is duplicated when this is 2. */
  channels?: 1 | 2;
  /** Preferred sample rate; the compiler may override it. Default 48000. */
  sampleRate?: number;
  /** Passed to every stem and to the mix as `ctx.seed`. Default 0. */
  seed?: number;
  /** Declared seamless loop. Passed through to consumers. */
  loop?: boolean;
  /** Contract version the source was written against, 2 or 3; compile fails on anything else. */
  api?: number;
}

/**
 * What every layer and the mix are given.
 * @core
 */
export interface Context {
  /** Sample rate in Hz. */
  sr: number;
  /** Number of frames to produce: round(duration * sr). */
  frames: number;
  /** Seconds, as declared. */
  duration: number;
  /** meta.seed, the same for every layer. Derive a voice's own with `hash(ctx.seed, "kick")`. */
  seed: number;
  /** meta.channels. */
  channels: 1 | 2;
}

/**
 * What a layer is given: the context, plus which layer it is.
 * @core
 */
export interface StemContext extends Context {
  /** This layer's name, as declared in `stems`. */
  stem: string;
}

/**
 * What the default export is given: the context, plus the rendered layers.
 * @core
 */
export interface MixContext extends Context {
  /**
   * Each layer by name, as `channels` planes of `frames` samples. Reading one makes the mix
   * whole-buffer; a mix stream never reads it and gets its layers' blocks as its third argument.
   */
  stems: Record<string, Float32Array[]>;
}

/**
 * What a layer or the mix returns: mono samples, or [left, right].
 * @core
 */
export type Output = Float32Array | number[] | [Float32Array | number[], Float32Array | number[]];

/**
 * A streaming layer: called once per block of BLOCK_FRAMES, in order from offset 0 (the last
 * block is shorter), returning exactly `frames` samples per plane. State kept in the closure
 * persists between blocks.
 * @core
 */
export type Stream = (offset: number, frames: number) => Output;

/**
 * A streaming mix: `stems[name]` is that layer's block as `channels` planes of `frames` samples.
 * @core
 */
export type MixStream = (offset: number, frames: number, stems: Record<string, Float32Array[]>) => Output;

/**
 * A source's layers, by name. Names match /^[A-Za-z][A-Za-z0-9_.-]{0,63}$/ and keep their order.
 * @core
 */
export type Stems = Record<string, (ctx: StemContext) => Output | Stream>;

/**
 * The shape of a source module.
 * @core
 */
export interface Source {
  meta: Meta;
  stems: Stems;
  default?: (ctx: MixContext) => Output | MixStream;
}

// ---------------------------------------------------------------- Scalars

/** @core */
export const PRELUDE_VERSION: number;
/**
 * Frames per block of a stream: a constant of the standard (4096).
 * @core
 */
export const BLOCK_FRAMES: number;
export const TAU: number;

export function clamp(x: number, lo: number, hi: number): number;
export function lerp(a: number, b: number, t: number): number;
/** Decibels to linear gain. */
export function db(decibels: number): number;
/** MIDI note number to Hz. */
export function mtof(midi: number): number;
export function softclip(x: number): number;
export function hardclip(x: number): number;
/** Wavefolder: reflects anything outside [-1, 1] back in. */
export function fold(x: number): number;
/**
 * FNV-1a over the arguments, as a seed: `new Noise(hash(ctx.seed, "kick"))`.
 * @core
 */
export function hash(...parts: (number | string)[]): number;

// ---------------------------------------------------------------- Randomness

/**
 * Seeded PRNG (mulberry32). The only randomness a source may use.
 * @core
 */
export class Random {
  constructor(seed?: number);
  /** [0, 1) */
  next(): number;
  /** [lo, hi) */
  range(lo: number, hi: number): number;
  /** [-1, 1) */
  bipolar(): number;
  /** Integer in [lo, hi) */
  int(lo: number, hi: number): number;
  chance(p: number): boolean;
  pick<T>(array: readonly T[]): T;
}

// ---------------------------------------------------------------- Oscillators

/** Phase accumulator in [0, 1). */
export class Phasor {
  constructor(sr: number, phase?: number);
  phase: number;
  /** Advance one sample at `freq` Hz; returns the phase before advancing. */
  next(freq: number): number;
}

export type Shape = (phase: number, width: number) => number;

/** Naive (aliasing) oscillators. Fine for most SFX; see BlepOsc for cleaner saws and squares. */
export class Osc {
  constructor(sr: number, shape: Shape, phase?: number);
  static sine(sr: number, phase?: number): Osc;
  static saw(sr: number, phase?: number): Osc;
  static square(sr: number, phase?: number): Osc;
  static tri(sr: number, phase?: number): Osc;
  static pulse(sr: number, width?: number, phase?: number): Osc;
  static shapes: { sine: Shape; saw: Shape; square: Shape; tri: Shape; pulse: Shape };
  width: number;
  next(freq: number): number;
}

/** Band-limited saw and square via PolyBLEP. */
export class BlepOsc {
  static saw(sr: number, phase?: number): BlepOsc;
  static square(sr: number, phase?: number): BlepOsc;
  next(freq: number): number;
}

// ---------------------------------------------------------------- Noise

/** Seeded noise. One instance per independent noise source. */
export class Noise {
  constructor(seed?: number);
  /** Uniform white in [-1, 1). */
  white(): number;
  /** Pink (-3 dB/oct), roughly [-1, 1]. */
  pink(): number;
  /** Brown (-6 dB/oct). */
  brown(): number;
}

// ---------------------------------------------------------------- Envelopes

/** An envelope is a function of time in seconds. */
export type Envelope = (t: number) => number;

export const Env: {
  /** Linear attack, exponential decay to silence. */
  ad(attack: number, decay: number): Envelope;
  /** Linear attack, exponential decay to sustain, hold until `gate`, exponential release. */
  adsr(attack: number, decay: number, sustain: number, release: number, gate: number): Envelope;
  /** exp(-t / tau) */
  exp(tau: number): Envelope;
  /** Straight line from `from` to `to` over `duration`, clamped. */
  line(from: number, to: number, duration: number): Envelope;
  /** Exponential sweep from `from` to `to` over `duration` (both > 0), clamped. */
  sweep(from: number, to: number, duration: number): Envelope;
  /** 1 until `duration`, then linear fade to 0 over `fade`. */
  gate(duration: number, fade: number): Envelope;
  /** Piecewise-linear through [[t0, v0], [t1, v1], ...] with ascending times. */
  points(pts: [number, number][]): Envelope;
};

// ---------------------------------------------------------------- Filters

/** One-pole low/high-pass, 6 dB/oct. Stateful: one instance per signal. */
export class OnePole {
  constructor(sr: number);
  lp(x: number, cutoff: number): number;
  hp(x: number, cutoff: number): number;
}

export type BiquadType =
  | "lowpass" | "highpass" | "bandpass" | "notch" | "allpass" | "peak" | "lowshelf" | "highshelf";

/** RBJ cookbook biquad. `set` is cheap enough to call per sample for modulation. */
export class Biquad {
  constructor(sr: number);
  static lowpass(sr: number, freq: number, q?: number): Biquad;
  static highpass(sr: number, freq: number, q?: number): Biquad;
  static bandpass(sr: number, freq: number, q?: number): Biquad;
  static notch(sr: number, freq: number, q?: number): Biquad;
  static peak(sr: number, freq: number, q?: number, gainDb?: number): Biquad;
  set(type: BiquadType, freq: number, q?: number, gainDb?: number): this;
  process(x: number): number;
}

/** Chamberlin state-variable filter. `process` returns the lowpass; `.band` / `.high` hold the others. */
export class Svf {
  constructor(sr: number);
  low: number;
  band: number;
  high: number;
  set(cutoff: number, resonance?: number): this;
  process(x: number): number;
}

// ---------------------------------------------------------------- Delays and reverb

/** Circular delay line with linear-interpolated reads. */
export class Delay {
  constructor(sr: number, maxSeconds: number);
  /** Read `seconds` back from the write head. */
  read(seconds: number): number;
  write(x: number): void;
  /** write then read. */
  process(x: number, seconds: number): number;
}

/** Feedback comb with one-pole damping in the loop. */
export class Comb {
  constructor(sr: number, seconds: number, feedback?: number, damp?: number);
  feedback: number;
  damp: number;
  process(x: number): number;
}

export class Allpass {
  constructor(sr: number, seconds: number, gain?: number);
  gain: number;
  process(x: number): number;
}

/** Small Schroeder reverb: 4 combs into 2 allpasses. */
export class Reverb {
  constructor(sr: number, options?: { size?: number; damp?: number; feedback?: number });
  process(x: number): number;
}

// ---------------------------------------------------------------- Buffers

/** Anything with a per-sample `process`. */
export interface Processor {
  process(x: number): number;
}

/**
 * Run `fn(t, i)` once per frame and collect the result.
 * @core
 */
export function render(ctx: Context, fn: (t: number, i: number) => number): Float32Array;
/**
 * render(), one block at a time: a stream that is bit-identical to `render(ctx, fn)` over the same frames.
 * @core
 */
export function stream(ctx: Context, fn: (t: number, i: number) => number): Stream;
/**
 * Sum buffers sample-wise; the result is as long as the longest.
 * @core
 */
export function mix(...buffers: ArrayLike<number>[]): Float32Array;
export function gain(buffer: ArrayLike<number>, g: number): Float32Array;
/**
 * Scale so the absolute peak hits `peak` (default -1 dBFS). Silence is left alone. Refuses inside a stream's block: use a limiter or a fixed gain.
 * @core
 */
export function normalize(buffer: ArrayLike<number>, peak?: number): Float32Array;
/**
 * Linear fade-in over `fadeIn` seconds and fade-out over `fadeOut` seconds. Refuses inside a stream's block: shape the level from the absolute time.
 * @core
 */
export function fade(ctx: Context, buffer: ArrayLike<number>, fadeIn: number, fadeOut: number): Float32Array;
/** Constant-power pan; position in [-1, 1]. */
export function pan(buffer: ArrayLike<number>, position?: number): [Float32Array, Float32Array];
/**
 * Place `buffer` into a new ctx.frames-long buffer starting at `at` seconds. Refuses inside a stream's block: copy into the block from `round(at * sr) - offset`.
 * @core
 */
export function place(ctx: Context, buffer: ArrayLike<number>, at: number): Float32Array;
/** Apply a per-sample processor over a buffer. */
export function filter(buffer: ArrayLike<number>, processor: Processor): Float32Array;
