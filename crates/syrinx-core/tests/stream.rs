//! The streaming contract: a layer or the mix pulled one block at a time renders the same bytes
//! as the whole path, the mix stage is classified the way the spec says, the non-causal helpers
//! refuse inside a block, and a stream can be cancelled and timed out block by block.

use std::time::{Duration, Instant};

use syrinx_core::{mix_from, render, render_each, ErrorKind, RenderOptions, Source, Stream, Target, BLOCK_FRAMES};

fn opts() -> RenderOptions {
    RenderOptions::default()
}

fn only(stems: &[&str]) -> RenderOptions {
    RenderOptions { target: Target::Stems(stems.iter().map(|s| s.to_string()).collect()), ..opts() }
}

/// The streaming fixture: a per-sample stream, a hand-written block function with setup-time
/// whole-buffer work, an api-2 whole layer beside them, and a mix stream with filter state.
const STREAMED: &str = r#"
import { Osc, stream, render, mix, gain, Biquad, filter, normalize } from "syrinx";
export const meta = { name: "s", duration: 0.5, channels: 2, seed: 3 };
export const stems = {
  tone(ctx) { const o = Osc.sine(ctx.sr); return stream(ctx, () => o.next(220) * 0.5); },
  hits(ctx) {
    const hit = normalize(render({ ...ctx, frames: 480 }, (t, i) => (i % 2 ? 0.25 : -0.25)), 0.5);
    const at = [0, 4000, 9000, 23000];
    return (offset, frames) => {
      const out = new Float32Array(frames);
      for (const s of at) for (let k = 0; k < hit.length; k++) { const j = s + k - offset; if (j >= 0 && j < frames) out[j] += hit[k]; }
      return out;
    };
  },
  pad(ctx) { return render(ctx, (t, i) => (i & 1023) / 2048); },
};
export default function (ctx) {
  const lp = [Biquad.lowpass(ctx.sr, 6000), Biquad.lowpass(ctx.sr, 6000)];
  return (offset, frames, { tone, hits, pad }) => [
    filter(mix(tone[0], hits[0], gain(pad[0], 0.5)), lp[0]),
    filter(mix(tone[1], hits[1], gain(pad[1], 0.5)), lp[1]),
  ];
}
"#;

/// The same layers written whole, and the same mix over whole planes: what STREAMED must equal.
const WHOLE: &str = r#"
import { Osc, render, mix, gain, Biquad, filter, normalize } from "syrinx";
export const meta = { name: "s", duration: 0.5, channels: 2, seed: 3 };
export const stems = {
  tone(ctx) { const o = Osc.sine(ctx.sr); return render(ctx, () => o.next(220) * 0.5); },
  hits(ctx) {
    const hit = normalize(render({ ...ctx, frames: 480 }, (t, i) => (i % 2 ? 0.25 : -0.25)), 0.5);
    const at = [0, 4000, 9000, 23000];
    const out = new Float32Array(ctx.frames);
    for (const s of at) for (let k = 0; k < hit.length; k++) { const j = s + k; if (j < ctx.frames) out[j] += hit[k]; }
    return out;
  },
  pad(ctx) { return render(ctx, (t, i) => (i & 1023) / 2048); },
};
export default function (ctx) {
  const lp = [Biquad.lowpass(ctx.sr, 6000), Biquad.lowpass(ctx.sr, 6000)];
  const { tone, hits, pad } = ctx.stems;
  return [
    filter(mix(tone[0], hits[0], gain(pad[0], 0.5)), lp[0]),
    filter(mix(tone[1], hits[1], gain(pad[1], 0.5)), lp[1]),
  ];
}
"#;

fn drain(stream: &mut Stream) -> Vec<f32> {
    let mut out = Vec::new();
    while let Some(block) = stream.next_block().unwrap() {
        assert_eq!(block.offset, out.len() / stream.source().channels() as usize);
        assert!(block.frames <= BLOCK_FRAMES);
        out.extend_from_slice(&block.samples);
    }
    out
}

// ------------------------------------------------------------------------------- identity

