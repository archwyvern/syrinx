// Type declarations for the JavaScript host.
//
// Hand-written beside index.js rather than generated: the package is plain JavaScript, and a
// consumer in TypeScript would otherwise see `any` for every render — which is not merely untidy.
// `err instanceof SyrinxError` cannot narrow an `any`, so the error path silently loses its
// `file`/`line`/`column` and a consumer's own strictness reports the loss as its own fault.
//
// prelude/syrinx.d.ts does the same job for the sound-authoring API; this is its counterpart for
// the host. Keep both in step with the JavaScript by hand.

/** One diagnostic from the static determinism check. */
export interface Diagnostic {
  /** 1-based. */
  line: number;
  /** 1-based. */
  column: number;
  message: string;
}

/** Banned names and why, in the order their messages are written. */
export const BANNED: readonly (readonly [name: string, why: string])[];

/**
 * Scans a source for names that would make it non-deterministic, returning every occurrence in
 * source order. An empty array means the source passed.
 */
export function check(source: string): Diagnostic[];

/** The library version this host was tested against; also the package version. */
export const SYRINX_VERSION: string;

/** How a render failed. */
export type SyrinxErrorKind =
  | "check"
  | "compile"
  | "runtime"
  | "timeout"
  | "contract"
  | "internal";

/** A render that did not produce samples. Carries a position when one was located. */
export class SyrinxError extends Error {
  readonly kind: SyrinxErrorKind;
  /** The module the position refers to (the source, or one of its imports), or null. */
  readonly file: string | null;
  /** 1-based; 0 when unknown. */
  readonly line: number;
  /** 1-based; 0 when unknown. */
  readonly column: number;
}

export interface RenderOptions {
  /** The source's path; relative imports resolve from its directory. */
  path: string;
  /** The source text, if it is already in hand; read from `path` otherwise. */
  source?: string;
  /**
   * Directory imports may not escape. `null` is unrestricted, which an editor opening someone
   * else's project should not use.
   */
  root?: string | null;
  /** 0 or omitted uses the source's own declared rate. */
  sampleRate?: number;
  /** Wall-clock budget; the default matches the Rust host's. */
  timeoutMs?: number;
}

export interface RenderedSound {
  /** Interleaved, `frames * channels` long, nominally in [-1, 1] but not clipped. */
  samples: Float32Array;
  sampleRate: number;
  /** 1 or 2. */
  channels: number;
  frames: number;
  /** Seconds, as `meta` declared. */
  duration: number;
  loop: boolean;
  name: string;
  seed: number;
  /** Every file the source imported, transitively, as absolute canonical paths. */
  dependencies: string[];
  /**
   * Whether the samples were produced one block at a time: the layer streamed, or the mix stage
   * streamed. The bytes are the same either way; this says which form the source took.
   */
  streaming: boolean;
  /** Wall time the render took. */
  elapsedMs: number;
}

/**
 * Renders a sound source to interleaved samples.
 *
 * @throws {SyrinxError} when the source is rejected, fails, or exceeds its time budget.
 */
export function render(options: RenderOptions): Promise<RenderedSound>;
