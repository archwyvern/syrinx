#!/usr/bin/env bash
# libsyrinx with V8 EXCLUDED: the v8 crate linked against a prebuilt shared librusty_v8 (what
# native-bin's Archwyvern.Natives.V8 ships, made from the same rusty_v8 release Cargo.toml pins
# exactly) instead of embedding rusty_v8's static archive. Beside it, a process holds one V8 for
# every Rust native that uses the crate. The CLI and the plain library (`make`) stay static and
# self-contained; this is a third output, not a change to them.
#
# The crate has no switch for dynamic linking, so its build script is patched
# (tools/v8-shared/build.rs.patch, eleven lines). The patch goes onto a vendored copy of the crate
# under target/shared, never onto the repository, and the build reads the copy through cargo's
# source replacement, so Cargo.lock is untouched and `cargo build` elsewhere never sees it.
#
#   RUSTY_V8_DYLIB_DIR=<dir> tools/v8-shared.sh                                   host target
#   RUSTY_V8_DYLIB_DIR=<dir> TARGET=x86_64-pc-windows-msvc tools/v8-shared.sh      cross, via cargo-xwin
#
# <dir> holds librusty_v8.so, or rusty_v8.lib (the import library) for the Windows target.
# RUSTY_V8_SRC_BINDING_PATH (the crate's bindgen output for the target, published beside the
# archive on rusty_v8's release) passes through; unset, the crate downloads it once.
# PROFILE (release) and CARGO (cargo) as in the Makefile. VENDOR_ONLY=1 stops after the vendoring
# step, for a build that then runs without network (native-bin's recipe builds inside an image).
#
# Output: dist/lib-shared/libsyrinx.so (syrinx.dll). On Linux the library carries an $ORIGIN run
# path, so librusty_v8.so beside it is found with no loader configuration. The library checks at
# first use that the V8 it loaded is the one it was built for (syrinx_core::V8_VERSION) and
# refuses otherwise; on Linux the loader refuses earlier still, by symbol version.
set -euo pipefail
cd "$(dirname "$0")/.."

: "${RUSTY_V8_DYLIB_DIR:?set RUSTY_V8_DYLIB_DIR to the directory holding librusty_v8.so (rusty_v8.lib on Windows)}"
RUSTY_V8_DYLIB_DIR=$(cd "$RUSTY_V8_DYLIB_DIR" && pwd)
PROFILE=${PROFILE:-release}
TARGET=${TARGET:-}
CARGO=${CARGO:-cargo}
OUT=$PWD/target/shared

v8_version=$(awk '/^name = "v8"$/ {found = 1; next} found && /^version = / {gsub(/"/, "", $3); print $3; exit}' Cargo.lock)
[ -n "$v8_version" ] || { echo "v8-shared: the v8 crate is not in Cargo.lock" >&2; exit 1; }

# Vendor every crate once per Cargo.lock, then patch the v8 crate's build script in the copy and
# drop that file from the copy's checksum list (cargo verifies vendored files by hash, and a
# patched file would fail it).
mkdir -p "$OUT/.cargo"
if [ ! -f "$OUT/.cargo/config.toml" ] || ! cmp -s Cargo.lock "$OUT/Cargo.lock.vendored"; then
    echo "v8-shared: vendoring crates into target/shared/vendor"
    rm -rf "$OUT/vendor" "$OUT/.cargo/config.toml"
    $CARGO vendor --locked --versioned-dirs "$OUT/vendor" > "$OUT/.cargo/config.toml"
    crate="$OUT/vendor/v8-$v8_version"
    [ -d "$crate" ] || { echo "v8-shared: $crate is not in the vendor tree" >&2; exit 1; }
    patch --silent -p1 -d "$crate" < tools/v8-shared/build.rs.patch
    python3 - "$crate/.cargo-checksum.json" <<'EOF'
import json, sys
path = sys.argv[1]
checksums = json.load(open(path))
checksums["files"].pop("build.rs", None)
json.dump(checksums, open(path, "w"))
EOF
    cp Cargo.lock "$OUT/Cargo.lock.vendored"
fi
[ -z "${VENDOR_ONLY:-}" ] || exit 0

# The build runs from target/shared so that cargo finds the source replacement in
# target/shared/.cargo/config.toml and nothing above it; paths are absolute for the same reason.
rustflags=${RUSTFLAGS:-}
build=("$CARGO" build)
lib=libsyrinx.so
lib_dir="$OUT/$PROFILE"
case "$TARGET" in
    "")
        case "$(uname -s)" in
            Linux)  rustflags="$rustflags -C link-arg=-Wl,-rpath,\$ORIGIN" ;;
            Darwin) rustflags="$rustflags -C link-arg=-Wl,-rpath,@loader_path"; lib=libsyrinx.dylib ;;
            *) echo "v8-shared: unsupported host $(uname -s)" >&2; exit 1 ;;
        esac ;;
    *-windows-msvc)
        # The static CRT, as the static build (see README, Windows): the DLL then imports system
        # DLLs only. Cross-building from Linux goes through cargo-xwin and the shims it needs.
        rustflags="$rustflags -C target-feature=+crt-static"
        lib=syrinx.dll
        lib_dir="$OUT/$TARGET/$PROFILE"
        if [ "${OS:-}" != Windows_NT ]; then
            build=("$CARGO" xwin build)
            export PATH="$PWD/tools/xwin-bin:$PATH"
        fi ;;
    *) echo "v8-shared: unsupported TARGET $TARGET (host, or x86_64-pc-windows-msvc)" >&2; exit 1 ;;
esac

echo "v8-shared: building syrinx-ffi against $RUSTY_V8_DYLIB_DIR${TARGET:+ for $TARGET}"
(cd "$OUT" && env \
    RUSTFLAGS="$rustflags" \
    RUSTY_V8_DYLIB_DIR="$RUSTY_V8_DYLIB_DIR" \
    "${build[@]}" "--$PROFILE" --locked --offline -p syrinx-ffi \
        --manifest-path "$OLDPWD/Cargo.toml" --target-dir "$OUT" ${TARGET:+--target "$TARGET"})

mkdir -p dist/lib-shared
cp "$lib_dir/$lib" dist/lib-shared/
echo
ls -la "dist/lib-shared/$lib"
case "$lib" in
    *.so) readelf -d "dist/lib-shared/$lib" | grep -E 'NEEDED|RUNPATH|RPATH' ;;
esac