#[test]
fn stream_is_bit_identical_to_render() {
    // The same stateful per-sample function, once through render() and once through stream().
    let src = r#"
import { Osc, Biquad, render, stream } from "syrinx";
export const meta = { duration: 0.3, channels: 1 };
const voice = (ctx) => { const o = Osc.saw(ctx.sr); const f = Biquad.lowpass(ctx.sr, 900, 2); return (t) => f.process(o.next(110 + 40 * t)); };
export const stems = {
  a(ctx) { return render(ctx, voice(ctx)); },
  b(ctx) { return stream(ctx, voice(ctx)); },
};
"#;
    let each = render_each(src, "x.syr", &opts(), &[]).unwrap();
    assert_eq!(each[0].samples, each[1].samples);
    assert!(each[0].samples.iter().any(|v| *v != 0.0), "the probe must produce a signal");

    let source = Source::open(src, "x.syr", &opts()).unwrap();
    let stems = source.stems(&["a", "b"]).unwrap();
    assert!(!stems[0].streaming(), "a whole layer is complete at setup");
    assert!(stems[1].streaming(), "a stream computes on demand");
    assert_eq!(stems[1].name(), "b");
}

#[test]
fn the_live_path_equals_the_whole_path() {
    let live = render(STREAMED, "s.syr", &opts()).unwrap();
    let each = render_each(STREAMED, "s.syr", &opts(), &[]).unwrap();
    let supplied: Vec<(String, Vec<f32>)> = each.iter().map(|r| (r.stem.clone().unwrap(), r.samples.clone())).collect();
    let whole_path = mix_from(STREAMED, "s.syr", &opts(), &supplied).unwrap();
    assert_eq!(live.samples, whole_path.samples);

    // And both equal the same sound written entirely in the api-2 forms.
    let api2 = render(WHOLE, "s.syr", &opts()).unwrap();
    assert_eq!(live.samples, api2.samples);
    assert_eq!(live.frames, 24_000);
    assert!(live.peak() > 0.1);

    let mut stream = Stream::open(STREAMED, "s.syr", &opts()).unwrap();
    assert!(stream.streaming());
    assert_eq!(drain(&mut stream), live.samples);
}

#[test]
fn the_block_sum_equals_the_whole_sum() {
    // No default export: the layers are summed per block, from a copy of the first, in order.
    let streamed = STREAMED.split("export default").next().unwrap();
    let whole = WHOLE.split("export default").next().unwrap();
    let a = render(streamed, "s.syr", &opts()).unwrap();
    let b = render(whole, "s.syr", &opts()).unwrap();
    assert_eq!(a.samples, b.samples);

    // A subset of streaming layers sums the same way, without the mix stage.
    let sub = render(STREAMED, "s.syr", &only(&["tone", "hits"])).unwrap();
    let each = render_each(STREAMED, "s.syr", &opts(), &["tone".into(), "hits".into()]).unwrap();
    let summed: Vec<f32> = each[0].samples.iter().zip(&each[1].samples).map(|(x, y)| x + y).collect();
    assert_eq!(sub.samples, summed);
}

#[test]
fn a_single_streaming_layer_needs_no_mix_stage() {
    let src = r#"
import { stream } from "syrinx";
export const meta = { duration: 0.2, channels: 1 };
export const stems = { a(ctx) { return stream(ctx, (t, i) => (i % 5) / 10); } };
"#;
    let r = render(src, "x.syr", &opts()).unwrap();
    assert_eq!(r.frames, 9_600);
    assert_eq!(r.samples[7], 0.2);
    let mut s = Stream::open(src, "x.syr", &opts()).unwrap();
    assert!(s.streaming());
    assert_eq!(drain(&mut s), r.samples);
}

// ------------------------------------------------------------------------------- classification

#[test]
fn the_four_forms() {
    let buffer_layer = "a(ctx) { return new Float32Array(ctx.frames).fill(0.25); }";
    let stream_layer = "a(ctx) { return (offset, frames) => new Float32Array(frames).fill(0.25); }";
    let whole_mix = "export default function (ctx) { return ctx.stems.a[0].map((v) => v * 2); }";
    let stream_mix = "export default function (ctx) { return (offset, frames, { a }) => a[0].map((v) => v * 2); }";
    let head = "export const meta = { duration: 0.2, channels: 1 };\n";
    for (layer, mix, layer_streams, mix_streams) in [
        (buffer_layer, whole_mix, false, false),
        (buffer_layer, stream_mix, false, true),
        (stream_layer, stream_mix, true, true),
        (stream_layer, whole_mix, true, false),
    ] {
        let src = format!("{head}export const stems = {{ {layer} }};\n{mix}\n");
        let r = render(&src, "x.syr", &opts()).unwrap();
        assert_eq!(r.samples[0], 0.5, "{layer} + {mix}");
        assert_eq!(r.samples[r.samples.len() - 1], 0.5);
        let source = Source::open(&src, "x.syr", &opts()).unwrap();
        assert_eq!(source.stems(&["a"]).unwrap()[0].streaming(), layer_streams, "{layer}");
        assert_eq!(source.mixer(&Target::Mix).unwrap().streaming(), mix_streams, "{mix}");
        let mut stream = Stream::open(&src, "x.syr", &opts()).unwrap();
        assert_eq!(stream.streaming(), layer_streams && mix_streams);
        assert_eq!(drain(&mut stream), r.samples);
    }
}

