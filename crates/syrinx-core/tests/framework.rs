//! The framework as the crate carries it: every file of `framework/`, and a vendoring that writes,
//! updates and never deletes.

use std::path::{Path, PathBuf};

use syrinx_core::framework::{self, Vendored};

fn on_disk() -> Vec<String> {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../framework"));
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("syrinx-framework-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn every_file_of_the_framework_is_embedded() {
    let embedded: Vec<String> = framework::FILES.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(embedded, on_disk());
    assert!(framework::file("dsp.js").unwrap().contains("export {"));
    assert!(framework::file("no-such-file.js").is_none());
}

#[test]
fn vendoring_writes_updates_and_never_deletes() {
    let dir = scratch("vendor");
    let first = framework::vendor(&dir, "9.9.9").unwrap();
    assert_eq!(first.len(), framework::FILES.len() + 1);
    assert!(first.iter().all(|(_, v)| *v == Vendored::Written), "{first:?}");
    assert_eq!(std::fs::read_to_string(dir.join("VERSION")).unwrap(), "syrinx-framework 9.9.9\n");

    std::fs::write(dir.join("dsp.js"), "// edited here\n").unwrap();
    std::fs::write(dir.join("mine.js"), "export const x = 1;\n").unwrap();
    let second = framework::vendor(&dir, "9.9.9").unwrap();
    for (path, state) in &second {
        let want = if path == "dsp.js" { Vendored::Updated } else { Vendored::Unchanged };
        assert_eq!(*state, want, "{path}");
    }
    assert_eq!(std::fs::read_to_string(dir.join("dsp.js")).unwrap(), framework::file("dsp.js").unwrap());
    assert_eq!(
        std::fs::read_to_string(dir.join("mine.js")).unwrap(),
        "export const x = 1;\n",
        "a project's own file is never touched"
    );

    let third = framework::vendor(&dir, "9.9.10").unwrap();
    let changed: Vec<&str> = third.iter().filter(|(_, s)| *s != Vendored::Unchanged).map(|(p, _)| p.as_str()).collect();
    assert_eq!(changed, ["VERSION"], "a new version rewrites the stamp and nothing else of the same text");
}
