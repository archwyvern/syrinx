use std::path::PathBuf;
use std::time::Duration;

use syrinx_core::{inspect, mix_from, render, render_each, ErrorKind, RenderOptions, Target};

const SINE: &str = r#"
import { Osc, render } from "./framework/dsp.js";
export const meta = { api: 4, name: "sine", duration: 0.1, channels: 1 };
export const stems = {
  sine(ctx) {
    const osc = Osc.sine(ctx.sr);
    return render(ctx, () => osc.next(440) * 0.5);
  },
};
"#;

fn opts() -> RenderOptions {
    RenderOptions::default()
}

fn only(stems: &[&str]) -> RenderOptions {
    RenderOptions { target: Target::Stems(stems.iter().map(|s| s.to_string()).collect()), ..opts() }
}

/// The path of a source named `file` in a project with the framework vendored at ./framework.
/// The host resolves an entry's imports from its path, so a source can use the framework whether
/// or not the file itself is written. One project per test process.
fn in_project(file: &str) -> String {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = scratch("project");
        syrinx_core::framework::vendor(&dir.join("framework"), "test").unwrap();
        dir
    });
    dir.join(file).to_string_lossy().into_owned()
}

/// A scratch directory unique to the calling test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("syrinx-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 1-based line and column of `needle`, so a position assertion says where the thing IS rather
/// than restating a hand-counted number that formatting can invalidate.
fn position_of(source: &str, needle: &str) -> (u32, u32) {
    for (i, line) in source.lines().enumerate() {
        if let Some(col) = line.find(needle) {
            return (i as u32 + 1, col as u32 + 1);
        }
    }
    panic!("{needle:?} is not in the source");
}

#[test]
fn renders_mono_at_default_rate() {
    let r = render(SINE, &in_project("sine.syr"), &opts()).unwrap();
    assert_eq!(r.sample_rate, 48_000);
    assert_eq!(r.channels, 1);
    assert_eq!(r.frames, 4_800);
    assert_eq!(r.samples.len(), 4_800);
    assert_eq!(r.meta.name.as_deref(), Some("sine"));
    assert_eq!(r.stem_names, vec!["sine".to_string()]);
    assert_eq!(r.stem, None, "one layer and no mix stage: it IS the sound, not a layer of one");
    assert_eq!(r.dependencies.len(), 1, "{:?}", r.dependencies);
    assert!(r.dependencies[0].ends_with("framework/dsp.js"), "the framework is a dependency like any import");
    assert!((r.peak() - 0.5).abs() < 1e-3);
}

#[test]
fn same_source_same_bytes() {
    let src = r#"
import { Noise, Reverb, Biquad, Env, render, filter } from "./framework/dsp.js";
export const meta = { api: 4, name: "n", duration: 0.5, channels: 2, seed: 99 };
export const stems = {
  noise(ctx) {
    const noise = new Noise(ctx.seed);
    const rev = new Reverb(ctx.sr);
    const lp = Biquad.lowpass(ctx.sr, 2000, 1.5);
    const m = render(ctx, (t) => lp.process(noise.pink()) * Env.ad(0.01, 0.1)(t));
    return [m, filter(m, rev)];
  },
};
"#;
    let a = render(src, &in_project("n.syr"), &opts()).unwrap();
    let b = render(src, &in_project("n.syr"), &opts()).unwrap();
    assert_eq!(a.samples, b.samples);
    assert_eq!(a.channels, 2);
    assert_eq!(a.samples.len(), 24_000 * 2);
}

// ------------------------------------------------------------------------------- stems

const TWO: &str = r#"
import { render } from "./framework/dsp.js";
export const meta = { api: 4, name: "two", duration: 0.01, channels: 1 };
export const stems = {
  a(ctx) { return render(ctx, () => 0.25); },
  b(ctx) { return render(ctx, () => 0.125); },
};
"#;

