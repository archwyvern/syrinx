// Finds the first input where prelude/math.js disagrees with the C reference.
//
//   tools/fdlibm-ref/build/golden --dump sin | node tools/fdlibm-ref/diff.mjs sin
//
// The dump is the reference's every input and output as hex; this runs the same inputs through the
// port and prints each mismatch (up to 20) with both outputs, then the totals.

import { readFileSync } from "node:fs";
import vm from "node:vm";

const name = process.argv[2];
if (!name) {
  console.error("usage: golden --dump <fn> | node diff.mjs <fn>");
  process.exit(2);
}

const context = vm.createContext({});
vm.runInContext(readFileSync(new URL("../../prelude/math.js", import.meta.url), "utf8"), context);
const M = vm.runInContext("Math", context);
const fn = M[name];
if (typeof fn !== "function") {
  console.error(`no Math.${name}`);
  process.exit(2);
}

const F64 = new Float64Array(1);
const U32 = new Uint32Array(F64.buffer);
const LITTLE = new Uint8Array(new Uint16Array([1]).buffer)[0] === 1;
const HI = LITTLE ? 1 : 0;
const LO = LITTLE ? 0 : 1;
function words(hex16) {
  U32[HI] = parseInt(hex16.slice(0, 8), 16);
  U32[LO] = parseInt(hex16.slice(8, 16), 16);
  return F64[0];
}
function hex(d) {
  F64[0] = d;
  return U32[HI].toString(16).padStart(8, "0") + U32[LO].toString(16).padStart(8, "0");
}
function canonical(hex16) {
  const d = words(hex16);
  return d !== d ? "7ff8000000000000" : hex16;
}

const lines = readFileSync(0, "utf8").split("\n").filter((l) => l.length > 0);
let mismatches = 0;
for (const line of lines) {
  const parts = line.split(" ");
  const expected = canonical(parts[parts.length - 1]);
  const actual = parts.length === 2
    ? canonical(hex(fn(words(parts[0]))))
    : canonical(hex(fn(words(parts[0]), words(parts[1]))));
  if (actual !== expected) {
    mismatches++;
    if (mismatches <= 20) {
      console.log(`${name}(${parts.slice(0, -1).join(", ")}): port ${actual}, reference ${expected}`);
    }
  }
}
console.log(`${mismatches} of ${lines.length} differ`);
process.exit(mismatches === 0 ? 0 : 1);
