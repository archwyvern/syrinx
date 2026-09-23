// syrinx/imports: what counts as an import, and a rewrite that touches only the specifiers.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { rewriteImports, scanImports } from "../js/imports.js";

test("both static forms are found; comments are not imports and strings are kept", () => {
  const src = [
    '// import x from "commented"',
    'import { a } from "syrinx";',
    'import "./side.js";',
    "export { b } from './b.js';",
    'const s = "from \\"quoted\\"";',
    "/* import y from \"blocked\" */",
    "",
  ].join("\n");
  assert.deepEqual(scanImports(src).map((i) => i.specifier), ["syrinx", "./side.js", "./b.js"]);
});

test("a rewrite splices each import once, in one pass", () => {
  const src = 'import { a } from "./a.js";\nimport { b } from "./a.js.js";\n';
  const out = rewriteImports(src, (s) => (s === "./a.js" ? "./a.js.js" : "/x.js"));
  assert.equal(out, 'import { a } from "./a.js.js";\nimport { b } from "/x.js";\n');
});

test("an astral character before an import does not shift the splice", () => {
  const src = "// \u{1F3B5} a note\nimport { a } from \"syrinx\";\n";
  assert.equal(rewriteImports(src, () => "/p.js"), "// \u{1F3B5} a note\nimport { a } from \"/p.js\";\n");
});

test("everything but the specifiers is left exactly as it was", () => {
  const src = "import {\n  a,\n  b,\n} from   './m.js'; // trailing\nconst x = 1;\n";
  assert.equal(rewriteImports(src, (s) => s), src.replace("'./m.js'", '"./m.js"'));
});
