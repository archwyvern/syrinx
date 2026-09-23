// The sweep every function is digested over, and the digest itself — a line-for-line mirror of
// tools/fdlibm-ref/sweep.h. The two must produce the same inputs in the same order and hash the
// outputs the same way, or the golden digests mean nothing. Every operation here is one both
// languages define identically: uint32 arithmetic, IEEE double add/multiply/divide, and bit
// construction of doubles from two words.

const F64 = new Float64Array(1);
const U32 = new Uint32Array(F64.buffer);
const LITTLE = new Uint8Array(new Uint16Array([1]).buffer)[0] === 1;
const HI = LITTLE ? 1 : 0;
const LO = LITTLE ? 0 : 1;

export function words(hi, lo) {
  U32[HI] = hi;
  U32[LO] = lo;
  return F64[0];
}

export function hiw(d) {
  F64[0] = d;
  return U32[HI];
}

export function low(d) {
  F64[0] = d;
  return U32[LO];
}

/** The double one ulp below a positive finite value. */
function prev(d) {
  F64[0] = d;
  if (U32[LO] === 0) {
    U32[HI] -= 1;
    U32[LO] = 0xFFFFFFFF;
  } else {
    U32[LO] -= 1;
  }
  return F64[0];
}

/** The double one ulp above a positive finite value. */
function next(d) {
  F64[0] = d;
  if (U32[LO] === 0xFFFFFFFF) {
    U32[HI] += 1;
    U32[LO] = 0;
  } else {
    U32[LO] += 1;
  }
  return F64[0];
}

/** Marsaglia xorshift128, fixed seed. */
class XorShift {
  constructor() {
    this.x = 0x9E3779B9;
    this.y = 0x243F6A88;
    this.z = 0xB7E15162;
    this.w = 0x6A09E667;
  }
  next() {
    const t = (this.x ^ (this.x << 11)) >>> 0;
    this.x = this.y;
    this.y = this.z;
    this.z = this.w;
    this.w = (this.w ^ (this.w >>> 19) ^ (t ^ (t >>> 8))) >>> 0;
    return this.w;
  }
}

function rotl(v, n) {
  return ((v << n) | (v >>> (32 - n))) >>> 0;
}

/**
 * Two independent 32-bit lanes over the output bit stream. NaN outputs are canonicalised first:
 * which NaN an engine hands back is not part of the contract.
 */
export class Digest {
  constructor() {
    this.fnv = 0x811C9DC5;
    this.mix = 0x9747B28C;
    this.count = 0;
  }
  add(y) {
    let hi = hiw(y);
    let lo = low(y);
    if (y !== y) {
      hi = 0x7FF80000;
      lo = 0;
    }
    let fnv = this.fnv;
    fnv = Math.imul(fnv ^ (lo & 255), 0x01000193) >>> 0;
    fnv = Math.imul(fnv ^ ((lo >>> 8) & 255), 0x01000193) >>> 0;
    fnv = Math.imul(fnv ^ ((lo >>> 16) & 255), 0x01000193) >>> 0;
    fnv = Math.imul(fnv ^ (lo >>> 24), 0x01000193) >>> 0;
    fnv = Math.imul(fnv ^ (hi & 255), 0x01000193) >>> 0;
    fnv = Math.imul(fnv ^ ((hi >>> 8) & 255), 0x01000193) >>> 0;
    fnv = Math.imul(fnv ^ ((hi >>> 16) & 255), 0x01000193) >>> 0;
    fnv = Math.imul(fnv ^ (hi >>> 24), 0x01000193) >>> 0;
    this.fnv = fnv;
    let mix = this.mix;
    mix = Math.imul(mix ^ lo, 0x9E3779B1) >>> 0;
    mix = rotl(mix, 13);
    mix = Math.imul(mix ^ hi, 0x85EBCA6B) >>> 0;
    mix = rotl(mix, 13);
    this.mix = mix;
    this.count++;
  }
  hex(lane) {
    return this[lane].toString(16).padStart(8, "0");
  }
}

function lin(a, b, n, visit) {
  const span = b - a;
  for (let i = 0; i < n; i++) {
    visit(a + span * (i / (n - 1)));
  }
}

/** Every input of a one-argument function's sweep, in order. */
export function unary(visit) {
  const rng = new XorShift();
  for (let i = 0; i < 200000; i++) {
    const h = rng.next();
    const l = rng.next();
    visit(words(h, l));
  }
  lin(-1.0, 1.0, 40001, visit);
  lin(-8.0, 8.0, 40001, visit);
  lin(-800.0, 800.0, 40001, visit);
  lin(-1e18, 1e18, 20001, visit);
  for (let i = 0; i < 40000; i++) {
    const e = i % 2047;
    const mh = rng.next() & 0xFFFFF;
    const l = rng.next();
    const sign = ((i & 1) << 31) >>> 0;
    visit(words((sign | (e << 20) | mh) >>> 0, l));
  }
  for (let k = 1; k <= 2000; k++) {
    const t = k * 1.5707963267948966;
    visit(t);
    visit(t - 1e-9);
    visit(t + 1e-9);
    visit(prev(t));
    visit(next(t));
  }
}

/** The special values paired with each other in the binary sweep. */
function specials() {
  return [
    0.0, -0.0, 1.0, -1.0, 0.5, -0.5, 2.0, -2.0, 3.0, 1.0 / 3.0, 10.0, -10.0,
    1e-300, -1e-300, 1e300, -1e300, words(0, 1), Infinity, -Infinity, NaN,
    0.1, 1.5, 100.0, 4503599627370496.0, 9007199254740992.0, 1e10, -1e10,
  ];
}

/** Every input pair of a two-argument function's sweep, in order. */
export function binary(visit) {
  const rng = new XorShift();
  for (let i = 0; i < 200000; i++) {
    const xh = rng.next();
    const xl = rng.next();
    const yh = rng.next();
    const yl = rng.next();
    visit(words(xh, xl), words(yh, yl));
  }
  lin(-8.0, 8.0, 201, (x) => {
    lin(-8.0, 8.0, 201, (y) => visit(x, y));
  });
  const s = specials();
  for (let i = 0; i < s.length; i++) {
    for (let j = 0; j < s.length; j++) {
      visit(s[i], s[j]);
    }
  }
  for (let i = 0; i < 2000; i++) {
    const e = Math.floor((i * 2047) / 2000);
    const mh = rng.next() & 0xFFFFF;
    const l = rng.next();
    const sign = ((i & 1) << 31) >>> 0;
    const x = words((sign | (e << 20) | mh) >>> 0, l);
    lin(-100.0, 100.0, 41, (y) => visit(x, y));
  }
}

export const UNARY = [
  "acos", "acosh", "asin", "asinh", "atan", "atanh", "cbrt", "cos", "cosh", "exp", "expm1",
  "log", "log1p", "log2", "log10", "sin", "sinh", "tan", "tanh",
];
export const BINARY = ["atan2", "pow", "hypot"];

/** Digest of `f` over the unary sweep. */
export function digestUnary(f) {
  const d = new Digest();
  unary((x) => d.add(f(x)));
  return d;
}

/** Digest of `f` over the binary sweep. */
export function digestBinary(f) {
  const d = new Digest();
  binary((x, y) => d.add(f(x, y)));
  return d;
}
