// A module's static imports, found and rewritten the way every syrinx host and publisher needs
// them. Both forms: `... from "x"` (which covers `export ... from "x"`) and the side-effect
// `import "x"` -- the Rust host loads whatever V8's resolve callback is handed, so leaving either out
// would make a legal source load there and fail here. Found on the source with comments blanked and
// strings kept: a specifier is a string, and prose in a comment is not an import. Positions are
// UTF-16 offsets into the original, so a rewrite splices the original text.
//
// Dynamic import() is not an import here; no host supports it (SPEC.md, clause 8).

import { strip } from "./check.js";

const IMPORTS = /(\bfrom\s*|\bimport\s+)(["'])([^"']+)\2/g;

/**
 * Every static import, in source order.
 *
 * @param {string} source
 * @returns {{ specifier: string, index: number, length: number, prefix: string }[]} `index` and
 *   `length` span the whole `from "x"` / `import "x"` clause; `prefix` is its text up to the quote.
 */
export function scanImports(source) {
  const scanned = strip(source, { preserveUtf16: true, keepStrings: true });
  return [...scanned.matchAll(IMPORTS)].map((m) => ({
    specifier: m[3],
    index: m.index,
    length: m[0].length,
    prefix: m[1],
  }));
}

/**
 * The module with every import's specifier replaced by `resolve(specifier)`, in one pass, so a
 * specifier that happens to contain another cannot be rewritten twice.
 *
 * @param {string} source
 * @param {(specifier: string) => string} resolve
 * @returns {string}
 */
export function rewriteImports(source, resolve) {
  let out = "";
  let at = 0;
  for (const i of scanImports(source)) {
    out += source.slice(at, i.index) + i.prefix + JSON.stringify(resolve(i.specifier));
    at = i.index + i.length;
  }
  return out + source.slice(at);
}
