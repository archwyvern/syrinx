fn main() {
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target == "linux" || target == "android" {
        // Bind every internal reference at link time so nothing loaded alongside this library
        // can interpose on its V8 (a host process with its own V8, such as Node, is the normal
        // case). rustc's generated version script still exports the v8 crate's `#[no_mangle]`
        // shims (`v8__*`, `crdtp__*`) alongside `syrinx_*`; a second version script and
        // --exclude-libs do not override it. Those exports are inert for callers, and only a
        // second rusty_v8-based library that is itself not -Bsymbolic could bind to them.
        println!("cargo:rustc-cdylib-link-arg=-Wl,-Bsymbolic");
    } else if target == "macos" {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-exported_symbol,_syrinx_*");
    }
}
