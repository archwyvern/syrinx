//! Windows icon and version information; the work lives in `syrinx-exe-resources`, shared with
//! every executable in the workspace.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    syrinx_exe_resources::embed("syrinx-player", "Plays syrinx sound sources.");
}
