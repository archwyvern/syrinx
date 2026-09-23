//! The syrinx framework, embedded: `framework/` of the repository at the commit this was built
//! from. A project takes a copy (`syrinx framework <dir>`) and imports it by relative path
//! (SPEC.md, clause 13). No host evaluates it on its own account: to a host it is just more of the
//! project's source.

use std::path::Path;

/// One file of the framework.
#[derive(Debug, Clone, Copy)]
pub struct File {
    /// Relative to the framework's root, `/`-separated.
    pub path: &'static str,
    pub text: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/framework.rs"));

/// What vendoring did to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendored {
    /// It was not there.
    Written,
    /// It was there with other contents, and now has these.
    Updated,
    /// It was there already, byte for byte.
    Unchanged,
}

/// A framework file's text, by its path relative to the framework's root.
pub fn file(path: &str) -> Option<&'static str> {
    FILES.iter().find(|f| f.path == path).map(|f| f.text)
}

/// Writes the framework into `dir`: every file, then `VERSION` (`syrinx-framework <version>`).
/// A file that differs is overwritten, and reported; nothing is ever deleted, so a project's own
/// files beside the framework are safe, and a file a later framework drops stays until the
/// project removes it.
pub fn vendor(dir: &Path, version: &str) -> std::io::Result<Vec<(String, Vendored)>> {
    let stamp = format!("syrinx-framework {version}\n");
    let all = FILES.iter().map(|f| (f.path, f.text)).chain(std::iter::once(("VERSION", stamp.as_str())));
    let mut report = Vec::with_capacity(FILES.len() + 1);
    for (path, text) in all {
        let target = dir.join(path);
        let state = match std::fs::read(&target) {
            Ok(existing) if existing == text.as_bytes() => Vendored::Unchanged,
            Ok(_) => Vendored::Updated,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vendored::Written,
            Err(e) => return Err(e),
        };
        if state != Vendored::Unchanged {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, text)?;
        }
        report.push((path.to_string(), state));
    }
    Ok(report)
}
