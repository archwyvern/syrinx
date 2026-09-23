//! The API reference, extracted from type declarations: the core module's (`prelude/syrinx.d.ts`,
//! [`TYPES`]) and the framework's (`framework/dsp.d.ts`).
//!
//! Each declarations file is the declared surface of one module, with a doc comment on every
//! export and `// ---- Name` markers dividing it into sections. [`docs`] turns them into structured
//! JSON for a documentation site to render, and checks each against its module's real exports so
//! the two cannot drift apart unnoticed. Which part of the reference an entry belongs to -- the
//! standard's core module (SPEC.md, clause 12) or the framework (clause 13) -- is decided by the
//! file it is declared in.
//!
//! The parser is deliberately strict: a line it does not recognise is an error, not a skip.
//! Silently dropping an export would produce documentation that is quietly incomplete, which is
//! worse than none.

use serde::Serialize;

use crate::{Error, API_FLOOR, BLOCK_FRAMES, PRELUDE_VERSION, TYPES};

/// Version of this JSON shape. 3 replaced the `core` flag on entries with modules, each a part of
/// the reference: the core, or the framework.
pub const DOCS_SCHEMA: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    /// A class: constructed with `new`, or used through its static methods.
    Class,
    /// An exported object whose members are called on it, like `Env.ad(...)`.
    Object,
    /// A free function.
    Function,
    /// An exported value.
    Constant,
    /// A shape that exists only in the types (`Meta`, `Context`).
    Interface,
    /// A function type (`Envelope`, `Shape`).
    Callback,
    /// A union of string literals: [`Entry::values`] holds them.
    Choice,
    /// Any other type alias.
    Alias,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MemberKind {
    Constructor,
    Method,
    Property,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub kind: MemberKind,
    pub name: String,
    /// The declaration as written, without `static` or the trailing semicolon.
    pub signature: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub doc: String,
    /// Called on the type itself rather than an instance.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_static: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub kind: EntryKind,
    pub name: String,
    /// The declaration as written: the whole thing for a one-liner, the header for a block.
    pub signature: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub doc: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Member>,
    /// The alternatives of a [`EntryKind::Choice`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

impl Entry {
    /// Whether this entry exists at run time. Interfaces and type aliases do not.
    pub fn is_value(&self) -> bool {
        matches!(self.kind, EntryKind::Class | EntryKind::Object | EntryKind::Function | EntryKind::Constant)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub name: String,
    pub entries: Vec<Entry>,
}

/// Which part of the reference a module is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Part {
    /// The standard's core module: what a conforming host must know about (SPEC.md, clause 12).
    Core,
    /// The framework: a library a project vendors, never required of a host (SPEC.md, clause 13).
    Framework,
}

/// One declarations file, parsed: the module's summary and its sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Declarations {
    /// Prose from the top of the declarations file.
    pub summary: String,
    pub groups: Vec<Group>,
}

/// One module's reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleDocs {
    /// How a source reaches it: `"syrinx"`, or the framework path relative to where a project
    /// vendored it (`framework/dsp.js`).
    pub module: String,
    pub part: Part,
    pub summary: String,
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Docs {
    pub schema: u32,
    /// What produced this file.
    pub generator: &'static str,
    /// The implementation these docs came from.
    pub version: &'static str,
    /// The source-contract version these docs describe (`meta.api`).
    pub api: u32,
    /// The oldest `meta.api` a host accepts; a source may declare anything from here to `api`.
    pub api_floor: u32,
    /// Frames per block of a stream, a constant of the standard.
    pub block_frames: usize,
    /// The core module first, then the framework's.
    pub modules: Vec<ModuleDocs>,
}