/// The claim the whole feature rests on: a layer rendered alone is exactly its contribution.
#[test]
fn mix_is_bit_identical_to_the_sum_of_its_stems() {
    let mix = render(TWO, &in_project("two.syr"), &opts()).unwrap();
    let each = render_each(TWO, &in_project("two.syr"), &opts(), &[]).unwrap();
    assert_eq!(each.len(), 2);
    assert_eq!(each[0].stem.as_deref(), Some("a"));
    assert_eq!(each[1].stem.as_deref(), Some("b"));

    let summed: Vec<f32> = each[0].samples.iter().zip(&each[1].samples).map(|(x, y)| x + y).collect();
    assert_eq!(mix.samples, summed);
    assert_eq!(mix.stem, None);
    assert_eq!(mix.stem_names, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn a_subset_is_summed_without_the_mix_stage() {
    let src = r#"
import { render } from "./framework/dsp.js";
export const meta = { api: 4, duration: 0.01, channels: 1 };
export const stems = {
  a(ctx) { return render(ctx, () => 0.25); },
  b(ctx) { return render(ctx, () => 0.125); },
};
export default function (ctx) { return ctx.stems.a[0].map((v) => v * 10); }
"#;
    let entry = in_project("x.syr");
    let mix = render(src, &entry, &opts()).unwrap();
    assert!((mix.samples[0] - 2.5).abs() < 1e-6, "the mix stage runs for Target::Mix");

    let a = render(src, &entry, &only(&["a"])).unwrap();
    assert!((a.samples[0] - 0.25).abs() < 1e-9, "a subset must not run the mix stage");
    assert_eq!(a.stem, None);

    let both = render(src, &entry, &only(&["a", "b"])).unwrap();
    assert!((both.samples[0] - 0.375).abs() < 1e-9);
}

#[test]
fn the_mix_stage_receives_planes_per_stem() {
    let src = r#"
import { render } from "./framework/dsp.js";
export const meta = { api: 4, duration: 0.01, channels: 2 };
export const stems = {
  a(ctx) { return render(ctx, () => 0.25); },
};
export default function (ctx) {
  const [left, right] = ctx.stems.a;
  if (left.length !== ctx.frames) throw new Error("plane is " + left.length + " long");
  return [left.map((v) => v * 2), right.map((v) => v * -1)];
}
"#;
    let entry = in_project("x.syr");
    let r = render(src, &entry, &opts()).unwrap();
    assert_eq!(r.channels, 2);
    assert!((r.samples[0] - 0.5).abs() < 1e-9);
    assert!((r.samples[1] + 0.25).abs() < 1e-9);
}

/// One isolate per stem: a stem cannot see that another one ran, in any order.
#[test]
fn stems_are_isolated_from_each_other() {
    let src = r#"
let calls = 0;
export const meta = { api: 4, duration: 0.001, channels: 1 };
export const stems = {
  a(ctx) { calls++; return new Float32Array(ctx.frames).fill(calls); },
  b(ctx) { calls++; return new Float32Array(ctx.frames).fill(calls); },
};
"#;
    let each = render_each(src, "x.syr", &opts(), &[]).unwrap();
    assert_eq!(each[0].samples[0], 1.0);
    assert_eq!(each[1].samples[0], 1.0, "b must not see a's increment");

    let alone = render(src, "x.syr", &only(&["b"])).unwrap();
    assert_eq!(alone.samples[0], 1.0);

    let reversed = render_each(src, "x.syr", &opts(), &["b".into(), "a".into()]).unwrap();
    assert_eq!(reversed[0].stem.as_deref(), Some("b"));
    assert_eq!(reversed[0].samples[0], 1.0);
    assert_eq!(reversed[1].samples[0], 1.0);
}

/// Stems sum in declaration order. The values are chosen so f32 addition is not associative:
/// 2^-24 is exactly half an ulp of 1.0 and disappears into it, but two of them do not.
#[test]
fn stems_sum_in_declaration_order() {
    let body = r#"
  big(ctx) { return new Float32Array(ctx.frames).fill(1); },
  tiny1(ctx) { return new Float32Array(ctx.frames).fill(Math.pow(2, -24)); },
  tiny2(ctx) { return new Float32Array(ctx.frames).fill(Math.pow(2, -24)); },
"#;
    let head = "export const meta = { api: 4, duration: 0.001, channels: 1 };\nexport const stems = {";
    let forward = format!("{head}{body}}};\n");
    let reversed = {
        let mut lines: Vec<&str> = body.trim_matches('\n').lines().collect();
        lines.reverse();
        format!("{head}\n{}\n}};\n", lines.join("\n"))
    };

    let a = render(&forward, "x.syr", &opts()).unwrap();
    let b = render(&reversed, "x.syr", &opts()).unwrap();
    assert_eq!(a.samples[0], 1.0, "the halves vanish into 1.0 one at a time");
    assert_eq!(b.samples[0], 1.0 + f32::powi(2.0, -23), "summed first, they do not");
    assert_ne!(a.samples, b.samples);
}

#[test]
fn mix_from_previously_rendered_stems_round_trips() {
    let src = r#"
import { render } from "./framework/dsp.js";
export const meta = { api: 4, duration: 0.01, channels: 2 };
export const stems = {
  a(ctx) { return render(ctx, (t, i) => (i % 7) / 10); },
  b(ctx) { return render(ctx, (t, i) => -(i % 3) / 10); },
};
export default function (ctx) {
  const out = [new Float32Array(ctx.frames), new Float32Array(ctx.frames)];
  for (const name of Object.keys(ctx.stems)) {
    for (let c = 0; c < 2; c++) {
      for (let i = 0; i < ctx.frames; i++) out[c][i] += ctx.stems[name][c][i] * 0.5;
    }
  }
  return out;
}
"#;
    let entry = in_project("x.syr");
    let direct = render(src, &entry, &opts()).unwrap();
    let each = render_each(src, &entry, &opts(), &[]).unwrap();
    let supplied: Vec<(String, Vec<f32>)> =
        each.iter().map(|r| (r.stem.clone().unwrap(), r.samples.clone())).collect();
    let mixed = mix_from(src, &entry, &opts(), &supplied).unwrap();
    assert_eq!(direct.samples, mixed.samples);

    // Supplied out of order, it still mixes in declaration order.
    let swapped: Vec<(String, Vec<f32>)> = supplied.iter().rev().cloned().collect();
    assert_eq!(mix_from(src, &entry, &opts(), &swapped).unwrap().samples, direct.samples);

    // A missing or unknown stem is refused rather than silently dropped.
    let e = mix_from(src, &entry, &opts(), &supplied[..1]).unwrap_err();
    assert!(e.message.contains("no audio supplied for stem \"b\""), "{}", e.message);
    let mut wrong = supplied.clone();
    wrong[0].1.truncate(4);
    let e = mix_from(src, &entry, &opts(), &wrong).unwrap_err();
    assert!(e.message.contains("stem \"a\" has 4 samples, expected"), "{}", e.message);
    let unknown = vec![("nope".to_string(), supplied[0].1.clone()), supplied[1].clone()];
    let e = mix_from(src, &entry, &opts(), &unknown).unwrap_err();
    assert!(e.message.contains("no audio supplied for stem \"a\""), "{}", e.message);
}

#[test]
fn unknown_stem_names_the_ones_that_exist() {
    let e = render(TWO, &in_project("two.syr"), &only(&["c"])).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Contract);
    assert!(e.message.contains("no stem named \"c\""), "{}", e.message);
    assert!(e.message.contains("a, b"), "{}", e.message);
}

#[test]
fn stem_names_must_start_with_a_letter() {
    // An integer-like key would sort itself to the front of the object, silently changing the
    // order the stems are summed in, so the grammar refuses it.
    let src = "export const meta = { api: 4, duration: 0.01 };\nexport const stems = { \"0\": (ctx) => [] };";
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert!(e.message.contains("must start with a letter"), "{}", e.message);

    let ok = "export const meta = { api: 4, duration: 0.01 };\nexport const stems = { \"drums.kick\": (ctx) => new Float32Array(ctx.frames) };";
    let r = render(ok, "x.syr", &opts()).unwrap();
    assert_eq!(r.stem_names, vec!["drums.kick".to_string()], "dots are reserved for a hierarchy");
}

#[test]
fn stems_are_reported_without_rendering() {
    let info = inspect(TWO, &in_project("two.syr"), &opts()).unwrap();
    assert_eq!(info.stems, vec!["a".to_string(), "b".to_string()]);
    assert!(!info.has_mix);

    let with_mix = format!("{TWO}\nexport default function (ctx) {{ return ctx.stems.a[0]; }}\n");
    assert!(inspect(&with_mix, &in_project("two.syr"), &opts()).unwrap().has_mix);
}

// ------------------------------------------------------------------------------- contract

#[test]
fn sample_rate_override_and_declared_rate() {
    let src = r#"
export const meta = { api: 4, duration: 1, sampleRate: 22050 };
export const stems = { a(ctx) { return new Float32Array(ctx.frames); } };
"#;
    let declared = render(src, "x.syr", &opts()).unwrap();
    assert_eq!(declared.sample_rate, 22_050);
    assert_eq!(declared.frames, 22_050);
    let forced = render(src, "x.syr", &RenderOptions { sample_rate: Some(8_000), ..opts() }).unwrap();
    assert_eq!(forced.sample_rate, 8_000);
    assert_eq!(forced.frames, 8_000);
}

#[test]
fn mono_return_is_duplicated_for_stereo_meta() {
    let src = r#"
import { render } from "./framework/dsp.js";
export const meta = { api: 4, duration: 0.01, channels: 2 };
export const stems = { a(ctx) { return render(ctx, (t, i) => i); } };
"#;
    let entry = in_project("x.syr");
    let r = render(src, &entry, &opts()).unwrap();
    assert_eq!(r.channels, 2);
    assert_eq!(r.samples[2], 1.0);
    assert_eq!(r.samples[3], 1.0);
}

#[test]
fn name_is_absent_unless_declared() {
    let src = "export const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: (ctx) => [] };";
    let info = inspect(src, "/some/where/thing.syr", &opts()).unwrap();
    assert_eq!(info.meta.name, None, "the contract gives a name no default");
    assert_eq!(info.meta.channels, 1);
    assert_eq!(info.meta.sample_rate, None);
    assert!(info.dependencies.is_empty());
    let named = src.replace("api: 4,", "api: 4, name: \"x\",");
    assert_eq!(inspect(&named, "thing.syr", &opts()).unwrap().meta.name.as_deref(), Some("x"));
}

#[test]
fn api_is_required_and_exactly_four() {
    let body = "\nexport const stems = { a: (ctx) => [] };";
    assert!(inspect(&format!("export const meta = {{ api: 4, duration: 0.01 }};{body}"), "x.syr", &opts()).is_ok());
    let missing = inspect(&format!("export const meta = {{ duration: 0.01 }};{body}"), "x.syr", &opts()).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::Contract);
    assert_eq!(missing.message, "meta.api is required: this compiler provides api 4 and accepts 4 to 4");
    for (api, shown) in [("3", "3"), ("5", "5"), ("4.5", "4.5"), ("\"4\"", "4"), ("null", "null")] {
        let e = inspect(&format!("export const meta = {{ api: {api}, duration: 0.01 }};{body}"), "x.syr", &opts()).unwrap_err();
        assert_eq!(e.message, format!("source declares meta.api {shown} but this compiler provides api 4 and accepts 4 to 4"), "api {api}");
    }
}

