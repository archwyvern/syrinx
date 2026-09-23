// What a host reads off a source before it renders anything: its meta, its layers, its geometry.
//
// Shared by the Node host (worker.js) and the browser host (browser.js) so the two cannot drift:
// the checks, their order and their messages are the Rust host's (`read_meta`, `read_stems` and
// `geometry` in host.rs), and a source one host accepts and another refuses is a divergence even
// when neither renders a wrong sample. Pure: no I/O, no engine, nothing but the module namespace.

/** The contract this host implements (PRELUDE_VERSION in lib.rs and the prelude). */
export const PRELUDE_VERSION = 4;

/** The oldest contract a source may declare (API_FLOOR in lib.rs): 0.9 broke with all before it. */
export const API_FLOOR = 4;

/** A source that breaks the contract. The message is the one every host reports. */
export class ContractError extends Error {
  constructor(message) {
    super(message);
    this.name = "ContractError";
  }
}

/**
 * The source's `meta`, validated field by field exactly as the Rust host's `read_meta`: the same
 * checks, in the same order, with the same messages. A field set to `undefined` counts as absent,
 * as it does there. `preludeVersion` is the contract of the prelude the source was linked against.
 *
 * Returns the fields the host uses; an absent name stays absent (there is no default), and the
 * seed is the integer the source wrote, never a conversion of something else.
 */
export function readMeta(module, preludeVersion) {
  const meta = module.meta;
  if (meta === undefined) throw new ContractError("source has no `export const meta = { ... }`");
  if (meta === null || (typeof meta !== "object" && typeof meta !== "function")) {
    throw new ContractError("`meta` is not an object");
  }
  const field = (key) => (meta[key] === undefined ? undefined : meta[key]);

  const api = field("api");
  if (api === undefined) {
    throw new ContractError(
      `meta.api is required: this compiler provides api ${preludeVersion} and accepts ${API_FLOOR} to ${preludeVersion}`);
  }
  const n = typeof api === "number" ? api : -1;
  if (!(n >= API_FLOOR && n <= preludeVersion) || !Number.isInteger(n)) {
    throw new ContractError(
      `source declares meta.api ${String(api)} but this compiler provides api ${preludeVersion} and accepts ${API_FLOOR} to ${preludeVersion}`);
  }
  const name = field("name");
  if (name !== undefined && typeof name !== "string") throw new ContractError("meta.name must be a string");
  const duration = field("duration");
  if (duration === undefined) throw new ContractError("meta.duration is required (seconds)");
  if (typeof duration !== "number") throw new ContractError("meta.duration must be a number of seconds");
  if (!(Number.isFinite(duration) && duration > 0 && duration <= 600)) {
    throw new ContractError("meta.duration must be in (0, 600] seconds");
  }
  const channels = field("channels") === undefined ? 1 : field("channels");
  if (typeof channels !== "number" || (channels !== 1 && channels !== 2)) {
    throw new ContractError("meta.channels must be 1 or 2");
  }
  const sampleRate = field("sampleRate");
  if (sampleRate !== undefined) {
    if (typeof sampleRate !== "number") throw new ContractError("meta.sampleRate must be a number");
    if (!(sampleRate >= 8000 && sampleRate <= 192000) || !Number.isInteger(sampleRate)) {
      throw new ContractError("meta.sampleRate must be an integer in [8000, 192000]");
    }
  }
  const seed = field("seed") === undefined ? 0 : field("seed");
  if (typeof seed !== "number") throw new ContractError("meta.seed must be a number");
  if (!(Number.isInteger(seed) && seed >= 0 && seed <= 4294967295)) {
    throw new ContractError("meta.seed must be an integer in [0, 4294967295]");
  }
  const loop = field("loop") === undefined ? false : field("loop");
  if (typeof loop !== "boolean") throw new ContractError("meta.loop must be a boolean");

  return { api, name, duration, channels, sampleRate, seed, loop };
}

/**
 * Stem names, in declaration order. The order of the checks matters as much as their text: every
 * host reports a bad NAME before it reports that its value is not a function.
 */
export function readStems(module) {
  const stems = module.stems;
  if (stems === undefined) {
    throw new ContractError(
      "source has no `stems` export; a sound is one or more named layers: export const stems = { name(ctx) { ... } }");
  }
  if (stems === null || typeof stems !== "object") {
    throw new ContractError("`stems` must be an object of functions");
  }
  const names = Object.keys(stems);
  for (const name of names) {
    // The leading letter is load-bearing: an integer-like key sorts itself to the front of a
    // JavaScript object, which would silently change the order the layers are summed in.
    if (!/^[A-Za-z][A-Za-z0-9_.-]{0,63}$/.test(name)) {
      throw new ContractError(`stem name "${name}" must start with a letter and contain only letters, digits, _ . -`);
    }
    if (typeof stems[name] !== "function") {
      throw new ContractError(`stem "${name}" is not a function`);
    }
  }
  if (names.length === 0) {
    throw new ContractError("`stems` is empty; declare at least one layer");
  }
  return { stems, names };
}

/**
 * The render's geometry, from what `readMeta` returned: an explicit rate wins, then the source's
 * own, then 48 kHz, and the frame count is round(duration * rate). A harness that computed frames any other way would show a
 * "host difference" that is really its own arithmetic.
 */
export function geometry(meta, sampleRate) {
  const rate = sampleRate || meta.sampleRate || 48000;
  const frames = Math.round(meta.duration * rate);
  if (frames === 0) throw new ContractError("meta.duration rounds to zero frames");
  return { rate, frames, channels: meta.channels };
}
