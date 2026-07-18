#!/usr/bin/env bash
#
# crosvm/mayhem/test.sh — run crosvm's functional suites and emit one CTRF summary.
# exit 0 iff no test failed.
#
# PATCH-grade oracle. Both suites assert concrete values (assert_eq! on bytes, indices and lengths),
# so a no-op / "exit(0)" / neutering patch to the code they cover fails them.
#
# Both suites compile the project crates under the same cfg set as the graded fuzz build (build.sh:
# `cargo fuzz build -O --debug-assertions` = [profile.release] panic = 'abort', --cfg fuzzing,
# -Zsanitizer=address, debug-assertions and overflow-checks on), so a patch that is only live in that
# build (#[cfg(fuzzing)], #[cfg(panic = "abort")], #[cfg(sanitize = "address")]) is live here too and
# cannot disable the code under test for the fuzzer alone. -Zpanic-abort-tests lets libtest run
# panic=abort tests (each in its own process, so a failing assertion is still reported as one failed
# test). --target keeps those flags off build scripts and proc-macros, as cargo-fuzz does. cargo
# compiles the project crates incrementally into target/x86_64-unknown-linux-gnu/debug (the image
# build runs this script once, so only a patched crate is rebuilt at grade time). The KAT crate also
# resolves the fuzz graph's crate features (disk/qcow, see its Cargo.toml), and the target dir is
# passed as --target-dir, not CARGO_TARGET_DIR: build.sh does not set that variable, so exporting it
# here would let option_env!() tell the two builds apart.
#
# The host crates (build scripts, proc-macros and their dependencies, which run at build time) get
# the same cfg set as in the fuzz build too. Under --target they are built with the profile's
# build-override settings instead of RUSTFLAGS. In the fuzz build that is [profile.release] with
# cargo's default build-override (opt-level 0): debug-assertions off, overflow-checks on (set in
# [profile.release]). The test profile's default has debug-assertions on, so a proc-macro could read
# its own cfg!(debug_assertions) at expansion time and emit code that is only live in the fuzz build.
# HOST_CONFIG turns it off for this script's host units. It is a `cargo --config` argument, not an
# exported CARGO_PROFILE_* variable, which option_env!() could see. With it every host unit of both
# suites has the same opt-level, codegen-units, debug-assertions, overflow-checks, panic (unwind) and
# lto as in the fuzz build (compared with cargo --unit-graph); the KAT graph's host units also resolve
# the same features (the disk suite's smaller graph builds serde and syn with fewer features).
#
#  1. `cargo test -p disk` — the disk crate's own unit tests. With the crate's default features
#     (qcow, composite-disk and android-sparse are feature-gated off) these are the 4
#     disk::sys::linux tests: async read/write through a raw image and raw image-type detection.
#     `-p disk` rather than --workspace avoids the VM-only crates' build requirements.
#  2. mayhem/virtqueue_kat — known-answer tests for the split virtqueue avail -> pop -> used path
#     (devices/src/virtio/queue/), the code virtqueue_fuzzer drives. They are ours, not upstream's:
#     the disk suite never compiles the devices crate, and the devices crate's own lib suite still
#     passes in full with SplitQueue::peek neutered, so neither can see that target. They use
#     well-formed, spec-aligned rings only, so any honest handling of malformed rings passes them;
#     two of them place the rings at exactly the spec's minimum alignment (16 / 2 / 4 bytes), so an
#     over-strict "fix" (e.g. page-aligned rings only) fails.
set -uo pipefail
[ -n "${SOURCE_DATE_EPOCH:-}" ] || unset SOURCE_DATE_EPOCH

: "${MAYHEM_JOBS:=$(nproc)}"
cd "$SRC"

# The graded fuzz build's cfg set (see the header). No debuginfo/linker flags: they set no cfg.
TEST_RUSTFLAGS="--cfg fuzzing -Cpanic=abort -Zpanic-abort-tests -Zsanitizer=address"
# The fuzz build's host-unit profile (see the header): debug-assertions off for build scripts and
# proc-macros, as in [profile.release.build-override].
HOST_CONFIG="profile.dev.build-override.debug-assertions=false"
TRIPLE="x86_64-unknown-linux-gnu"

