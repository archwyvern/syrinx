// What a host reads off a source before it renders anything: its meta, its layers, its geometry.
//
// Shared by the Node host (worker.js) and the browser host (browser.js) so the two cannot drift:
// the checks, their order and their messages are the Rust host's (`read_meta`, `read_stems` and
// `geometry` in host.rs), and a source one host accepts and another refuses is a divergence even
// when neither renders a wrong sample. Pure: no I/O, no engine, nothing but the module namespace.

/** The oldest contract a source may declare; api 3 is additive over 2 (API_FLOOR in lib.rs). */
export const API_FLOOR = 2;

/** A source that breaks the contract. The message is the one every host reports. */
export class ContractError extends Error {
  constructor(message) {
    super(message);
    this.name = "ContractError";
  }
}

/** The source's `meta`, checked against the contract range this prelude provides. */
export function readMeta(module, preludeVersion) {
  const meta = module.meta;
  if (!meta || typeof meta !== "object") {
    throw new ContractError("source has no `export const meta = { ... }`");
  }
  if (meta.api !== undefined && !(Number.isInteger(meta.api) && meta.api >= API_FLOOR && meta.api <= preludeVersion)) {
    throw new ContractError(
      `source declares meta.api ${meta.api} but this compiler provides api ${preludeVersion} and accepts ${API_FLOOR} to ${preludeVersion}`);
  }
  return meta;
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
 * The render's geometry: an explicit rate wins, then the source's own, then 48 kHz, and the frame
 * count is round(duration * rate). A harness that computed frames any other way would show a
 * "host difference" that is really its own arithmetic.
 */
export function geometry(meta, sampleRate) {
  const rate = sampleRate || meta.sampleRate || 48000;
  const frames = Math.round(meta.duration * rate);
  if (frames === 0) throw new ContractError("meta.duration rounds to zero frames");
  return { rate, frames, channels: meta.channels ?? 1 };
}
