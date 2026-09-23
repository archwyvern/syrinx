# The JavaScript host

Renders a `.syr` sound source without the native library, so an editor can preview a sound
in-process: no FFI, no built game, no platform binary.

```js
import { render, inspect, check, SYRINX_VERSION } from "syrinx";

const sound = await render({ path: "sfx/laser.syr", root: "/path/to/project" });
// { samples: Float32Array (interleaved), sampleRate, channels, frames, duration, loop,
//   name (undefined unless declared), seed, stem, stemNames, hasMix, dependencies, streaming, elapsedMs }
```

`inspect` reads a source's declarations without rendering it; `renderEach` renders its layers
separately; `mixFrom` runs only the mix stage over layers rendered earlier.

`root` is the directory imports may not escape. Passing `null` removes the jail; an editor
opening someone else's project should not.

A failure is a `SyrinxError` with `kind` (`check` | `compile` | `runtime` | `timeout` |
`contract` | `internal`), `message`, and `file` / `line` / `column` when the failure was located —
the same shape the C ABI reports, so a consumer can present both hosts identically.

## Other entry points

- `syrinx/browser` — the host for a web page, `open({ entry, math, run, prelude })` in a Web
  Worker; see the repository README.
- `syrinx/imports` — `scanImports(source)` and `rewriteImports(source, resolve)`: a module's static
  imports found (comments skipped) and rewritten in one splice, the way the Node host loads a
  module graph. A publisher that serves sources to a page rewrites them with this.
- `syrinx/check` — the determinism check on its own, and `strip`.

## The Rust library is the reference

`crates/syrinx-core` is what the game's content pipeline bakes with. This package exists only
because it produces the **same** sound, and that is tested rather than assumed:

- `test/identity.test.js` renders every example through both hosts and compares **raw floats**
  (never the 16-bit conversion, which would hide anything below about 3e-5), with a control that
  perturbs one sample by 1e-9 and asserts it is caught — so a passing run cannot mean a broken
  comparator.
- `test/check.test.js` runs the determinism check's fixtures through both implementations and
  compares line, column and message. A source one host accepts and the other rejects is a worse
  failure than a numeric difference, because it is silent until the other host runs.

Both compare against the built CLI and **fail rather than skip** when it is missing. `make test`
builds it first.

## Why the two hosts agree

Not because V8's libm happens to be stable — it is not: V8 12 is fdlibm, V8 15 routes most of
`Math` to LLVM's libc and `Math.tanh` to the platform C library. They agree because neither
host uses the engine's math. `prelude/math.js` replaces every implementation-defined `Math`
function with a port of fdlibm and freezes `Math`; the worker evaluates it before the module
graph is even read, and the Rust host does the same in its isolate. What is left is IEEE double
arithmetic, which every engine computes identically. `test/math.test.js` checks the port against
the C reference bit for bit, and `test/standard.test.js` checks that both hosts actually
installed it (the standard's `Math` is frozen and its `random` throws; V8's is neither).

Identity was also measured the old way before the standard math existed — Node 22 (V8 12.4)
and Electron 35 (V8 13.4) against rusty_v8 15.2, 406,080 samples, zero differing — which is
how it was learned that the agreement was luck rather than a property. Linux and Windows are
the targets; macOS is not one.

**To measure Windows** (the repo already cross-builds it, and no CI is required):

```sh
# On Linux: cross-build the Windows CLI. The shims in tools/xwin-bin are required — see their
# own comments for why cargo-xwin cannot use clang-cl and llvm-lib directly.
PATH="$PWD/tools/xwin-bin:$PATH" cargo xwin build --release --target x86_64-pc-windows-msvc
```

Then on the Windows machine, with this repository checked out and Node installed:

```
set SYRINX_CLI=C:\path\to\syrinx.exe
node --test test/*.test.js
```

That compares Windows-V8-in-Node against Windows-rusty_v8, which is the pair a Windows editor
would ship. Until it has been run, the honest statement is "identical where measured".

## Version pinning

The package version **is** the library version it was tested against, exported as
`SYRINX_VERSION`. A consumer pins the revision matching whatever bakes its sounds — the Rust
library, or an engine that embeds the standard's files — because a new standard (a changed
prelude, run wrapper or math) can change what an unchanged source renders to. Bumping one side
without the other is exactly the drift this package exists to rule out.
