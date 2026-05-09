#!/usr/bin/env bash
# Run cargo test --release N times in a tight loop and report stability.
# Useful for catching parallel-test races on global-state mutations
# (env vars, static caches, singletons) before they bite in CI.
#
# Usage:
#   tools/check-flake.sh [N]
#
# Defaults to N=10. Exits 0 if all runs pass, 1 if any failed.
set -euo pipefail

N="${1:-10}"
if ! [[ "$N" =~ ^[0-9]+$ ]]; then
    echo "usage: $0 [N]" >&2
    exit 2
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "─ check-flake.sh ─ running cargo test --release $N time(s) in $REPO_ROOT"
echo

# Pre-build once so we measure test variance, not compile time.
cargo build --release --tests >/dev/null 2>&1

passed=0
failed=0
slowest_ms=0
for ((i=1; i<=N; i++)); do
    start_ns=$(date +%s%N 2>/dev/null || gdate +%s%N)
    out=$(cargo test --release 2>&1)
    end_ns=$(date +%s%N 2>/dev/null || gdate +%s%N)
    elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))
    if [[ $elapsed_ms -gt $slowest_ms ]]; then slowest_ms=$elapsed_ms; fi
    summary=$(echo "$out" | grep -E "^test result" | tail -1 || true)
    if echo "$summary" | grep -qE "^test result: ok\."; then
        printf "  run %2d/%d  PASS  (%dms)  %s\n" "$i" "$N" "$elapsed_ms" "$summary"
        passed=$((passed + 1))
    else
        printf "  run %2d/%d  FAIL  (%dms)  %s\n" "$i" "$N" "$elapsed_ms" "$summary"
        failed=$((failed + 1))
        # Show the first failing test name from the run, if any.
        echo "$out" | grep -E "^test .* FAILED|^---- " | head -3 | sed 's/^/         /'
    fi
done

echo
echo "─ summary ─ $passed/$N passed, $failed failed, slowest run ${slowest_ms}ms"
if [[ $failed -gt 0 ]]; then
    echo "  flake detected; consider adding a test-mod Mutex to serialize"
    echo "  parallel access to any process-global state the failing test touches."
    echo "  See feedback_env_var_test_mutex.md memory for the pattern."
    exit 1
fi
exit 0
