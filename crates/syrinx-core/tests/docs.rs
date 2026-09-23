//! The committed reference JSON must match what the compiler would emit right now.

#[test]
fn committed_reference_is_current() {
    let docs = syrinx_core::docs::docs().expect("declarations and prelude agree");
    let generated = serde_json::to_string_pretty(&docs).unwrap() + "\n";
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/syrinx-docs.json");
    let committed = std::fs::read_to_string(path).expect("docs/syrinx-docs.json exists");
    assert_eq!(
        generated, committed,
        "docs/syrinx-docs.json is stale; regenerate it with `syrinx docs --out docs/syrinx-docs.json`"
    );
}

/// The boundary between the standard's core module and the framework (SPEC.md, clauses 12-13).
/// Moving a name across it changes what a conforming host must ship, so it is pinned here: a
/// change to either list is a change to the standard, made on purpose.
#[test]
fn the_core_and_the_framework_are_exactly_these() {
    let docs = syrinx_core::docs::docs().expect("declarations and modules agree");
    let names = |i: usize| {
        let mut names: Vec<&str> =
            docs.modules[i].groups.iter().flat_map(|g| g.entries.iter()).map(|e| e.name.as_str()).collect();
        names.sort_unstable();
        names
    };
    assert_eq!(docs.modules[0].module, "syrinx");
    let mut core = vec![
        "BLOCK_FRAMES",
        "Context",
        "Meta",
        "MixContext",
        "MixStream",
        "Output",
        "PRELUDE_VERSION",
        "Random",
        "Source",
        "StemContext",
        "Stems",
        "Stream",
        "hash",
        "inBlock",
    ];
    core.sort_unstable();
    assert_eq!(names(0), core);
    assert_eq!(docs.modules[1].module, "framework/dsp.js");
    let mut dsp = vec![
        "Allpass",
        "BiquadType",
        "Biquad",
        "BlepOsc",
        "Comb",
        "Delay",
        "Env",
        "Envelope",
        "Noise",
        "OnePole",
        "Osc",
        "Phasor",
        "Processor",
        "Reverb",
        "Shape",
        "Svf",
        "TAU",
        "clamp",
        "db",
        "fade",
        "filter",
        "fold",
        "gain",
        "hardclip",
        "lerp",
        "mix",
        "mtof",
        "normalize",
        "pan",
        "place",
        "render",
        "softclip",
        "stream",
    ];
    dsp.sort_unstable();
    assert_eq!(names(1), dsp);
}

/// What `"syrinx"` exports at run time is the core and nothing else (SPEC.md, clause 12). The
/// framework is imported by relative path; a name that crept back into the prelude would let a
/// source depend on the framework without saying so.
#[test]
fn the_core_module_exports_the_core() {
    let names = syrinx_core::prelude_exports().unwrap();
    assert_eq!(names, ["BLOCK_FRAMES", "PRELUDE_VERSION", "Random", "hash", "inBlock"]);
}