#[test]
fn seeds_are_unsigned_32_bit_integers() {
    let body = "\nexport const stems = { a: (ctx) => [] };";
    for (seed, accepted) in [
        ("0", Some(0u32)),
        ("4294967295", Some(u32::MAX)),
        ("1e0", Some(1)),
        ("4294967296", None),
        ("-1", None),
        ("0.5", None),
        ("NaN", None),
        ("Infinity", None),
    ] {
        let r = inspect(&format!("export const meta = {{ api: 4, duration: 0.01, seed: {seed} }};{body}"), "x.syr", &opts());
        match (accepted, r) {
            (Some(want), Ok(info)) => assert_eq!(info.meta.seed, want, "seed {seed}"),
            (None, Err(e)) => assert_eq!(e.message, "meta.seed must be an integer in [0, 4294967295]", "seed {seed}"),
            (want, got) => panic!("seed {seed}: expected {want:?}, got {got:?}"),
        }
    }
    let e = inspect(&format!("export const meta = {{ api: 4, duration: 0.01, seed: \"7\" }};{body}"), "x.syr", &opts()).unwrap_err();
    assert_eq!(e.message, "meta.seed must be a number");
}

#[test]
fn inspect_computes_the_geometry() {
    let src = "export const meta = { api: 4, duration: 0.5, sampleRate: 44100 };\nexport const stems = { a: (ctx) => [] };";
    let info = inspect(src, "x.syr", &opts()).unwrap();
    assert_eq!((info.sample_rate, info.frames), (44_100, 22_050));
    let forced = inspect(src, "x.syr", &RenderOptions { sample_rate: Some(8_000), ..opts() }).unwrap();
    assert_eq!((forced.sample_rate, forced.frames), (8_000, 4_000));
    let zero = "export const meta = { api: 4, duration: 1e-9 };\nexport const stems = { a: (ctx) => [] };";
    let e = inspect(zero, "x.syr", &opts()).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Contract, "meta.duration rounds to zero frames"));
}

