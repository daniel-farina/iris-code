#!/usr/bin/env bash
# End-to-end test for install.sh.
# Spins up a tiny `python3 -m http.server` serving a fake GitHub release tree,
# runs install.sh against it via HIPPO_API_BASE_URL / HIPPO_DOWNLOAD_BASE_URL
# overrides, and asserts the install completes correctly.
#
# Run: bash tools/install-tests/test_e2e.sh
# Exit 0 on pass, 1 on fail.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
INSTALL_SCRIPT="$REPO_ROOT/install.sh"
FAKE_VERSION="v9.9.9"

command -v python3 >/dev/null 2>&1 || {
    echo "skip: python3 not installed; e2e test needs python3 -m http.server"
    exit 0
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"; [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null; exit ${RC:-0}' EXIT INT TERM

# === Lay out a fake GitHub release tree ===
#
# /
# ├── repos/daniel-farina/hippo-code/releases/latest        (JSON)
# └── daniel-farina/hippo-code/releases/download/v9.9.9/    (artifacts)
#     ├── hip-v9.9.9-darwin-arm64.tar.gz
#     ├── hip-v9.9.9-darwin-arm64.tar.gz.sha256
#     ├── hip-v9.9.9-darwin-x86_64.tar.gz
#     ├── ...

DOC_ROOT="$WORK/server-root"
mkdir -p "$DOC_ROOT/repos/daniel-farina/hippo-code/releases"
mkdir -p "$DOC_ROOT/daniel-farina/hippo-code/releases/download/$FAKE_VERSION"

# Create a fake `hip` binary (just a script that prints a sentinel).
mkdir -p "$WORK/build"
cat > "$WORK/build/hip" <<'BIN'
#!/usr/bin/env sh
echo "fake-hip $FAKE_VERSION ok"
BIN
chmod +x "$WORK/build/hip"

# Build the platform-specific tarballs (we only need the platform we're
# actually testing on, but we build all six so install.sh's platform
# detection has something to find).
detect_os() {
    case "$(uname -s | tr '[:upper:]' '[:lower:]')" in
        darwin) echo darwin ;;
        linux)  echo linux  ;;
        *)      echo darwin ;;  # default for the test
    esac
}
detect_arch() {
    case "$(uname -m)" in
        arm64|aarch64) echo arm64 ;;
        *)             echo x86_64 ;;
    esac
}

THIS_OS="$(detect_os)"
THIS_ARCH="$(detect_arch)"

for OS in darwin linux; do
    for ARCH in arm64 x86_64; do
        ARTIFACT="hip-${FAKE_VERSION}-${OS}-${ARCH}.tar.gz"
        DEST="$DOC_ROOT/daniel-farina/hippo-code/releases/download/$FAKE_VERSION/$ARTIFACT"
        tar czf "$DEST" -C "$WORK/build" hip
        if command -v shasum >/dev/null 2>&1; then
            (cd "$DOC_ROOT/daniel-farina/hippo-code/releases/download/$FAKE_VERSION" \
                && shasum -a 256 "$ARTIFACT" > "$ARTIFACT.sha256")
        else
            (cd "$DOC_ROOT/daniel-farina/hippo-code/releases/download/$FAKE_VERSION" \
                && sha256sum "$ARTIFACT" > "$ARTIFACT.sha256")
        fi
    done
done

# Latest-release JSON. install.sh only reads `tag_name`, so anything else is
# decorative.
cat > "$DOC_ROOT/repos/daniel-farina/hippo-code/releases/latest" <<JSON
{
  "url": "http://localhost/repos/daniel-farina/hippo-code/releases/123",
  "tag_name": "$FAKE_VERSION",
  "name": "$FAKE_VERSION"
}
JSON

# === Pick a free port + start http.server ===
# We bind to 0 to ask the OS for a free port. Python prints the URL on stderr;
# we don't bother parsing it - we just give it a known port and check.
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
echo "── starting fake release server on http://127.0.0.1:$PORT (root=$DOC_ROOT) ──"
( cd "$DOC_ROOT" && python3 -m http.server "$PORT" >/dev/null 2>&1 ) &
SERVER_PID=$!

