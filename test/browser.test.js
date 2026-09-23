// Byte identity: the browser host against the Rust host.
//
// browser.js is the host a web page runs in a Web Worker. It is exercised here in a Node worker,
// which is the same thing to it: a realm that can only import URLs. Each example gets a fresh
// worker, its imports rewritten to URLs the way a page's publisher rewrites them, and is rendered
// through `open` -- every layer, then the mix -- and compared raw float for raw float with the CLI.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { rewriteImports } from "../js/imports.js";
import { fileURLToPath, pathToFileURL } from "node:url";
import { Worker } from "node:worker_threads";

const REPO = dirname(dirname(fileURLToPath(import.meta.url)));
const CLI = process.env.SYRINX_CLI ?? join(REPO, "target", "release", "syrinx");
const EXAMPLES = join(REPO, "examples");
const PRELUDE = pathToFileURL(join(REPO, "prelude", "prelude.js")).href;
const URLS = {
  math: pathToFileURL(join(REPO, "prelude", "math.js")).href,
  run: pathToFileURL(join(REPO, "prelude", "run.module.js")).href,
  prelude: PRELUDE,
};

test("the run wrapper's module form is run.js, byte for byte, behind `export default`", () => {
  const run = readFileSync(join(REPO, "prelude", "run.js"), "utf8");
  const module = readFileSync(join(REPO, "prelude", "run.module.js"), "utf8");
  assert.equal(module, "export default " + run, "regenerate it: printf 'export default ' > prelude/run.module.js && cat prelude/run.js >> prelude/run.module.js");
});

/** The Rust host's samples for a source, interleaved. */
function rustSamples(sourcePath) {
  const out = join(mkdtempSync(join(tmpdir(), "syrinx-browser-cli-")), "out.f32");
  execFileSync(CLI, ["compile", sourcePath, "-o", out], { stdio: ["ignore", "pipe", "pipe"] });
  const bytes = readFileSync(out);
  const aligned = new Uint8Array(bytes.byteLength);
  aligned.set(bytes);
  return new Float32Array(aligned.buffer);
}

/**
 * The examples and the framework they import, as a page would serve them: every module's
 * `"syrinx"` rewritten to the prelude's URL by syrinx/imports, relative imports left relative (they
 * resolve to files beside them), sources renamed to .mjs so the realm loads them as modules.
 * Returns the served examples directory.
 */
function served() {
  const dir = mkdtempSync(join(tmpdir(), "syrinx-browser-src-"));
  cpSync(EXAMPLES, join(dir, "examples"), { recursive: true, filter: (p) => !p.includes("/out") });
  cpSync(join(REPO, "framework"), join(dir, "framework"), { recursive: true });
  writeFileSync(join(dir, "package.json"), '{ "type": "module" }\n');
  const rewrite = (file) => {
    const text = rewriteImports(readFileSync(file, "utf8"), (s) => (s === "syrinx" ? PRELUDE : s));
    writeFileSync(file, text);
  };
  const walk = (at) => {
    for (const f of readdirSync(at, { withFileTypes: true })) {
      const path = join(at, f.name);
      if (f.isDirectory()) walk(path);
      else if (f.name.endsWith(".js")) rewrite(path);
      else if (f.name.endsWith(".syr")) {
        rewrite(path);
        renameSync(path, path.replace(/\.syr$/, ".mjs"));
      }
    }
  };
  walk(dir);
  return join(dir, "examples");
}

/** Renders a source in a fresh worker through browser.js, interleaved. */
function browserSamples(entry) {
  const code = `
    import { parentPort, workerData } from "node:worker_threads";
    import { open } from ${JSON.stringify(pathToFileURL(join(REPO, "js", "browser.js")).href)};
    const s = await open(workerData);
    const whole = (x) => {
      if (typeof x !== "function") return x;
      const out = Array.from({ length: s.channels }, () => new Float32Array(s.frames));
      for (let o = 0; o < s.frames; o += s.BLOCK_FRAMES) x(o).forEach((p, c) => out[c].set(p, o));
      return out;
    };
    const layers = s.names.map((n) => whole(s.stem(n)));
    let planes;
    if (layers.length === 1 && !s.hasMix) planes = layers[0];
    else {
      const m = s.mixer(0);
      if (m === null) planes = s.mixWhole(layers);
      else if (typeof m === "function") {
        planes = Array.from({ length: s.channels }, () => new Float32Array(s.frames));
        for (let o = 0; o < s.frames; o += s.BLOCK_FRAMES) {
          const n = Math.min(s.BLOCK_FRAMES, s.frames - o);
          const blocks = layers.map((l) => l.map((p) => p.subarray(o, o + n)));
          m(o, blocks).forEach((p, c) => planes[c].set(p, o));
        }
      } else planes = m;
    }
    const out = new Float32Array(s.frames * s.channels);
    for (let c = 0; c < s.channels; c++) for (let i = 0; i < s.frames; i++) out[i * s.channels + c] = planes[c][i];
    parentPort.postMessage(out, [out.buffer]);`;
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("data:text/javascript," + encodeURIComponent(code)), {
      workerData: { ...URLS, entry: pathToFileURL(entry).href },
    });
    worker.once("message", (m) => { void worker.terminate(); resolve(m); });
    worker.once("error", reject);
  });
}

test("every example renders through the browser host exactly as through the CLI", async () => {
  assert.ok(existsSync(CLI),
    `no syrinx CLI at ${CLI} — run \`make build\` (or set SYRINX_CLI). A skipped identity test proves nothing.`);
  const dir = served();
  const sources = readdirSync(EXAMPLES).filter((f) => f.endsWith(".syr"));
  assert.ok(sources.length >= 5, "the examples are the fixtures");
  for (const f of sources) {
    const expected = rustSamples(join(EXAMPLES, f));
    const actual = await browserSamples(join(dir, f.replace(/\.syr$/, ".mjs")));
    assert.equal(actual.length, expected.length, `${f}: length`);
    let differing = 0, first = -1;
    for (let i = 0; i < actual.length; i++) {
      if (!Object.is(actual[i], expected[i])) {
        differing++;
        if (first < 0) first = i;
      }
    }
    assert.equal(differing, 0, `${basename(f)}: ${differing} of ${actual.length} samples differ, first at ${first}`);
  }
});

test("a broken contract is reported with the message every host gives", async () => {
  const dir = mkdtempSync(join(tmpdir(), "syrinx-browser-contract-"));
  const entry = join(dir, "bad.mjs");
  writeFileSync(entry, "export const meta = { api: 4, duration: 1 };\nexport const stems = { \"1a\": () => [0] };\n");
  const code = `
    import { parentPort, workerData } from "node:worker_threads";
    import { open } from ${JSON.stringify(pathToFileURL(join(REPO, "js", "browser.js")).href)};
    try { await open(workerData); parentPort.postMessage(null); }
    catch (e) { parentPort.postMessage({ name: e.name, kind: e.kind, message: e.message }); }`;
  const result = await new Promise((resolve, reject) => {
    const worker = new Worker(new URL("data:text/javascript," + encodeURIComponent(code)), {
      workerData: { ...URLS, entry: pathToFileURL(entry).href },
    });
    worker.once("message", (m) => { void worker.terminate(); resolve(m); });
    worker.once("error", reject);
  });
  assert.deepEqual(result, {
    name: "SyrinxError",
    kind: "contract",
    message: 'stem name "1a" must start with a letter and contain only letters, digits, _ . -',
  });
});
