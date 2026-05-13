#!/usr/bin/env bash
# Unit tests for install.sh's pure functions.
# Sources install.sh in HIPPO_TEST_DEFINE_ONLY mode, then exercises functions
# in isolation. No network. No filesystem mutations outside a tmpdir.
#
# Run: bash tools/install-tests/test_unit.sh
# Exit 0 if all assertions pass; 1 if any fail.

set -u

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
INSTALL_SCRIPT="$REPO_ROOT/install.sh"

PASS=0
FAIL=0
RESULTS=""

# assert <description> <actual> <expected>
assert_eq() {
    local desc="$1"; local actual="$2"; local expected="$3"
    if [ "$actual" = "$expected" ]; then
        PASS=$((PASS + 1))
        RESULTS="${RESULTS}  ok  ${desc}\n"
    else
        FAIL=$((FAIL + 1))
        RESULTS="${RESULTS}  FAIL ${desc}\n     expected: ${expected}\n     actual:   ${actual}\n"
    fi
}

# assert_rc <description> <command...> -- <expected_rc>
assert_rc() {
    local desc="$1"; shift
    local expected_rc="$1"; shift
    "$@" >/dev/null 2>&1
    local actual_rc=$?
    if [ "$actual_rc" = "$expected_rc" ]; then
        PASS=$((PASS + 1))
        RESULTS="${RESULTS}  ok  ${desc}\n"
    else
        FAIL=$((FAIL + 1))
        RESULTS="${RESULTS}  FAIL ${desc} (rc=${actual_rc}, expected ${expected_rc})\n"
    fi
}

# Source install.sh in define-only mode.
HIPPO_TEST_DEFINE_ONLY=1
# shellcheck disable=SC1090
. "$INSTALL_SCRIPT"
# install.sh enables `set -e` for its own runtime safety; in tests we need
# to allow expected non-zero return codes from helpers like detect_os when
# they legitimately reject unsupported platforms.
set +e

echo "── unit tests for $INSTALL_SCRIPT ──"
echo

# ========== detect_os ==========
HIPPO_MOCK_OS=Darwin
assert_eq "detect_os: Darwin → darwin"  "$(detect_os)"  "darwin"

HIPPO_MOCK_OS=DARWIN
assert_eq "detect_os: DARWIN → darwin"  "$(detect_os)"  "darwin"

HIPPO_MOCK_OS=Linux
assert_eq "detect_os: Linux → linux"    "$(detect_os)"  "linux"

HIPPO_MOCK_OS=FreeBSD
assert_rc "detect_os: FreeBSD → rc=1"   1               detect_os
unset HIPPO_MOCK_OS

# ========== detect_arch ==========
HIPPO_MOCK_ARCH=arm64
assert_eq "detect_arch: arm64 → arm64"     "$(detect_arch)"  "arm64"

HIPPO_MOCK_ARCH=aarch64
assert_eq "detect_arch: aarch64 → arm64"   "$(detect_arch)"  "arm64"

HIPPO_MOCK_ARCH=x86_64
assert_eq "detect_arch: x86_64 → x86_64"   "$(detect_arch)"  "x86_64"

HIPPO_MOCK_ARCH=amd64
assert_eq "detect_arch: amd64 → x86_64"    "$(detect_arch)"  "x86_64"

HIPPO_MOCK_ARCH=mips
assert_rc "detect_arch: mips → rc=1"       1                detect_arch
unset HIPPO_MOCK_ARCH

# ========== parse_tag_name ==========
JSON_REAL='{"url":"https://...","tag_name":"v0.2.0","name":"v0.2.0"}'
assert_eq "parse_tag_name: real shape → v0.2.0"  "$(parse_tag_name "$JSON_REAL")"  "v0.2.0"

JSON_SPACES='{"tag_name" :   "v1.2.3-beta"}'
assert_eq "parse_tag_name: extra spaces → v1.2.3-beta"  "$(parse_tag_name "$JSON_SPACES")"  "v1.2.3-beta"

JSON_NO_TAG='{"name":"foo"}'
assert_eq "parse_tag_name: missing → empty"  "$(parse_tag_name "$JSON_NO_TAG")"  ""

