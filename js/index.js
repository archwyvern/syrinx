// The JavaScript host for Syrinx: render a `.syr` sound source without the native library.
//
// The Rust host (crates/syrinx-core) remains the reference implementation and the one the game's
// content pipeline bakes with. This exists so an EDITOR can preview a sound in-process — no FFI,
// no built game, no platform-specific binary — and it is only worth having if it produces the
// SAME sound. That is not an assumption: `test/identity.test.js` renders every example through
// both hosts and compares raw floats, and `test/check.test.js` compares the determinism check's
// diagnostics against the Rust implementation's, positions included.
//
// What that test can and cannot say is written down in test/identity.test.js. Read it before
// trusting this on a platform it has not been run on.
//
// A source is one or more named layers. Each is rendered in its own worker — its own realm, its
// own module graph — which is what makes a layer's audio independent of which other layers ran
// and in what order, and is also why they can run at the same time. A layer or a mix that
// streams is driven block by block inside its worker, the same blocks in the same order as the
// Rust host pulls, and comes back whole: this package renders, it does not play.

import { readFile } from "node:fs/promises";
import { realpath } from "node:fs/promises";
import { dirname, join } from "node:path";
import { availableParallelism } from "node:os";
import { fileURLToPath, pathToFileURL } from "node:url";
import { Worker } from "node:worker_threads";
import { createRequire } from "node:module";

export { check, BANNED } from "./check.js";

const HERE = dirname(fileURLToPath(import.meta.url));

const STANDARD = {
  preludeUrl: pathToFileURL(join(HERE, "..", "prelude", "prelude.js")).href,
  mathUrl: pathToFileURL(join(HERE, "..", "prelude", "math.js")).href,
  runUrl: pathToFileURL(join(HERE, "..", "prelude", "run.js")).href,
};

/**
 * The library version this host was tested against. It is the package version too: the two are
 * the same number on purpose, because a JS host that renders differently from the Rust host of
 * the same name is the exact failure this package exists to rule out. Pin them together.
 */
export const SYRINX_VERSION = createRequire(import.meta.url)("../package.json").version;

export { SyrinxError } from "./error.js";
import { SyrinxError } from "./error.js";

/** The inputs every entry point takes, resolved once. */
async function inputs({ path, source, root, timeoutMs = 20000 }) {
  return {
    path,
    text: source ?? await readFile(path, "utf-8"),
    jail: root === null || root === undefined ? null : await realpath(root),
    started: Date.now(),
    timeoutMs,
  };
}

/**
 * Runs one worker to completion. The budget belongs to the whole render, so each stage is given
 * what is left of it rather than a fresh copy — otherwise a source with six layers and a mix
 * could take seven times the timeout the caller asked for.
 */
function spawn(base, mode, extra = {}, transferList = []) {
  const remaining = base.timeoutMs - (Date.now() - base.started);
  const worker = new Worker(join(HERE, "worker.js"), {
    workerData: {
      mode,
      sourcePath: base.path,
      source: base.text,
      root: base.jail,
      sampleRate: base.sampleRate ?? 0,
      ...STANDARD,
      ...extra,
    },
    transferList,
  });

  let timer;
  return new Promise((resolve, reject) => {
    // terminate() is the point of the worker: it stops JavaScript that is still running, which
    // no amount of racing on the calling thread can do.
    timer = setTimeout(() => {
      void worker.terminate();
      reject(new SyrinxError("timeout", `rendering ${base.path} exceeded ${base.timeoutMs} ms`, base.path));
    }, Math.max(remaining, 0));
    worker.once("message", resolve);
    worker.once("error", (err) => reject(new SyrinxError("internal", err.message, base.path)));
    worker.once("exit", (code) => {
      // A worker that exits without a message was killed or crashed; without this the promise
      // would hang until the timeout and report the wrong kind.
      if (code !== 0) reject(new SyrinxError("internal", `render worker exited with code ${code}`, base.path));
    });
  }).then((result) => {
    if (!result.ok) throw new SyrinxError(result.kind, result.message, result.file, result.line, result.column);
    return result;
  }).finally(() => {
    clearTimeout(timer);
    void worker.terminate();
  });
}

/** Planes to interleaved frames. A permutation, so it does not have to happen in the standard. */
function interleave(planes, frames, channels) {
  const out = new Float32Array(frames * channels);
  for (let c = 0; c < channels; c++) {
    const plane = planes[c];
    for (let i = 0; i < frames; i++) out[i * channels + c] = plane[i];
  }
  return out;
}

function result(reply, base) {
  const { planes, ...rest } = reply;
  return { ...rest, samples: interleave(planes, reply.frames, reply.channels), elapsedMs: Date.now() - base.started };
}

/** Every distinct ArrayBuffer behind a list of plane lists, so a transfer cannot name one twice. */
function buffersOf(planeLists) {
  return [...new Set(planeLists.flat().map((p) => p.buffer))];
}

/** Every name is a declared layer and none repeats. */
function validateSelection(stems, info, path) {
  if (stems.length === 0) throw new SyrinxError("contract", "no stems selected", path);
  stems.forEach((stem, i) => {
    if (!info.stemNames.includes(stem)) {
      throw new SyrinxError("contract", `no stem named "${stem}"; this source declares ${info.stemNames.join(", ")}`, path);
    }
    if (stems.slice(0, i).includes(stem)) throw new SyrinxError("contract", `stem "${stem}" selected twice`, path);
  });
}

/**
 * The layers a subset sums, in DECLARATION order whatever order was asked: f32 addition is not
 * associative, so `["a", "b"]` and `["b", "a"]` would otherwise differ by an ulp. Mirrors
 * host.rs `plan`.
 */
