// Static determinism check — a 1:1 port of crates/syrinx-core/src/check.rs.
//
// Sound sources must produce the same bytes every time they are compiled, on every machine.
// Anything that reads the wall clock, the process, or entropy is rejected before the source ever
// reaches V8, with a located error. This is a lexical scan, not a parser: comments and string
// literals are skipped so that a banned name in a comment does not trip it, but a determined
// author can still smuggle one through (`globalThis["Ma"+"th"]`). The purpose is to catch honest
// mistakes early, not to sandbox hostile code.
//
// PORTING NOTES, because "1:1" is a claim and these are where it could quietly stop being true:
//   - Rust iterates `Vec<char>`, i.e. Unicode SCALARS. `Array.from(source)` does the same;
//     iterating a JS string by index would split astral characters and shift every later column.
//   - Rust's column is `line[..start].chars().count() + 1` — scalars before the match, not UTF-16
//     units. `[...prefix].length` matches; `String.prototype.indexOf`'s own index does not.
//   - Both sides sort by (line, column) with a STABLE sort, so two banned names at the same
//     position keep BANNED order. Array.prototype.sort has been required to be stable since ES2019.
// check.test.js runs the same fixtures through both implementations and compares line, column and
// message, which is what actually holds this honest.

/** Banned names and why. Order matters only for the message. */
export const BANNED = [
  ["Math.random", "unseeded randomness; use `new Random(seed)` from the prelude"],
  ["Date", "wall clock"],
  ["performance", "wall clock"],
  ["setTimeout", "timers do not exist in a sound source"],
  ["setInterval", "timers do not exist in a sound source"],
  ["queueMicrotask", "scheduling does not exist in a sound source"],
  ["eval", "dynamic code defeats the determinism check"],
  ["Function", "dynamic code defeats the determinism check"],
  ["globalThis", "reaching the realm's globals defeats the determinism check"],
  ["Intl", "locale-dependent"],
  ["crypto", "entropy"],
  ["require", 'use `import` from "syrinx" or a relative path'],
  ["fetch", "no I/O in a sound source"],
  ["console", "no I/O in a sound source"],
];

/**
 * The exponentiation operator compiles to the engine's own pow, which the standard math cannot
 * replace the way it replaces `Math.pow`. It is the one operator in the language that reaches
 * transcendental arithmetic.
 */
const EXPONENT_WHY = "exponentiation uses the engine's own pow, not the standard's; call Math.pow(x, y)";

/**
 * Scan `source` and report every use of a banned name, and every `**`.
 * @param {string} source
 * @returns {{line: number, column: number, message: string}[]} 1-based positions, ordered.
 */
export function check(source) {
  const stripped = strip(source);
  const out = [];
  for (const [name, why] of BANNED) {
    for (const [line, column] of findIdentifier(stripped, name)) {
      out.push({ line, column, message: `\`${name}\` is not allowed in a sound source: ${why}` });
    }
  }
  for (const [line, column] of findExponent(stripped)) {
    out.push({ line, column, message: `\`**\` is not allowed in a sound source: ${EXPONENT_WHY}` });
  }
  out.sort((a, b) => (a.line - b.line) || (a.column - b.column));
  return out;
}

/**
 * Every `**` (which covers `**=`) in the stripped text. Comments and literals are already blanks,
 * and no other JavaScript token contains two adjacent asterisks. Columns count code points, as
 * findIdentifier's do.
 */
function findExponent(text) {
  const hits = [];
  const lines = text.split("\n").map((l) => (l.endsWith("\r") ? l.slice(0, -1) : l));
  for (let lineIndex = 0; lineIndex < lines.length; lineIndex++) {
    const line = lines[lineIndex];
    let from = 0;
    for (;;) {
      const start = line.indexOf("**", from);
      if (start < 0) break;
      hits.push([lineIndex + 1, [...line.slice(0, start)].length + 1]);
      from = start + 2;
    }
  }
  return hits;
}

/**
 * Replace comments and string/template literal bodies with spaces, preserving every newline and
 * every column so positions in the result map 1:1 onto the source.
 */
/**
 * Blank comments and string literals, keeping everything else in place.
 *
 * `preserveUtf16` decides what a blanked character costs. The check wants SCALARS, because
 * check.rs counts columns in `Vec<char>` and the two must agree to the column. A caller that
 * needs to splice the ORIGINAL at positions found in the stripped text wants UTF-16 units
 * instead, or every astral character inside a preceding literal shifts it by one.
 *
 * `keepStrings` leaves string and template contents in place and blanks only comments. The
 * check wants them blanked, so a banned name inside a literal is data. The module loader must
 * NOT blank them, because an import specifier is itself a string literal.
 *
 * Both default to the check's behaviour, so the 1:1 port with check.rs is unaffected.
 */