#[test]
fn enumerating_ctx_stems_keeps_a_mix_whole() {
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return new Float32Array(ctx.frames).fill(0.25); }, b(ctx) { return new Float32Array(ctx.frames).fill(0.5); } };
export default function (ctx) {
  const names = Object.keys(ctx.stems);
  const out = new Float32Array(ctx.frames);
  for (const name of names) for (let i = 0; i < ctx.frames; i++) out[i] += ctx.stems[name][0][i];
  return out;
}
"#;
    let r = render(src, "x.syr", &opts()).unwrap();
    assert_eq!(r.samples[0], 0.75);
    let source = Source::open(src, "x.syr", &opts()).unwrap();
    let mut mixer = source.mixer(&Target::Mix).unwrap();
    assert!(!mixer.streaming());
    let a = vec![0.25f32; 4800];
    let b = vec![0.5f32; 4800];
    assert_eq!(mixer.mix_all(&[&a, &b]).unwrap().samples[0], 0.75);
}

#[test]
fn a_mix_that_swallows_the_sentinel_is_still_whole_form() {
    // Classification is on the record of the read, not on the throw propagating.
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return new Float32Array(ctx.frames).fill(0.25); } };
export default function (ctx) {
  let a;
  try { a = ctx.stems.a; } catch (e) { a = [new Float32Array(ctx.frames).fill(-1)]; }
  return a[0].map((v) => v * 2);
}
"#;
    let r = render(src, "x.syr", &opts()).unwrap();
    assert_eq!(r.samples[0], 0.5, "the real planes, not the fallback");
    assert!(!Source::open(src, "x.syr", &opts()).unwrap().mixer(&Target::Mix).unwrap().streaming());
}

#[test]
fn a_mix_that_ignores_its_layers_is_the_mix() {
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return new Float32Array(ctx.frames).fill(0.25); } };
export default function (ctx) { return new Float32Array(ctx.frames).fill(0.125); }
"#;
    let r = render(src, "x.syr", &opts()).unwrap();
    assert_eq!(r.samples[0], 0.125);
    assert_eq!(r.frames, 4800);
    let source = Source::open(src, "x.syr", &opts()).unwrap();
    let mut mixer = source.mixer(&Target::Mix).unwrap();
    assert!(!mixer.streaming());
    let m = mixer.mix_all(&[&vec![0.25f32; 4800]]).unwrap();
    assert_eq!(m.samples[0], 0.125);
    assert_eq!(m.meta.name, "x");
    assert_eq!(m.stem_names, vec!["a".to_string()]);
}

#[test]
fn a_mix_that_reads_its_layers_and_streams_names_the_mistake() {
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return new Float32Array(ctx.frames).fill(0.25); } };
export default function (ctx) {
  let peak = 0;
  try { peak = ctx.stems.a[0][0]; } catch (e) {}
  return (offset, frames, { a }) => a[0];
}
"#;
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert!(e.message.contains("read ctx.stems and returned a stream"), "{}", e.message);
    assert!(e.message.contains("(offset, frames, stems)"), "{}", e.message);
}

#[test]
fn a_stream_reading_ctx_stems_fails_at_the_first_block() {
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return (offset, frames) => new Float32Array(frames); } };
export default function (ctx) { return (offset, frames) => ctx.stems.a[0]; }
"#;
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Runtime);
    assert!(e.message.contains("ctx.stems.a is not available to a stream"), "{}", e.message);
    assert!(e.message.contains("the blocks are the third argument"), "{}", e.message);
}

// ------------------------------------------------------------------------------- block rules

#[test]
fn a_block_returning_a_function_is_refused() {
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return (offset, frames) => (() => 0); } };
"#;
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert!(e.message.contains("stem \"a\" (block at frame 0)"), "{}", e.message);
    assert!(e.message.contains("got function"), "{}", e.message);
}

