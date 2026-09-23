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
const API = "source declares meta.api 1 but this compiler provides api 3 and accepts 2 to 3";

const CASES = [
  ["export const meta = null;", "`meta` is not an object"],
  ["export const meta = { duration: 1, name: 3 };", "meta.name must be a string"],
  ["export const meta = {};", "meta.duration is required (seconds)"],
  ['export const meta = { duration: "1" };', "meta.duration must be a number of seconds"],
  ["export const meta = { duration: 700 };", "meta.duration must be in (0, 600] seconds"],
  ["export const meta = { duration: -1 };", "meta.duration must be in (0, 600] seconds"],
  ["export const meta = { duration: 1e-9 };", "meta.duration rounds to zero frames"],
  ["export const meta = { duration: 1, channels: 3 };", "meta.channels must be 1 or 2"],
  ["export const meta = { duration: 1, channels: null };", "meta.channels must be 1 or 2"],
  ['export const meta = { duration: 1, sampleRate: "x" };', "meta.sampleRate must be a number"],
  ["export const meta = { duration: 1, sampleRate: 1000 };", "meta.sampleRate must be an integer in [8000, 192000]"],
  ["export const meta = { duration: 1, sampleRate: 44100.5 };", "meta.sampleRate must be an integer in [8000, 192000]"],
  ['export const meta = { duration: 1, seed: "7" };', "meta.seed must be a number"],
  ["export const meta = { duration: 1, seed: null };", "meta.seed must be a number"],
  ["export const meta = { duration: 1, loop: 1 };", "meta.loop must be a boolean"],
  ["export const meta = { duration: 1, api: 1 };", API],
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

/** The CLI's contract error for a source, from a real compile (`check` never computes the frame count). */
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
function inBrowserHost(entry, body) {
  const code = `
    import { parentPort, workerData } from "node:worker_threads";
    import { open } from ${JSON.stringify(pathToFileURL(join(REPO, "js", "browser.js")).href)};
    try {
      const s = await open(workerData);
      parentPort.postMessage({ ok: true, value: await (async (s) => { ${body} })(s) });
    } catch (e) { parentPort.postMessage({ ok: false, kind: e.kind, message: e.message }); }`;
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("data:text/javascript," + encodeURIComponent(code)), {
      workerData: { ...URLS, entry: pathToFileURL(entry).href },
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

test("every host hands a source the same seed, however it is written", async () => {
  // A seed outside u32 is converted by the reference's saturating cast: -1 is 0, not 4294967295.
  for (const seed of ["-1", "5e9", "3.7", "0.5", "4294967296"]) {
    const s = source(
      `import { Random } from "syrinx";\n` +
      `export const meta = { duration: 0.01, seed: ${seed} };\n` +
      "export const stems = { a: (ctx) => { const r = new Random(ctx.seed); return Array.from({ length: ctx.frames }, () => r.next()); } };\n");
    const out = join(dir, `seed${n}.f32`);
    execFileSync(CLI, ["compile", s.syr, "-o", out], { stdio: ["ignore", "pipe", "pipe"] });
    const raw = readFileSync(out);
    const cli = Array.from(new Float32Array(Uint8Array.from(raw).buffer));
    const node = Array.from((await render({ path: s.syr })).samples);
    const browser = await inBrowserHost(s.js, "const d = s.stem('a'); return Array.from(typeof d === 'function' ? d(0)[0] : d[0]);");
    assert.deepEqual(node, cli, `the Node host's seed ${seed}`);
    assert.deepEqual(browser.value, cli.slice(0, browser.value.length), `the browser host's seed ${seed}`);
  }
});
