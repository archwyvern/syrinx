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
