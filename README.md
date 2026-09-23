<p align="center"><img src="logo/syrinx-wordmark.svg" alt="syrinx" width="360"></p>

# syrinx

A compiler for sounds. JavaScript is the source, audio is the binary.

A sound effect is a small JavaScript file. syrinx runs it inside an embedded V8 against a small
core -- seeded randomness, the block protocol -- and a framework of synthesis primitives and
instruments the project keeps beside its sources, and writes out PCM. The same source always produces the same
bytes — sources are statically rejected if they touch anything non-deterministic — so sounds
can live in a repo as code and be baked by a content pipeline like any other asset.

The standard itself -- what a source is and what a host must do with it -- is
[SPEC.md](SPEC.md); the API reference is generated into [docs/API.md](docs/API.md).

syrinx never plays audio itself. The library only renders, whole or one block at a time; the
CLI's `play` pipes blocks to whatever system player is on PATH as they are computed, and
`crates/syrinx-player` is a desktop player built on the same library.

## Build

```
make            # -> dist/ (see below)
make test
make examples   # renders examples/*.syr into examples/out/
```

Requires a Rust toolchain. The first build downloads the prebuilt V8 for the host target
(~100 MB); it is linked statically, so the outputs have no runtime dependency on it.

`dist/` after `make`:

| path                     | what                                                       |
|--------------------------|------------------------------------------------------------|
| `bin/syrinx`            | the CLI                                                    |
| `lib/libsyrinx.so`      | the shared library (`syrinx.dll` on Windows)              |
| `include/syrinx.h`      | C header                                                   |
| `prelude.js`             | the prelude (the `"syrinx"` module)                        |
| `syrinx.d.ts`            | type declarations for editors                              |
| `framework/`             | the framework, to copy into a project                      |

### Windows