/// Extracts the reference and verifies every module's declarations against its real exports.
pub fn docs() -> Result<Docs, Error> {
    let core = verified("syrinx", Part::Core, parse(TYPES)?, crate::host::prelude_exports()?)?;
    let file = |path: &str| {
        crate::framework::file(path).ok_or_else(|| Error::internal(format!("framework/{path} is not embedded")))
    };
    let dsp = verified(
        "framework/dsp.js",
        Part::Framework,
        parse(file("dsp.d.ts")?)?,
        crate::host::module_exports("framework/dsp.js", file("dsp.js")?)?,
    )?;
    Ok(Docs {
        schema: DOCS_SCHEMA,
        generator: "syrinx",
        version: env!("CARGO_PKG_VERSION"),
        api: PRELUDE_VERSION,
        api_floor: API_FLOOR,
        block_frames: BLOCK_FRAMES,
        modules: vec![core, dsp],
    })
}

/// The module's reference, once its declared values are exactly its exports.
fn verified(module: &str, part: Part, declared: Declarations, actual: Vec<String>) -> Result<ModuleDocs, Error> {
    let values: Vec<&str> =
        declared.groups.iter().flat_map(|g| g.entries.iter()).filter(|e| e.is_value()).map(|e| e.name.as_str()).collect();
    let missing: Vec<&str> = actual.iter().filter(|a| !values.contains(&a.as_str())).map(String::as_str).collect();
    let extra: Vec<&str> = values.iter().filter(|d| !actual.iter().any(|a| a == *d)).copied().collect();
    if !missing.is_empty() || !extra.is_empty() {
        let mut message = format!("the type declarations of {module} and the module disagree:");
        if !missing.is_empty() {
            message.push_str(&format!("\n  exported but undeclared: {}", missing.join(", ")));
        }
        if !extra.is_empty() {
            message.push_str(&format!("\n  declared but not exported: {}", extra.join(", ")));
        }
        return Err(Error::contract(message));
    }
    Ok(ModuleDocs { module: module.to_string(), part, summary: declared.summary, groups: declared.groups })
}

/// Parses one declarations file. Public for tests; [`docs`] is the entry point.
pub fn parse(source: &str) -> Result<Declarations, Error> {
    let mut groups: Vec<Group> = Vec::new();
    let mut summary: Vec<String> = Vec::new();
    let mut doc = String::new();
    let mut lines = source.lines().enumerate().peekable();

    while let Some((no, raw)) = lines.next() {
        let line = raw.trim();
        let at = no + 1;

        if line.is_empty() {
            continue;
        }
        if let Some(name) = section_name(line) {
            groups.push(Group { name: name.to_string(), entries: Vec::new() });
            continue;
        }
        if line.starts_with("//") {
            // Leading prose is the module's own summary; a stray comment later is just a note.
            if groups.is_empty() {
                summary.push(line.trim_start_matches('/').trim().to_string());
            }
            continue;
        }
        // A type-only import (the framework's declarations name the core's `Context`) declares
        // nothing of its own.
        if groups.is_empty() && line.starts_with("import type ") && line.ends_with(';') {
            continue;
        }
        if line.starts_with("/**") {
            doc = read_doc_comment(line, &mut lines, at)?;
            continue;
        }

        let Some(group) = groups.last_mut() else {
            return Err(syntax(at, line, "a declaration before the first `// ---- Section` marker"));
        };
        let entry = read_entry(line, &mut lines, at, std::mem::take(&mut doc))?;
        group.entries.push(entry);
    }

    if groups.is_empty() {
        return Err(Error::contract("type declarations contain no `// ---- Section` markers"));
    }
    Ok(Declarations { summary: summary.join("\n").trim().to_string(), groups })
}

type Lines<'a> = std::iter::Peekable<std::iter::Enumerate<std::str::Lines<'a>>>;

fn syntax(line_no: usize, text: &str, what: &str) -> Error {
    Error::contract(format!("declarations:{line_no}: cannot parse {what}: {text}"))
}

/// `// ---- Name` introduces a section.
fn section_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("//")?.trim_start();
    let rest = rest.trim_start_matches('-');
    let trimmed = rest.trim();
    if line.trim_start_matches("//").trim_start().starts_with("----") && !trimmed.is_empty() { Some(trimmed) } else { None }
}