#[test]
fn a_block_of_the_wrong_length_is_refused() {
    // An inclusive bound: the classic scheduler bug that padding or truncating would hide.
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return (offset, frames) => new Float32Array(frames + 1); } };
"#;
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert!(e.message.contains("returned 4097 samples for a 4096-frame block"), "{}", e.message);

    // A fixed-size scratch buffer fails on the last, shorter block, naming it.
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { const scratch = new Float32Array(4096); return (offset, frames) => scratch; } };
"#;
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert!(e.message.contains("(block at frame 4096) returned 4096 samples for a 704-frame block"), "{}", e.message);

    // A whole return keeps the lenient rule.
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return new Float32Array(10).fill(1); } };
"#;
    let r = render(src, "x.syr", &opts()).unwrap();
    assert_eq!(r.frames, 4800);
    assert_eq!(r.samples[9], 1.0);
    assert_eq!(r.samples[10], 0.0);
}

#[test]
fn mono_blocks_are_widened_and_the_plane_count_is_checked() {
    let src = r#"
export const meta = { duration: 0.1, channels: 2 };
export const stems = { a(ctx) { return (offset, frames) => new Float32Array(frames).fill(0.5); } };
"#;
    let r = render(src, "x.syr", &opts()).unwrap();
    assert_eq!(r.channels, 2);
    assert_eq!((r.samples[0], r.samples[1]), (0.5, 0.5));

    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return (offset, frames) => [new Float32Array(frames), new Float32Array(frames)]; } };
"#;
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert!(e.message.contains("returned 2 channel(s) but meta.channels is 1"), "{}", e.message);
}

#[test]
fn whole_render_helpers_refuse_inside_a_block() {
    for (call, fix) in [
        ("normalize(out)", "limiter"),
        ("fade(ctx, out, 0.01, 0.01)", "Env.gate"),
        ("place(ctx, out, 0)", "- offset"),
    ] {
        let src = format!(
            "import {{ normalize, fade, place }} from \"syrinx\";\nexport const meta = {{ duration: 0.1, channels: 1 }};\n\
             export const stems = {{ a(ctx) {{ return (offset, frames) => {{ const out = new Float32Array(frames); return {call}; }}; }} }};\n"
        );
        let e = render(&src, "x.syr", &opts()).unwrap_err();
        assert!(e.message.contains("cannot run inside a stream"), "{call}: {}", e.message);
        assert!(e.message.contains(fix), "{call}: {}", e.message);
    }
    // In setup, before the stream is returned, all three are as legal as ever.
    let src = r#"
import { normalize, fade, place, render } from "syrinx";
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) {
  const hit = place(ctx, fade(ctx, normalize(render({ ...ctx, frames: 100 }, () => 0.25)), 0.0001, 0.0001), 0.01);
  return (offset, frames) => hit.subarray(offset, offset + frames);
} };
"#;
    let r = render(src, "x.syr", &opts()).unwrap();
    assert!((r.peak() - 0.891).abs() < 0.01, "peak {}", r.peak());
}

#[test]
fn a_non_finite_block_sample_names_its_absolute_frame() {
    let src = r#"
export const meta = { duration: 0.2, channels: 2 };
export const stems = { a(ctx) { return (offset, frames) => { const out = new Float32Array(frames); if (offset <= 5000 && 5000 < offset + frames) out[5000 - offset] = NaN; return out; }; } };
"#;
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Contract);
    assert!(e.message.contains("stem \"a\" produced a non-finite sample at frame 5000 channel 0"), "{}", e.message);
}

#[test]
fn the_block_size_is_the_standards() {
    let (prelude, wrapper) = syrinx_core::standard_block_frames().unwrap();
    assert_eq!(prelude, BLOCK_FRAMES);
    assert_eq!(wrapper, BLOCK_FRAMES);
    assert_eq!(BLOCK_FRAMES, 4096);
}

// ------------------------------------------------------------------------------- the mixer

