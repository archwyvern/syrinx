//! Windows resources for the workspace's executables, called from their build scripts.
//!
//! A PE carries its icon and its version information as resources, which is what Explorer,
//! the taskbar and the Properties dialog read. An ELF has no equivalent -- a Linux desktop
//! takes an icon from a .desktop entry, not from the binary -- so this does nothing on any
//! other target.
//!
//! The icon is generated from the logo:
//!
//! ```text
//! magick -background none -density 512 logo/syrinx.svg \
//!     -define icon:auto-resize=256,128,64,48,32,16 logo/syrinx.ico
//! ```
//!
//! This is a build dependency, not a library: it depends on `winresource` being a PLAIN build
//! dependency of the caller's crate graph, never behind `[target.'cfg(windows)']`, because that
//! cfg selects on the HOST and a Windows binary cross-compiled from Linux would ship unbranded.

use std::path::{Path, PathBuf};
use std::process::Command;

const COPYRIGHT: &str = "Copyright (c) 2026 Dominic Grostate. MIT.";

/// Embeds the logo and a version block into the executable being built, when the target is
/// Windows. `exe_name` is the file name without `.exe`; `description` is what Explorer shows.
pub fn embed(exe_name: &str, description: &str) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo/syrinx.ico"));
    println!("cargo:rerun-if-changed={}", icon.display());

    // A native resource compiler is the straight path: rc.exe on Windows, llvm-rc anywhere.
    if cfg!(target_os = "windows") || which("llvm-rc").is_some() || which("rc").is_some() {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(icon.to_str().expect("icon path is UTF-8"));
        resource.set("ProductName", "syrinx");
        resource.set("FileDescription", description);
        resource.set("LegalCopyright", COPYRIGHT);
        resource.set("InternalName", &format!("{exe_name}.exe"));
        resource.set("OriginalFilename", &format!("{exe_name}.exe"));
        // A missing resource compiler must not silently produce an unbranded binary.
        resource.compile().unwrap_or_else(|e| panic!("cannot compile Windows resources: {e}"));
        return;
    }

    // Cross-compiling from a machine whose only resource compiler is mingw's. windres emits a
    // COFF .res, and the MSVC linker takes a .res on its command line, so the same icon and
    // version block still reach the executable.
    if let Some(windres) = which("x86_64-w64-mingw32-windres") {
        compile_with_windres(&windres, &icon, exe_name, description);
        return;
    }

    panic!(
        "no resource compiler for a Windows target: install llvm-rc (Fedora: dnf install llvm) \
         or mingw's windres (mingw64-binutils), or the executable would ship unbranded"
    );
}

fn compile_with_windres(windres: &Path, icon: &Path, exe_name: &str, description: &str) {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    // FILEVERSION wants four comma-separated numbers; a semver has three.
    let numeric = format!("{},0", version.replace('.', ","));

    let script = out.join(format!("{exe_name}.rc"));
    std::fs::write(
        &script,
        format!(
            r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x4
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{exe_name}.exe"
      VALUE "LegalCopyright", "{COPYRIGHT}"
      VALUE "OriginalFilename", "{exe_name}.exe"
      VALUE "ProductName", "syrinx"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            icon = icon.display(),
        ),
    )
    .expect("writing the resource script");

    let res = out.join(format!("{exe_name}.res"));
    let status = Command::new(windres)
        .args(["-O", "res", "-i"])
        .arg(&script)
        .arg("-o")
        .arg(&res)
        .status()
        .expect("running windres");
    assert!(status.success(), "windres failed on {}", script.display());

    // Binaries only: a .res on the link line of a test harness is neither wanted nor valid.
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

/// The first `name` on PATH, if any.
fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|dir| dir.join(name)).find(|candidate| candidate.is_file()))
}