/// The most common mistake moving a 0.x source to 1.0: a framework name taken from the core.
#[test]
fn a_framework_name_from_the_core_is_a_compile_error_naming_it() {
    let src = "import { Osc } from \"syrinx\";\nexport const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: (ctx) => [] };";
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Compile);
    assert!(e.message.contains("Osc"), "{}", e.message);
}

#[test]
fn rejects_math_random_before_running() {
    let src = "import { render } from \"./framework/dsp.js\";\nexport const meta = { api: 4, duration: 0.01 };\nexport const stems = {\n  a(ctx) {\n    return render(ctx, () => Math.random());\n  },\n};";
    let entry = in_project("x.syr");
    let e = render(src, &entry, &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Check);
    let (line, column) = position_of(src, "Math.random");
    assert_eq!((e.file.as_deref(), e.line, e.column), (Some(entry.as_str()), Some(line), Some(column)));
}

#[test]
fn syntax_error_is_positioned() {
    let src = "export const meta = { api: 4, duration: 0.01 };\nexport const stems = {\n  a(ctx) { return 1 +; },\n};";
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Compile);
    assert_eq!(e.line, Some(3));
    assert_eq!(e.file.as_deref(), Some("x.syr"));
}

#[test]
fn runtime_throw_is_positioned_in_source() {
    let src = "export const meta = { api: 4, duration: 0.01 };\nexport const stems = {\n  a(ctx) {\n    throw new Error(\"boom\");\n  },\n};";
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Runtime);
    assert_eq!(e.line, Some(position_of(src, "throw new Error").0));
    assert_eq!(e.file.as_deref(), Some("x.syr"));
    assert!(e.message.contains("boom"), "{}", e.message);
}

