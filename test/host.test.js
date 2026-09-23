// The host's own guarantees: the import jail, the timeout, and the module cache.
//
// These are not comparisons against the Rust host — they are properties this host must have on
// its own, and each one is here because its absence is invisible until it matters. A jail that
// does not hold looks exactly like a jail that does, until a source reads a file it should not.
// A timeout that only stops WAITING looks exactly like one that stops the work, until a runaway
// source pins a core for the life of the editor.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { cpSync, mkdtempSync, mkdirSync, writeFileSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { render, renderEach, inspect } from "../js/index.js";

const REPO = dirname(dirname(fileURLToPath(import.meta.url)));
const CLI = process.env.SYRINX_CLI ?? join(REPO, "target", "release", "syrinx");

/** The Rust host's verdict on a source: its stderr when it refuses, or null when it renders. */
function rustRefusal(path, extra = []) {
  try {
    execFileSync(CLI, ["compile", path, "-o", join(dirname(path), "out.f32"), ...extra], { stdio: ["ignore", "pipe", "pipe"] });
    return null;
  } catch (err) {
    return String(err.stderr);
  }
}

/** A project directory with the given files, plus the boilerplate a source needs. */
function project(files) {
  const root = mkdtempSync(join(tmpdir(), "syrinx-host-"));
  for (const [name, content] of Object.entries(files)) {
    const full = join(root, name);
    mkdirSync(join(full, ".."), { recursive: true });
    writeFileSync(full, content);
  }
  return root;
}

/** A project with the framework vendored at ./framework, as `syrinx framework` would leave it. */
function withFramework(files) {
  const root = project(files);
  cpSync(join(REPO, "framework"), join(root, "framework"), { recursive: true });
  return root;
}

const TONE = 'export const meta = { api: 4, name: "t", duration: 0.01, channels: 1, seed: 1 };\n'
  + "export const stems = { t: (ctx) => new Float32Array(ctx.frames) };\n";

test("a source renders and reports what it declared", async () => {
  const root = project({ "a.syr": TONE });
  const result = await render({ path: join(root, "a.syr"), root });

  assert.equal(result.name, "t");
  assert.equal(result.channels, 1);
  assert.equal(result.sampleRate, 48000);
  assert.equal(result.frames, 480);
  assert.equal(result.samples.length, 480);
  assert.equal(result.loop, false);
  assert.deepEqual(result.dependencies, []);
});

test("an explicit sample rate wins over the source's own", async () => {
  const root = project({ "a.syr": TONE });
  const result = await render({ path: join(root, "a.syr"), root, sampleRate: 44100 });
  assert.equal(result.sampleRate, 44100);
  assert.equal(result.frames, 441);
});

test("imports are reported, and a module imported twice is one instance", async () => {
  // The shared module counts its own evaluations. If the cache keyed on anything but the
  // canonical path — or did not exist — this renders 2 and the sound depends on how many
  // times a module happened to be reached.
  const root = project({
    "lib/shared.js": "let n = 0;\nn += 1;\nexport const evaluations = n;\n",
    "a.syr": 'import { evaluations } from "./lib/shared.js";\n'
      + 'import { evaluations as again } from "./lib/shared.js";\n'
      + 'export const meta = { api: 4, name: "t", duration: 0.01, channels: 1, seed: 1 };\n'
      + "export const stems = { t: (ctx) => new Float32Array(ctx.frames).fill(evaluations + again) };\n",
  });

  const result = await render({ path: join(root, "a.syr"), root });
  assert.equal(result.samples[0], 2, "each import should see the same single evaluation");
  assert.equal(result.dependencies.length, 1, "one file, however many times it is imported");
});

test("an import outside the root is refused, naming the root", async () => {
  const root = project({ "a.syr": 'import { x } from "../outside.js";\n' + TONE });
  writeFileSync(join(root, "..", "outside.js"), "export const x = 1;\n");

  await assert.rejects(
    () => render({ path: join(root, "a.syr"), root }),
    (err) => {
      assert.equal(err.kind, "compile");
      assert.match(err.message, /outside the project root/);
      return true;
    });
});

