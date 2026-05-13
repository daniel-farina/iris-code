#!/usr/bin/env bash
# Run all install.sh test suites. Used by CI and locally.
#
# Exit 0 if all suites pass; non-zero otherwise.

set -u

DIR="$(cd "$(dirname "$0")" && pwd)"
overall_rc=0

echo "════════════════════════════════════════════════════════════"
echo "  install.sh tests"
echo "════════════════════════════════════════════════════════════"
echo

# Static check: shellcheck if available (warn-only; install.sh is POSIX sh
# but shellcheck has good catches even so).
if command -v shellcheck >/dev/null 2>&1; then
    echo "── shellcheck install.sh ──"
    if ! shellcheck -s sh "$DIR/../../install.sh"; then
        echo "shellcheck reported issues (continuing anyway)"
    else
        echo "  ok  no shellcheck warnings"
    fi
    echo
else
    echo "(skipping shellcheck — not installed)"
    echo
fi

# Unit tests
echo "── unit tests ──"
if bash "$DIR/test_unit.sh"; then
    echo
else
    overall_rc=1
fi

# E2E tests
echo
echo "── e2e tests ──"
if bash "$DIR/test_e2e.sh"; then
    echo
else
    overall_rc=1
fi

echo "════════════════════════════════════════════════════════════"
if [ "$overall_rc" = "0" ]; then
    echo "  ALL INSTALL TESTS PASSED"
else
    echo "  install tests FAILED"
fi
echo "════════════════════════════════════════════════════════════"
exit "$overall_rc"
