// The browser host: a source opened in the current realm -- a Web Worker -- ready to render.
//
// A page cannot read files or evaluate strings (under `script-src 'self'` it cannot eval at all),
// so this host is handed URLs and imports them, in the order the standard requires:
//
//   1. `math`  -- prelude/math.js, the standard math, FIRST: the source must run against its frozen
//                 fdlibm Math, never the engine's;
//   2. `run`   -- prelude/run.module.js, the run wrapper as a module (run.js is one expression whose
//                 value a host needs, which a page cannot eval);
//   3. `prelude` -- prelude/prelude.js, the URL the source's `import ... from "syrinx"` points at,
//                 so the host reads the contract version from the same module instance;
//   4. `entry` -- the source, with every import already rewritten to a URL the realm can load.
//
// Then it reads the contract exactly as the other hosts do (contract.js): meta, layers, geometry.
//
// What stays with the page, because only the page can do it: serving those URLs, rewriting a
// source's imports (a publisher does it once; see README), running the determinism check on the
// source text before it is served, giving each layer its own worker (the Rust host gives each its
// own isolate), and terminating a worker that runs past its budget. `examples/` render through
// this host byte for byte as they do through the CLI; test/browser.test.js is the proof.

import { ContractError, geometry, readMeta, readStems } from "./contract.js";
import { SyrinxError } from "./error.js";

export { SyrinxError };

const message = (err) => (err instanceof Error ? err.message : String(err));

/**
 * Opens a source. Resolves to its declared contract and the calls that render it.
 *
 * @param {{ entry: string, math: string, run: string, prelude: string, sampleRate?: number }} urls
 */
export async function open({ entry, math, run, prelude, sampleRate = 0 }) {
  await import(/* @vite-ignore */ /* webpackIgnore: true */ math);
  const runner = (await import(/* @vite-ignore */ /* webpackIgnore: true */ run)).default;
  if (!runner || typeof runner.stem !== "function" || typeof runner.mix !== "function") {
    throw new SyrinxError("internal", `${run} is not the run wrapper as a module (export default <run.js>)`);
  }
  const { PRELUDE_VERSION } = await import(/* @vite-ignore */ /* webpackIgnore: true */ prelude);

  let module;
  try {
    module = await import(/* @vite-ignore */ /* webpackIgnore: true */ entry);
  } catch (err) {
    throw new SyrinxError("compile", message(err), entry);
  }

  let meta, read, geo;
  try {
    meta = readMeta(module, PRELUDE_VERSION);
    read = readStems(module);
    geo = geometry(meta, sampleRate);
  } catch (err) {
    if (err instanceof ContractError) throw new SyrinxError("contract", err.message, entry);
    throw err;
  }
  const { rate, frames, channels } = geo;
  const { stems, names } = read;
  const seed = meta.seed >>> 0;
  const duration = meta.duration;
  const mix = typeof module.default === "function" ? module.default : null;
  if (module.default !== undefined && mix === null) {
    throw new SyrinxError("contract", "the default export is not a function", entry);
  }
  const runtime = (what, call) => {
    try {
      return call();
    } catch (err) {
      if (err instanceof SyrinxError) throw err;
      throw new SyrinxError("runtime", `${what}: ${message(err)}`, entry);
    }
  };

  return {
    meta,
    name: meta.name ?? "",
    loop: meta.loop === true,
    names,
    sampleRate: rate,
    frames,
    channels,
    seed,
    duration,
    hasMix: mix !== null,
    BLOCK_FRAMES: runner.BLOCK_FRAMES,

    /**
     * One layer: a driver `(offset) => planes` pulled once per block in order from 0 when it
     * streams, or its whole planes. A driver's block may be its own scratch buffer: copy it out
     * before pulling the next.
     */
    stem(stem) {
      if (!names.includes(stem)) {
        throw new SyrinxError("contract", `no stem named "${stem}"; this source declares ${names.join(", ")}`, entry);
      }
      return runtime(`stem "${stem}"`, () => runner.stem(stems[stem], rate, frames, duration, seed, channels, stem));
    },

    /**
     * The mix stage, probed: a driver `(offset, blocks) => planes` when it streams (`blocks` parallel
     * to `names`, first block at `from`), null when it reads ctx.stems -- a whole-buffer mix, which
     * the other hosts run again in a fresh realm, so open the source in a new worker and call
     * `mixWhole` there -- or the mix's planes when it returned a buffer without reading its layers.
     */
    mixer(from = 0) {
      return runtime("the mix", () => runner.mix(mix, names, null, rate, frames, duration, seed, channels, from));
    },

    /** A whole-buffer mix over every layer's planes, parallel to `names`. */
    mixWhole(planes) {
      return runtime("the mix", () => runner.mix(mix, names, planes, rate, frames, duration, seed, channels, 0));
    },
  };
}