test("a symlink pointing out of the root is refused too", async () => {
  // The reason the jail canonicalises rather than testing the prefix of the written path: a
  // symlink INSIDE the root resolves to a file outside it, and a string comparison on the
  // specifier would happily let it through.
  const outside = mkdtempSync(join(tmpdir(), "syrinx-outside-"));
  writeFileSync(join(outside, "secret.js"), "export const x = 1;\n");
  const root = project({ "a.syr": 'import { x } from "./link.js";\n' + TONE });
  symlinkSync(join(outside, "secret.js"), join(root, "link.js"));

  await assert.rejects(
    () => render({ path: join(root, "a.syr"), root }),
    (err) => {
      assert.match(err.message, /outside the project root/);
      return true;
    });
});

test("a sibling directory sharing the root's name is outside it", async () => {
  // The jail is component-wise, not a string prefix. With a root of `<x>` an import resolving
  // into `<x>2` must be refused: a prefix test accepts it, the Rust host refuses it, and the
  // disagreement is a jail escape into any sibling whose name merely starts with the root's.
  const root = mkdtempSync(join(tmpdir(), "syrinx-jail-"));
  const sibling = root + "2";
  mkdirSync(sibling, { recursive: true });
  writeFileSync(join(sibling, "outside.js"), "export const x = 1;\n");
  writeFileSync(join(root, "a.syr"), `import { x } from "../${basename(sibling)}/outside.js";\n` + TONE);

  await assert.rejects(
    () => render({ path: join(root, "a.syr"), root }),
    (err) => {
      assert.match(err.message, /outside the project root/);
      return true;
    });
});

test("the root itself is inside the root", async () => {
  // The other half of the component-wise test: `resolved === root` must pass, or a source sitting
  // directly in the root would be refused by its own jail.
  const root = project({ "lib.js": "export const gain = 0.5;\n", "a.syr": 'import { gain } from "./lib.js";\n' + TONE });
  const result = await render({ path: join(root, "a.syr"), root });
  assert.equal(result.dependencies.length, 1);
});

test("a side-effect import is loaded, jailed and reported like any other", async () => {
  // `import "./x.js"` is a legal form the Rust host loads through V8's resolve callback. A
  // rewriter that only matched `from "..."` would leave it pointing at a path Node resolves
  // against a data: URL, so a valid source would render there and fail here.
  // The effect is observed through a shared module rather than a global, because `globalThis` is
  // banned by the determinism check — which is itself the right answer, and cost me a test.
  // Reading `seen.length` back proves side.js was EVALUATED, not merely resolved.
  const root = project({
    "state.js": "export const seen = [];\n",
    "side.js": 'import { seen } from "./state.js";\nseen.push(1);\n',
    "a.syr": 'import "./side.js";\n'
      + 'import { seen } from "./state.js";\n'
      + 'export const meta = { api: 4, name: "t", duration: 0.01, channels: 1, seed: 1 };\n'
      + "export const stems = { t: (ctx) => new Float32Array(ctx.frames).fill(seen.length) };\n",
  });

  const result = await render({ path: join(root, "a.syr"), root });
  assert.equal(result.frames, 480);
  assert.equal(result.samples[0], 1, "the side-effect module should have run exactly once");
  assert.equal(result.dependencies.length, 2);
  assert.ok(result.dependencies.some((d) => d.endsWith("side.js")));
});

test("a side-effect import cannot escape the jail either", async () => {
  const root = project({ "a.syr": 'import "../outside.js";\n' + TONE });
  writeFileSync(join(root, "..", "outside.js"), "export const x = 1;\n");
  await assert.rejects(
    () => render({ path: join(root, "a.syr"), root }),
    (err) => {
      assert.match(err.message, /outside the project root/);
      return true;
    });
});

test("a bare specifier other than syrinx is refused", async () => {
  const root = project({ "a.syr": 'import fs from "node:fs";\n' + TONE });
  await assert.rejects(
    () => render({ path: join(root, "a.syr"), root }),
    (err) => {
      assert.equal(err.kind, "compile");
      assert.equal(err.message, 'cannot import "node:fs": only "syrinx" and relative paths can be imported');
      return true;
    });
});

