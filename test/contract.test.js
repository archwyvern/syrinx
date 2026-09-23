// Every host reads `meta` the same way: the same rejections, with the same messages, and the same
// seed handed to the source. The Rust host is the reference; the Node host (index.js) and the
// browser host (browser.js) share js/contract.js, and both are held to the CLI here.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { Worker } from "node:worker_threads";
import { inspect, render } from "../js/index.js";

const REPO = dirname(dirname(fileURLToPath(import.meta.url)));
const CLI = process.env.SYRINX_CLI ?? join(REPO, "target", "release", "syrinx");
const PRELUDE = pathToFileURL(join(REPO, "prelude", "prelude.js")).href;
const URLS = {
  math: pathToFileURL(join(REPO, "prelude", "math.js")).href,
  run: pathToFileURL(join(REPO, "prelude", "run.module.js")).href,
  prelude: PRELUDE,
};
const STEMS = "export const stems = { a: () => [0] };\n";
const API = (v) => `source declares meta.api ${v} but this compiler provides api 4 and accepts 4 to 4`;
const SEED = "meta.seed must be an integer in [0, 4294967295]";

const CASES = [
  ["export const meta = null;", "`meta` is not an object"],
  ["export const meta = { duration: 1 };", "meta.api is required: this compiler provides api 4 and accepts 4 to 4"],
  ["export const meta = { api: 3, duration: 1 };", API(3)],
  ["export const meta = { api: 5, duration: 1 };", API(5)],
  ["export const meta = { api: 4.5, duration: 1 };", API(4.5)],
  ['export const meta = { api: "4", duration: 1 };', API(4)],
  ["export const meta = { api: null, duration: 1 };", API(null)],
  ["export const meta = { api: 4, duration: 1, name: 3 };", "meta.name must be a string"],
  ["export const meta = { api: 4 };", "meta.duration is required (seconds)"],
  ['export const meta = { api: 4, duration: "1" };', "meta.duration must be a number of seconds"],
  ["export const meta = { api: 4, duration: 700 };", "meta.duration must be in (0, 600] seconds"],
  ["export const meta = { api: 4, duration: -1 };", "meta.duration must be in (0, 600] seconds"],
  ["export const meta = { api: 4, duration: 1e-9 };", "meta.duration rounds to zero frames"],
  ["export const meta = { api: 4, duration: 1, channels: 3 };", "meta.channels must be 1 or 2"],
  ["export const meta = { api: 4, duration: 1, channels: null };", "meta.channels must be 1 or 2"],
  ['export const meta = { api: 4, duration: 1, sampleRate: "x" };', "meta.sampleRate must be a number"],
  ["export const meta = { api: 4, duration: 1, sampleRate: 1000 };", "meta.sampleRate must be an integer in [8000, 192000]"],
  ["export const meta = { api: 4, duration: 1, sampleRate: 44100.5 };", "meta.sampleRate must be an integer in [8000, 192000]"],
  ['export const meta = { api: 4, duration: 1, seed: "7" };', "meta.seed must be a number"],
  ["export const meta = { api: 4, duration: 1, seed: null };", "meta.seed must be a number"],
  ["export const meta = { api: 4, duration: 1, seed: -1 };", SEED],
  ["export const meta = { api: 4, duration: 1, seed: 0.5 };", SEED],
  ["export const meta = { api: 4, duration: 1, seed: 4294967296 };", SEED],
  ["export const meta = { api: 4, duration: 1, seed: NaN };", SEED],
  ["export const meta = { api: 4, duration: 1, seed: Infinity };", SEED],
  ["export const meta = { api: 4, duration: 1, loop: 1 };", "meta.loop must be a boolean"],
];

const dir = mkdtempSync(join(tmpdir(), "syrinx-contract-"));
writeFileSync(join(dir, "package.json"), '{ "type": "module" }\n');
let n = 0;
/** A source on disk, as .syr for the CLI and the Node host and as .js (prelude by URL) for a page. */
function source(text) {
  const base = join(dir, `s${n++}`);
  writeFileSync(`${base}.syr`, text);
  writeFileSync(`${base}.js`, text.replaceAll('from "syrinx"', `from ${JSON.stringify(PRELUDE)}`));
  return { syr: `${base}.syr`, js: `${base}.js` };
}

/** The CLI's contract error for a source, from a real compile. */
function cliError(path) {
  try {
    execFileSync(CLI, ["compile", path, "-o", join(dir, "out.f32")], { stdio: ["ignore", "pipe", "pipe"] });
    return null;
  } catch (err) {
    const out = String(err.stdout) + String(err.stderr);
    return /contract error: (.*)$/m.exec(out)?.[1] ?? out;
  }
}

