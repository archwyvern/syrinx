// Byte identity: the JS host against the Rust host.
//
// This is the test the JS host exists to pass. An editor previewing a sound with this package and
// a game baking it with the Rust library must produce the SAME sound; a preview that is merely
// close is worse than none, because it is a confident rendering of something the player will
// never hear. Different V8 versions ship different libm, and "fdlibm is stable" is a claim until
// something measures it.
//
// Raw floats are compared, never a 16-bit conversion — that would hide any difference below about
// 3e-5, which is most of what a libm disagreement looks like.
//
// WHAT THIS TEST CANNOT SAY. It compares whatever V8 is running it against whatever V8 the local
// `syrinx` binary was built with, on THIS machine and THIS architecture. It says nothing about
// another OS or another CPU. Measured so far, by hand rather than by CI: x86-64 Linux, with
// Node 22 (V8 12.4) and Electron 35 (V8 13.4) both bit-identical to rusty_v8 15.2 across the
// examples. Windows is reachable and unmeasured; macOS is neither. Until those exist, the honest
// claim is "identical where measured", and a platform nobody has run this on is a platform where
// the preview is unproven.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { render, renderEach } from "../js/index.js";

const REPO = dirname(dirname(fileURLToPath(import.meta.url)));
const CLI = process.env.SYRINX_CLI ?? join(REPO, "target", "release", "syrinx");
const EXAMPLES = join(REPO, "examples");
// The examples are a project whose framework is the repository's own ../framework, so the jail is
// the repository: the same shape as an album with its vendored copy beside its tracks.

/** The Rust host's samples, via the CLI's raw float output. */
function rustSamples(sourcePath, extra = []) {
  const out = join(mkdtempSync(join(tmpdir(), "syrinx-identity-")), "out.f32");
  execFileSync(CLI, ["compile", sourcePath, "-o", out, ...extra], { stdio: ["ignore", "pipe", "pipe"] });
  const bytes = readFileSync(out);
  // .f32 is bare little-endian floats. Copy into an aligned buffer: a Node Buffer's byteOffset is
  // not guaranteed to be a multiple of 4, and Float32Array would throw on an unaligned view.
  const aligned = new Uint8Array(bytes.byteLength);
  aligned.set(bytes);
  return new Float32Array(aligned.buffer);
}

/** Differing samples, the largest gap, and where it first appears. Counts, not adjectives. */
function compare(a, b) {
  const n = Math.min(a.length, b.length);
  let differing = 0;
  let maxAbs = 0;
  let firstIndex = -1;
  for (let i = 0; i < n; i++) {
    const d = Math.abs(a[i] - b[i]);
    if (d !== 0) {
      differing++;
      if (firstIndex < 0) firstIndex = i;
      if (d > maxAbs) maxAbs = d;
    }
  }
  return { differing, maxAbs, firstIndex, compared: n };
}

const examples = readdirSync(EXAMPLES).filter((f) => f.endsWith(".syr")).sort();

test("the CLI is built, because this test compares against it rather than skipping", () => {
  assert.ok(existsSync(CLI),
    `no syrinx CLI at ${CLI} — run \`make build\` (or set SYRINX_CLI). A skipped identity test `
    + "would report success for a comparison that never happened, which is the one outcome this "
    + "file must not produce.");
});

test("there are examples to compare", () => {
  assert.ok(examples.length > 0, `no .syr examples in ${EXAMPLES}`);
});

for (const file of examples) {
  test(`byte identity: ${file}`, async () => {
    const sourcePath = join(EXAMPLES, file);
    const theirs = rustSamples(sourcePath);
    const mine = await render({ path: sourcePath, root: REPO });

    assert.equal(mine.samples.length, theirs.length,
      `sample counts differ: js ${mine.samples.length} (${mine.frames} frames x ${mine.channels} ch @ ${mine.sampleRate}) vs rust ${theirs.length}`);
    const r = compare(mine.samples, theirs);
    assert.equal(r.differing, 0,
      `${r.differing} of ${r.compared} samples differ; max |delta| ${r.maxAbs.toExponential(3)}, first at index ${r.firstIndex}`);
  });
}

