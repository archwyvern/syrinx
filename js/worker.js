// The render itself, in a worker thread.
//
// It runs here rather than on the caller's thread for one reason: a sound source is a loop the
// author wrote, and a loop can fail to end. `worker.terminate()` is the only thing in Node that
// stops running JavaScript, which makes it the analogue of the Rust host's watchdog calling
// `IsolateHandle::terminate_execution`. A promise race around an in-process call cannot stop
// anything — it only stops WAITING, while the loop keeps a core busy for the life of the process.

import { readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { realpath } from "node:fs/promises";
import { dirname, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import vm from "node:vm";
import { parentPort, workerData } from "node:worker_threads";
import { check, strip } from "./check.js";
import { ContractError, geometry, readMeta, readStems } from "./contract.js";

// The standard math goes in FIRST, in this worker's own realm, before the module graph is even
// read: a module compiled before it would still see the engine's Math. It is a classic script,
// not a module, so that it can be the same file in every host; vm.runInThisContext evaluates it
// against this isolate's globals, which is exactly the isolate the source will run in. The
// caller's thread is untouched — a worker has its own Math.
vm.runInThisContext(readFileSync(fileURLToPath(workerData.mathUrl), "utf8"), { filename: "syrinx:math" });

/**
 * The runner, evaluated from prelude/run.js — the same file the Rust host embeds. It decides how a
 * return value becomes planes (mono widened to stereo, short buffers zero-padded, an array of
 * planes vs a single buffer), whole or one block at a time, and two hosts disagreeing about any
 * of that is a different sound, not a different error. One file, evaluated by each host, is how
 * they cannot disagree.
 */
const run = vm.runInThisContext(readFileSync(fileURLToPath(workerData.runUrl), "utf8"), { filename: "syrinx:run" });

/** Frames per block of a stream: the run wrapper's, which a Rust test pins to the prelude's. */
const BLOCK_FRAMES = run.BLOCK_FRAMES;

/**
 * Drives a stream to the end, the way the Rust host does: one call per block, in order from
 * offset 0, the block's planes copied into whole planes. `blocksAt(offset)` supplies a mix
 * stream's second argument, the layers' blocks; a layer's stream takes none.
 */
function drain(driver, frames, channels, blocksAt) {
  const out = Array.from({ length: channels }, () => new Float32Array(frames));
  for (let offset = 0; offset < frames; offset += BLOCK_FRAMES) {
    const planes = blocksAt === undefined ? driver(offset) : driver(offset, blocksAt(offset));
    for (let c = 0; c < channels; c++) out[c].set(planes[c], offset);
  }
  return out;
}

/** The layers' blocks at `offset`, chopped from their whole planes; parallel to the layer list. */
function chop(planeLists, frames) {
  return (offset) => {
    const n = Math.min(BLOCK_FRAMES, frames - offset);
    return planeLists.map((planes) => planes.map((p) => p.subarray(offset, offset + n)));
  };
}

/** A failure with the shape the C ABI reports, so both hosts fail the same way. */
function fail(kind, message, file = null, line = 0, column = 0) {
  return { ok: false, kind, message, file, line, column };
}

/**
 * Resolves one import, enforcing the jail. Mirrors host.rs: canonicalise, then require the result
 * to be under the root. Canonicalisation is what makes it a jail rather than a prefix test — a
 * symlink out of the tree resolves to where it points, not to where it sits.
 */
async function resolveImport(specifier, importerPath, root, dependencies) {
  if (specifier === "syrinx") return workerData.preludeUrl;
  if (!specifier.startsWith("./") && !specifier.startsWith("../")) {
    throw Object.assign(new Error(
      `cannot import "${specifier}" from ${importerPath}: only "syrinx" and relative paths are importable`),
      { kind: "compile" });
  }
  const candidate = resolve(dirname(importerPath), specifier);
  let resolved;
  try {
    resolved = await realpath(candidate);
  } catch {
    throw Object.assign(new Error(`cannot import "${specifier}" from ${importerPath}: ${candidate} does not exist`),
      { kind: "compile" });
  }
  // Containment is COMPONENT-WISE, as Rust's `Path::starts_with` is. A bare string prefix test
  // lets `/home/u/warren2/lib.js` through a jail rooted at `/home/u/warren` — any sibling whose
  // name merely begins with the root's — which the Rust host refuses. Two hosts disagreeing about
  // what is reachable is worse than either answer alone.
  if (root !== null && !(resolved === root || resolved.startsWith(root + sep))) {
    throw Object.assign(new Error(
      `cannot import "${specifier}" from ${importerPath}: ${resolved} is outside the project root ${root}`),
      { kind: "compile" });
  }
  dependencies.add(resolved);
  return pathToFileURL(resolved).href;
}

/**
 * Loads the module graph, returning the URL of the rewritten module.
 *
 * Every module in the graph is rewritten and linked to the rewritten URL of its imports — NOT to
 * the file on disk. Linking to the file would hand it to Node's own resolver, and `from "syrinx"`
 * inside it would then resolve to whatever npm package called syrinx happens to be reachable
 * (this repository's own, for one), silently giving a source the HOST instead of the prelude.
 *
 * Keyed by canonical path, so a module imported twice is one instance — the same URL is the same
 * entry in Node's registry. That is observable: a module holding state would otherwise render
 * differently depending on how many times it was reached.
 */
async function load(entryPath, entrySource, root, dependencies, cache) {
  const canonical = await realpath(entryPath);
  const cached = cache.get(canonical);
  if (cached) return cached;

  const source = entrySource ?? await readFile(canonical, "utf-8");
  const problems = check(source);
  if (problems.length > 0) {
    const first = problems[0];
    throw Object.assign(new Error(first.message),
      { kind: "check", file: canonical, line: first.line, column: first.column });
  }

  // Both static import forms: `... from "x"` (which also covers `export ... from "x"`) and the
  // side-effect `import "x"`. The Rust host loads whatever V8's resolve callback is handed, so
  // omitting the side-effect form would make a legal source work there and fail here — a silent
  // divergence on valid code, which is the failure this package most needs to avoid.
  //
  // Dynamic `import()` is not rewritten and not supported, on EITHER host: the Rust side has no
  // dynamic-import callback installed, so a source using it fails there too. That is a shared
  // limitation rather than a divergence.
  //
  // The scan runs over the source with comments and string literals blanked, because prose is not
  // an import: `// vowel sliding from "a" to "o"` is a comment, and scanning raw text would try to
  // resolve "a" and fail the compile on a source the Rust host accepts without complaint -- Rust is
  // handed real module records by V8's parser and never pattern-matches text.
  //
  // strip() is asked to preserve UTF-16 length rather than its usual scalar count, so a match's
  // index is an index into `source` itself and the original can be spliced at it. With the scalar
  // count every astral character inside a preceding literal would shift the splice by one.
  const IMPORTS = /(\bfrom\s*|\bimport\s+)(["'])([^"']+)\2/g;
  const scanned = strip(source, { preserveUtf16: true, keepStrings: true });
  const matches = [...scanned.matchAll(IMPORTS)];
  const specifiers = [...new Set(matches.map((m) => m[3]))];
  const urls = new Map();
  for (const specifier of specifiers) {
    const resolved = await resolveImport(specifier, canonical, root, dependencies);
    urls.set(specifier, specifier === "syrinx"
      ? resolved
      : await load(fileURLToPath(resolved), null, root, dependencies, cache));
  }
  // One pass over the original, splicing at the positions the scan found, so a specifier that
  // happens to contain another cannot be rewritten twice.
  let rewritten = "";
  let at = 0;
  for (const m of matches) {
    const url = urls.get(m[3]);
    if (url === undefined) continue;
    rewritten += source.slice(at, m.index) + m[1] + JSON.stringify(url);
    at = m.index + m[0].length;
  }
  rewritten += source.slice(at);

  const url = "data:text/javascript;base64," + Buffer.from(rewritten, "utf-8").toString("base64");
  cache.set(canonical, url);
  return url;
}

/** Loads the module graph and validates what every mode needs. */
async function prepare() {
  const { sourcePath, source, root, sampleRate } = workerData;
  const dependencies = new Set();
  let module;
  try {
    module = await import(await load(sourcePath, source, root, dependencies, new Map()));
  } catch (err) {
    return { failure: fail(err.kind ?? "compile", err.message, err.file ?? sourcePath, err.line ?? 0, err.column ?? 0) };
  }

  // The contract is read by the module every host shares (contract.js): meta, then the layers,
  // then the geometry, with host.rs's messages.
  let meta, read, geo;
  try {
    const { PRELUDE_VERSION } = await import(workerData.preludeUrl);
    meta = readMeta(module, PRELUDE_VERSION);
    read = readStems(module);
    geo = geometry(meta, sampleRate);
  } catch (err) {
    if (err instanceof ContractError) return { failure: fail("contract", err.message, sourcePath) };
    throw err;
  }
  const { rate, frames } = geo;

  return {
    module,
    meta,
    stems: read.stems,
    names: read.names,
    rate,
    frames,
    channels: geo.channels,
    dependencies: [...dependencies],
  };
}

/** What every successful reply carries, so the caller never has to ask twice. */
function declared(prepared) {
  return {
    ok: true,
    name: prepared.meta.name ?? "",
    duration: prepared.meta.duration,
    seed: prepared.meta.seed,
    loop: prepared.meta.loop,
    sampleRate: prepared.rate,
    channels: prepared.channels,
    frames: prepared.frames,
    stemNames: prepared.names,
    hasMix: typeof prepared.module.default === "function",
    dependencies: prepared.dependencies,
  };
}

/** Every distinct ArrayBuffer behind a list of planes, so a transfer cannot name one twice. */
function buffersOf(planes) {
  return [...new Set(planes.map((p) => p.buffer))];
}

async function main() {
  const { mode, sourcePath } = workerData;
  const prepared = await prepare();
  if (prepared.failure) return prepared.failure;

  if (mode === "inspect") {
    return declared(prepared);
  }

  if (mode === "stem") {
    const stem = workerData.stem;
    if (!prepared.names.includes(stem)) {
      return fail("contract", `no stem named "${stem}"; this source declares ${prepared.names.join(", ")}`, sourcePath);
    }
    let planes;
    let streaming;
    try {
      const result = run.stem(prepared.stems[stem], prepared.rate, prepared.frames, prepared.meta.duration,
        prepared.meta.seed, prepared.channels, stem);
      // A layer that streams is drained here, block by block, into whole planes: the same blocks
      // the Rust host pulls, in the same order, so the same bytes.
      streaming = typeof result === "function";
      planes = streaming ? drain(result, prepared.frames, prepared.channels) : result;
    } catch (err) {
      return fail("runtime", err instanceof Error ? err.message : String(err), sourcePath);
    }
    const nonFinite = firstNonFinite(planes);
    if (nonFinite !== null) {
      return fail("contract",
        `stem "${stem}" produced a non-finite sample at frame ${nonFinite.frame} channel ${nonFinite.channel}`, sourcePath);
    }
    return { ...declared(prepared), stem, streaming, planes };
  }

  if (mode === "mix" || mode === "mix-whole") {
    const { stemNames, stemPlanes } = workerData;
    const useDefault = workerData.useDefault && typeof prepared.module.default === "function";
    if (workerData.useDefault && !useDefault && prepared.module.default !== undefined) {
      return fail("contract", "the default export is not a function", sourcePath);
    }
    const fn = useDefault ? prepared.module.default : null;
    const what = useDefault ? "the default export" : "the mix";
    const args = [prepared.rate, prepared.frames, prepared.meta.duration, prepared.meta.seed, prepared.channels, 0];
    let planes;
    let streaming = false;
    try {
      if (mode === "mix") {
        // The probe, as the Rust host's Mixer does it: the default export is called once with
        // layers whose reads are recorded and refused. It read them: it is a whole-buffer mix,
        // and the main thread runs it again with the planes in a FRESH worker (a fresh realm, as
        // the Rust host uses a fresh isolate), so this aborted call leaves no state behind. It
        // returned a stream: drive it here over the planes chopped into blocks. It returned a
        // buffer without reading: that is the mix.
        const result = run.mix(fn, stemNames, null, ...args);
        if (result === null) return { ok: true, whole: true, planes: stemPlanes };
        streaming = typeof result === "function";
        planes = streaming ? drain(result, prepared.frames, prepared.channels, chop(stemPlanes, prepared.frames)) : result;
      } else {
        planes = run.mix(fn, stemNames, stemPlanes, ...args);
      }
    } catch (err) {
      return fail("runtime", err instanceof Error ? err.message : String(err), sourcePath);
    }
    const nonFinite = firstNonFinite(planes);
    if (nonFinite !== null) {
      return fail("contract",
        `${what} produced a non-finite sample at frame ${nonFinite.frame} channel ${nonFinite.channel}`, sourcePath);
    }
    return { ...declared(prepared), stem: null, streaming, planes };
  }

  return fail("internal", `unknown worker mode "${mode}"`, sourcePath);
}

/** Where the first non-finite sample is, or null. The hosts report the same coordinates. */
function firstNonFinite(planes) {
  for (let i = 0; i < planes[0].length; i++) {
    for (let c = 0; c < planes.length; c++) {
      if (!Number.isFinite(planes[c][i])) return { frame: i, channel: c };
    }
  }
  return null;
}

/** Every distinct ArrayBuffer a reply carries: its planes, or the layers' planes handed back. */
function transferables(result) {
  if (!result.ok) return [];
  if (result.whole) return [...new Set(result.planes.flat().map((p) => p.buffer))];
  return buffersOf(result.planes ?? []);
}

main().then(
  (result) => parentPort.postMessage(result, transferables(result)),
  (err) => parentPort.postMessage(fail("internal", err instanceof Error ? err.message : String(err))),
);
