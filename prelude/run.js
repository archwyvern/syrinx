// syrinx run wrapper -- how a source's layers become audio, whole or one block at a time.
//
// This file is a single expression, evaluated as a script by every host, exactly once per
// isolate, whose value is an object with two entry points. It decides what the contract's shapes
// mean: a layer is called with its own context and its return value becomes exactly `channels`
// planes of exactly `frames` samples, or a stream that yields those planes one block at a time;
// the mix is the default export applied to the layers, a mix stream fed their blocks, or their
// plain sum. Two hosts disagreeing about any of that is a different sound, not a different
// error, which is why the wrapper is one file rather than a copy in each host.
//
// The division of labour with the hosts is: ARITHMETIC HAPPENS HERE, permutations may happen
// anywhere. Summing layers in Rust or C# would accumulate in a different type than JavaScript's
// doubles and the mix would differ by host. Interleaving planes, chopping whole planes into
// blocks and copying a block out are permutations of values already decided, so a host may do
// those itself. A block may be the stream's own scratch buffer: a host copies it out before it
// pulls the next one.
//
// Streams. A layer or the default export may return a function instead of a buffer. `stem` and
// `mix` then return a driver: a function the host calls once per block, in order, with the
// block's first frame (0, BLOCK_FRAMES, 2 * BLOCK_FRAMES, ...), which derives the block's length
// itself, so a host cannot pull a wrong-sized block, and refuses an offset out of sequence, so a
// host bug fails loudly instead of rendering something. Around every block call the flag behind
// `__syrinx.block` is raised: the prelude's whole-render helpers (normalize, fade, place) read it
// and refuse inside a block. The global is frozen and the getter reads a variable of this scope,
// so nothing outside this file can set it.
//
// The mix stage is classified by a probe. Called with `buffers === null`, `mix` hands the default
// export a `ctx.stems` whose getters record the read and then throw. Read: it is a whole-buffer
// mix, `mix` returns null, and the host calls again with the layers' planes in a fresh isolate.
// Not read and a function came back: a mix stream, and the driver is returned. Classification is
// on the record, never on the throw propagating: a mix that wraps its reads in try/catch would
// otherwise swallow the throw, return some buffer, and be taken for the mix.
//
// The entry points are plain closures rather than methods: a host that called them with an
// undefined receiver would break, and there is no reason to make how a host invokes them matter.
(function () {
  /** Frames per block of a stream. Must equal the prelude's BLOCK_FRAMES; a Rust test pins both. */
  const BLOCK_FRAMES = 4096;

  if (typeof __syrinx !== "undefined") throw new Error("internal: run.js evaluated twice in one isolate");
  let inBlock = false;
  globalThis.__syrinx = Object.freeze({ get block() { return inBlock; } });

  const READ_AND_STREAMED =
    "the default export read ctx.stems and returned a stream: a mix stream may not read ctx.stems; "
    + "the blocks are its third argument, (offset, frames, stems)";

  const asBuffer = (x, what, expected) => {
    if (x instanceof Float32Array) return x;
    if (Array.isArray(x) && (x.length === 0 || typeof x[0] === "number")) return Float32Array.from(x);
    throw new TypeError(`${what} must return ${expected}, got ${x === null ? "null" : typeof x}`);
  };

  /**
   * Return value -> exactly `channels` planes of exactly `frames`. A whole return is fitted to
   * `frames` (zero-padded or truncated, the rule since api 1); a block's length must match, because
   * padding a block would hide an inclusive bound or a fixed-size scratch buffer and render
   * something anyway.
   */
  const planes = (result, frames, channels, what, block) => {
    let out;
    if (Array.isArray(result) && result.length > 0 && typeof result[0] !== "number") {
      out = result.map((p) => asBuffer(p, what, "an array of channel buffers"));
    } else {
      out = [asBuffer(result, what, "a Float32Array, a number[] or [left, right]")];
    }
    // Widening COPIES rather than repeating the reference. An aliased pair would be transferred
    // twice by the JavaScript host (a duplicate transferable throws) and would let anything
    // downstream mutate both channels through one of them.
    if (out.length === 1 && channels === 2) out = [out[0], Float32Array.from(out[0])];
    if (out.length !== channels) {
      throw new RangeError(`${what} returned ${out.length} channel(s) but meta.channels is ${channels}`);
    }
    return out.map((p) => {
      if (p.length === frames) return p;
      if (block) throw new RangeError(`${what} returned ${p.length} samples for a ${frames}-frame block`);
      const fitted = new Float32Array(frames);
      fitted.set(p.subarray(0, Math.min(p.length, frames)));
      return fitted;
    });
  };

  /**
   * The sum of the layers, plane by plane, in the order given. Accumulated in a Float32Array: the
   * rounding at every step is part of the definition, so that every host -- and a caller summing
   * the layers itself -- agrees.
   *
   * It starts from a COPY OF THE FIRST layer rather than from zeros, which makes summing a single
   * layer exactly that layer. Starting from zeros would not: `0.0 + -0.0` is `+0.0`, so a source
   * with one layer would render one way through the mix and another through the shortcut every
   * host takes when there is nothing to combine.
   */
  const sum = (buffers, frames, channels) => {
    const out = [];
    for (let c = 0; c < channels; c++) {
      const acc = buffers[0][c].slice();
      for (let b = 1; b < buffers.length; b++) {
        const plane = buffers[b][c];
        for (let i = 0; i < frames; i++) acc[i] += plane[i];
      }
      out.push(acc);
    }
    return out;
  };

  /**
   * The driver of a stream: offsets in order from `from`, the block flag raised around the call,
   * the block normalised. `fn(offset, n, blocks)` is the stream itself, adapted by the caller.
   */
  const driver = (fn, frames, channels, what, from) => {
    if (from % BLOCK_FRAMES !== 0 || from < 0 || from >= frames) {
      throw new Error(`internal: a stream cannot start at frame ${from}: not a block boundary before the end`);
    }
    let cursor = from;
    return (offset, blocks) => {
      if (offset !== cursor) {
        throw new Error(`internal: block at frame ${offset} pulled out of order (expected ${cursor})`);
      }
      const n = Math.min(BLOCK_FRAMES, frames - offset);
      cursor += n;
      inBlock = true;
      let result;
      try {
        result = fn(offset, n, blocks);
      } finally {
        inBlock = false;
      }
      return planes(result, n, channels, `${what} (block at frame ${offset})`, true);
    };
  };

  return {
    BLOCK_FRAMES,

    /**
     * Calls one layer and normalises what it returned: planes when it rendered whole, or a driver
     * `(offset) => planes` when it streams.
     */
    stem(fn, sr, frames, duration, seed, channels, name) {
      const what = `stem "${name}"`;
      const result = fn({ sr, frames, duration, seed, channels, stem: name });
      if (typeof result === "function") return driver((offset, n) => result(offset, n), frames, channels, what, 0);
      return planes(result, frames, channels, what, false);
    },

    /**
     * The mix stage. `fn` is the source's default export, or null when it has none, in which case
     * the mix is the sum of the layers in the order of `names`.
     *
     * `names` and `buffers` are parallel arrays; each entry of `buffers` is itself an array of
     * `channels` planes. With `buffers`: the whole mix, as planes. With `buffers === null`: the
     * probe. Returns null when `fn` read ctx.stems (call again with the planes), a driver
     * `(offset, blocks) => planes` when the mix streams (`blocks` parallel to `names`, first block
     * at `from`), or planes when `fn` returned a buffer without reading its layers.
     */
    mix(fn, names, buffers, sr, frames, duration, seed, channels, from) {
      const byName = (blocks) => {
        const stems = {};
        for (let i = 0; i < names.length; i++) stems[names[i]] = blocks[i];
        return stems;
      };
      if (fn === null || fn === undefined) {
        if (buffers !== null) return sum(buffers, frames, channels);
        return driver((offset, n, blocks) => sum(blocks, n, channels), frames, channels, "the mix", from);
      }
      if (buffers !== null) {
        const result = fn({ sr, frames, duration, seed, channels, stems: byName(buffers) });
        if (typeof result === "function") throw new TypeError(READ_AND_STREAMED);
        return planes(result, frames, channels, "the default export", false);
      }
      let read = false;
      const stems = {};
      for (const name of names) {
        Object.defineProperty(stems, name, {
          enumerable: true,
          get() {
            read = true;
            throw new TypeError(
              `ctx.stems.${name} is not available to a stream: the blocks are the third argument, (offset, frames, stems)`);
          },
        });
      }
      let result;
      try {
        result = fn({ sr, frames, duration, seed, channels, stems });
      } catch (e) {
        if (read) return null;
        throw e;
      }
      if (read) {
        if (typeof result === "function") throw new TypeError(READ_AND_STREAMED);
        return null;
      }
      if (typeof result === "function") {
        return driver((offset, n, blocks) => result(offset, n, byName(blocks)), frames, channels, "the default export", from);
      }
      return planes(result, frames, channels, "the default export", false);
    },
  };
})()