test("an import that cannot resolve fails the same way on both hosts, naming the importer", async () => {
  for (const [line, message] of [
    ['import x from "lodash";', 'cannot import "lodash": only "syrinx" and relative paths can be imported'],
    ['import x from "./nope.js";', 'cannot import "./nope.js": no such file'],
    ['import "./lib/missing.js";', 'cannot import "./lib/missing.js": no such file'],
  ]) {
    const root = project({ "a.syr": `${line}\n${TONE}` });
    await assert.rejects(() => render({ path: join(root, "a.syr"), root }), (err) => {
      assert.equal(err.kind, "compile");
      assert.equal(err.message, message);
      assert.ok(err.file.endsWith("a.syr"), `the error names the importing module: ${err.file}`);
      return true;
    });
    assert.match(rustRefusal(join(root, "a.syr")) ?? "", new RegExp(`compile error: ${message.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`));
  }
});

test("a framework name taken from the core is a compile error naming it, on both hosts", async () => {
  // The mistake every 0.4 source makes on 0.9. The Rust host's V8 names the module as the source
  // wrote it; the Node host maps its own URL for the prelude back to "syrinx" so the two agree.
  const root = project({ "a.syr": 'import { Osc } from "syrinx";\n' + TONE });
  const message = "The requested module 'syrinx' does not provide an export named 'Osc'";
  await assert.rejects(() => render({ path: join(root, "a.syr"), root }), (err) => {
    assert.equal(err.kind, "compile");
    assert.match(err.message, new RegExp(message));
    return true;
  });
  assert.match(rustRefusal(join(root, "a.syr")) ?? "", new RegExp(message));
});

test("a source that never finishes is stopped, not merely abandoned", async () => {
  // The point of the worker. A promise race would resolve this rejection just the same while the
  // loop kept running; `worker.terminate()` is what actually stops it, and the process exiting
  // cleanly after this test is the evidence.
  const root = project({
    "spin.syr": 'export const meta = { api: 4, name: "spin", duration: 0.01, channels: 1, seed: 1 };\n'
      + "export const stems = { spin: (ctx) => { for (;;) {} } };\n",
  });

  const started = Date.now();
  await assert.rejects(
    () => render({ path: join(root, "spin.syr"), root, timeoutMs: 250 }),
    (err) => {
      assert.equal(err.kind, "timeout");
      assert.match(err.message, /exceeded 250 ms/);
      return true;
    });
  assert.ok(Date.now() - started < 5000, "the timeout should fire promptly, not wait for the render");
});

