#!/usr/bin/env bash
#
# crosvm/mayhem/test.sh (mining layer) — run the rutabaga C-ABI known-answer tests and emit one CTRF
# summary. exit 0 iff no test failed.
#
# mayhem/rutabaga_ffi/tests/ffi_kat.rs asserts concrete values (pixel bytes of a create_3d ->
# attach_backing -> transfer_write -> transfer_read round trip, error codes of rejected calls), so a
# no-op / "exit(0)" / neutering patch to the code it covers fails it. It compiles under the same cfg
# set as the graded fuzz build (build.sh: --cfg fuzzing, -Zsanitizer=address, debug-assertions on,
# [profile.release] panic = 'abort' + overflow-checks). -Zpanic-abort-tests lets libtest run
# panic=abort tests (each in its own process). --target keeps those flags off build scripts and
# proc-macros, as cargo-fuzz does; --target-dir (not CARGO_TARGET_DIR, which option_env!() could see)
# shares the root target/ dir. HOST_CONFIG gives host units the fuzz build's debug-assertions=off.
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

# rutabaga_ffi is its own workspace, so it takes a fresh copy of the root Cargo.lock each run (same
# pinned versions as every other build, resolvable offline). env -u SRC: kept as in build.sh.
KAT=mayhem/rutabaga_ffi
cp -f "$SRC/Cargo.lock" "$KAT/Cargo.lock"
run_suite "cargo test rutabaga_ffi" \
  env -u SRC RUSTFLAGS="$TEST_RUSTFLAGS" \
  cargo test --manifest-path "$KAT/Cargo.toml" --target "$TRIPLE" --config "$HOST_CONFIG" \
    --target-dir "$SRC/target" --no-fail-fast --jobs "$MAYHEM_JOBS"

emit_ctrf "cargo-test" "$PASSED" "$FAILED" "$IGNORED"
