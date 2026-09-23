// The framework as a project gets it: every module passes the determinism check, imports only the
// core and its own siblings, loads under the host, and is byte for byte what the CLI vendors.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { check } from "../js/check.js";
import { scanImports } from "../js/imports.js";
import { inspect } from "../js/index.js";

const REPO = dirname(dirname(fileURLToPath(import.meta.url)));
const FRAMEWORK = join(REPO, "framework");
const CLI = process.env.SYRINX_CLI ?? join(REPO, "target", "release", "syrinx");

function walk(dir) {
  return readdirSync(dir).flatMap((f) => {
    const p = join(dir, f);
    return statSync(p).isDirectory() ? walk(p) : [p];
  });
}
const files = walk(FRAMEWORK).sort();
const modules = files.filter((f) => f.endsWith(".js"));

test("the framework has the modules its README maps", () => {
  const names = modules.map((m) => relative(FRAMEWORK, m).split(sep).join("/"));
  for (const expected of ["dsp.js", "music.js", "fx.js", "master.js", "instruments/organic.js", "voice/vocal.js", "sample/machine.js"]) {
    assert.ok(names.includes(expected), `framework/${expected} is missing`);
  }
  const readme = readFileSync(join(FRAMEWORK, "README.md"), "utf8");
  for (const name of names) assert.ok(readme.includes(`\`${name}\``), `README.md does not map ${name}`);
});

test("every module passes the determinism check", () => {
  for (const m of modules) assert.deepEqual(check(readFileSync(m, "utf8")), [], relative(REPO, m));
});

test("every module imports the core or a sibling, and nothing else", () => {
  for (const m of modules) {
    for (const { specifier } of scanImports(readFileSync(m, "utf8"))) {
      if (specifier === "syrinx") continue;
      assert.ok(specifier.startsWith("./") || specifier.startsWith("../"), `${relative(REPO, m)} imports ${specifier}`);
      const target = resolve(dirname(m), specifier);
      assert.ok(target.startsWith(FRAMEWORK + sep), `${relative(REPO, m)} reaches outside the framework: ${specifier}`);
      assert.ok(modules.includes(target), `${relative(REPO, m)}: ${specifier} does not exist`);
    }
  }
});

test("every module loads under the host, as a vendored copy", async () => {
  const root = mkdtempSync(join(tmpdir(), "syrinx-framework-"));
  cpSync(FRAMEWORK, join(root, "framework"), { recursive: true });
  const specs = modules.map((m) => "./framework/" + relative(FRAMEWORK, m).split(sep).join("/"));
  const imports = specs.map((s, i) => `import * as m${i} from ${JSON.stringify(s)};`).join("\n");
  writeFileSync(join(root, "all.syr"),
    `${imports}\nconst loaded = [${specs.map((_, i) => `m${i}`).join(", ")}].length;\n` +
    "export const meta = { api: 4, duration: 0.01 };\n" +
    "export const stems = { a: (ctx) => new Float32Array(ctx.frames).fill(loaded) };\n");
  const info = await inspect({ path: join(root, "all.syr"), root });
  assert.equal(info.dependencies.length, modules.length, "every module, once");
});

test("the CLI vendors exactly this text, and stamps the release", () => {
  assert.ok(existsSync(CLI), `no syrinx CLI at ${CLI} — run \`make build\` (or set SYRINX_CLI)`);
  const out = mkdtempSync(join(tmpdir(), "syrinx-vendored-"));
  execFileSync(CLI, ["framework", out], { stdio: "ignore" });
  for (const f of files) assert.equal(readFileSync(join(out, relative(FRAMEWORK, f)), "utf8"), readFileSync(f, "utf8"), relative(REPO, f));
  const version = JSON.parse(readFileSync(join(REPO, "package.json"), "utf8")).version;
  assert.equal(readFileSync(join(out, "VERSION"), "utf8"), `syrinx-framework ${version}\n`);
});