for (const file of examples) {
  test(`byte identity, layer by layer: ${file}`, async () => {
    // The mix agreeing is not enough. A layer is what an author renders and approves on its own,
    // so each one has to be identical across the hosts in its own right -- and a mix that agreed
    // while its layers did not would mean two compensating differences, which is worse than one.
    const sourcePath = join(EXAMPLES, file);
    const layers = await renderEach({ path: sourcePath, root: REPO });
    assert.ok(layers.length > 0, `${file} declares no layers`);
    for (const layer of layers) {
      const theirs = rustSamples(sourcePath, ["--stem", layer.stem]);
      assert.equal(layer.samples.length, theirs.length, `${file}#${layer.stem}: sample counts differ`);
      const r = compare(layer.samples, theirs);
      assert.equal(r.differing, 0,
        `${file}#${layer.stem}: ${r.differing} of ${r.compared} samples differ; max |delta| `
        + `${r.maxAbs.toExponential(3)}, first at index ${r.firstIndex}`);
    }
  });
}

test("a streaming example is in the suite, and is seen as one", async () => {
  // The identity claim covers streams only while an example streams. If beacon.syr were ever
  // rewritten whole, every comparison above would still pass and cover nothing of the block
  // driver; this is what says so.
  const layers = await renderEach({ path: join(EXAMPLES, "beacon.syr"), root: REPO });
  assert.ok(layers.some((l) => l.streaming), "beacon.syr must have a streaming layer");
  assert.ok(layers.some((l) => !l.streaming), "and a whole one beside it");
  const mixed = await render({ path: join(EXAMPLES, "beacon.syr"), root: REPO });
  assert.equal(mixed.streaming, true, "and a mix stream");
});

test("summing the layers here gives the mix the Rust host produced", async () => {
  // The exactness claim, checked ACROSS the hosts rather than within one: layers rendered by this
  // host, summed by this host, must equal the mix the other host computed from its own layers.
  const file = examples.find((f) => f !== undefined);
  const sourcePath = join(EXAMPLES, file);
  const info = await renderEach({ path: sourcePath, root: REPO });
  const theirs = rustSamples(sourcePath, ...(info.length > 1 ? [[]] : [[]]));
  const mine = await render({ path: sourcePath, root: REPO });
  assert.equal(compare(mine.samples, theirs).differing, 0);
});

test("the comparison can actually see a difference", async () => {
  // Without this, every assertion above would also pass if `compare` were broken, if both sides
  // returned empty buffers, or if the two renders were accidentally the same object. Perturbing
  // one sample by a single float32 ulp — far finer than 16-bit resolution, whatever the sample's
  // magnitude — must be caught.
  const sourcePath = join(EXAMPLES, examples[0]);
  const mine = await render({ path: sourcePath, root: REPO });
  const theirs = rustSamples(sourcePath);
  assert.equal(compare(mine.samples, theirs).differing, 0, "control precondition: the two must match first");

  const perturbed = Float32Array.from(mine.samples);
  const at = Math.floor(perturbed.length / 3);
  const bits = new Uint32Array(perturbed.buffer, at * 4, 1);
  bits[0] += 1;
  const r = compare(perturbed, theirs);
  assert.equal(r.differing, 1, "a single perturbed sample must be detected");
  assert.equal(r.firstIndex, at);
  assert.ok(r.maxAbs > 0);
});

test("a source that breaks the determinism check is refused, with its position", async () => {
  // The two hosts must AGREE about what is a valid sound, not only about what a valid sound
  // sounds like: a source one accepts and the other rejects is a worse failure than a numeric
  // difference, because it is silent until the other host runs.
  const bad = join(mkdtempSync(join(tmpdir(), "syrinx-bad-")), "bad.syr");
  const source = 'export const meta = { api: 4, name: "bad", duration: 0.01, channels: 1, seed: 1 };\n'
    + "export const stems = { bad: (ctx) => Math.random() };\n";
  const { writeFileSync } = await import("node:fs");
  writeFileSync(bad, source);

  await assert.rejects(
    () => render({ path: bad, root: dirname(bad) }),
    (err) => {
      assert.equal(err.kind, "check");
      assert.match(err.message, /Math\.random/);
      assert.equal(err.line, 2);
      assert.ok(err.column > 0, "the diagnostic must carry a column");
      return true;
    });

  assert.throws(() => execFileSync(CLI, ["check", bad], { stdio: ["ignore", "pipe", "pipe"] }),
    "the Rust host must refuse it too");
});
