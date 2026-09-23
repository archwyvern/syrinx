// The standard math against its reference.
//
// prelude/math.js replaces the implementation-defined Math functions with a port of fdlibm so that
// a render is the same bytes on every engine. That claim rests on the port computing what the C
// computes, bit for bit, and this is where it is checked: every function is run over the sweep in
// sweep.js (a mirror of tools/fdlibm-ref/sweep.h) and its output stream digested; the digest must
// equal the one tools/fdlibm-ref/golden.cc produced from the C compiled with contraction off.
//
// A digest mismatch names the function. To name the input, run
//   tools/fdlibm-ref/build/golden --dump <fn> | node tools/fdlibm-ref/diff.mjs <fn>
// which needs the reference built (`make -C tools/fdlibm-ref build/golden`). The 64 samples in the
// fixture are tried first here, so a difference among them is reported with its input directly.
//
// A mismatch is a port bug. The fixture is regenerated only when the reference changes, never to
// make a test pass.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { BINARY, UNARY, Digest, digestBinary, digestUnary, hiw, low, words } from "./sweep.js";

const golden = JSON.parse(readFileSync(new URL("./fixtures/math-golden.json", import.meta.url), "utf8"));

// The standard runs in its own context so this process's Math stays native: the test needs the
// native functions for nothing, but a test that patched the globals it runs under would be
// measuring itself.
const context = vm.createContext({});
vm.runInContext(readFileSync(new URL("../prelude/math.js", import.meta.url), "utf8"), context);
const M = vm.runInContext("Math", context);

function hex(v) {
  return v.toString(16).padStart(8, "0");
}

function hex64(d) {
  return d !== d ? "7ff8000000000000" : hex(hiw(d)) + hex(low(d));
}

/** The first fixture sample the port gets wrong, described, or null. */
function firstSampleMismatch(name, f, samples) {
  for (const s of samples) {
    const isBinary = s.length === 6;
    const x = words(parseInt(s[0], 16), parseInt(s[1], 16));
    const y = isBinary ? words(parseInt(s[2], 16), parseInt(s[3], 16)) : undefined;
    const expected = words(parseInt(s[s.length - 2], 16), parseInt(s[s.length - 1], 16));
    const actual = isBinary ? f(x, y) : f(x);
    if (hex64(actual) !== hex64(expected)) {
      const args = isBinary ? `${hex64(x)} (${x}), ${hex64(y)} (${y})` : `${hex64(x)} (${x})`;
      return `${name}(${args}): port ${hex64(actual)} (${actual}), reference ${hex64(expected)} (${expected})`;
    }
  }
  return null;
}

test("the standard installs the functions and freezes Math", () => {
  assert.ok(Object.isFrozen(M), "Math must be frozen so a source cannot swap a function back");
  for (const name of [...UNARY, ...BINARY]) {
    assert.equal(typeof M[name], "function", `Math.${name} missing`);
  }
  assert.throws(() => M.random(), /Math\.random is not available/);
  assert.equal(M.sqrt(2), Math.sqrt(2), "exact functions stay native");
});

test("the fixture covers every function the standard defines", () => {
  for (const name of [...UNARY, ...BINARY]) {
    assert.ok(golden.functions[name], `no golden entry for ${name}; run make -C tools/fdlibm-ref golden`);
  }
});

for (const name of UNARY) {
  test(`fdlibm identity: ${name}`, () => {
    const expected = golden.functions[name];
    const d = digestUnary(M[name]);
    assert.equal(d.count, expected.count, "sweep sizes differ: sweep.js and sweep.h have drifted");
    const sample = firstSampleMismatch(name, M[name], expected.samples);
    assert.equal(sample, null, sample ?? "");
    assert.deepEqual({ fnv: d.hex("fnv"), mix: d.hex("mix") }, { fnv: expected.fnv, mix: expected.mix },
      `${name} differs from the reference on an input outside the samples; use golden --dump ${name} | diff.mjs ${name}`);
  });
}

for (const name of BINARY) {
  test(`fdlibm identity: ${name}`, () => {
    const expected = golden.functions[name];
    const d = digestBinary(M[name]);
    assert.equal(d.count, expected.count, "sweep sizes differ: sweep.js and sweep.h have drifted");
    const sample = firstSampleMismatch(name, M[name], expected.samples);
    assert.equal(sample, null, sample ?? "");
    assert.deepEqual({ fnv: d.hex("fnv"), mix: d.hex("mix") }, { fnv: expected.fnv, mix: expected.mix },
      `${name} differs from the reference on an input outside the samples; use golden --dump ${name} | diff.mjs ${name}`);
  });
}

test("the digest can see one flipped bit", () => {
  // Without this, every identity above would also pass if the digest ignored its input.
  let i = 0;
  const nudged = (x) => {
    const y = M.sin(x);
    if (i++ !== 1000) return y;
    return words(hiw(y), (low(y) ^ 1) >>> 0);
  };
  const d = digestUnary(nudged);
  const reference = golden.functions.sin;
  assert.notEqual(d.hex("fnv"), reference.fnv);
  assert.notEqual(d.hex("mix"), reference.mix);
  // And two identical streams digest identically, so the lanes are a function of the outputs.
  const a = new Digest();
  const b = new Digest();
  for (const v of [1.5, -0, NaN, 1e300]) {
    a.add(v);
    b.add(v);
  }
  assert.equal(a.hex("fnv"), b.hex("fnv"));
  assert.equal(a.hex("mix"), b.hex("mix"));
});