# ========== verify_sha256 ==========
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
echo "hello world" > "$TMP/file.txt"
# Compute the real sha for the test content.
if command -v shasum >/dev/null 2>&1; then
    REAL_SHA="$(shasum -a 256 "$TMP/file.txt" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
    REAL_SHA="$(sha256sum "$TMP/file.txt" | awk '{print $1}')"
else
    REAL_SHA=""
fi

if [ -n "$REAL_SHA" ]; then
    echo "$REAL_SHA  file.txt" > "$TMP/file.txt.sha256"
    if verify_sha256 "$TMP/file.txt" "$TMP/file.txt.sha256"; then
        PASS=$((PASS + 1))
        RESULTS="${RESULTS}  ok  verify_sha256: matching sha → rc=0\n"
    else
        FAIL=$((FAIL + 1))
        RESULTS="${RESULTS}  FAIL verify_sha256: matching sha should rc=0\n"
    fi

    echo "0000000000000000000000000000000000000000000000000000000000000000  file.txt" > "$TMP/wrong.sha256"
    if verify_sha256 "$TMP/file.txt" "$TMP/wrong.sha256"; then
        FAIL=$((FAIL + 1))
        RESULTS="${RESULTS}  FAIL verify_sha256: wrong sha should rc!=0\n"
    else
        PASS=$((PASS + 1))
        RESULTS="${RESULTS}  ok  verify_sha256: wrong sha → rc!=0\n"
    fi
else
    RESULTS="${RESULTS}  skip verify_sha256: no shasum/sha256sum present\n"
fi

# ========== locate_binary ==========
mkdir -p "$TMP/extracted/nested"
echo "#!/bin/sh" > "$TMP/extracted/nested/hip"
chmod +x "$TMP/extracted/nested/hip"
FOUND="$(locate_binary "$TMP/extracted" hip)"
assert_eq "locate_binary: nested executable → path"  "$FOUND"  "$TMP/extracted/nested/hip"

mkdir -p "$TMP/empty"
assert_rc "locate_binary: missing → rc=1"  1  locate_binary "$TMP/empty" hip

# ========== install_atomic ==========
mkdir -p "$TMP/installs"
echo "#!/bin/sh" > "$TMP/srcbin"
chmod +x "$TMP/srcbin"
mv "$TMP/srcbin" "$TMP/hip"
DEST="$(install_atomic "$TMP/hip" "$TMP/installs")"
assert_eq "install_atomic: dest path"  "$DEST"  "$TMP/installs/hip"
[ -x "$TMP/installs/hip" ] && {
    PASS=$((PASS + 1))
    RESULTS="${RESULTS}  ok  install_atomic: result is executable\n"
} || {
    FAIL=$((FAIL + 1))
    RESULTS="${RESULTS}  FAIL install_atomic: result is not executable\n"
}

# Re-run to verify idempotency (overwrites cleanly)
DEST2="$(install_atomic "$TMP/hip" "$TMP/installs")"
assert_eq "install_atomic: idempotent rerun"  "$DEST2"  "$TMP/installs/hip"

# Read-only target → expect rc=1 (no auto-sudo)
mkdir -p "$TMP/readonly"
chmod 555 "$TMP/readonly"
assert_rc "install_atomic: read-only dest → rc=1"  1  install_atomic "$TMP/hip" "$TMP/readonly/sub"
chmod 755 "$TMP/readonly"  # so the trap can clean up

# ========== pick_downloader ==========
# In this environment we should have at least curl OR wget, so this returns 0.
DL="$(pick_downloader)" && {
    case "$DL" in
        curl|wget)
            PASS=$((PASS + 1))
            RESULTS="${RESULTS}  ok  pick_downloader: returned ${DL}\n"
            ;;
        *)
            FAIL=$((FAIL + 1))
            RESULTS="${RESULTS}  FAIL pick_downloader: returned unexpected: ${DL}\n"
            ;;
    esac
} || {
    FAIL=$((FAIL + 1))
    RESULTS="${RESULTS}  FAIL pick_downloader: rc=1 (no curl/wget on host?)\n"
}

# ========== Summary ==========
echo
printf '%b' "$RESULTS"
echo
TOTAL=$((PASS + FAIL))
echo "── ${PASS}/${TOTAL} passed, ${FAIL} failed ──"

[ "$FAIL" = "0" ]