#[test]
fn top_level_throw_is_positioned() {
    let src = "export const meta = { api: 4, duration: 0.01 };\nconst x = undefined;\nx.y;\nexport const stems = { a: (ctx) => [] };";
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Runtime);
    assert_eq!(e.line, Some(3));
}

#[test]
fn harness_errors_carry_no_source_position() {
    let src = "export const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: (ctx) => 'nope' };";
    let e = render(src, "x.syr", &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Runtime);
    assert_eq!(e.line, None);
    assert!(e.message.contains("must return"), "{}", e.message);
    assert!(e.message.contains("stem \"a\""), "the message must say which layer: {}", e.message);
}

#[test]
fn infinite_loop_times_out() {
    let src = "export const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: (ctx) => { for (;;) {} } };";
    let started = std::time::Instant::now();
    let e = render(src, "x.syr", &RenderOptions { timeout: Duration::from_millis(300), ..opts() }).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn the_mix_stage_has_a_watchdog_too() {
    let src = r#"
export const meta = { api: 4, duration: 0.01, channels: 1 };
export const stems = { a(ctx) { return new Float32Array(ctx.frames); } };
export default function (ctx) { for (;;) {} }
"#;
    let started = std::time::Instant::now();
    let e = render(src, "x.syr", &RenderOptions { timeout: Duration::from_millis(300), ..opts() }).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The budget belongs to the render, not to each layer. With far more layers than cores they run
/// in batches; a per-layer timeout would restart the clock for every batch, so the whole thing
/// would take batches x budget instead of one budget.
#[test]
fn the_timeout_is_a_budget_for_the_whole_render() {
    let mut src = String::from("export const meta = { api: 4, duration: 0.01, channels: 1 };\nexport const stems = {\n");
    for i in 0..256 {
        src.push_str(&format!("  s{i}(ctx) {{ for (;;) {{}} }},\n"));
    }
    src.push_str("};\n");

    let budget = Duration::from_millis(200);
    let started = std::time::Instant::now();
    let e = render(&src, "x.syr", &RenderOptions { timeout: budget, ..opts() }).unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(e.kind, ErrorKind::Timeout);
    assert!(
        elapsed < budget * 5,
        "256 spinning layers took {elapsed:?} against a {budget:?} budget: the deadline is not shared"
    );
}

#[test]
fn contract_errors() {
    let cases: &[(&str, &str)] = &[
        ("export const stems = { a: (ctx) => [] };", "meta"),
        ("export const meta = { api: 4, duration: 0.01 };", "`stems` export"),
        ("export const meta = { api: 4, duration: 0.01 };\nexport default function (ctx) { return []; }", "`stems` export"),
        ("export const meta = { api: 4, duration: 0.01 };\nexport const stems = 5;", "object of functions"),
        ("export const meta = { api: 4, duration: 0.01 };\nexport const stems = {};", "is empty"),
        ("export const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: 5 };", "is not a function"),
        ("export const meta = { api: 4, duration: -1 };\nexport const stems = { a: (ctx) => [] };", "duration"),
        ("export const meta = { api: 4, duration: 0.01, channels: 3 };\nexport const stems = { a: (ctx) => [] };", "channels"),
        (
            "export const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: (ctx) => [new Float32Array(1), new Float32Array(1)] };",
            "channel",
        ),
        ("export const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: (ctx) => [NaN] };", "non-finite"),
        (
            "export const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: (ctx) => [] };\nexport default 5;",
            "not a function",
        ),
    ];
    for (src, expect) in cases {
        let e = render(src, "x.syr", &opts()).unwrap_err();
        assert!(e.message.contains(expect), "{src:?} -> {}", e.message);
    }
}

#[test]
fn imports_relative_modules_and_reports_them() {
    let dir = scratch("imports");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    syrinx_core::framework::vendor(&dir.join("framework"), "test").unwrap();
    std::fs::write(
        dir.join("lib/tone.js"),
        "import { Osc } from \"../framework/dsp.js\";\nexport function tone(sr, f) { const o = Osc.sine(sr); return () => o.next(f); }\nexport const GAIN = 0.25;\n",
    )
    .unwrap();
    std::fs::write(dir.join("lib/util.js"), "export { GAIN } from \"./tone.js\";\nexport const twice = (x) => x * 2;\n").unwrap();
    let entry = dir.join("beep.syr");
    let src = r#"
import { render } from "./framework/dsp.js";
import { tone, GAIN } from "./lib/tone.js";
import { twice } from "./lib/util.js";
export const meta = { api: 4, duration: 0.05 };
export const stems = {
  beep(ctx) {
    const t = tone(ctx.sr, 1000);
    return render(ctx, () => t() * twice(GAIN));
  },
};
"#;
    std::fs::write(&entry, src).unwrap();

    let r = render(src, entry.to_str().unwrap(), &RenderOptions { root: Some(dir.clone()), ..opts() }).unwrap();
    assert!((r.peak() - 0.5).abs() < 1e-3);
    let mut deps = r.dependencies.clone();
    deps.sort();
    let mut expected = vec![
        std::fs::canonicalize(dir.join("lib/tone.js")).unwrap(),
        std::fs::canonicalize(dir.join("lib/util.js")).unwrap(),
        std::fs::canonicalize(dir.join("framework/dsp.js")).unwrap(),
    ];
    expected.sort();
    assert_eq!(deps, expected, "diamond import must load tone.js once, and the framework once for both");

    assert_eq!(inspect(src, entry.to_str().unwrap(), &opts()).unwrap().dependencies.len(), 3);
}

#[test]
fn import_outside_root_is_rejected() {
    let dir = scratch("jail");
    std::fs::create_dir_all(dir.join("project")).unwrap();
    std::fs::write(dir.join("secret.js"), "export const x = 1;\n").unwrap();
    let entry = dir.join("project/a.syr");
    let src = "import { x } from \"../secret.js\";\nexport const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: () => [x] };";
    std::fs::write(&entry, src).unwrap();

    let e = render(src, entry.to_str().unwrap(), &RenderOptions { root: Some(dir.join("project")), ..opts() }).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Compile);
    let project = std::fs::canonicalize(dir.join("project")).unwrap();
    let secret = std::fs::canonicalize(dir.join("secret.js")).unwrap();
    assert_eq!(e.message, format!("cannot import \"../secret.js\": {} is outside the project root {}", secret.display(), project.display()));
    assert_eq!(e.file.as_deref(), Some(project.join("a.syr").to_str().unwrap()), "the error names the module that imported");

    // Without a root it is allowed.
    assert!(render(src, entry.to_str().unwrap(), &opts()).is_ok());
}

#[test]
fn bare_specifiers_and_missing_files_are_errors() {
    let dir = scratch("bare");
    let entry = dir.join("a.syr");
    let bare = "import x from \"lodash\";\nexport const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: () => [] };";
    std::fs::write(&entry, bare).unwrap();
    let e = render(bare, entry.to_str().unwrap(), &opts()).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Compile, "cannot import \"lodash\": only \"syrinx\" and relative paths can be imported"));

    let missing = "import x from \"./nope.js\";\nexport const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: () => [] };";
    let e = render(missing, entry.to_str().unwrap(), &opts()).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Compile, "cannot import \"./nope.js\": no such file"));
}

