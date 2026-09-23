// The hosts install the standard.
//
// math.test.js proves prelude/math.js computes what fdlibm computes. This file proves a HOST ran
// it: a host that forgot would render with the engine's own Math and every other test would still
// pass, because the engine's Math is also a perfectly good Math. The observable difference is that
// the standard's Math is frozen and its `random` throws. Each case is put to both hosts, because
// the guarantee is that they agree.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { render } from "../js/index.js";

const REPO = dirname(dirname(fileURLToPath(import.meta.url)));
const CLI = process.env.SYRINX_CLI ?? join(REPO, "target", "release", "syrinx");

function source(body) {
  const root = mkdtempSync(join(tmpdir(), "syrinx-standard-"));
  const path = join(root, "s.syr");
  writeFileSync(path, 'export const meta = { api: 4, name: "s", duration: 0.01, channels: 1, seed: 1 };\n' + body);
  return { root, path };
}

/** The Rust host's verdict on a source: its stderr when it refuses, or null when it renders. */
function rustRefusal(path) {
  try {
    execFileSync(CLI, ["compile", path, "-o", join(dirname(path), "out.f32")], { stdio: ["ignore", "pipe", "pipe"] });
    return null;
  } catch (err) {
    return String(err.stderr);
  }
}

test("Math is frozen inside a source, on both hosts", async () => {
  const { root, path } = source(
    "export const stems = { s: (ctx) => { Math.sin = () => 0; return new Float32Array(ctx.frames); } };\n");
  await assert.rejects(() => render({ path, root }), (err) => {
    assert.equal(err.kind, "runtime");
    assert.match(err.message, /read only|not extensible|Cannot assign/);
    return true;
  });
  const refusal = rustRefusal(path);
  assert.ok(refusal !== null, "the Rust host must refuse it too");
  assert.match(refusal, /read only|not extensible|Cannot assign/);
});

test("Math.random reached around the lexical check throws at run time, on both hosts", async () => {
  // `M["ran" + "dom"]` is exactly the smuggling the check's own doc comment admits it cannot see.
  const { root, path } = source(
    'const M = [Math][0];\nexport const stems = { s: (ctx) => { M["ran" + "dom"](); return new Float32Array(ctx.frames); } };\n');
  await assert.rejects(() => render({ path, root }), (err) => {
    assert.equal(err.kind, "runtime");
    assert.match(err.message, /Math\.random is not available/);
    return true;
  });
  const refusal = rustRefusal(path);
  assert.ok(refusal !== null, "the Rust host must refuse it too");
  assert.match(refusal, /Math\.random is not available/);
});

test("the exponentiation operator is refused with its position, on both hosts", async () => {
  const body = "export const stems = { s: (ctx) => new Float32Array(ctx.frames).fill(2 ** -3) };\n";
  const { root, path } = source(body);
  // Taken from the text rather than hand-counted, so the assertion says where the operator IS.
  const column = body.indexOf("**") + 1;
  await assert.rejects(() => render({ path, root }), (err) => {
    assert.equal(err.kind, "check");
    assert.match(err.message, /Math\.pow/);
    assert.equal(err.line, 2);
    assert.equal(err.column, column);
    return true;
  });
  const refusal = rustRefusal(path);
  assert.ok(refusal !== null, "the Rust host must refuse it too");
  assert.match(refusal, new RegExp(`2:${column}`));
});

test("a source that uses every replaced function renders identically on both hosts", async () => {
  // The identity test covers the examples; this covers the functions the examples do not reach,
  // through the accumulating shape (each sample feeds the next) that turns one differing bit into
  // a different waveform.
  const { root, path } = source(`
export const stems = { s: (ctx) => {
  const out = new Float32Array(ctx.frames);
  let s = 0.1;
  for (let i = 0; i < ctx.frames; i++) {
    const t = i / ctx.sr;
    s = Math.tanh(s + Math.sin(t * 700) * 0.3) * 0.9
      + Math.cos(t * 3) * Math.exp(-t) * 0.05
      + Math.atan2(s, 1 + t) * 0.01
      + Math.log1p(Math.abs(s)) * 0.01
      + Math.cbrt(s) * 0.001
      + Math.pow(1.0001, i) * 1e-6
      + Math.expm1(s * 0.01)
      + Math.sinh(s * 0.1) * 0.01 + Math.cosh(s * 0.1) * 0.001 - 0.001
      + Math.asinh(s) * 0.001 + Math.atan(s) * 0.001 + Math.asin(s * 0.5) * 0.001
      + Math.acos(s * 0.5) * 0.0001 + Math.atanh(s * 0.5) * 0.001 + Math.acosh(2 + s) * 0.0001
      + Math.log(2 + s) * 0.0001 + Math.log2(2 + s) * 0.0001 + Math.log10(2 + s) * 0.0001
      + Math.tan(s * 0.5) * 0.001 + Math.hypot(s, t) * 0.0001;
    out[i] = s;
  }
  return out;
} };
`);
  const mine = await render({ path, root });
  assert.equal(rustRefusal(path), null, "the Rust host must render it");
  const { readFileSync } = await import("node:fs");
  const bytes = readFileSync(join(root, "out.f32"));
  const aligned = new Uint8Array(bytes.byteLength);
  aligned.set(bytes);
  const theirs = new Float32Array(aligned.buffer);
  assert.equal(mine.samples.length, theirs.length);
  let differing = 0;
  for (let i = 0; i < theirs.length; i++) {
    if (mine.samples[i] !== theirs[i]) differing++;
  }
  assert.equal(differing, 0, `${differing} of ${theirs.length} samples differ`);
  assert.ok(mine.samples.some((v) => v !== 0), "the probe must produce a signal, not silence");
});
