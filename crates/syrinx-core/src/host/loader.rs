//! Module loading: `"syrinx"` is the built-in prelude, relative specifiers are files under the
//! project root, nothing else exists. Every user module in the graph goes through the static
//! determinism check before it is compiled, and a module imported twice is one instance.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{Error, PRELUDE};

use super::PRELUDE_SPECIFIER;

/// Per-isolate loader state. Lives in a thread-local because V8's resolve callback cannot
/// capture anything, and each isolate has its own thread.
pub(super) struct Loader {
    /// Imports may not escape this directory. `None` = no jail.
    pub(super) root: Option<PathBuf>,
    /// Compiled modules by canonical path, so a module imported twice is one instance.
    pub(super) modules: HashMap<PathBuf, v8::Global<v8::Module>>,
    /// Module identity hash -> the path it was loaded from, for resolving its relative imports.
    /// The prelude maps to `None`: it cannot import anything.
    pub(super) origins: HashMap<i32, Option<PathBuf>>,
    /// Every user file loaded through an import, in load order. Excludes the entry module.
    pub(super) dependencies: Vec<PathBuf>,
    /// An error raised inside the resolve callback, carried out so the caller reports it with
    /// its own kind and position instead of V8's generic "module not found" exception.
    pub(super) failure: Option<Error>,
}

thread_local! {
    pub(super) static LOADER: RefCell<Option<Loader>> = const { RefCell::new(None) };
}

pub(super) fn with_loader<T>(f: impl FnOnce(&mut Loader) -> T) -> T {
    LOADER.with(|l| f(l.borrow_mut().as_mut().expect("loader installed")))
}

fn module_origin<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::ScriptOrigin<'s> {
    let name = v8::String::new(scope, name).unwrap();
    v8::ScriptOrigin::new(scope, name.into(), 0, 0, false, 0, None, false, false, true, None)
}

/// Compiles `code` as a module named `origin_name` and registers it with the loader.
pub(super) fn compile_registered<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    code: &str,
    origin_name: &str,
    path: Option<PathBuf>,
) -> Option<v8::Local<'s, v8::Module>> {
    let code = v8::String::new(scope, code)?;
    let origin = module_origin(scope, origin_name);
    let mut source = v8::script_compiler::Source::new(code, Some(&origin));
    let module = v8::script_compiler::compile_module(scope, &mut source)?;
    let global = v8::Global::new(scope, module);
    with_loader(|l| {
        l.origins.insert(module.get_identity_hash().get(), path.clone());
        if let Some(p) = path {
            l.modules.insert(p, global);
        }
    });
    Some(module)
}

pub(super) fn throw_str(scope: &mut v8::PinScope<'_, '_>, message: &str) {
    let msg = v8::String::new(scope, message).unwrap();
    let exc = v8::Exception::error(scope, msg);
    scope.throw_exception(exc);
}

/// V8 asks for `specifier` as imported from `referrer`.
pub(super) fn resolve_module<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    _import_attributes: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    v8::callback_scope!(unsafe scope, context);
    let spec = specifier.to_rust_string_lossy(scope);

    if spec == PRELUDE_SPECIFIER {
        // One prelude instance per isolate, keyed under a path that cannot collide with a file.
        let key = PathBuf::from("\0syrinx");
        if let Some(existing) = with_loader(|l| l.modules.get(&key).cloned()) {
            return Some(v8::Local::new(scope, &existing));
        }
        return compile_registered(scope, PRELUDE, PRELUDE_SPECIFIER, Some(key));
    }

    let referrer_path = with_loader(|l| l.origins.get(&referrer.get_identity_hash().get()).cloned().flatten());
    let Some(referrer_path) = referrer_path else {
        // Only the prelude has no path, and the prelude imports nothing.
        throw_str(scope, &format!("cannot import \"{spec}\" from the prelude"));
        return None;
    };

    if !(spec.starts_with("./") || spec.starts_with("../")) {
        let e = Error::contract(format!(
            "cannot import \"{spec}\": only \"{PRELUDE_SPECIFIER}\" and relative paths (./x.js, ../lib/y.syr) can be imported"
        ));
        return fail(scope, e);
    }

    let base = referrer_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let candidate = base.join(&spec);
    let resolved = match std::fs::canonicalize(&candidate) {
        Ok(p) => p,
        Err(err) => {
            let e = Error::contract(format!("cannot import \"{spec}\" from {}: {err}", referrer_path.display()));
            return fail(scope, e);
        }
    };
    let jailed = with_loader(|l| l.root.clone());
    if let Some(root) = jailed
        && !resolved.starts_with(&root)
    {
        let e = Error::contract(format!(
            "cannot import \"{spec}\" from {}: {} is outside the project root {}",
            referrer_path.display(),
            resolved.display(),
            root.display()
        ));
        return fail(scope, e);
    }

    if let Some(existing) = with_loader(|l| l.modules.get(&resolved).cloned()) {
        return Some(v8::Local::new(scope, &existing));
    }

    let code = match std::fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(err) => {
            let e = Error::contract(format!("cannot read {}: {err}", resolved.display()));
            return fail(scope, e);
        }
    };
    let diagnostics = crate::check::check(&code);
    if !diagnostics.is_empty() {
        let mut e = Error::check(diagnostics);
        e.file = Some(resolved.to_string_lossy().into_owned());
        return fail(scope, e);
    }

    with_loader(|l| l.dependencies.push(resolved.clone()));
    let name = resolved.to_string_lossy().into_owned();
    compile_registered(scope, &code, &name, Some(resolved))
    // A compile error here surfaces through the instantiate TryCatch with the file's own
    // resource name, which is what the caller wants.
}

/// Records `e` as the reason resolution failed and throws so V8 aborts instantiation.
fn fail<'s>(scope: &mut v8::PinScope<'s, '_>, e: Error) -> Option<v8::Local<'s, v8::Module>> {
    let message = e.message.clone();
    with_loader(|l| {
        l.failure.get_or_insert(e);
    });
    throw_str(scope, &message);
    None
}