# Wait for it to come up (max 5s)
for _ in 1 2 3 4 5 6 7 8 9 10; do
    if curl -sf "http://127.0.0.1:$PORT/repos/daniel-farina/hippo-code/releases/latest" >/dev/null; then
        break
    fi
    sleep 0.5
done
curl -sf "http://127.0.0.1:$PORT/repos/daniel-farina/hippo-code/releases/latest" >/dev/null \
    || { echo "FAIL: fake server didn't come up"; RC=1; exit 1; }

# === Run install.sh against the fake server ===
INSTALL_DIR="$WORK/installed"
echo "── running install.sh ──"
HIPPO_API_BASE_URL="http://127.0.0.1:$PORT" \
HIPPO_DOWNLOAD_BASE_URL="http://127.0.0.1:$PORT" \
HIPPO_INSTALL_DIR="$INSTALL_DIR" \
NO_COLOR=1 \
sh "$INSTALL_SCRIPT" || { echo "FAIL: install.sh exited non-zero"; RC=1; exit 1; }

# === Assertions ===
PASS=0
FAIL=0

assert() {
    local desc="$1"; local cond="$2"
    if eval "$cond"; then
        PASS=$((PASS+1))
        echo "  ok  $desc"
    else
        FAIL=$((FAIL+1))
        echo "  FAIL $desc"
    fi
}

echo
assert "binary installed at expected path"  "[ -x \"$INSTALL_DIR/hip\" ]"
assert "binary is the fake we built"        "\"$INSTALL_DIR/hip\" 2>&1 | grep -q 'fake-hip'"

# Re-run the install (idempotency)
HIPPO_API_BASE_URL="http://127.0.0.1:$PORT" \
HIPPO_DOWNLOAD_BASE_URL="http://127.0.0.1:$PORT" \
HIPPO_INSTALL_DIR="$INSTALL_DIR" \
NO_COLOR=1 \
sh "$INSTALL_SCRIPT" >/dev/null 2>&1 || { echo "FAIL: re-install crashed"; RC=1; exit 1; }
assert "idempotent re-install still works"  "[ -x \"$INSTALL_DIR/hip\" ]"

# HIPPO_VERSION pin path
HIPPO_API_BASE_URL="http://127.0.0.1:$PORT" \
HIPPO_DOWNLOAD_BASE_URL="http://127.0.0.1:$PORT" \
HIPPO_INSTALL_DIR="$INSTALL_DIR" \
HIPPO_VERSION="$FAKE_VERSION" \
NO_COLOR=1 \
sh "$INSTALL_SCRIPT" >/dev/null 2>&1 || { echo "FAIL: pinned-version install crashed"; RC=1; exit 1; }
assert "HIPPO_VERSION pin path works"  "[ -x \"$INSTALL_DIR/hip\" ]"

# Tampered tarball detection
ARTIFACT="hip-${FAKE_VERSION}-${THIS_OS}-${THIS_ARCH}.tar.gz"
ARTIFACT_PATH="$DOC_ROOT/daniel-farina/hippo-code/releases/download/$FAKE_VERSION/$ARTIFACT"
echo "tampered junk" > "$ARTIFACT_PATH"
# Don't update the .sha256, so the install should die on checksum mismatch.
set +e
HIPPO_API_BASE_URL="http://127.0.0.1:$PORT" \
HIPPO_DOWNLOAD_BASE_URL="http://127.0.0.1:$PORT" \
HIPPO_INSTALL_DIR="$INSTALL_DIR" \
NO_COLOR=1 \
sh "$INSTALL_SCRIPT" > /tmp/install-tamper.log 2>&1
RC_TAMPER=$?
set -e
assert "tampered tarball is rejected (rc!=0)"  "[ $RC_TAMPER != 0 ]"
assert "tamper failure mentions checksum"      "grep -qi 'checksum' /tmp/install-tamper.log"

# === Summary ===
TOTAL=$((PASS + FAIL))
echo
echo "── e2e: ${PASS}/${TOTAL} passed, ${FAIL} failed ──"

[ "$FAIL" = "0" ]
RC=$?
