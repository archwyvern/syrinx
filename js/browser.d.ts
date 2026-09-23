// Type declarations for the browser host (browser.js). Hand-written; keep in step by hand, as
// index.d.ts is for the Node host.

import type { SyrinxError } from "./index.js";
export { SyrinxError };

/** Where the realm loads the standard and the source from. Every one is a URL it can import. */
export interface OpenUrls {
  /** The source, its imports already rewritten to loadable URLs. */
  entry: string;
  /** prelude/math.js -- imported first. */
  math: string;
  /** prelude/run.module.js -- the run wrapper as a module. */
  run: string;
  /** prelude/prelude.js -- the same URL the source's `"syrinx"` import was rewritten to. */
  prelude: string;
  /** 0 or omitted = the source's own declared rate. */
  sampleRate?: number;
}

/** A block, or a whole render: one Float32Array per channel. */
export type Planes = Float32Array[];

export interface OpenSource {
  /** The source's `meta` as the host read it: validated, absent fields undefined. */
  meta: { api: number; name?: string; duration: number; channels: 1 | 2; sampleRate?: number; seed: number; loop: boolean };
  /** The declared name; undefined when the source declares none (there is no default). */
  name?: string;
  loop: boolean;
  /** Layer names, in declaration order. */
  names: string[];
  sampleRate: number;
  frames: number;
  channels: number;
  seed: number;
  duration: number;
  hasMix: boolean;
  BLOCK_FRAMES: number;
  /** A streaming layer's driver, pulled in order from 0, or a whole layer's planes. */
  stem(name: string): ((offset: number) => Planes) | Planes;
  /**
   * The mix, probed: a stream's driver (blocks parallel to `names`), null for a whole-buffer mix
   * (run `mixWhole` on a freshly opened source), or planes.
   */
  mixer(from?: number): ((offset: number, blocks: Planes[]) => Planes) | Planes | null;
  /** A whole-buffer mix over every layer's planes, parallel to `names`. */
  mixWhole(planes: Planes[]): Planes;
}

/**
 * Opens a source in the current realm: math, run wrapper, prelude, then the source. Refuses
 * (kind `contract`) a runtime whose prelude is outside this host's contract range.
 */
export function open(urls: OpenUrls): Promise<OpenSource>;