test("a source with no layers is a contract error naming the fix", async () => {
  const root = project({
    "a.syr": 'export const meta = { api: 4, name: "t", duration: 0.01, channels: 1, seed: 1 };\n',
  });
  await assert.rejects(
    () => render({ path: join(root, "a.syr"), root }),
    (err) => {
      assert.equal(err.kind, "contract");
      assert.match(err.message, /has no `stems` export/);
      assert.match(err.message, /export const stems = \{ name\(ctx\)/);
      return true;
    });
});

test("a source on the old contract is told what changed, not merely refused", async () => {
  // The break's whole error path: a default export and nothing else used to BE a sound.
  const root = project({
    "a.syr": 'export const meta = { api: 4, name: "t", duration: 0.01 };\n'
      + "export default (ctx) => new Float32Array(ctx.frames);\n",
  });
  await assert.rejects(
    () => render({ path: join(root, "a.syr"), root }),
    (err) => {
      assert.match(err.message, /one or more named layers/);
      return true;
    });
});

test("a source that throws reports the throw, positioned at the source", async () => {
  const root = project({
    "a.syr": 'export const meta = { api: 4, name: "t", duration: 0.01, channels: 1, seed: 1 };\n'
      + 'export const stems = { t: () => { throw new Error("boom"); } };\n',
  });
  await assert.rejects(
    () => render({ path: join(root, "a.syr"), root }),
    (err) => {
      assert.equal(err.kind, "runtime");
      assert.match(err.message, /boom/);
      return true;
    });
});

// ------------------------------------------------------------------------------- streams

test("a layer that streams renders block by block to what render() gives", async () => {
  // The same stateful per-sample function through render() and through stream(): the block
  // driver in the worker must pull the same blocks in the same order as the Rust host, and both
  // must equal the whole render.
  const root = withFramework({
    "a.syr": 'import { Osc, Biquad, render, stream } from "./framework/dsp.js";\n'
      + 'export const meta = { api: 4, name: "t", duration: 0.3, channels: 1, seed: 1 };\n'
      + "const voice = (ctx) => { const o = Osc.saw(ctx.sr); const f = Biquad.lowpass(ctx.sr, 900, 2); return (t) => f.process(o.next(110 + 40 * t)); };\n"
      + "export const stems = { whole: (ctx) => render(ctx, voice(ctx)), streamed: (ctx) => stream(ctx, voice(ctx)) };\n",
  });
  const layers = await renderEach({ path: join(root, "a.syr"), root });
  assert.equal(layers[0].streaming, false);
  assert.equal(layers[1].streaming, true);
  assert.deepEqual(layers[1].samples, layers[0].samples);
  assert.ok(layers[0].samples.some((v) => v !== 0), "the probe must produce a signal");
});

test("the mix stage is classified by the probe, the same way as the Rust host", async () => {
  // The four layer/mix forms, each summed to the same arithmetic; `streaming` says which form the
  // host saw, and a mix that read ctx.stems inside try/catch is still whole-form.
  const bufferLayer = "a(ctx) { return new Float32Array(ctx.frames).fill(0.25); }";
  const streamLayer = "a(ctx) { return (offset, frames) => new Float32Array(frames).fill(0.25); }";
  const wholeMix = "export default function (ctx) { return ctx.stems.a[0].map((v) => v * 2); }";
  const streamMix = "export default function (ctx) { return (offset, frames, { a }) => a[0].map((v) => v * 2); }";
  const swallowingMix = "export default function (ctx) { let a; try { a = ctx.stems.a; } catch (e) { a = [new Float32Array(ctx.frames).fill(-1)]; } return a[0].map((v) => v * 2); }";
  const cases = [
    [bufferLayer, wholeMix, false],
    [bufferLayer, streamMix, true],
    [streamLayer, streamMix, true],
    [streamLayer, wholeMix, false],
    [bufferLayer, swallowingMix, false],
  ];
  for (const [layer, mix, streaming] of cases) {
    const root = project({
      "a.syr": 'export const meta = { api: 4, name: "t", duration: 0.2, channels: 1, seed: 1 };\n'
        + `export const stems = { ${layer} };\n${mix}\n`,
    });
    const r = await render({ path: join(root, "a.syr"), root });
    assert.equal(r.samples[0], 0.5, `${layer} + ${mix}`);
    assert.equal(r.samples[r.samples.length - 1], 0.5);
    assert.equal(r.streaming, streaming, `${layer} + ${mix}`);
  }
});

test("a mix that reads ctx.stems and returns a stream names the mistake, on both hosts", async () => {
  const root = project({
    "a.syr": 'export const meta = { api: 4, name: "t", duration: 0.1, channels: 1, seed: 1 };\n'
      + "export const stems = { a(ctx) { return new Float32Array(ctx.frames); } };\n"
      + "export default function (ctx) { let p; try { p = ctx.stems.a[0][0]; } catch (e) {} return (offset, frames, { a }) => a[0]; }\n",
  });
  await assert.rejects(() => render({ path: join(root, "a.syr"), root }), (err) => {
    assert.match(err.message, /read ctx\.stems and returned a stream/);
    return true;
  });
  const refusal = rustRefusal(join(root, "a.syr"));
  assert.ok(refusal !== null, "the Rust host must refuse it too");
  assert.match(refusal, /read ctx\.stems and returned a stream/);
});

test("the whole-render helpers refuse inside a block, on both hosts", async () => {
  for (const [call, fix] of [["normalize(out)", /limiter/], ["fade(ctx, out, 0.01, 0.01)", /Env\.gate/], ["place(ctx, out, 0)", /- offset/]]) {
    const root = withFramework({
      "a.syr": 'import { normalize, fade, place } from "./framework/dsp.js";\n'
        + 'export const meta = { api: 4, name: "t", duration: 0.1, channels: 1, seed: 1 };\n'
        + `export const stems = { a(ctx) { return (offset, frames) => { const out = new Float32Array(frames); return ${call}; }; } };\n`,
    });
    await assert.rejects(() => render({ path: join(root, "a.syr"), root }), (err) => {
      assert.equal(err.kind, "runtime");
      assert.match(err.message, /cannot run inside a stream/);
      assert.match(err.message, fix);
      return true;
    });
    const refusal = rustRefusal(join(root, "a.syr"));
    assert.ok(refusal !== null, `the Rust host must refuse ${call} too`);
    assert.match(refusal, /cannot run inside a stream/);
  }
});

test("a block of the wrong length is refused, on both hosts", async () => {
  const root = project({
    "a.syr": 'export const meta = { api: 4, name: "t", duration: 0.1, channels: 1, seed: 1 };\n'
      + "export const stems = { a(ctx) { return (offset, frames) => new Float32Array(frames + 1); } };\n",
  });
  await assert.rejects(() => render({ path: join(root, "a.syr"), root }), (err) => {
    assert.match(err.message, /returned 4097 samples for a 4096-frame block/);
    return true;
  });
  assert.match(rustRefusal(join(root, "a.syr")) ?? "", /returned 4097 samples for a 4096-frame block/);
});

test("a subset sums in declaration order whatever order was asked", async () => {
  // 2^-24 is half an ulp of 1.0: added one at a time to 1.0 each vanishes, added to each other
  // first they do not. Declaration order is big, tiny1, tiny2, so the sum is exactly 1.0 either
  // way the subset is written.
  const root = project({
    "a.syr": 'export const meta = { api: 4, name: "t", duration: 0.01, channels: 1, seed: 1 };\n'
      + "export const stems = {\n"
      + "  big(ctx) { return new Float32Array(ctx.frames).fill(1); },\n"
      + "  tiny1(ctx) { return new Float32Array(ctx.frames).fill(Math.pow(2, -24)); },\n"
      + "  tiny2(ctx) { return new Float32Array(ctx.frames).fill(Math.pow(2, -24)); },\n"
      + "};\n",
  });
  const forward = await render({ path: join(root, "a.syr"), root, stems: ["big", "tiny1", "tiny2"] });
  const backward = await render({ path: join(root, "a.syr"), root, stems: ["tiny2", "tiny1", "big"] });
  assert.equal(forward.samples[0], 1.0);
  assert.deepEqual(backward.samples, forward.samples);
  await assert.rejects(() => render({ path: join(root, "a.syr"), root, stems: ["big", "big"] }), /selected twice/);
});

test("meta.api 4 is accepted and anything else refused, as the Rust host does", async () => {
  for (const [api, ok] of [[4, true], [3, false], [5, false], [1, false]]) {
    const root = project({
      "a.syr": `export const meta = { api: ${api}, name: "t", duration: 0.01, channels: 1, seed: 1 };\n`
        + "export const stems = { t: (ctx) => new Float32Array(ctx.frames) };\n",
    });
    if (ok) {
      await render({ path: join(root, "a.syr"), root });
      assert.equal(rustRefusal(join(root, "a.syr")), null, `api ${api} must render on the Rust host`);
    } else {
      await assert.rejects(() => render({ path: join(root, "a.syr"), root }), (err) => {
        assert.equal(err.kind, "contract");
        assert.match(err.message, /accepts 4 to 4/);
        return true;
      });
      assert.match(rustRefusal(join(root, "a.syr")) ?? "", /accepts 4 to 4/);
    }
  }
});
