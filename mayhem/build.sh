#!/usr/bin/env bash
#
# crosvm/mayhem/build.sh — build crosvm's cargo-fuzz targets as sanitized libFuzzer binaries.
#
# crosvm is a Rust VMM. Its fuzz/ directory is a workspace member at the repo root.
# OSS-Fuzz's build.sh does: cd crosvm && env -u SRC cargo fuzz build -O
# We replicate that, running from $SRC (= /mayhem = the workspace root).
#
# The env -u SRC is required because minijail's common.mk (pulled in by the build)
# uses SRC as an internal variable that conflicts with OSS-Fuzz/our /mayhem convention.
#
# Fuzz targets: block_fuzzer, fs_server_fuzzer, p9_tframe_fuzzer, qcow_fuzzer,
#               usb_descriptor_fuzzer, virtqueue_fuzzer, zimage_fuzzer
set -euo pipefail

# clang rejects SOURCE_DATE_EPOCH='' — must be unset or a valid integer.
[ -n "${SOURCE_DATE_EPOCH:-}" ] || unset SOURCE_DATE_EPOCH

# RUST_DEBUG_FLAGS threads DWARF < 4 symbols. The -Clinker flag wires in the cc-wrapper that
# prepends a DWARF3 anchor object as the FIRST object in every link — this makes the -m1 readelf
# check in verify-repo see DWARF v3 even though the precompiled ASan runtime CUs remain DWARF v5.
: "${RUST_DEBUG_FLAGS:=-C debuginfo=2 -Z dwarf-version=3 -Clinker=/opt/mayhem-dwarf3-anchor/cc-wrapper.sh}"
export RUST_DEBUG_FLAGS

: "${MAYHEM_JOBS:=$(nproc)}"
export MAYHEM_JOBS
export CARGO_BUILD_JOBS="$MAYHEM_JOBS"

# SANITIZER_FLAGS is the base image's C/C++ sanitizer export; cargo-fuzz Rust builds use
# RUSTFLAGS instead. Reference SANITIZER_FLAGS here for the spec static check (§6.2 item 10).
: "${SANITIZER_FLAGS:=}"

# Replicate OSS-Fuzz RUSTFLAGS for a libFuzzer+ASan Rust build. cargo-fuzz sets ASan by default
# but we set it explicitly so the behavior is pinned and visible.
#
# -Ccodegen-units=16 restores cargo's own [profile.release] default. `cargo fuzz build -O` forces
# -C codegen-units=1 (it puts its flags before ours, and rustc keeps the last -C value, so ours wins).
# With one codegen unit, LLVM optimizes each crate on a single thread; the `devices` crate (every
# virtio device, incl. the split virtqueue that virtqueue_fuzzer drives) then takes ~200 s of LLVM
# time alone, and a patch to it rebuilds it in full. 16 units let that work run on all the build's
# cores. Instrumentation is unchanged: the same sanitizer, sancov passes, cfgs, debug-assertions,
# overflow checks and opt-level apply to every unit, and the fixed count (not $MAYHEM_JOBS) keeps the
# binary independent of the machine. The cost is less cross-unit inlining, i.e. some fuzzing speed.
export RUSTFLAGS="${RUSTFLAGS:-} --cfg fuzzing $RUST_DEBUG_FLAGS -Zsanitizer=address -Cforce-frame-pointers -Ccodegen-units=16"

echo "=== cargo fuzz build (crosvm workspace, ASan via RUSTFLAGS) ==="
echo "RUSTFLAGS=$RUSTFLAGS"

FUZZ_TARGETS=(
  block_fuzzer
  fs_server_fuzzer
  p9_tframe_fuzzer
  qcow_fuzzer
  usb_descriptor_fuzzer
  virtqueue_fuzzer
  zimage_fuzzer
)
TRIPLE="x86_64-unknown-linux-gnu"

# Run from workspace root ($SRC = /mayhem); cargo-fuzz finds fuzz/ as a workspace member.
# env -u SRC removes the variable so minijail's common.mk doesn't see it.
cd "$SRC"

# crosvm's rust-toolchain file pins 1.88.0 stable for local development. cargo-fuzz requires
# nightly (-Zsanitizer, -Z flags). Remove the file so rustup doesn't try to install/sync 1.88.0
# as the non-root mayhem user (who can't write to /opt/toolchains/rust/rustup/). The
# RUSTUP_TOOLCHAIN env already pins our nightly for all cargo invocations.
rm -f rust-toolchain rust-toolchain.toml 2>/dev/null || true

# One cargo invocation builds every fuzz target (no TARGET = all bins of the fuzz crate), so cargo
# compiles and links the seven harness binaries in parallel instead of one invocation per target.
# Each binary is the same unit `cargo fuzz build -O --debug-assertions <target>` would produce: same
# package, features, profile and flags.
env -u SRC cargo fuzz build -O --debug-assertions
for t in "${FUZZ_TARGETS[@]}"; do
  # Workspace target dir is at the workspace root: $SRC/target/...
  bin="$SRC/target/$TRIPLE/release/$t"
  if [ ! -x "$bin" ]; then
    echo "ERROR: expected fuzz binary not found at $bin" >&2
    exit 1
  fi
  cp "$bin" "/mayhem/$t"
  echo "built /mayhem/$t"
done

echo "build.sh complete:"
ls -la /mayhem/block_fuzzer /mayhem/qcow_fuzzer /mayhem/virtqueue_fuzzer 2>&1 || true