/** Runs `body` (the text of an async function over `s`, the opened source) in a fresh worker. */
function inBrowserHost(entry, body, urls = {}) {
  const code = `
    import { parentPort, workerData } from "node:worker_threads";
    import { open } from ${JSON.stringify(pathToFileURL(join(REPO, "js", "browser.js")).href)};
    try {
      const s = await open(workerData);
      parentPort.postMessage({ ok: true, value: await (async (s) => { ${body} })(s) });
    } catch (e) { parentPort.postMessage({ ok: false, kind: e.kind, message: e.message }); }`;
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("data:text/javascript," + encodeURIComponent(code)), {
      workerData: { ...URLS, ...urls, entry: pathToFileURL(entry).href },
    });
    worker.once("message", (m) => { void worker.terminate(); resolve(m); });
    worker.once("error", reject);
  });
}

test("every host rejects a bad meta with the reference's message", async () => {
  assert.ok(existsSync(CLI), `no syrinx CLI at ${CLI} — run \`make build\` (or set SYRINX_CLI)`);
  for (const [meta, message] of CASES) {
    const s = source(meta + "\n" + STEMS);
    assert.equal(cliError(s.syr), message, `the CLI on: ${meta}`);
    await assert.rejects(inspect({ path: s.syr }), (e) => e.kind === "contract" && e.message === message, `the Node host on: ${meta}`);
    const b = await inBrowserHost(s.js, "return null;");
    assert.deepEqual({ kind: b.kind, message: b.message }, { kind: "contract", message }, `the browser host on: ${meta}`);
  }
});

test("every host hands a source the seed it wrote, at the edges of the range too", async () => {
  for (const seed of ["0", "7", "1e0", "4294967295"]) {
    const s = source(
      `import { Random } from "syrinx";\n` +
      `export const meta = { api: 4, duration: 0.01, seed: ${seed} };\n` +
      "export const stems = { a: (ctx) => { const r = new Random(ctx.seed); return Array.from({ length: ctx.frames }, () => r.next()); } };\n");
    const out = join(dir, `seed${n}.f32`);
    execFileSync(CLI, ["compile", s.syr, "-o", out], { stdio: ["ignore", "pipe", "pipe"] });
    const raw = readFileSync(out);
    const cli = Array.from(new Float32Array(Uint8Array.from(raw).buffer));
    const node = await render({ path: s.syr });
    const browser = await inBrowserHost(s.js, "const d = s.stem('a'); return { seed: s.seed, first: Array.from(typeof d === 'function' ? d(0)[0] : d[0]) };");
    assert.equal(node.seed, Number(seed), `the Node host's seed ${seed}`);
    assert.equal(browser.value.seed, Number(seed), `the browser host's seed ${seed}`);
    assert.deepEqual(Array.from(node.samples), cli, `the Node host's samples for seed ${seed}`);
    assert.deepEqual(browser.value.first, cli.slice(0, browser.value.first.length), `the browser host's samples for seed ${seed}`);
  }
});

test("every host reports an undeclared name as absent", async () => {
  const s = source("export const meta = { api: 4, duration: 0.01 };\n" + STEMS);
  assert.equal((await inspect({ path: s.syr })).meta.name, undefined, "the Node host");
  const b = await inBrowserHost(s.js, "return [s.name === undefined, s.meta.name === undefined];");
  assert.deepEqual(b.value, [true, true], "the browser host");
  assert.match(execFileSync(CLI, ["check", s.syr], { encoding: "utf8" }), / name=- /, "the CLI shows it as -");
});

test("the browser host refuses a runtime of another contract version, naming both", async () => {
  // A page may be handed a release's own runtime. One from another contract is refused before a
  // source is linked against it, rather than driven through a run wrapper this host does not know.
  const old = join(dir, "prelude-api3.mjs");
  const text = readFileSync(join(REPO, "prelude", "prelude.js"), "utf8");
  assert.ok(text.includes("const PRELUDE_VERSION = 4;"));
  writeFileSync(old, text.replace("const PRELUDE_VERSION = 4;", "const PRELUDE_VERSION = 3;"));
  const s = source("export const meta = { api: 4, duration: 0.01 };\n" + STEMS);
  const b = await inBrowserHost(s.js, "return null;", { prelude: pathToFileURL(old).href });
  assert.deepEqual({ kind: b.kind, message: b.message }, {
    kind: "contract",
    message: `the runtime at ${pathToFileURL(old).href} is api 3; this host implements api 4 to 4`,
  });
});
