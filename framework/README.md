# syrinx-framework

The library a syrinx sound is written with: the primitives, an arrangement engine, effects, a
mastering chain and instruments. It is not part of the standard. A host never ships or evaluates
it on its own account; to a host it is simply more of a project's source. Use as much of it as
you like, or none of it: a source needs nothing but the core, `"syrinx"`.

## Getting it

A project keeps its own copy, beside its sources, and imports it by relative path -- the standard
admits only `"syrinx"` and relative specifiers (SPEC.md, clauses 8 and 13):

```
syrinx framework ./framework
```

That writes every file and `framework/VERSION` (the release it came from). Run it again after
upgrading syrinx to update the copy: files that differ are overwritten and named, nothing is
deleted, and your own files beside it are left alone. Commit the copy with the project.

```js
import { hash } from "syrinx";
import { Env, render } from "./framework/dsp.js";
import { layer, sequence } from "./framework/music.js";
import { grand } from "./framework/instruments/organic.js";
```

Editors pick up `dsp.d.ts` beside `dsp.js` on their own; point a `jsconfig.json` path for
`"syrinx"` at the output of `syrinx types` and the core resolves too.

## The rule

A released function never changes what it renders. A better voice is a new name beside the old
one -- `piano` and `grand` both stay -- so updating a project's copy can add sound but never alter
a sound anyone approved, and `syrinx hash --check` says so if it ever did. Floating point is not
associative: even "tidying" `a * 0.28 * vel` into `a * (0.28 * vel)` is a different sound, so
bodies are not rewritten once released.

## The map

| module | what | main exports |
|---|---|---|
| `dsp.js` | the primitives: scalars, oscillators, noise, envelopes, filters, delay lines, a small reverb, buffer helpers. Until syrinx 1.0 this was the prelude | `Osc` `BlepOsc` `Phasor` `Noise` `Env` `OnePole` `Biquad` `Svf` `Delay` `Comb` `Allpass` `Reverb` `render` `stream` `mix` `gain` `normalize` `fade` `pan` `place` `filter` `db` `mtof` `clamp` `lerp` `softclip` `hardclip` `fold` `TAU` |
| `music.js` | placing notes in time, into one shared stereo pair or a layer (the streaming form) | `layer` `mono` `stereo` `each` `input` `play` `sequence` `stamp` `curve` `ducker` `grid` `pattern` `gateCurve` `arp` `melody` `n` |
| `fx.js` | a mix's effects | `Freeverb` `PingPong` `Chorus` `Phaser` `compressStereo` `limitStereo` `eqStereo` `reverbInto` `delayInto` `chorusInto` `phaserInto` `saturate` `sweepLowpass` |
| `master.js` | the last stage: section levels, a master chain, a fade, a widener | `levels` `master` `fadeOut` `widen` |
| `instruments/synth.js` | electronic voices and a drum machine | `supersaw` `pad` `lead` `driftLead` `techLead` `fmPiano` `theremin` `sub` `midBass` `darkBass` `phatBass` `kickHit` `snareHit` `clapHit` `hatHit` `crashHit` ... |
| `instruments/organic.js` | the band: pianos, strings, cello, bass and acoustic guitar, an acoustic kit | `piano` `grand` `strings` `cello` `bassGuitar` `acousticString` `acKick` `acSnare` `rideHit` |
| `instruments/dark.js` | darksynth and EBM | `arpVoice` `brass` `growlBass` `tornBass` `pulseLead` `swarm` `gatedSnareHit` `industrialKick` `toneVoice` |
| `instruments/metal.js` | guitars through an amp, the metal kit, a choir, an orchestral hit | `ksString` `chug` `makeAmp` `ampInto` `metalSnare` `tomHit` `chinaHit` `choir` `orchHit` |
| `voice/vocal.js` | a singing and speaking voice by formant synthesis | `speak` `sing` `say` `voice` `plan` `room` `Resonator` `PH` |
| `voice/tract.js` | an articulatory voice, a port of Pink Trombone | `articulate` `GESTURE` |
| `voice/convert.js` | a person's voice made a machine's: analysis and resynthesis | `convert` `analyze` `synthesize` `surgery` `larynx` |
| `sample/sample.js` | whole-take processing of a recorded voice | `load` `pitchShift` `timeStretch` `ghostChorus` `whisperDouble` `radio` `vocode` `shimmer` ... |
| `sample/machine.js` | ways to break a voice | `ringMod` `bitcrush` `stutter` `dropouts` `tapeStop` `speaker` `staticBursts` `metalRoom` ... |

`dsp.js` has declarations (`dsp.d.ts`) and a generated reference in the repository's
[`docs/API.md`](../docs/API.md); every other module documents itself in its header and on each
export.

## Conventions

- A layer is `(ctx) => Output | Stream`, as the standard defines it. `layer(ctx)` from `music.js`
  is a stereo pair that streams: everything that writes into a pair writes into it too, and the
  work runs on each block as the host pulls the stream. `each(out, inputs, fn)` is the loop over
  samples inside one; a streaming master reads its layers with `input(mix, name)`.
- An instrument is `(ctx, ev, o) -> voice`: `ev` is `{ midi, vel, gate, seed }` (`vel` 0..1, `gate`
  the seconds the key is held), `o` its options. A mono voice is `(t, k) -> sample`, a stereo one
  `(t, k, out2)`. A hit (`...Hit`) is rendered once into a buffer and stamped, like a sample.
- Randomness is always seeded: take a voice's seed from `hash(ctx.seed, ...)`, never from
  `ctx.seed` alone, or two layers draw the same numbers.
- Needing the whole render (a peak, an ending) inside a stream's block throws, naming the fix.

## Credits

`voice/tract.js` is a port of the DSP of Pink Trombone, copyright 2017 Neil Thapen, MIT licence
(<https://dood.al/pinktrombone/>). Everything else is syrinx's own, under the repository's MIT
licence.
