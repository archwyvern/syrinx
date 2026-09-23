// The syrinx module: the core of the standard, and the shape of a source.
//
// A source is an ES module that imports from "syrinx", exports `meta` and `stems`, and may
// default-export a function combining them. A layer, and the mix, return their samples whole or
// as a stream the host pulls one block at a time. What the core exports is what the standard
// needs a source to have; oscillators, filters and the rest are the framework (framework/dsp.d.ts
// and its siblings). `syrinx types` emits these declarations; point an editor at them (a
// jsconfig.json, or Monaco's addExtraLib) for autocomplete and hover. Sources are plain
// JavaScript -- the types are for tooling, not because anything is compiled from TypeScript.

// ---------------------------------------------------------------- The source contract

/**
 * What a source declares about itself. The fields are checked in this order.
 * @core
 */
export interface Meta {
  /** Contract version the source was written against: 4. Required; any other is refused. */
  api: number;
  /** Display name. No default: a source that declares none has none. */
  name?: string;
  /** Length in seconds, in (0, 600]. Required. */
  duration: number;
  /** 1 (default) or 2. A mono return is duplicated when this is 2. */
  channels?: 1 | 2;
  /** Preferred sample rate, an integer in [8000, 192000]; a host may override it. Default 48000. */
  sampleRate?: number;
  /** An integer in [0, 4294967295], handed to every layer and to the mix as `ctx.seed`. Default 0. */
  seed?: number;
  /** Declared seamless loop. Passed through to consumers; it changes no sample. */
  loop?: boolean;
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

// ---------------------------------------------------------------- Constants

/**
 * The contract version this module implements: 4.
 * @core
 */
export const PRELUDE_VERSION: number;
/**
 * Frames per block of a stream: a constant of the standard (4096).
 * @core
 */
export const BLOCK_FRAMES: number;

// ---------------------------------------------------------------- Seeds

/**
 * FNV-1a over the arguments, as a seed: `new Random(hash(ctx.seed, "kick"))`.
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

// ---------------------------------------------------------------- The block protocol

/**
 * True while the host is computing one block of a stream; false during a layer's setup and for a
 * whole-buffer layer. Code that needs the whole render (a peak, an ending) refuses when it is true.
 * @core
 */
export function inBlock(): boolean;