#[test]
fn restart_reproduces_blocks_for_a_stateless_mix() {
    let src = r#"
export const meta = { duration: 0.5, channels: 1 };
export const stems = { a(ctx) { return new Float32Array(ctx.frames).map((_, i) => i); }, b(ctx) { return new Float32Array(ctx.frames).fill(1); } };
export default function (ctx) { return (offset, frames, { a, b }) => a[0].map((v, i) => v * 0.5 + b[0][i]); }
"#;
    let source = Source::open(src, "x.syr", &opts()).unwrap();
    let frames = source.frames();
    let a: Vec<f32> = (0..frames).map(|i| i as f32).collect();
    let b = vec![1.0f32; frames];
    let mut mixer = source.mixer(&Target::Mix).unwrap();
    assert!(mixer.streaming());
    assert_eq!(mixer.stem_names(), &["a".to_string(), "b".to_string()]);

    let block = |offset: usize| {
        let n = BLOCK_FRAMES.min(frames - offset);
        [&a[offset..offset + n], &b[offset..offset + n]]
    };
    let first = mixer.mix(0, &block(0)).unwrap();
    assert_eq!((first.offset, first.frames), (0, BLOCK_FRAMES));
    assert_eq!(first.samples[3], 2.5);
    let second = mixer.mix(BLOCK_FRAMES, &block(BLOCK_FRAMES)).unwrap();
    let third = mixer.mix(2 * BLOCK_FRAMES, &block(2 * BLOCK_FRAMES)).unwrap();

    mixer.restart(2 * BLOCK_FRAMES).unwrap();
    let again = mixer.mix(2 * BLOCK_FRAMES, &block(2 * BLOCK_FRAMES)).unwrap();
    assert_eq!(again, third);
    let e = mixer.mix(0, &block(0)).unwrap_err();
    assert!(e.message.contains("out of order"), "{}", e.message);

    let e = mixer.restart(100).unwrap_err();
    assert!(e.message.contains("block boundary"), "{}", e.message);
    let e = mixer.restart(frames).unwrap_err();
    assert!(e.message.contains("before the end"), "{}", e.message);

    // mix_all over whole layers is the canonical sequence, block by block.
    let all = mixer.mix_all(&[&a, &b]).unwrap();
    assert_eq!(&all.samples[..BLOCK_FRAMES], &first.samples[..]);
    assert_eq!(&all.samples[BLOCK_FRAMES..2 * BLOCK_FRAMES], &second.samples[..]);
    assert_eq!(all.samples, render(src, "x.syr", &opts()).unwrap().samples);

    // The wrong number or length of layers is refused, naming the layer.
    mixer.restart(0).unwrap();
    let e = mixer.mix(0, &[&a[..BLOCK_FRAMES]]).unwrap_err();
    assert!(e.message.contains("expects 2 layer(s)"), "{}", e.message);
    let e = mixer.mix(0, &[&a[..10], &b[..BLOCK_FRAMES]]).unwrap_err();
    assert!(e.message.contains("stem \"a\" has 10 samples for the block at frame 0"), "{}", e.message);
}

#[test]
fn mix_on_a_whole_form_mixer_names_mix_all() {
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = { a(ctx) { return new Float32Array(ctx.frames).fill(0.25); } };
export default function (ctx) { return ctx.stems.a[0]; }
"#;
    let source = Source::open(src, "x.syr", &opts()).unwrap();
    let mut mixer = source.mixer(&Target::Mix).unwrap();
    assert!(!mixer.streaming());
    let e = mixer.mix(0, &[&[0.0f32; 4096]]).unwrap_err();
    assert!(e.message.contains("use mix_all"), "{}", e.message);
    let e = mixer.restart(0).unwrap_err();
    assert!(e.message.contains("use mix_all"), "{}", e.message);
    let planes = vec![0.25f32; 4800];
    assert_eq!(mixer.mix_all(&[&planes]).unwrap().samples[0], 0.25);
    // A second mix_all runs in a fresh isolate too.
    assert_eq!(mixer.mix_all(&[&planes]).unwrap().samples[4799], 0.25);
}

#[test]
fn a_subset_mixer_sums_the_subset_in_declaration_order() {
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = {
  big(ctx) { return new Float32Array(ctx.frames).fill(1); },
  tiny1(ctx) { return new Float32Array(ctx.frames).fill(Math.pow(2, -24)); },
  tiny2(ctx) { return new Float32Array(ctx.frames).fill(Math.pow(2, -24)); },
};
export default function (ctx) { throw new Error("the default export must not run for a subset"); }
"#;
    let forward = render(src, "x.syr", &only(&["big", "tiny1", "tiny2"])).unwrap();
    let backward = render(src, "x.syr", &only(&["tiny2", "tiny1", "big"])).unwrap();
    assert_eq!(forward.samples, backward.samples);
    assert_eq!(forward.samples[0], 1.0, "the halves vanish into 1.0 one at a time");

    let source = Source::open(src, "x.syr", &opts()).unwrap();
    let mixer = source.mixer(&Target::Stems(vec!["tiny2".into(), "big".into()])).unwrap();
    assert_eq!(mixer.stem_names(), &["big".to_string(), "tiny2".to_string()]);
    assert!(mixer.streaming());
    let e = source.mixer(&Target::Stems(vec!["big".into(), "big".into()])).unwrap_err();
    assert!(e.message.contains("selected twice"), "{}", e.message);
}

