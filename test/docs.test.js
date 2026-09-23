// The committed docs/API.md is what tools/api-md.mjs makes of the committed docs/syrinx-docs.json.
// (That the JSON itself is what the compiler emits is crates/syrinx-core/tests/docs.rs.)

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { apiMarkdown } from "../tools/api-md.mjs";

const REPO = dirname(dirname(fileURLToPath(import.meta.url)));

test("docs/API.md is current", () => {
  const docs = JSON.parse(readFileSync(join(REPO, "docs", "syrinx-docs.json"), "utf8"));
  const committed = readFileSync(join(REPO, "docs", "API.md"), "utf8");
  assert.equal(committed, apiMarkdown(docs), "docs/API.md is stale: run `make docs`");
});

test("SPEC.md names every reference section it cites", () => {
  const spec = readFileSync(join(REPO, "SPEC.md"), "utf8");
  for (const marker of ["<!-- reference: core -->", "<!-- reference: framework -->"]) {
    assert.ok(spec.includes(marker), `SPEC.md has lost ${marker}`);
  }
});
