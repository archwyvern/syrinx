//! Embeds the framework: every file under ../../framework, as (path, include_str!) pairs in
//! `$OUT_DIR/framework.rs`, sorted by path. `src/framework.rs` includes it.

use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read framework/") {
        let path = entry.expect("a framework entry").path();
        if path.is_dir() {
            // A new or removed file anywhere under the framework changes what is embedded.
            println!("cargo:rerun-if-changed={}", path.display());
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../../framework").canonicalize().expect("framework/ exists beside crates/");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut rows: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|p| (p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), p))
        .collect();
    rows.sort();
    let mut code = String::from("/// Every file of the framework, sorted by path.\npub const FILES: &[File] = &[\n");
    for (rel, abs) in &rows {
        println!("cargo:rerun-if-changed={}", abs.display());
        code.push_str(&format!("    File {{ path: {rel:?}, text: include_str!({:?}) }},\n", abs.display().to_string()));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("framework.rs");
    std::fs::write(out, code).expect("write framework.rs");
}