// ------------------------------------------------------------------------------- lifetimes

#[test]
fn dropping_a_stream_stops_a_spinning_layer() {
    let src = r#"
export const meta = { duration: 2, channels: 1 };
export const stems = { a(ctx) { return (offset, frames) => { if (offset >= 8192) for (;;) {} return new Float32Array(frames); }; } };
"#;
    let started = Instant::now();
    {
        let mut stream = Stream::open(src, "x.syr", &RenderOptions { timeout: Duration::from_secs(30), ..opts() }).unwrap();
        let first = stream.next_block().unwrap().unwrap();
        assert_eq!(first.offset, 0);
        // The layer is now spinning on its third block, ahead of us. Drop must stop it.
    }
    assert!(started.elapsed() < Duration::from_secs(3), "drop took {:?}", started.elapsed());
}

#[test]
fn a_slow_block_is_a_timeout_and_the_error_is_sticky() {
    let src = r#"
export const meta = { duration: 2, channels: 1 };
export const stems = { a(ctx) { return (offset, frames) => { if (offset === 8192) for (;;) {} return new Float32Array(frames); }; } };
"#;
    let started = Instant::now();
    let mut stream = Stream::open(src, "x.syr", &RenderOptions { timeout: Duration::from_millis(300), ..opts() }).unwrap();
    assert_eq!(stream.next_block().unwrap().unwrap().offset, 0);
    assert_eq!(stream.next_block().unwrap().unwrap().offset, 4096);
    let e = stream.next_block().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(stream.next_block().unwrap_err().kind, ErrorKind::Timeout, "sticky");
}

#[test]
fn a_paused_consumer_does_not_trip_the_block_budget() {
    let src = r#"
import { stream } from "syrinx";
export const meta = { duration: 1, channels: 1 };
export const stems = { a(ctx) { return stream(ctx, (t) => t); } };
"#;
    let mut s = Stream::open(src, "x.syr", &RenderOptions { timeout: Duration::from_millis(200), ..opts() }).unwrap();
    assert!(s.next_block().unwrap().is_some());
    // Longer than the budget, with the producer's queue full and every block already computed.
    std::thread::sleep(Duration::from_millis(500));
    let mut count = 1;
    while s.next_block().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, 48_000_usize.div_ceil(BLOCK_FRAMES));
}

#[test]
fn a_setup_error_in_one_layer_surfaces_from_stems() {
    let src = r#"
export const meta = { duration: 0.1, channels: 1 };
export const stems = {
  a(ctx) { return (offset, frames) => new Float32Array(frames); },
  b(ctx) { throw new Error("no b today"); },
};
"#;
    let source = Source::open(src, "x.syr", &opts()).unwrap();
    let e = source.stems(&["a", "b"]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Runtime);
    assert!(e.message.contains("no b today"), "{}", e.message);
    let e = source.stems(&["a", "c"]).unwrap_err();
    assert!(e.message.contains("no stem named \"c\""), "{}", e.message);
    let e = source.stems(&["a", "a"]).unwrap_err();
    assert!(e.message.contains("selected twice"), "{}", e.message);
}

#[test]
fn the_source_reports_what_it_opened() {
    let source = Source::open(STREAMED, "s.syr", &opts()).unwrap();
    assert_eq!(source.meta().name, "s");
    assert_eq!(source.sample_rate(), 48_000);
    assert_eq!(source.channels(), 2);
    assert_eq!(source.frames(), 24_000);
    assert!(source.has_mix());
    assert_eq!(source.stem_names(), &["tone".to_string(), "hits".to_string(), "pad".to_string()]);
    assert!(source.dependencies().is_empty());
    let stems = source.stems(&["pad", "tone"]).unwrap();
    assert_eq!(stems.iter().map(|s| s.name()).collect::<Vec<_>>(), vec!["pad", "tone"]);
    assert_eq!(stems.iter().map(|s| s.streaming()).collect::<Vec<_>>(), vec![false, true]);
}