/// Reads `/** ... */`, one line or many, and returns the prose inside.
fn read_doc_comment(first: &str, lines: &mut Lines<'_>, at: usize) -> Result<String, Error> {
    if let Some(inner) = first.strip_prefix("/**").and_then(|r| r.strip_suffix("*/")) {
        return Ok(inner.trim().to_string());
    }
    let mut parts: Vec<String> = Vec::new();
    let head = first.trim_start_matches("/**").trim();
    if !head.is_empty() {
        parts.push(head.to_string());
    }
    for (_, raw) in lines.by_ref() {
        let line = raw.trim();
        if line == "*/" || line.ends_with("*/") {
            let tail = line.trim_end_matches("*/").trim().trim_start_matches('*').trim();
            if !tail.is_empty() {
                parts.push(tail.to_string());
            }
            return Ok(parts.join("\n").trim().to_string());
        }
        parts.push(line.trim_start_matches('*').trim().to_string());
    }
    Err(syntax(at, first, "an unterminated doc comment"))
}

/// True when `text` has as many closing brackets as opening ones.
fn balanced(text: &str) -> bool {
    let mut depth = 0i32;
    let mut in_string = None::<char>;
    for c in text.chars() {
        match in_string {
            Some(q) if c == q => in_string = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' | '`' => in_string = Some(c),
                '{' | '(' | '[' => depth += 1,
                '}' | ')' | ']' => depth -= 1,
                _ => {}
            },
        }
    }
    depth == 0
}

fn read_entry(first: &str, lines: &mut Lines<'_>, at: usize, doc: String) -> Result<Entry, Error> {
    let Some(rest) = first.strip_prefix("export ") else {
        return Err(syntax(at, first, "a top-level declaration (expected `export ...`)"));
    };

    // A block declaration: `export class X {`, `export interface X {`, `export const X: {`.
    if first.ends_with('{') {
        let (kind, name) = if let Some(r) = rest.strip_prefix("class ") {
            (EntryKind::Class, identifier(r))
        } else if let Some(r) = rest.strip_prefix("interface ") {
            (EntryKind::Interface, identifier(r))
        } else if let Some(r) = rest.strip_prefix("const ") {
            (EntryKind::Object, identifier(r))
        } else {
            return Err(syntax(at, first, "a block declaration"));
        };
        let members = read_members(lines, at)?;
        // `export const Env: {` heads an object literal; the colon belongs to the block that
        // was just consumed, not to the declaration a reader sees.
        let signature = first.trim_end_matches('{').trim().trim_end_matches(':').trim().to_string();
        return Ok(Entry { kind, name: name.to_string(), signature, doc, members, values: Vec::new() });
    }

    // A single declaration, possibly wrapped over several lines until its semicolon.
    let mut text = first.to_string();
    while !(text.ends_with(';') && balanced(&text)) {
        let Some((_, next)) = lines.next() else {
            return Err(syntax(at, first, "a declaration with no terminating semicolon"));
        };
        text.push(' ');
        text.push_str(next.trim());
    }
    let signature = text.trim_end_matches(';').trim().to_string();
    let rest = signature.strip_prefix("export ").unwrap_or(&signature);

    if let Some(r) = rest.strip_prefix("function ") {
        return Ok(Entry { kind: EntryKind::Function, name: identifier(r).to_string(), signature, doc, members: Vec::new(), values: Vec::new() });
    }
    if let Some(r) = rest.strip_prefix("const ") {
        return Ok(Entry { kind: EntryKind::Constant, name: identifier(r).to_string(), signature, doc, members: Vec::new(), values: Vec::new() });
    }
    if let Some(r) = rest.strip_prefix("type ") {
        let name = identifier(r).to_string();
        let body = r.split_once('=').map(|(_, b)| b.trim()).unwrap_or_default();
        let values = string_literals(body);
        let kind = if !values.is_empty() {
            EntryKind::Choice
        } else if body.contains("=>") {
            EntryKind::Callback
        } else {
            EntryKind::Alias
        };
        return Ok(Entry { kind, name, signature, doc, members: Vec::new(), values });
    }
    Err(syntax(at, first, "a top-level declaration"))
}

/// Members up to the block's closing brace.
fn read_members(lines: &mut Lines<'_>, block_at: usize) -> Result<Vec<Member>, Error> {
    let mut members = Vec::new();
    let mut doc = String::new();

    while let Some((no, raw)) = lines.next() {
        let line = raw.trim();
        let at = no + 1;
        if line.is_empty() {
            continue;
        }
        if line == "}" || line == "};" {
            return Ok(members);
        }
        if line.starts_with("/**") {
            doc = read_doc_comment(line, lines, at)?;
            continue;
        }
        if line.starts_with("//") {
            continue;
        }

        let mut text = line.to_string();
        while !(text.ends_with(';') && balanced(&text)) {
            let Some((_, next)) = lines.next() else {
                return Err(syntax(at, line, "a member with no terminating semicolon"));
            };
            text.push(' ');
            text.push_str(next.trim());
        }
        let text = text.trim_end_matches(';').trim().to_string();
        let (is_static, declaration) = match text.strip_prefix("static ") {
            Some(r) => (true, r.trim().to_string()),
            None => (false, text),
        };
        let declaration = declaration.strip_prefix("readonly ").map_or(declaration.clone(), |r| r.trim().to_string());
        let name = identifier(&declaration).to_string();
        if name.is_empty() {
            return Err(syntax(at, line, "a member"));
        }
        // `name?: string` and `foo?(): void` are optional members; the mark is not part of
        // the classification.
        let after = declaration[name.len()..].trim_start().trim_start_matches('?').trim_start();
        let kind = if name == "constructor" {
            MemberKind::Constructor
        } else if after.starts_with('(') || after.starts_with('<') {
            MemberKind::Method
        } else if after.starts_with(':') {
            MemberKind::Property
        } else {
            return Err(syntax(at, line, "a member"));
        };
        members.push(Member { kind, name, signature: declaration, doc: std::mem::take(&mut doc), is_static });
    }
    Err(syntax(block_at, "", "a block with no closing brace"))
}

/// The leading identifier of a declaration, without any generic parameters.
fn identifier(text: &str) -> &str {
    let text = text.trim_start();
    let end = text.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).unwrap_or(text.len());
    &text[..end]
}

/// The double-quoted literals of a union type, in order.
fn string_literals(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        out.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dsp() -> Declarations {
        parse(crate::framework::file("dsp.d.ts").unwrap()).unwrap()
    }

    fn entry<'a>(declarations: &'a Declarations, name: &str) -> &'a Entry {
        declarations.groups.iter().flat_map(|g| g.entries.iter()).find(|e| e.name == name).expect(name)
    }

    #[test]
    fn parses_the_shipped_declarations() {
        let core = parse(TYPES).unwrap();
        assert!(core.summary.starts_with("The syrinx module"), "{}", core.summary);
        let names: Vec<&str> = core.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["The source contract", "Constants", "Seeds", "Randomness", "The block protocol"]);

        let dsp = dsp();
        assert!(dsp.summary.starts_with("The syrinx framework's dsp module"), "{}", dsp.summary);
        let names: Vec<&str> = dsp.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            ["Scalars", "Oscillators", "Noise", "Envelopes", "Filters", "Delays and reverb", "Buffers"]
        );
    }

    #[test]
    fn reads_a_class_with_static_and_instance_members() {
        let dsp = dsp();
        let biquad = entry(&dsp, "Biquad");
        assert_eq!(biquad.kind, EntryKind::Class);
        assert!(biquad.doc.starts_with("RBJ cookbook biquad"), "{}", biquad.doc);
        let ctor = &biquad.members[0];
        assert_eq!(ctor.kind, MemberKind::Constructor);
        assert_eq!(ctor.signature, "constructor(sr: number)");
        let lowpass = biquad.members.iter().find(|m| m.name == "lowpass").unwrap();
        assert!(lowpass.is_static);
        assert_eq!(lowpass.signature, "lowpass(sr: number, freq: number, q?: number): Biquad");
        let process = biquad.members.iter().find(|m| m.name == "process").unwrap();
        assert!(!process.is_static);
        assert_eq!(process.kind, MemberKind::Method);
    }

    #[test]
    fn reads_properties_generics_and_object_literals() {
        let dsp = dsp();
        let osc = entry(&dsp, "Osc");
        let width = osc.members.iter().find(|m| m.name == "width").unwrap();
        assert_eq!(width.kind, MemberKind::Property);
        // A property whose type is an object literal stays on one line.
        let shapes = osc.members.iter().find(|m| m.name == "shapes").unwrap();
        assert_eq!(shapes.kind, MemberKind::Property);
        assert!(shapes.is_static);
        assert!(shapes.signature.contains("{ sine: Shape"), "{}", shapes.signature);
        let core = parse(TYPES).unwrap();
        let random = entry(&core, "Random");
        let pick = random.members.iter().find(|m| m.name == "pick").unwrap();
        assert_eq!(pick.kind, MemberKind::Method);
        assert_eq!(pick.signature, "pick<T>(array: readonly T[]): T");
    }

    #[test]
    fn reads_functions_constants_interfaces_and_type_aliases() {
        let dsp = dsp();
        let db = entry(&dsp, "db");
        assert_eq!(db.kind, EntryKind::Function);
        assert_eq!(db.signature, "export function db(decibels: number): number");
        assert_eq!(db.doc, "Decibels to linear gain.");

        assert_eq!(entry(&dsp, "TAU").kind, EntryKind::Constant);
        assert_eq!(entry(&dsp, "Env").kind, EntryKind::Object);
        assert_eq!(entry(&dsp, "Env").signature, "export const Env");
        assert_eq!(entry(&dsp, "Env").members.len(), 7);
        assert_eq!(entry(&dsp, "Envelope").kind, EntryKind::Callback);

        let core = parse(TYPES).unwrap();
        assert_eq!(entry(&core, "Meta").kind, EntryKind::Interface);
        assert_eq!(entry(&core, "Output").kind, EntryKind::Alias);
        assert_eq!(entry(&core, "inBlock").kind, EntryKind::Function);

        // A wrapped union of string literals becomes a choice with its alternatives.
        let biquad_type = entry(&dsp, "BiquadType");
        assert_eq!(biquad_type.kind, EntryKind::Choice);
        assert_eq!(biquad_type.values, ["lowpass", "highpass", "bandpass", "notch", "allpass", "peak", "lowshelf", "highshelf"]);
    }

    #[test]
    fn every_declaration_lands_in_a_group() {
        for source in [TYPES, crate::framework::file("dsp.d.ts").unwrap()] {
            let declarations = parse(source).unwrap();
            let entries: usize = declarations.groups.iter().map(|g| g.entries.len()).sum();
            // One per `export` in the declarations file.
            assert_eq!(entries, source.lines().filter(|l| l.starts_with("export ")).count());
        }
    }

    #[test]
    fn rejects_what_it_cannot_parse() {
        let bad = "// ---- X\nexport enum Colours { Red }\n";
        assert!(parse(bad).unwrap_err().message.contains("cannot parse"));
        let orphan = "export const x: number;\n";
        assert!(parse(orphan).unwrap_err().message.contains("before the first"));
        let unterminated = "// ---- X\nexport class A {\n  foo(): void;\n";
        assert!(parse(unterminated).unwrap_err().message.contains("closing brace"));
        let late_import = "// ---- X\nimport type { A } from \"syrinx\";\n";
        assert!(parse(late_import).unwrap_err().message.contains("cannot parse"), "an import inside a section is not skipped");
    }

    #[test]
    fn declarations_match_the_modules() {
        // The real check: every runtime export documented, and nothing documented that is not
        // exported, in the core and in the framework. Fails when a module and its declarations
        // drift apart.
        let docs = docs().unwrap();
        assert_eq!(docs.modules.iter().map(|m| (m.module.as_str(), m.part)).collect::<Vec<_>>(), [
            ("syrinx", Part::Core),
            ("framework/dsp.js", Part::Framework)
        ]);
        let values = |m: &ModuleDocs| -> Vec<String> {
            m.groups.iter().flat_map(|g| g.entries.iter()).filter(|e| e.is_value()).map(|e| e.name.clone()).collect()
        };
        assert!(values(&docs.modules[0]).contains(&"inBlock".to_string()));
        assert!(values(&docs.modules[1]).contains(&"normalize".to_string()));
    }
}
