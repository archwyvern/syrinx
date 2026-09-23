# The syrinx standard

What a source is, what a host must do with it, and how to tell whether an implementation got it
right.

This document is normative. A requirement is marked **Must.**, a prohibition **May not.**, and an
explanation that is not itself a requirement **Note.** Everything else is commentary. The API
reference it cites is generated from the declarations (`prelude/syrinx.d.ts` for the core and the
framework's own) into [`docs/API.md`](docs/API.md) and `docs/syrinx-docs.json`; nothing in it is
written by hand. This text is the standard syrinx 1.0.0 implements: contract version 4.

<a id="scope"></a>

## 1. Scope

syrinx defines how a JavaScript module becomes audio. A source declares how long it is and what
layers it has; a host runs it and writes samples. The same source, run by any conforming host, on
any machine, produces the same bytes.

That last property is the whole point, and it is what makes this a standard rather than a
description of one program. A sound can live in a repository as code, be reviewed as code, and be
baked by a content pipeline like any other asset, because rebuilding it cannot change it.

> **Note.** This document specifies the source language and the host contract. It does not specify
> how a host is written, what it is written in, or what it does with the samples once it has them.

<a id="conformance"></a>

## 2. Conformance

A conforming host renders every valid source to exactly the samples the reference implementation
renders. Not equivalent audio, not audio within a tolerance — the same IEEE-754 values, in the same
order.

> **Must.** A host conforms when its output, compared against the reference implementation's as raw
> 32-bit floats, differs in zero samples.

Comparison is made on raw floats and never on a quantised form. A 16-bit conversion hides any
difference below roughly 3×10<sup>-5</sup>, which is ample room for two hosts to disagree while
appearing to agree.

The Rust host is the reference, and it generates the API reference this document cites. The
JavaScript hosts -- one for Node, one for a web page -- share one reading of the contract. They are
compared against the reference automatically rather than assumed to match: every example, layer by
layer, raw float for raw float, and every contract rejection, message for message.

Separate implementations are what make the agreement mean anything. Hosts that shared code would
demonstrate only that the code is deterministic; these share nothing but the standard's own files —
the math, the run wrapper and the prelude.

<a id="source"></a>

## 3. The source module

A source is an ES module. It imports the core from `"syrinx"`, exports `meta` and `stems`, and
may default-export a function that combines the layers into the finished sound. Each entry of
`stems` is a layer.

```js title="a complete source"
import { Osc, Env, render } from "./framework/dsp.js";

export const meta = { api: 4, name: "ping", duration: 0.4, channels: 1, seed: 3 };

export const stems = {
  body(ctx) {
    const osc = Osc.sine(ctx.sr);
    const env = Env.ad(0.001, 0.35);
    return render(ctx, (t) => osc.next(880) * env(t));
  },
};
```

The oscillator, the envelope and `render` come from the framework, which the project keeps beside
its sources (clause 13); the core is what `"syrinx"` itself provides (clause 12).

`meta` is an object. Every field is optional except `api` and `duration`, and a field set to
`undefined` is absent:

| field | type | rule |
|---|---|---|
| `api` | number | required: the contract version the source was written against, 4; see clause 11 |
| `name` | string | what the sound is called. There is no default: a host reports an absent name as absent |
| `duration` | number | seconds, in (0, 600] |
| `channels` | number | 1 or 2, default 1; a mono return is duplicated when it is 2 |
| `sampleRate` | number | an integer in [8000, 192000], default 48000; a host may override it |
| `seed` | number | an integer in [0, 4294967295], default 0; handed to every layer and to the mix |
| `loop` | boolean | default false; whether the sound is meant to loop. It changes no sample |

> **Must.** A host rejects a field of the wrong type or out of range as a `contract` error, checking
> the fields in the order of this table and reporting the reference's message.

> **Must.** A seed is what the source wrote or it is refused: `meta.seed must be a number` when it is
> not a number, `meta.seed must be an integer in [0, 4294967295]` when it is a number outside that
> range or not an integer. No host converts one; a conversion is a rule every host would have to
> reproduce exactly, for a seed nobody meant.

> **Note.** A tool that needs to call a nameless sound something -- the reference CLI prints the file
> name -- chooses that label itself. It is not part of the source's contract.

> **Must.** Layer names match `/^[A-Za-z][A-Za-z0-9_.-]{0,63}$/` and keep their declaration order. A
> host reports a malformed name before it reports that the value is not a function.

Layers exist so that a sound can be rendered in pieces — one layer at a time for inspection, or all
of them for the mix. A layer's audio does not depend on which other layers ran, or in what order:
each is evaluated in its own realm with its own module graph.

<a id="context"></a>

## 4. Contexts

Every layer and the mix receive a context. It carries the sample rate (`sr`), the exact frame count
to produce (`frames`), the declared `duration`, the `seed` and the `channels` count; a layer's
context also carries its own name (`stem`).

> **Must.** `ctx.frames` is `round(duration * sr)`. A layer produces exactly that many frames per
> plane.

`ctx.seed` is `meta.seed`, identical for every layer. A layer that needs its own stream of
randomness derives one — `hash(ctx.seed, "kick")` — rather than mutating a shared generator, so that
rendering one layer alone gives the same result as rendering it alongside the others.

The mix context additionally exposes `ctx.stems`, the rendered layers by name. Reading it is what
distinguishes a whole-buffer mix from a streaming one; see clause 5.

<a id="returns"></a>

## 5. Return values and the block protocol

A layer returns its samples whole, or returns a function that the host pulls one block at a time.
Both forms produce the same audio; streaming exists so that playback can begin before the render has
finished.

```js title="the two forms"
// whole
(ctx) => Float32Array | number[] | [left, right]

// streaming: called once per block, in order, from offset 0
(offset, frames) => Float32Array | number[] | [left, right]
```

> **Must.** A stream is called once per block of `BLOCK_FRAMES` (4096), in order, starting at offset
> 0. Only the last block is shorter. It returns exactly `frames` samples per plane.

A whole return is fitted to `ctx.frames` — zero-padded or truncated. A block's length is not: it
must match exactly. Padding a block would quietly conceal an inclusive bound or a fixed-size scratch
buffer and render something anyway, so a host rejects it instead.

> **Must.** The driver refuses an offset out of sequence. A host that pulls the wrong block fails
> loudly rather than producing audio.

State kept in a stream's closure persists between blocks — that is how oscillators, filters and
phases survive the block boundary. Consequently a block handed back to the host may be the stream's
own scratch buffer, so a host copies a block out before pulling the next one.

The mix stage is classified by probe rather than by declaration. The default export is called once
with no buffers, and `ctx.stems` is replaced by getters that record any read and then throw. If it
read the layers, it is a whole-buffer mix and is called again with real planes. If it read nothing
and returned a function, it is a mix stream.

> **Must.** Classification is decided on the record of the read, never on the throw propagating. A
> mix that wrapped its reads in `try`/`catch` would otherwise swallow the throw, return a buffer, and
> be mistaken for a whole-buffer mix.

> **May not.** A mix stream may not read `ctx.stems`. Its layers arrive as the third argument,
> `(offset, frames, stems)`, where `stems[name]` is that layer's block.

Code that needs the entire render -- the framework's `normalize`, `fade` and `place`, say -- asks
the core's `inBlock()` and refuses inside a block. The flag behind it is frozen and its getter
closes over a variable private to the run wrapper, so a source can read it but never set it.

<a id="math"></a>

## 6. The standard math

ECMAScript leaves the transcendental functions implementation-defined. Two engines, or two versions
of one engine, may each legitimately return results that differ in the last bit. Fed through an IIR
filter, a last-bit difference becomes a different waveform.

So syrinx does not use the engine's mathematics. The standard supplies a port of fdlibm — Sun's
freely distributable libm, as vendored by V8 through version 12 — and freezes `Math`. What remains is
IEEE-754 add, subtract, multiply, divide and square root, which every engine on every platform
computes identically.

> **Must.** Every host evaluates the standard math, once per isolate, before the prelude and before
> any source module. It is the first file of the standard for that reason.

Functions exact by specification — `abs`, `ceil`, `floor`, `fround`, `max`, `min`, `round`, `sign`,
`sqrt`, `trunc` — are left alone. Everything else is replaced. `Math.random` throws.

> **Note.** The first two hosts were once measured as agreeing without this file — Node 22 against
> rusty_v8 15.2, 406,080 samples, zero differing. That agreement was luck rather than a property,
> and finding out it was luck is why the standard math exists.

<a id="determinism"></a>

## 7. Determinism

A source that reads the clock, the process or any source of entropy cannot render the same bytes
twice. Such sources are rejected before they reach the engine, with a located error.

> **May not.** These names are not available to a source:
>
> - `Math.random` — unseeded randomness; use `new Random(seed)`
> - `Date`, `performance` — wall clock
> - `setTimeout`, `setInterval`, `queueMicrotask` — timers and scheduling do not exist in a sound source
> - `eval`, `Function`, `globalThis` — dynamic code defeats the check
> - `Intl` — locale-dependent
> - `crypto` — entropy
> - `require` — use `import`
> - `fetch`, `console` — no I/O in a sound source

> **May not.** `**` is rejected. Exponentiation uses the engine's own `pow` rather than the
> standard's; write `Math.pow(x, y)`.

> **Note.** The check is a lexical scan, not a parser. Comments and string literals are skipped so
> that a banned name in prose does not trip it, but a determined author can still smuggle one
> through — `globalThis["Ma"+"th"]`. Its purpose is to catch honest mistakes early. It is not a
> sandbox, and a host that runs untrusted sources needs one.

<a id="resolution"></a>

## 8. Module resolution

> **Must.** Only `"syrinx"` and relative paths are importable. Bare specifiers are rejected.

A host links each module to the rewritten form of its imports rather than to the file on disk.
Handing a source to the ambient module resolver would let `from "syrinx"` resolve to whatever
package of that name happened to be reachable, silently giving the source a host instead of the
prelude.

> **Must.** Modules are keyed by canonical path: a module imported twice is one instance. This is
> observable, because a module holding state would otherwise render differently depending on how
> many paths reached it.

> **Must.** An import that does not resolve is a `compile` error whose file is the importing module,
> with the message for the way it failed: `cannot import "S": only "syrinx" and relative paths can
> be imported`, `cannot import "S": no such file`, `cannot import "S": R is outside the project root
> ROOT`, `cannot import "S": R cannot be read`, or, for any other failure to resolve the path,
> `cannot import "S": it cannot be resolved` -- where `S` is the specifier as written and `R` and
> `ROOT` are canonical paths.

> **May not.** Dynamic `import()` is not supported. It is a limitation of the standard rather than
> of any one host — no host installs a dynamic-import callback.

A host may confine resolution to a root directory, so that imports cannot escape the project. A host
opening a source it did not write should.

<a id="numeric"></a>

## 9. The numeric model

The division of labour between the standard and a host is one rule:

> **Must.** Arithmetic happens in JavaScript. Permutations may happen anywhere.

Summing layers in Rust, or C#, or any host language would accumulate in a different type than
JavaScript's doubles, and the mix would differ by host. Interleaving planes, chopping a plane into
blocks and copying a block out are rearrangements of values already decided, so a host may do those
itself.

Within a source, floating-point arithmetic is not associative, and the standard does not pretend
otherwise. Rewriting `a * 0.28 * vel` as `a * (0.28 * vel)` changes the render. Evaluation order is
part of the sound.

<a id="errors"></a>

## 10. Errors

A failure is reported with a kind, a message, and — when the failure could be located — a file,
line and column. Every host reports the same six kinds so that a consumer can present both
identically.

| kind | code | meaning |
|---|---|---|
| `check` | 1 | the static determinism check rejected the source |
| `compile` | 2 | the module graph could not be built: a syntax error, an import that does not resolve, or an import naming an export its module lacks |
| `runtime` | 3 | the source threw |
| `timeout` | 4 | the source ran past its time budget and was killed |
| `contract` | 5 | bad meta or bad return value |
| `internal` | 6 | failure inside the host |

> **Must.** A source that one host accepts and another rejects is a worse failure than a numeric
> difference, because it stays silent until the other host runs. Hosts agree on diagnostics — kind,
> message, line and column — not merely on samples.

> **Note.** One message is the engine's rather than the standard's: a link error, an import naming
> an export its module does not have. Hosts on different engines may word it differently, and the
> JavaScript host names the prelude `"syrinx"` in it as the reference does; the kind agrees.

<a id="versioning"></a>

## 11. Versioning

A source declares `meta.api`, the contract version it was written against. The field is required,
because a source that says which contract it was written against cannot be read as another by a
later host. A host accepts a range, from `API_FLOOR` to `PRELUDE_VERSION`, and the check is binary:
in range it compiles, out of range it fails with a contract error naming both.

syrinx 1.0.0 implements contract 4 and accepts 4 alone: 1.0 moved everything that was not the
standard out of the prelude and broke with every earlier contract. The range widens again only for
an additive change, so that a source written against one contract stays valid under the next.

> **Must.** A source without `meta.api` is rejected with `meta.api is required: this compiler
> provides api P and accepts F to P`, and one outside the range with `source declares meta.api V but
> this compiler provides api P and accepts F to P`.

`syrinx info` reports the version of the implementation, the prelude's contract version
(`PRELUDE_VERSION`), the range of `meta.api` it accepts, `BLOCK_FRAMES` and the engine. The reference
implementation is released as tags, `vMAJOR.MINOR.PATCH`, and this document is versioned with it:
the text at a tag is the standard that tag implements.

Nothing here says whether a source still renders the same bytes as it did yesterday, and nothing
tries to. A source that imports a module is affected by changes to that module; that is what
importing means, and it is the author's business, not the standard's.

`BLOCK_FRAMES` is 4096 and is a constant of the standard, not a host setting.

<a id="core-api"></a>

## 12. The core module

The core is what a conforming host must know about: the contract it implements, the constants it
honours, the seeded randomness the determinism check names, and the one question the block protocol
lets a source ask. `"syrinx"` is the prelude, `prelude/prelude.js`, and it exports exactly
`PRELUDE_VERSION`, `BLOCK_FRAMES`, `Random`, `hash` and `inBlock`; `prelude/syrinx.d.ts` declares
them with the contract's types, listed under "The core module" in [`docs/API.md`](docs/API.md).

> **Must.** A host's `"syrinx"` exports exactly the core, and `inBlock()` is true exactly while the
> host is computing one block of a stream -- false during a layer's setup and for a whole layer.

<!-- reference: core -->

<a id="framework"></a>

## 13. The framework

Everything else a sound is made of -- oscillators, envelopes, filters, delays, the buffer helpers,
the arrangement engine, effects and instruments -- is not part of this standard. It is the
framework, `framework/` in the reference implementation's repository: a library a project chooses,
the way it chooses any library. Its primitives are listed under "The framework" in
[`docs/API.md`](docs/API.md).

> **Must.** A host never ships the framework and never evaluates it. Nothing in this section is
> required of a conforming implementation.

The reason for the line is what a host must know. A host implements the contract, drives the block
protocol and honours the whole-render helpers. It never needs to know that `Biquad` exists. Anything
on the far side of that question is somebody's library, and a standard that carries a library
carries it forever.

The framework is developed in the same repository as the reference host — they move together and
are tested together — but it is its own package (`syrinx-framework`, versioned with the repository),
distributed on its own: `syrinx framework <dir>` writes a copy into a project, with a `VERSION` file
naming the release it came from. A source that calls the framework depends on it as firmly as on the
standard, and that dependency is tracked the same way everything else is: by what the source
renders to, not by a number either side declares.

A framework function, once released, never changes what it renders. A better voice is a new name
beside the old one, so that updating a project's copy can add sound but never alter a sound
someone approved.

> **Must.** An album project imports the framework by relative path, because clause 8 admits only
> `"syrinx"` and relative specifiers. The framework therefore lives under the project root like any
> other source of the project.

> **Note.** That constraint is deliberate and worth keeping. Admitting bare specifiers would pull a
> package-resolution algorithm into the standard — one that Node, a browser and a .NET host would
> each have to agree on exactly, having inherited three different answers already. Of everything a
> fourth implementation could get wrong, resolution is the likeliest, and relative paths cost one
> directory to avoid it entirely.

> **Note.** The boundary is pinned by the reference implementation's tests: the names `"syrinx"`
> exports, and the names the framework's `dsp.js` exports, are each a list a change to which is a
> change to the standard, made on purpose.

Bit-exactness is not relaxed here. A framework function is as binding as a core one for any source
that calls it. The difference is only whose problem it is to ship.

<!-- reference: framework -->

<a id="testing"></a>

## 14. Checking a host

A second implementation is worth having only if it produces the same sound, and that is measured
rather than assumed. Render the same source through both hosts and compare the raw floats.

> **Must.** Compare raw floats, never a quantised form, and include a control that perturbs one
> sample and asserts the comparison catches it — otherwise a passing run cannot be distinguished
> from a broken comparator.

```text title="what a check reports"
samples compared : 19200
differing        : 0
largest delta    : 0
IDENTICAL (raw float, bit for bit)
control (1e-9 perturbation detected): yes
```

Hosts should agree on rejections too. A source that one accepts and another rejects is a worse
failure than a numeric difference, because it stays silent until the other host runs.

> **Note.** Check real sources, not a toy. A one-line example exercises almost none of the standard,
> and divergence hides in the modules a small example never imports.