# emit_ctrf <tool> <passed> <failed> [skipped] [pending] [other]
emit_ctrf() {
  local tool="$1" passed="$2" failed="$3" skipped="${4:-0}" pending="${5:-0}" other="${6:-0}"
  local tests=$(( passed + failed + skipped + pending + other ))
  cat > "${CTRF_REPORT:-$SRC/ctrf-report.json}" <<JSON
{
  "results": {
    "tool": { "name": "$tool" },
    "summary": {
      "tests": $tests,
      "passed": $passed,
      "failed": $failed,
      "pending": $pending,
      "skipped": $skipped,
      "other": $other
    }
  }
}
JSON
  printf 'CTRF {"results":{"tool":{"name":"%s"},"summary":{"tests":%d,"passed":%d,"failed":%d,"pending":%d,"skipped":%d,"other":%d}}}\n' \
    "$tool" "$tests" "$passed" "$failed" "$pending" "$skipped" "$other"
  [ "$failed" -eq 0 ]
}

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not available — cannot run the test suite" >&2
  emit_ctrf "cargo-test" 0 1 0; exit 2
fi

PASSED=0; FAILED=0; IGNORED=0

# run_suite <label> <cmd...> — run one `cargo test` invocation and add its counts to the totals.
# libtest prints one line per test binary:
#   test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; ...
# A binary that never prints that line (compile error, a test binary killed by a signal) leaves no
# count behind, so a non-zero cargo exit with no parsed failure counts as one failure, and so does
# an invocation that exits 0 without reporting a single test.
run_suite() {
  local label="$1"; shift
  local out rc p f i sp=0 sf=0 si=0
  echo "=== $label ==="
  out="$("$@" 2>&1)"; rc=$?
  printf '%s\n' "$out"
  while read -r p f i; do
    sp=$(( sp + p )); sf=$(( sf + f )); si=$(( si + i ))
  done < <(printf '%s\n' "$out" \
    | sed -n 's/^test result:.* \([0-9][0-9]*\) passed; \([0-9][0-9]*\) failed; \([0-9][0-9]*\) ignored.*/\1 \2 \3/p')
  if [ "$rc" -ne 0 ] && [ "$sf" -eq 0 ]; then
    echo "$label: cargo exited $rc but no failing test was parsed (build error or crashed test binary) — counting 1 failure" >&2
    sf=1
  elif [ "$(( sp + sf + si ))" -eq 0 ]; then
    echo "$label: cargo exited $rc but reported no tests — counting 1 failure" >&2
    sf=1
  fi
  echo "=== $label: $sp passed, $sf failed, $si ignored (cargo rc=$rc) ==="
  PASSED=$(( PASSED + sp )); FAILED=$(( FAILED + sf )); IGNORED=$(( IGNORED + si ))
}

# 1. disk crate unit tests.
run_suite "cargo test -p disk" \
  env RUSTFLAGS="$TEST_RUSTFLAGS" cargo test -p disk --target "$TRIPLE" --config "$HOST_CONFIG" --no-fail-fast --jobs "$MAYHEM_JOBS"

# 2. split-virtqueue known-answer tests. The crate is its own workspace, so it takes a fresh copy of
# the root Cargo.lock each run (same pinned versions as every other build, resolvable offline) and
# shares the root target/ dir (--target-dir, see the header). env -u SRC: minijail's common.mk (built
# via devices) uses SRC itself.
KAT=mayhem/virtqueue_kat
cp -f "$SRC/Cargo.lock" "$KAT/Cargo.lock"
run_suite "cargo test virtqueue_kat" \
  env -u SRC RUSTFLAGS="$TEST_RUSTFLAGS" \
  cargo test --manifest-path "$KAT/Cargo.toml" --target "$TRIPLE" --config "$HOST_CONFIG" \
    --target-dir "$SRC/target" --no-fail-fast --jobs "$MAYHEM_JOBS"

emit_ctrf "cargo-test" "$PASSED" "$FAILED" "$IGNORED"