export function strip(source, { preserveUtf16 = false, keepStrings = false } = {}) {
  const CODE = 0, LINE_COMMENT = 1, BLOCK_COMMENT = 2, STR = 3, TEMPLATE = 4;
  const blank = (ch) => (preserveUtf16 ? " ".repeat(ch.length) : " ");
  const text = (ch) => (keepStrings ? ch : blank(ch));
  const chars = Array.from(source);
  let out = "";
  let state = CODE;
  let quote = "";
  let i = 0;
  while (i < chars.length) {
    const c = chars[i];
    const next = i + 1 < chars.length ? chars[i + 1] : undefined;
    if (state === CODE) {
      if (c === "/" && next === "/") {
        state = LINE_COMMENT;
        out += "  ";
        i += 2;
        continue;
      }
      if (c === "/" && next === "*") {
        state = BLOCK_COMMENT;
        out += "  ";
        i += 2;
        continue;
      }
      if (c === '"' || c === "'") {
        state = STR;
        quote = c;
        out += c;
      } else if (c === "`") {
        state = TEMPLATE;
        out += c;
      } else {
        out += c;
      }
    } else if (state === LINE_COMMENT) {
      if (c === "\n") {
        state = CODE;
        out += "\n";
      } else {
        out += blank(c);
      }
    } else if (state === BLOCK_COMMENT) {
      if (c === "*" && next === "/") {
        state = CODE;
        out += "  ";
        i += 2;
        continue;
      }
      out += c === "\n" ? "\n" : blank(c);
    } else if (state === STR) {
      if (c === "\\") {
        out += text(c);
        if (next !== undefined) {
          out += next === "\n" ? "\n" : text(next);
          i += 2;
          continue;
        }
      } else if (c === quote || c === "\n") {
        state = CODE;
        out += c;
      } else {
        out += text(c);
      }
    } else {
      // Template substitutions (`${...}`) are code, but nesting them properly needs a real
      // parser; treat the whole literal as text. A banned name inside a substitution therefore
      // slips through this check and fails at run time.
      if (c === "\\") {
        out += text(c);
        if (next !== undefined) {
          out += next === "\n" ? "\n" : text(next);
          i += 2;
          continue;
        }
      } else if (c === "`") {
        state = CODE;
        out += c;
      } else {
        out += c === "\n" ? "\n" : text(c);
      }
    }
    i += 1;
  }
  return out;
}

function isIdent(c) {
  // Rust's char::is_alphanumeric is Unicode-aware, so a plain /[a-z0-9]/i test would disagree
  // about, say, `café`. \p{Alphabetic} and \p{Nd} are the closest JS equivalents.
  return c === "_" || c === "$" || /[\p{Alphabetic}\p{Nd}]/u.test(c);
}

/** Every occurrence of `name` (which may be dotted) as a whole identifier path. */
function findIdentifier(text, name) {
  const hits = [];
  // Rust's `str::lines()` also strips a trailing \r on \r\n; split on \n and drop it by hand.
  const lines = text.split("\n").map((l) => (l.endsWith("\r") ? l.slice(0, -1) : l));
  // A trailing newline yields one empty final element in JS but no final line in Rust; an empty
  // line can hold no match, so the extra element is harmless rather than worth special-casing.
  for (let lineIndex = 0; lineIndex < lines.length; lineIndex++) {
    const line = lines[lineIndex];
    let from = 0;
    for (;;) {
      const start = line.indexOf(name, from);
      if (start < 0) break;
      const end = start + name.length;
      // Code POINTS on both edges, not UTF-16 units: `line[start - 1]` on an astral character
      // yields a lone surrogate, which is neither alphanumeric nor '.', so a match right after
      // one would be accepted here and rejected by Rust.
      const beforeOk = start === 0 || (() => {
        const before = [...line.slice(0, start)];
        const prev = before[before.length - 1];
        return !isIdent(prev) && prev !== ".";
      })();
      const afterOk = end >= line.length || !isIdent([...line.slice(end)][0]);
      if (beforeOk && afterOk) {
        hits.push([lineIndex + 1, [...line.slice(0, start)].length + 1]);
      }
      from = end;
    }
  }
  return hits;
}