The V8 crate ships prebuilt libraries for `x86_64-pc-windows-msvc` only, so the Windows build
uses the MSVC target -- never `-gnu`. Cross-building from Linux works with
[cargo-xwin](https://github.com/rust-cross/cargo-xwin), which downloads Microsoft's own CRT and
SDK headers:

```sh
cargo install cargo-xwin
rustup target add x86_64-pc-windows-msvc
rustup component add llvm-tools          # for the MSVC librarian; see tools/xwin-bin/llvm-lib
sudo dnf install clang lld               # clang-cl and lld-link

PATH="$PWD/tools/xwin-bin:$PATH" cargo xwin build --release --target x86_64-pc-windows-msvc
```

`tools/xwin-bin/` holds two shims the cross build needs and cargo-xwin gives no other way to
supply; each explains itself in its own header.

The executable carries the logo and its version information as PE resources -- what Explorer,
the taskbar and the Properties dialog read. An ELF has no equivalent, so on Linux the binary is
just a binary. The icon comes from `logo/syrinx.ico`, regenerated from the logo with

```sh
magick -background none -density 512 logo/syrinx.svg \
    -define icon:auto-resize=256,128,64,48,32,16 logo/syrinx.ico
```

Windows has no `windres`, so `rc.exe` or `llvm-rc` compiles the resource there; cross-building
from a machine that has neither, `build.rs` falls back to mingw's `windres`, whose `.res` output
the MSVC linker accepts. If no resource compiler exists at all the build fails rather than
quietly shipping an unbranded binary.

**What the Windows artifacts depend on** (read off their import tables, not assumed):

| | needs |
|---|---|
| `syrinx.dll` | nothing but Windows: `kernel32`, `ntdll`, `advapi32`, `winmm`, `dbghelp`, `bcryptprimitives`, one API set |
| `syrinx.exe` | the above, plus `VCRUNTIME140.dll` and `VCRUNTIME140_1.dll` |

V8, the prelude and every encoder are inside both. The exe's two extra DLLs come from V8's
prebuilt, which is linked against the dynamic C++ runtime; they are part of the Visual C++
redistributable, present on most machines and shippable beside the executable. The library --
what the engine loads -- needs none of it.

`dist/` after `make`:

| path                     | what                                                       |
|--------------------------|------------------------------------------------------------|
| `bin/syrinx`            | the CLI                                                    |
| `lib/libsyrinx.so`      | the shared library (`syrinx.dll` on Windows)              |
| `include/syrinx.h`      | C header                                                   |
| `prelude.js`             | the prelude (the `"syrinx"` module)                        |
| `syrinx.d.ts`            | type declarations for editors                              |

Windows: the V8 crate ships prebuilt libraries for `x86_64-pc-windows-msvc` only, so build with
the MSVC toolchain (or `cargo-xwin` from Linux), not `-gnu`. That build carries the logo and its
version information as PE resources, which is what Explorer, the taskbar and the Properties
dialog read; an ELF has no equivalent, so on Linux the binary is just a binary. The icon comes
from `logo/syrinx.ico`, regenerated from the logo with

```sh
magick -background none -density 512 logo/syrinx.svg \
    -define icon:auto-resize=256,128,64,48,32,16 logo/syrinx.ico
```

## CLI

```
syrinx compile laser.syr                     # -> laser.wav (16-bit)
syrinx compile laser.syr -o laser.wav --bits 32
syrinx compile laser.syr -o laser.f32        # raw interleaved float, what an engine cache wants
syrinx compile laser.syr --sample-rate 44100 --timeout 5
syrinx compile sounds/ -o build/sounds --format opus   # every .syr under sounds/, in parallel, structure mirrored
syrinx compile sounds/ -q                            # next to each source, wav
syrinx compile track.syr --stem lead -o lead.wav     # one layer, without the mix stage
syrinx compile track.syr --stem drums --stem bass    # a subset, summed
syrinx compile track.syr --bounce stems/             # every layer to its own lossless wav
syrinx compile track.syr --mix-from stems/ -o mix.wav  # only the mix stage, over those layers
syrinx hash sounds/ --write syrinx.lock              # render hashes, per sound AND per layer
syrinx hash sounds/ --check syrinx.lock              # CI: fails if any sound's output changed
syrinx play laser.syr --repeat 3             # a streaming source starts as soon as its first block exists
syrinx check examples/*.syr                  # determinism check + meta, no render
syrinx check --root sounds/ sounds/*.syr    # ... with imports jailed to sounds/
syrinx info
syrinx framework ./framework                # copy the framework into a project (see below)
syrinx prelude                              # print the prelude: the core module, "syrinx"
syrinx types                                # print syrinx.d.ts for editors
syrinx docs --out docs/syrinx-docs.json     # the API reference (core and framework) as JSON
```

### Output formats

The extension of `-o` picks the format. Native, no external tools:

| extension | what | knobs |
|---|---|---|
| `.wav` | PCM | `--bits 16` (default), `24`, `32` (float) |
| `.f32` `.pcm` `.raw` | headerless interleaved float | |
| `.flac` | lossless | `--bits 16` or `24` |
| `.mp3` | LAME, CBR | `--bitrate` (default 160) |
| `.ogg` | Vorbis, VBR | `--bitrate` |
| `.opus` | Opus in Ogg | `--bitrate`; needs a 48 kHz render |

Anything else (`.m4a`, `.mp4`, `.aiff`, `.caf`, `.wma`, ...) is handed to `ffmpeg` if it is on
PATH, with the render piped in as raw float.

For a game: the engine cache wants `.f32` or `.wav`; `.flac` when a lossless file must be small;
`.ogg` or `.opus` for shipped streams (Opus is the better codec, Vorbis the more widely decoded).
MP3 and AAC add encoder delay and padding that break seamless loops — fine for one-shots and
sharing, wrong for `loop: true` sources.

## Writing a sound

Sources use the `.syr` extension. They are plain JavaScript ES modules; the extension exists so
a content pipeline can claim them without sniffing every `.js` file. Tell your editor:

- VS Code: `"files.associations": { "*.syr": "javascript" }`, and `syrinx types > syrinx.d.ts`
  next to a `jsconfig.json` for autocomplete
- Monaco: `monaco.languages.register({ id: "javascript", extensions: [".syr"] })` and
  `addExtraLib(syrinx types)`

A sound is one or more named **layers**, and optionally a mix that combines them.

```js
import { hash } from "syrinx";
import { BlepOsc, Noise, Env, render, normalize } from "./framework/dsp.js";
import { crack } from "./lib/space.js";

export const meta = { api: 4, name: "laser", duration: 0.35, channels: 1, seed: 7 };

export const stems = {
  tick(ctx) {
    return render(ctx, crack(ctx, { seed: hash(ctx.seed, ctx.stem) }));
  },
  body(ctx) {
    const osc = BlepOsc.saw(ctx.sr);
    const noise = new Noise(hash(ctx.seed, ctx.stem));
    const freq = Env.sweep(2400, 180, 0.3);
    const amp = Env.ad(0.003, 0.09);
    return render(ctx, (t) => (osc.next(freq(t)) + noise.white() * 0.2) * amp(t));
  },
};

// Optional. Without it the mix is the sum of the layers, in declaration order.
export default function (ctx) {
  const { tick, body } = ctx.stems;
  return normalize(render(ctx, (t, i) => tick[0][i] + body[0][i]));
}
```

`meta` — `api` (the contract the source was written against: 4, required), `name` (no default: a
tool that needs a label picks one), `duration` (seconds, required), `channels` (1 or 2),
`sampleRate` (optional preference; the caller may override), `seed` (an integer in
[0, 4294967295]), `loop` (declared seamless loop; passed through).

`stems` is required and non-empty. Names match `/^[A-Za-z][A-Za-z0-9_.-]{0,63}$/` -- the leading
letter matters, because an integer-like key sorts itself to the front of a JavaScript object and
would silently change the order the layers are summed in. Dots are allowed and reserved for a
hierarchy (`drums.kick`). Declaration order is summing order, listing order and bounce order.

A layer receives `{ sr, frames, duration, seed, channels, stem }`; the mix receives
`{ sr, frames, duration, seed, channels, stems }`, where each layer is `channels` planes of
`frames` samples. Both return a `Float32Array` (or `number[]`), or `[left, right]` for stereo --
or a stream that yields those one block at a time (below). Samples are nominally in [-1, 1] and
are not clipped; the CLI reports peak and flags clipping. Non-finite samples are an error.

`ctx.seed` is `meta.seed` in every layer, so two layers each writing `new Noise(ctx.seed)` get
the same noise. Derive a voice's own draw with `hash(ctx.seed, "kick")`: stable when layers are
reordered or renamed around it.

### Streams

A layer may return a **stream** instead of a buffer: a function the host calls once per block,
in order from frame 0, that returns that block's samples. The mix may do the same, and then
receives every layer's block as its third argument instead of reading `ctx.stems`. A host can
start playing such a source as soon as its first block exists; `examples/beacon.syr` is one.

```js
export const stems = {
  drone(ctx) {                                    // render(), one block at a time: same fn, same t and i
    const osc = Osc.sine(ctx.sr);
    return stream(ctx, (t) => osc.next(55) * 0.5);
  },
  hits(ctx) {                                     // a hand-written stream: state lives in the closure
    const ping = normalize(render({ ...ctx, frames: 3840 }, (t) => ...));   // setup may use every helper
    return (offset, frames) => {
      const out = new Float32Array(frames);
      /* copy the part of each ping that overlaps [offset, offset + frames) */
      return out;
    };
  },
  swell(ctx) { return render(ctx, (t) => ...); },  // a whole buffer beside them is fine: rendered at setup
};

export default function (ctx) {
  const lp = Biquad.lowpass(ctx.sr, 6000);
  return (offset, frames, { drone, hits, swell }) => filter(mix(drone[0], hits[0], swell[0]), lp);
}
```

- **Blocks are `BLOCK_FRAMES` (4096) frames**, a constant of the standard exported by the
  core, so a block boundary can never leak into the samples on one host and not another.
  The last block is shorter. A block's return goes through the same rules as a whole return
  (mono duplicated when `channels` is 2, the plane count checked) but its length must be exactly
  the block's: a wrong length is an error naming the block, because padding it would hide an
  inclusive bound or a fixed-size scratch buffer.
- **Closure state persists between blocks.** Oscillators, filters and delay lines are made in
  setup and advanced by every block; that is what makes a stream causal.
- **Nothing non-causal.** The framework's `normalize` needs the peak of the whole render, `fade`
  its end, `place` the whole timeline; inside a block all three throw, naming the fix (a limiter or
  a fixed gain; a gain from the absolute time with `Env.line` and `Env.gate`; a copy into the
  block from `round(at * sr) - offset`). They know by asking the core's `inBlock()`, which your own
  code may ask too. A look-ahead effect carries its own delay line. In setup they stay legal.
- **The mix stage is classified by a probe.** The host calls the default export once with a
  `ctx.stems` whose reads are recorded and refused. It read them: it is a whole-buffer mix and
  runs, in a fresh isolate, over the whole layers once they exist -- so a whole-buffer master
  over streaming layers is legal, the host just cannot start it early. It returned a function
  without reading: it is a mix stream. A mix stream that reads `ctx.stems`, in setup or in a
  block, is an error naming the third argument. `Object.keys(ctx.stems)` is fine either way.
- **A mixer may be restarted** at a block boundary (`Mixer::restart` in the library, what the
  player does to seek); its first blocks then differ from the canonical render until its state
  warms. The canonical render is the sequence from frame 0.
- Port a master before its layers: a mix stream over whole layers works, a whole-buffer master
  over streaming layers works, so either order renders -- but only the first order streams the
  moment it is done.

### Why layers

Each layer renders in its own isolate, which is what makes its audio independent of which other
layers ran and in what order -- and also lets them render at the same time. Three things follow:

- **`--stem lead` renders only that layer.** A sixth of the work, which is what you want while
  you are writing it.
- **An approval survives the next layer.** A master ending in `softclip` and `normalize` is a
  function of the sum, so adding a bass changes the drums you already approved. As a layer, the
  drums are byte-identical and `syrinx hash` says so.
- **`--bounce` then `--mix-from`** re-runs only the mix over layers rendered earlier, so
  rebalancing costs a fraction of a second instead of a full render. `--mix-from` warns when the
  source has changed since the bounce; it cannot say which layer changed, because a layer depends
  on the module's shared constants as much as on its own body.

Layers are **not a sound bank**. They share one timeline, one rate and one duration, and they
sum. One file holding several unrelated sounds is a different thing that does not exist.

### Imports

`"syrinx"` is the core. Relative paths (`./lib/x.js`, `../shared/y.syr`) are files, resolved
from the importing module and jailed to `--root` when one is given (the engine passes its
project root). Nothing else can be imported: no bare specifiers, no `node_modules`, no URLs. A
module imported twice is one instance. Every module in the graph goes through the determinism
check, and the compiler reports the closure (`syrinx check` prints it; `syrinx_render_dependency`
in the C API) so a cache can key on it. An import that does not resolve is a compile error naming
the module that asked.

The core (`syrinx prelude`, types in `syrinx types`) is small on purpose: `PRELUDE_VERSION`,
`BLOCK_FRAMES`, seeded `Random`, `hash` for deriving seeds, and `inBlock()`. Everything a sound is
made of is the framework.

### The framework

`framework/` is a library of its own: the primitives that were the prelude until 1.0 (`dsp.js`:
oscillators, noise, envelopes, filters, delays and reverb, the buffer helpers and the scalars),
and on top of them the arrangement engine, effects, a mastering chain and instruments -- the
pianos, strings, synths, drum kits and voices the Firmament album was written with. A host never
loads it on its own account; a project takes a copy and imports it by relative path, like any of
its own modules:

```
syrinx framework ./framework        # writes it, and framework/VERSION; run again to update
```

```js
import { Env, render } from "./framework/dsp.js";
import { grand } from "./framework/instruments/organic.js";
```

A released framework function never changes what it renders; a better voice gets a new name. So
updating a project's copy can add to it without moving a sound anyone approved -- and
`syrinx hash --check` would say so if it did. `framework/README.md` is the map.

### Hearing without ears

Most sounds will be written by an agent, so the loop has to be verifiable without listening:

```
syrinx analyze laser.syr                  # attack/decay, brightness, tonal vs noisy, band balance, onsets
syrinx analyze sounds/ --json             # one JSON object per sound
syrinx spectrogram laser.syr              # -> laser.syr.png, log-frequency STFT to look at
syrinx compare laser.syr reference.wav    # ten axes, each with a tolerance and a note to act on
```

`compare` scores a candidate against a reference (either can be a `.syr` or a `.wav`) on
envelope shape, brightness and its trajectory, attack, decay, crest, flatness, spectral tilt,
onset count and six-band balance, and names the worst offender ("decay 4.0x too long", "sub 31 dB
under the reference"). It exits non-zero below 0.7. The measurements are ANALYSIS_VERSION 4 of the
analyzer, so numbers from earlier runs carry over.

### Reference documentation

`syrinx docs` turns the type declarations into JSON: the core module and the framework's, each
with its groups, entries, members, signatures and prose. It checks each module's declarations
against its real exports first, so documentation cannot quietly drift from the code — a missing or
surplus declaration fails the command, and a test keeps the committed `docs/syrinx-docs.json` in
step. The rendered reference is published at
https://docs-archwyvern.web.app/syrinx.

### Hashes as regression tests

`syrinx hash` prints a BLAKE3 of each source's rendered PCM. Commit the lockfile and run
`--check` in CI: an unintended change to a sound, a shared module, the framework or the core shows
up as `changed  path`, exactly like a snapshot test. Determinism is what makes this meaningful.

### Determinism

`Math.random`, `Date`, `performance`, timers, `eval`/`Function`, `globalThis`, `Intl`, `crypto`,
`require`, any I/O and the `**` operator are rejected before a module runs, with its file, line
and column. Use `new Random(seed)` / `new Noise(seed)` and take the seed from `meta.seed` or
`ctx.seed`; write `Math.pow(x, y)` rather than `x ** y`.

The math is the standard's, not the engine's. The ECMAScript specification leaves `Math.sin`,
`Math.exp`, `Math.pow` and the rest implementation-defined, and V8 has changed its
implementation more than once (fdlibm, then LLVM's libc, with `Math.tanh` from the platform C
library in between), so two engines can legitimately differ in the last bit — and through a
filter, a last bit is a different waveform. `prelude/math.js` therefore replaces every such
function with a port of fdlibm and freezes `Math`, and every host runs it before anything else.
What remains is IEEE double add, subtract, multiply, divide and sqrt, which every engine on every
platform computes identically, so a source renders the same bytes whatever V8 renders it. The
port is checked bit for bit against the C (`tools/fdlibm-ref/`, `make math-golden`) by
`test/math.test.js`; `**` is banned because exponentiation compiles to the engine's own pow and
cannot be replaced.

A source that runs past its time budget (default 20 s) is terminated and reported as a timeout;
a runaway loop cannot hang the host.

## Library

C ABI in `include/syrinx.h`. The GStreamer element and the VLC module are its consumers in this
repository. A host on another runtime can also skip this library and embed the standard's files
in its own engine (see below).

Every call returns an opaque result that is either OK or carries an error kind, message and
position; free it with `syrinx_render_free`. Calls are independent — each render uses its own
V8 isolate — so rendering from several threads at once is fine. `syrinx_version()` is the
prelude/contract version: key cache artefacts on it.

The same sound one block at a time: `syrinx_stream_open` (the `stems` argument is NULL for the
mix, or `"a,b"` for a subset summed without the mix stage), `syrinx_stream_info` for the open
result and the geometry, then `syrinx_stream_next` until it returns NULL. Each block is a
result of its own (`syrinx_render_frames`, `syrinx_render_offset`, `syrinx_render_samples`),
freed with `syrinx_render_free`; `syrinx_stream_free` stops whatever is still computing.
`syrinx_stream_streaming` says whether the blocks are computed on demand or the whole sound was
rendered at open.

In Rust the library goes further, which is what the player is built on: `Source::open`, then
`Source::stems` (one isolate and thread per layer, blocks pulled in order, a few computed ahead)
and `Source::mixer` (the mix stage fed one block of every layer at a time, restartable at a
block boundary, or the whole layers at once). `Stream` composes the three into pre-mixed
blocks, and `render` drains a `Stream`.

### The standard, and hosting it yourself

The library is one host of the syrinx standard, not the standard itself. The standard is four
JavaScript files, and a host on any engine that evaluates them in this order renders the same
bytes as this one:

| File | Kind | When |
|---|---|---|
| `prelude/math.js` | classic script | first, once per isolate — before the prelude and any source |
| `js/check.js` | ES module | `check(source)` on every user module before it is compiled |
| `prelude/prelude.js` | ES module | served for `import ... from "syrinx"` |
| `prelude/run.js` | an object, `{ BLOCK_FRAMES, stem, mix }` | once per isolate: a layer's or the mix's return value -> planes, whole or one block at a time; the sum of the layers; the probe that classifies a mix |

Relative imports are files under the project root, canonicalised, and nothing else is
importable; a module imported twice is one instance. The hosts in this repository are the
Rust library (`crates/syrinx-core`) and the JavaScript package (`js/`); neither re-implements
any of these files, and a new host should not either -- embed them verbatim.

### In a browser

`syrinx/browser` is the host for a web page. A page cannot read files or evaluate strings, so it
is handed URLs and imports them in the standard's order: `prelude/math.js` first, then the run
wrapper as a module (`prelude/run.module.js`, which is `run.js` behind `export default`, pinned
byte for byte by a test), then the prelude, then the source:

```js
import { open } from "syrinx/browser";

// in a Web Worker
const source = await open({
  math: "/syrinx/math.js",
  run: "/syrinx/run.module.js",
  prelude: "/syrinx/prelude.js",
  entry: "/sounds/laser.js",   // its imports already rewritten to URLs, "syrinx" to `prelude`
});
const layer = source.stem(source.names[0]);          // a driver per block, or whole planes
const mix = source.mixer(0);                          // a mix stream, or null for a whole-buffer mix
```

It reads the contract with the same module as the Node host (`js/contract.js`), so a source is
accepted, refused and measured identically, and it refuses a runtime whose prelude is another
contract version -- so a page may take the runtime from wherever a release says it lives. Rewriting
a module's imports is `rewriteImports` from `syrinx/imports`, the same scan the Node host loads
with. What only the page can do stays with the page:
serving the files, rewriting a source's imports to URLs (once, when it is published), running
`check()` on the text before serving it, a worker per layer, and terminating one that runs past
its budget. `test/browser.test.js` renders every example through it and compares raw floats with
the CLI.

## Versions

Releases are tags, `vMAJOR.MINOR.PATCH`; the package, the crates and `syrinx info` carry the same
number. Depend on a tag rather than a branch:

```json
"syrinx": "github:archwyvern/syrinx#v1.0.0"
```

A new standard -- a changed prelude, math or run wrapper -- can change what an unchanged source
renders to, so pin the version that bakes your sounds and move it deliberately.

1.0 moved everything that was not the standard out of the prelude, into the framework. A 0.x
source moves across by taking those names from `framework/dsp.js` instead of `"syrinx"` and
declaring `api: 4`; its samples do not change (the examples and a 215-source album workspace were
proven byte for byte).

## Playing .syr in VLC

`vlc/` holds a VLC 3 demux module. It compiles the source with `libsyrinx` when the file is
opened and hands VLC raw float PCM, so play, pause, seek, the progress bar and the title come for
free; a source that fails to compile shows its located error in VLC's message log.

```
sudo dnf install vlc-devel          # headers + pkg-config for the plugin API
make                                # dist/ first
make -C vlc                         # -> vlc/build/libsyrinx_plugin.so (+ libsyrinx.so beside it)
VLC_PLUGIN_PATH=$PWD/vlc/build vlc examples/explosion.syr
sudo make -C vlc install            # into VLC's demux plugin dir, for good
make -C vlc install-user            # no sudo: ~/.local/lib/vlc/plugins + VLC_PLUGIN_PATH in ~/.config/environment.d
```

VLC 3 only scans its own plugin directory and `VLC_PLUGIN_PATH`. `install-user` sets the
variable three ways so no re-login is needed: `environment.d` for future sessions, an import
into the running session, and a user-local `vlc.desktop` override whose `Exec` carries it, which
is what any launcher (Files, browser downloads, the app grid) actually uses. It also registers
`.syr` as `audio/x-syrinx` with VLC as the default handler.

The module is tied to the VLC major it was built against (3.x here); VLC 4 changed the demux
API and would need a port.

## Playing .syr in GNOME's Audio Player, Showtime, or any GStreamer player

`gst/` is a GStreamer plugin: a typefinder that recognises a sound source as `audio/x-syrinx`
and a `syrinxdec` element that renders it through libsyrinx and streams the PCM, with exact
seeking, duration, and the sound's name as its title. Anything on playbin plays a `.syr` like a
WAV, which on Fedora means Decibels (Audio Player), Showtime, and `gst-play-1.0`.

```sh
make                       # at the root: dist/
make -C gst headers        # only if gstreamer1-devel is not installed; no sudo (dnf download + rpm2cpio)
make -C gst                # gst/build/libgstsyrinx.so
make -C gst install-user   # ~/.local/share/gstreamer-1.0/plugins, the MIME type, Decibels as the handler
```

GStreamer scans the per-user plugin directory on its own, so unlike VLC nothing has to be put
into the session environment. `install-user` also makes Decibels the default for
`audio/x-syrinx` when it is installed; `xdg-mime default vlc.desktop audio/x-syrinx` hands it
back to VLC.

## Playing .syr with syrinx-player

`crates/syrinx-player` is a small desktop player for sources (Linux and Windows), one window:
a playlist, a seek bar drawn from the sound, a fader with mute and solo per layer, play, pause,
loop, volume. It renders through this library and keeps what it rendered under
`~/.cache/syrinx/player` (`%LOCALAPPDATA%\syrinx\player` on Windows), so a track opened once
plays at once the next time, and an edit to a source or one of its imports re-renders and
resumes where you were.

```
syrinx-player tracks/maw.syr sfx/         # files and folders; they replace a running player's list
make install-user                         # Linux: on PATH + launcher entry + audio/x-syrinx handler
syrinx-player --register                  # Windows: associate .syr with this exe for the current user
```

Drop files or folders onto the window to add them. A second launch hands its paths to the
running player instead of opening another window, so double-clicking a `.syr` in the file
manager replaces what is playing. Keys: Space play/pause, Home start, arrows seek 5 s (Shift:
30 s) and volume, L loop, N/P next and previous, Delete removes the selected row, O opens the
file dialog, R re-renders the current track.

The faders scale each layer before the mix stage. For a whole-buffer source (every source today)
the canonical mix plays while all faders sit at unity; move one and the player plays the plain
sum of the gained layers with the master bypassed, and says so in the status line, because a
master written over the finished sum cannot be re-run on the fly. Streaming sources run their
mix stage live over the gained layers, master included.

## Layout

```
crates/syrinx-core   host (V8), static check, prelude embedding, wav encoding
crates/syrinx-ffi    cdylib, C ABI over core
crates/syrinx-cli    the syrinx binary
crates/syrinx-player the desktop player (egui + cpal)
crates/syrinx-exe-resources  build-script helper: the icon and version block of the Windows executables
prelude/math.js       the standard math: fdlibm ports replacing the engine's Math, run first
prelude/prelude.js    the core module sources import as "syrinx"
prelude/run.js        the run wrapper every host evaluates: return value -> planes, whole or per block; the sum
prelude/run.module.js the same, behind `export default`, for a host that can only import (a browser)
prelude/syrinx.d.ts   the core's type declarations, and a source of the reference docs
framework/            the framework: its own package, vendored into projects (`syrinx framework`)
js/                   the JavaScript hosts (npm package at the repo root): index.js for Node, browser.js for a
                      page; contract.js is what both read off a source, check.js is the check, imports.js
                      finds and rewrites a module's imports
test/                 the JS hosts' tests: math identity vs the C reference, Node and browser host vs Rust parity
tools/fdlibm-ref/     the vendored fdlibm C the math port is checked against (make math-golden)
docs/syrinx-docs.json the API reference, generated from those declarations (make docs); docs/API.md renders it
SPEC.md               the standard: the source language, the host contract, conformance
include/syrinx.h     C header
examples/             sample sources (.syr)
vlc/                  VLC 3 demux module: play .syr in VLC
gst/                  GStreamer plugin: play .syr in Decibels, Showtime, any playbin player
logo/                 icon + wordmark (SVG, PNG, and the .ico the Windows build embeds)
desktop/              the player's Linux launcher entry (installed by make install-user)
```

## Licence

MIT -- see [LICENSE](LICENSE).
