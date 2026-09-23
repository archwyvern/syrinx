//! The standard on its own: a fresh isolate with the math and the prelude evaluated, for the
//! checks that read what the prelude (or a framework module) exports and what block size each file
//! declares.

use std::collections::HashMap;

use crate::{Error, ErrorKind, PRELUDE};

use super::loader::{LOADER, Loader, compile_registered, resolve_module};
use super::wrapper::run_object;
use super::{MAX_HEAP_BYTES, PRELUDE_SPECIFIER, caught_in, get, init_v8, install_standard_math, on_own_thread};

/// The names the prelude actually exports at run time.
///
/// Used to verify the type declarations against the real module: the documentation is only
/// trustworthy if the two cannot drift apart.
pub fn prelude_exports() -> Result<Vec<String>, Error> {
    module_exports(PRELUDE_SPECIFIER, PRELUDE)
}

/// The names a module exports at run time, evaluated in a fresh isolate after the standard math
/// with the prelude behind `"syrinx"` -- how a framework module's declarations are checked. The
/// module may import the core and nothing else.
pub fn module_exports(specifier: &str, code: &str) -> Result<Vec<String>, Error> {
    on_own_thread(|| {
        with_module(code, specifier, |scope, namespace| {
            let names = namespace
                .get_own_property_names(scope, v8::GetPropertyNamesArgsBuilder::new().build())
                .ok_or_else(|| Error::internal(format!("cannot enumerate the exports of {specifier}")))?;
            let mut out = Vec::new();
            for i in 0..names.length() {
                if let Some(key) = names.get_index(scope, i).filter(|k| k.is_string()) {
                    out.push(key.to_rust_string_lossy(scope));
                }
            }
            out.sort();
            Ok(out)
        })
    })
}

/// The block size the prelude and the run wrapper each declare, so a test can pin both to
/// [`BLOCK_FRAMES`]: the constant lives in three places and must not drift in any of them.
#[doc(hidden)]
pub fn standard_block_frames() -> Result<(usize, usize), Error> {
    on_own_thread(|| {
        with_module(PRELUDE, PRELUDE_SPECIFIER, |scope, namespace| {
            let prelude = get(scope, namespace, "BLOCK_FRAMES")
                .and_then(|v| v.number_value(scope))
                .ok_or_else(|| Error::contract("the prelude exports no BLOCK_FRAMES"))?;
            let run = run_object(scope)?;
            let wrapper = get(scope, run, "BLOCK_FRAMES")
                .and_then(|v| v.number_value(scope))
                .ok_or_else(|| Error::contract("the run wrapper carries no BLOCK_FRAMES"))?;
            Ok((prelude as usize, wrapper as usize))
        })
    })
}

/// A fresh isolate with the standard math evaluated, then `code` as a module named `specifier`
/// (the prelude itself, or a module importing it), and `body` over that module's namespace.
fn with_module<T>(
    code: &str,
    specifier: &str,
    body: impl for<'a, 'b> FnOnce(&mut v8::PinScope<'a, 'b>, v8::Local<'a, v8::Object>) -> Result<T, Error>,
) -> Result<T, Error> {
    init_v8();
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default().heap_limits(0, MAX_HEAP_BYTES));
    LOADER.with(|l| {
        *l.borrow_mut() = Some(Loader {
            root: None,
            modules: HashMap::new(),
            origins: HashMap::new(),
            dependencies: Vec::new(),
            failure: None,
        })
    });
    struct Uninstall;
    impl Drop for Uninstall {
        fn drop(&mut self) {
            LOADER.with(|l| *l.borrow_mut() = None);
        }
    }
    let _uninstall = Uninstall;

    v8::scope!(let handle_scope, isolate);
    let context = v8::Context::new(handle_scope, Default::default());
    let scope = &mut v8::ContextScope::new(handle_scope, context);
    install_standard_math(scope)?;
    let namespace = {
        v8::tc_scope!(let tc, scope);
        let module = compile_registered(tc, code, specifier, None).ok_or_else(|| caught_in(tc, ErrorKind::Compile))?;
        // The resolve callback hands `"syrinx"` the prelude before it asks where the importer lives,
        // so a module compiled without a path may import the core; anything relative fails.
        if module.instantiate_module(tc, resolve_module).is_none() {
            return Err(caught_in(tc, ErrorKind::Compile));
        }
        if module.evaluate(tc).is_none() {
            return Err(caught_in(tc, ErrorKind::Runtime));
        }
        v8::Local::<v8::Object>::try_from(module.get_module_namespace())
            .map_err(|_| Error::internal(format!("{specifier} namespace is not an object")))?
    };
    body(scope, namespace)
}
