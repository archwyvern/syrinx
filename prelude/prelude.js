// syrinx prelude -- the core module every sound source is compiled against: `import ... from "syrinx"`.
//
// The core is what the standard itself needs a source to have: the contract version and block
// size, the seeded randomness that stands in for Math.random, a hash for deriving seeds, and the
// one question the block protocol lets a source ask -- is a block being computed right now?
// Everything else a sound is made of -- oscillators, filters, envelopes, buffers -- is the
// framework (framework/ in the syrinx repository): a library a project vendors and imports by
// relative path. Everything here is deterministic: no wall clock, no Math.random, no I/O.
//
// Contract (PRELUDE_VERSION 4):
//   meta   = { api: 4, duration, name?, channels?, sampleRate?, seed?, loop? }
//   stems  = { <name>({ sr, frames, duration, seed, channels, stem }) -> Output | Stream }
//   default({ sr, frames, duration, seed, channels, stems }) -> Output | MixStream   (optional)
//   Output    = Float32Array | number[] | [left, right]
//   Stream    = (offset, frames) => Output              called once per block of BLOCK_FRAMES, in order
//   MixStream = (offset, frames, stems) => Output       stems[name] = that layer's block as planes
// A mix that reads ctx.stems is whole-buffer; a mix stream gets its layers' blocks as its third
// argument and never reads ctx.stems.

const PRELUDE_VERSION = 4;
// Frames per block of a stream: a constant of the standard, so a block boundary can never leak
// into the samples on one host and not another. 85 ms at 48 kHz.
const BLOCK_FRAMES = 4096;

// ---------------------------------------------------------------- seeds

// FNV-1a over the arguments, as a seed. Two layers each writing `new Noise(ctx.seed)` get the
// same noise, because every stem is handed the same meta.seed; `hash(ctx.seed, "kick")` gives
// each voice its own draw, stable when stems are reordered or renamed around it.
const HASH_SCRATCH = new DataView(new ArrayBuffer(8));
function hash(...parts) {
  let h = 0x811c9dc5;
  const byte = (b) => { h = Math.imul(h ^ (b & 0xff), 0x01000193) >>> 0; };
  for (const part of parts) {
    if (typeof part === "number") {
      HASH_SCRATCH.setFloat64(0, part, true);
      for (let i = 0; i < 8; i++) byte(HASH_SCRATCH.getUint8(i));
    } else {
      const text = String(part);
      for (let i = 0; i < text.length; i++) {
        const c = text.charCodeAt(i);
        byte(c);
        byte(c >>> 8);
      }
    }
    // Separator, so hash("ab", "c") and hash("a", "bc") differ.
    byte(0);
  }
  return h >>> 0;
}

// ---------------------------------------------------------------- randomness (seeded, mulberry32)

class Random {
  constructor(seed = 0) { this.state = (seed | 0) >>> 0; }
  // [0, 1)
  next() {
    let t = (this.state = (this.state + 0x6d2b79f5) >>> 0);
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }
  // [lo, hi)
  range(lo, hi) { return lo + (hi - lo) * this.next(); }
  // [-1, 1)
  bipolar() { return this.next() * 2 - 1; }
  int(lo, hi) { return lo + Math.floor(this.next() * (hi - lo)); }
  chance(p) { return this.next() < p; }
  pick(array) { return array[this.int(0, array.length)]; }
}

// ---------------------------------------------------------------- the block protocol

// True while the host is computing one block of a stream. The run wrapper raises the flag around
// every block call and freezes the global behind a getter over its own variable, so only the host
// can set it. Code that needs the whole render -- a peak, an ending -- asks this and refuses.
function inBlock() {
  return typeof __syrinx !== "undefined" && __syrinx.block === true;
}

// ---------------------------------------------------------------- exports

export { PRELUDE_VERSION, BLOCK_FRAMES, Random, hash, inBlock };