#[test]
fn check_failure_in_an_import_names_that_file() {
    let dir = scratch("checkdep");
    std::fs::write(dir.join("bad.js"), "export const now = () => Date.now();\n").unwrap();
    let entry = dir.join("a.syr");
    let src = "import { now } from \"./bad.js\";\nexport const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: () => [now()] };";
    std::fs::write(&entry, src).unwrap();
    let e = render(src, entry.to_str().unwrap(), &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Check);
    assert!(e.file.as_deref().unwrap().ends_with("bad.js"), "{:?}", e.file);
    assert_eq!(e.line, Some(1));
}

#[test]
fn runtime_error_inside_an_import_names_that_file() {
    let dir = scratch("throwdep");
    std::fs::write(dir.join("boom.js"), "export function boom() {\n  throw new Error(\"inside\");\n}\n").unwrap();
    let entry = dir.join("a.syr");
    let src = "import { boom } from \"./boom.js\";\nexport const meta = { api: 4, duration: 0.01 };\nexport const stems = { a: () => { boom(); } };";
    std::fs::write(&entry, src).unwrap();
    let e = render(src, entry.to_str().unwrap(), &opts()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Runtime);
    assert!(e.file.as_deref().unwrap().ends_with("boom.js"), "{:?}", e.file);
    assert_eq!(e.line, Some(2));
}

#[test]
fn renders_concurrently_from_many_threads() {
    let handles: Vec<_> = (0..8)
        .map(|_| std::thread::spawn(|| render(SINE, &in_project("sine.syr"), &opts()).unwrap().samples))
        .collect();
    let first = handles.into_iter().map(|h| h.join().unwrap()).reduce(|a, b| {
        assert_eq!(a, b);
        a
    });
    assert!(first.is_some());
}
