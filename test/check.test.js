// The determinism check, JS against Rust.
//
// js/check.js claims to be a 1:1 port of crates/syrinx-core/src/check.rs. A claim like that rots
// the first time either side is edited, so it is compared rather than asserted: every fixture goes
// through both implementations and the LINE, COLUMN and MESSAGE must match exactly. Positions are
// the part that would silently drift — a diagnostic pointing at the wrong column is worse than no
// diagnostic, because it sends the author to the wrong line.
//
// The Rust side runs through the CLI (`syrinx check`), which is the same code path a user gets.
// If the CLI is not built, these tests FAIL rather than skip: the whole point is the comparison,
// and a green run that compared nothing would be a lie. `make test` builds it first.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { check } from "../js/check.js";

const REPO = dirname(dirname(fileURLToPath(import.meta.url)));
const CLI = process.env.SYRINX_CLI ?? join(REPO, "target", "release", "syrinx");

/**
 * A valid module, so the two sides are comparable.
 *
 * `syrinx check` does not only run the lexical scan: it also EXECUTES the module to read its
 * `meta`, so a fixture that is not a loadable module comes back as a ReferenceError or a syntax
 * error and says nothing about the check. Every parity fixture is therefore a real module, with
 * the interesting text first so its line numbers read as written.
 */
function asModule(body) {
  return body + '\nexport const meta = { api: 4, name: "fixture", duration: 0.01, channels: 1, seed: 1 };\n'
    + "export const stems = { fixture: (ctx) => new Float32Array(ctx.frames) };\n";
}

/** Sources chosen for the edges, not the happy path. Each is wrapped by asModule. */
const FIXTURES = {
  "a plain violation": "const x = 1;\nlet y = Math.random();",
  "several on one line": "const a = () => Date.now() + performance.now();",
  "several lines": "let a = eval('1');\n// nothing\nlet b = crypto;",
  "comments and strings are not code": "// Date is fine here\n/* Math.random too */\nconst s = 'Date';\nconst t = `performance`;",
  "substrings are not identifiers": "const update = 1; const myDate = 2; const evalue = 3;",
  "a property with a banned name is fine": "const obj = {}; obj.Date; obj.eval;",
  "positions survive stripping": "const a = 'x'; // c\nconst b = () => Date.now();",
  "escaped quote does not end the string": "const s = 'it\\'s Date';\nconst t = Date;",
  "block comment spanning lines": "/* Date\n   performance\n*/\nconst x = crypto;",
  "banned name at column 1": "Date;",
  "banned name at end of a line": "const z = fetch;",
  "template literal is text": "const t = `${1}`; const u = Date;",
  "windows line endings": "const a = 1;\r\nconst b = () => Date.now();",
  "non-ascii before a violation": "const café = 1; const x = () => Date.now();",
  "an emoji shifts the column": "const s = '🎵'; const x = () => Date.now();",
  "clean source": "const gain = 0.5;",
  "the exponentiation operator": "const a = 2 ** 10;\nlet b = 1;\nb **= 2;\n/** doc */ const c = '**';",
  "an emoji before the exponent": "const s = '🎵'; const a = 2 ** 3;",
};

/**
 * Edges that CANNOT be a valid module, so they cannot be compared through the CLI. These assert
 * the behaviour check.rs documents rather than parity, and they are labelled that way: an
 * unterminated string is a syntax error, and the CLI would report that instead of the scan.
 */
const JS_ONLY = {
  "a newline ends an unterminated string": ["const s = 'oops\nconst t = Date.now();\n", [[2, 11]]],
  "a banned name at the very end, no newline": ["const z = fetch", [[1, 11]]],
};

/** The Rust check, via the CLI. Diagnostics go to STDOUT; stderr carries only the tally. */
function rustCheck(source) {
  const dir = mkdtempSync(join(tmpdir(), "syrinx-check-"));
  const file = join(dir, "fixture.syr");
  writeFileSync(file, source);
  let stdout;
  try {
    stdout = execFileSync(CLI, ["check", file], { encoding: "utf-8", stdio: ["ignore", "pipe", "pipe"] });
    return []; // exit 0 = accepted
  } catch (err) {
    stdout = String(err.stdout ?? "");
    if (stdout.length === 0) throw err; // not a rejection: a broken rig, and it should say so
    return parseDiagnostics(stdout);
  }
}

/**
 * `<path>:<line>:<column>: <message>` lines.
 *
 * The CLI prints ONE diagnostic inline with its headline ("...:2:9: determinism check failed:
 * `Math.random` is ...") and SEVERAL under a bare headline line, so the prefix is stripped when
 * present rather than assumed either way — a parser that only knew one shape would silently
 * return zero diagnostics for the other and make a mismatch look like agreement.
 */
function parseDiagnostics(text) {
  const out = [];
  for (const raw of text.split("\n")) {
    const m = /:(\d+):(\d+):\s*(.*)$/.exec(raw);
    if (!m) continue;
    out.push({
      line: Number(m[1]),
      column: Number(m[2]),
      message: m[3].replace(/^determinism check failed:\s*/, "").trim(),
    });
  }
  return out;
}

test("the CLI is built, because these tests compare against it rather than skipping", () => {
  assert.ok(existsSync(CLI),
    `no syrinx CLI at ${CLI} — run \`make build\` (or set SYRINX_CLI). These tests compare the JS `
    + "check against the Rust one; skipping them would report success for a comparison never made.");
});

for (const [label, source] of Object.entries(FIXTURES)) {
  test(`check parity: ${label}`, () => {
    const wrapped = asModule(source);
    const mine = check(wrapped);
    const theirs = rustCheck(wrapped);

    assert.equal(mine.length, theirs.length,
      `different diagnostic counts\n  js:   ${JSON.stringify(mine)}\n  rust: ${JSON.stringify(theirs)}`);
    for (let i = 0; i < mine.length; i++) {
      assert.equal(mine[i].line, theirs[i].line, `line differs at ${i}: ${JSON.stringify([mine[i], theirs[i]])}`);
      assert.equal(mine[i].column, theirs[i].column, `column differs at ${i}: ${JSON.stringify([mine[i], theirs[i]])}`);
      assert.ok(theirs[i].message.includes(mine[i].message) || mine[i].message.includes(theirs[i].message),
        `message differs at ${i}:\n  js:   ${mine[i].message}\n  rust: ${theirs[i].message}`);
    }
  });
}

for (const [label, [source, expected]] of Object.entries(JS_ONLY)) {
  test(`check (js only, not comparable through the CLI): ${label}`, () => {
    assert.deepEqual(check(source).map((d) => [d.line, d.column]), expected);
  });
}
