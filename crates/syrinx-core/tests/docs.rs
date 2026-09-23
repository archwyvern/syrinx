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
/// change to this list is a change to the standard, made on purpose.
#[test]
fn core_module_is_exactly_the_tagged_declarations() {
    let docs = syrinx_core::docs::docs().expect("declarations and prelude agree");
    let mut core: Vec<&str> =
        docs.groups.iter().flat_map(|g| g.entries.iter()).filter(|e| e.core).map(|e| e.name.as_str()).collect();
    core.sort_unstable();
    let mut expected = vec![
        "BLOCK_FRAMES", "Context", "Meta", "MixContext", "MixStream", "Output", "PRELUDE_VERSION", "Random", "Source",
        "StemContext", "Stems", "Stream", "fade", "hash", "mix", "normalize", "place", "render", "stream",
    ];
    expected.sort_unstable();
    assert_eq!(core, expected);
    // The tag is metadata, not prose: it never reaches a reader.
    assert!(docs.groups.iter().flat_map(|g| g.entries.iter()).all(|e| !e.doc.contains("@core")));
}
