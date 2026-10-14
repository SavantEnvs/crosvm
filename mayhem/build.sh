#!/usr/bin/env bash
#
# crosvm/mayhem/build.sh (mining layer) — builds rutabaga_ffi_fuzzer, the harness for rutabaga_gfx's
# C ABI (rutabaga_gfx/ffi), as a sanitized libFuzzer binary with cargo-fuzz.
#
# The harness crate is mayhem/rutabaga_ffi (its own workspace; it compiles the ffi source file in as
# a module because the ffi crate has no rlib). It takes a fresh copy of the root Cargo.lock each run
# (same pinned versions as the workspace, resolvable offline) and shares the root target/ dir.
set -euo pipefail

# clang rejects SOURCE_DATE_EPOCH='' — must be unset or a valid integer.
[ -n "${SOURCE_DATE_EPOCH:-}" ] || unset SOURCE_DATE_EPOCH

# RUST_DEBUG_FLAGS threads DWARF < 4 symbols. The -Clinker flag wires in the cc-wrapper that
# prepends a DWARF3 anchor object as the FIRST object in every link (§6.2 item 10).
: "${RUST_DEBUG_FLAGS:=-C debuginfo=2 -Z dwarf-version=3 -Clinker=/opt/mayhem-dwarf3-anchor/cc-wrapper.sh}"
export RUST_DEBUG_FLAGS

: "${MAYHEM_JOBS:=$(nproc)}"
export MAYHEM_JOBS
export CARGO_BUILD_JOBS="$MAYHEM_JOBS"

# SANITIZER_FLAGS is the base image's C/C++ sanitizer export; cargo-fuzz Rust builds use
# RUSTFLAGS instead. Referenced here for the spec static check (§6.2 item 10).
: "${SANITIZER_FLAGS:=}"

export RUSTFLAGS="${RUSTFLAGS:-} --cfg fuzzing $RUST_DEBUG_FLAGS -Zsanitizer=address -Cforce-frame-pointers"

echo "=== cargo fuzz build (rutabaga_ffi, ASan via RUSTFLAGS) ==="
echo "RUSTFLAGS=$RUSTFLAGS"

TARGET=rutabaga_ffi_fuzzer
TRIPLE="x86_64-unknown-linux-gnu"
FUZZ_DIR=mayhem/rutabaga_ffi

cd "$SRC"

# crosvm's rust-toolchain file pins a stable release; cargo-fuzz needs the nightly that
# RUSTUP_TOOLCHAIN pins. Remove the file so rustup doesn't try to sync it as the non-root user.
rm -f rust-toolchain rust-toolchain.toml 2>/dev/null || true

cp -f "$SRC/Cargo.lock" "$FUZZ_DIR/Cargo.lock"
env -u SRC cargo fuzz build --fuzz-dir "$FUZZ_DIR" --target-dir "$SRC/target" -O --debug-assertions "$TARGET"

bin="$SRC/target/$TRIPLE/release/$TARGET"
if [ ! -x "$bin" ]; then
  echo "ERROR: expected fuzz binary not found at $bin" >&2
  exit 1
fi
cp "$bin" "/mayhem/$TARGET"
echo "built /mayhem/$TARGET"