function selectionOf(stems, info, path) {
  if (stems === null || stems === undefined) return info.stemNames;
  validateSelection(stems, info, path);
  return info.stemNames.filter((name) => stems.includes(name));
}

/**
 * What a source declares, without rendering it.
 *
 * @returns {Promise<{meta: object, stems: string[], hasMix: boolean, dependencies: string[]}>}
 */
export async function inspect(options) {
  const base = await inputs(options);
  const reply = await spawn(base, "inspect");
  return {
    meta: {
      name: reply.name,
      duration: reply.duration,
      channels: reply.channels,
      sampleRate: reply.sampleRate,
      seed: reply.seed,
      loop: reply.loop,
    },
    stems: reply.stemNames,
    hasMix: reply.hasMix,
    dependencies: reply.dependencies,
  };
}

/** Renders the named layers, one worker each, bounded by the machine's parallelism. */
async function renderPlanes(base, selected) {
  const replies = new Array(selected.length);
  let next = 0;
  const worker = async () => {
    for (;;) {
      const i = next++;
      if (i >= selected.length) return;
      replies[i] = await spawn(base, "stem", { stem: selected[i] });
    }
  };
  const lanes = Math.max(1, Math.min(availableParallelism(), selected.length));
  await Promise.all(Array.from({ length: lanes }, worker));
  return replies;
}

/**
 * Renders each named layer separately. An empty or absent `stems` renders them all; results come
 * back in the order asked for.
 *
 * @returns {Promise<Array<object>>}
 */
export async function renderEach({ stems = null, sampleRate = 0, ...rest }) {
  const base = { ...(await inputs(rest)), sampleRate };
  const info = await spawn(base, "inspect");
  // Separate renders in the order asked for: nothing is summed here.
  let selected = info.stemNames;
  if (stems && stems.length) {
    validateSelection(stems, info, base.path);
    selected = stems;
  }
  const replies = await renderPlanes(base, selected);
  return replies.map((reply) => result(reply, base));
}

/**
 * The mix stage over the layers' whole planes: the probe worker first; a whole-buffer mix is
 * then run in a fresh worker with the planes, the way the Rust host uses a fresh isolate.
 */
async function mixStage(base, stemNames, planes, useDefault) {
  const probe = await spawn(base, "mix", { stemNames, stemPlanes: planes, useDefault }, buffersOf(planes));
  if (!probe.whole) return probe;
  return spawn(base, "mix-whole", { stemNames, stemPlanes: probe.planes, useDefault }, buffersOf(probe.planes));
}

/**
 * Renders a sound source to interleaved samples.
 *
 * @param {object} options
 * @param {string} options.path        The source's path; relative imports resolve from its directory.
 * @param {string} [options.source]    The source text, if it is already in hand; read from `path` otherwise.
 * @param {string|null} [options.root] Directory imports may not escape. Null is unrestricted, which
 *                                     an editor should not use on someone else's project.
 * @param {number} [options.sampleRate] 0 or omitted = the source's own declared rate.
 * @param {number} [options.timeoutMs] Wall-clock budget for the whole render. Matches the Rust host's.
 * @param {string[]|null} [options.stems] Render only these layers, summed, without the mix stage.
 * @returns {Promise<{samples: Float32Array, sampleRate: number, channels: number, frames: number,
 *                    duration: number, loop: boolean, name: string, seed: number, stem: string|null,
 *                    stemNames: string[], dependencies: string[], streaming: boolean, elapsedMs: number}>}
 */
export async function render({ stems = null, sampleRate = 0, ...rest }) {
  const base = { ...(await inputs(rest)), sampleRate };
  const info = await spawn(base, "inspect");
  const wanted = stems && stems.length ? stems : null;
  const selected = selectionOf(wanted, info, base.path);
  // A subset never runs the mix stage: the mix reads its layers by name and cannot be handed
  // fewer than the source declares.
  const useDefault = wanted === null && info.hasMix;

  const replies = await renderPlanes(base, selected);

  // One layer and nothing to combine it with. Summing it would be a copy — and `run.mix` starts
  // from the first layer precisely so that this shortcut and the sum cannot disagree.
  if (selected.length === 1 && !useDefault) {
    return { ...result(replies[0], base), stem: null };
  }

  const mixed = await mixStage(base, selected, replies.map((r) => r.planes), useDefault);
  return result(mixed, base);
}

/**
 * Runs only the mix stage, over layers rendered earlier.
 *
 * @param {object} options
 * @param {Object<string, Float32Array>} options.stems Interleaved samples per layer name.
 */
export async function mixFrom({ stems, sampleRate = 0, ...rest }) {
  const base = { ...(await inputs(rest)), sampleRate };
  const info = await spawn(base, "inspect");
  const supplied = Object.keys(stems ?? {});
  for (const stem of info.stemNames) {
    if (!supplied.includes(stem)) throw new SyrinxError("contract", `no audio supplied for stem "${stem}"`, base.path);
  }
  for (const stem of supplied) {
    if (!info.stemNames.includes(stem)) {
      throw new SyrinxError("contract", `no stem named "${stem}"; this source declares ${info.stemNames.join(", ")}`, base.path);
    }
  }

  const expected = info.frames * info.channels;
  const planes = info.stemNames.map((stem) => {
    const samples = stems[stem];
    if (samples.length !== expected) {
      throw new SyrinxError("contract",
        `stem "${stem}" has ${samples.length} samples, expected ${expected} (${info.frames} frames x ${info.channels} channels)`,
        base.path);
    }
    return Array.from({ length: info.channels }, (_, c) => {
      const plane = new Float32Array(info.frames);
      for (let i = 0; i < info.frames; i++) plane[i] = samples[i * info.channels + c];
      return plane;
    });
  });

  const mixed = await mixStage(base, info.stemNames, planes, info.hasMix);
  return result(mixed, base);
}
