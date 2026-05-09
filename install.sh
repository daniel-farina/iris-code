#!/usr/bin/env sh
# iris-code installer
# Usage: curl -sSL https://raw.githubusercontent.com/daniel-farina/iris-code/main/install.sh | sh
# Env:
#   IRIS_INSTALL_DIR  Override install directory (default: ~/.local/bin)
#   NO_COLOR          Disable colored output if set to any value

set -eu

REPO="daniel-farina/iris-code"
BIN_NAME="iris-code"
INSTALL_DIR="${IRIS_INSTALL_DIR:-$HOME/.local/bin}"

# --- Colors (respect NO_COLOR) ---
if [ -z "${NO_COLOR:-}" ] && [ -t 1 ]; then
    C_RED="$(printf '\033[31m')"
    C_GREEN="$(printf '\033[32m')"
    C_YELLOW="$(printf '\033[33m')"
    C_BLUE="$(printf '\033[34m')"
    C_BOLD="$(printf '\033[1m')"
    C_RESET="$(printf '\033[0m')"
else
    C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""; C_BOLD=""; C_RESET=""
fi

info()  { printf '%s==>%s %s\n' "$C_BLUE"   "$C_RESET" "$1"; }
ok()    { printf '%s OK%s %s\n' "$C_GREEN"  "$C_RESET" "$1"; }
warn()  { printf '%swarn%s %s\n' "$C_YELLOW" "$C_RESET" "$1" >&2; }
die()   { printf '%serror%s %s\n' "$C_RED"  "$C_RESET" "$1" >&2; exit 1; }

# --- Detect platform ---
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$OS" in
    darwin) OS="darwin" ;;
    linux)  OS="linux" ;;
    *) die "unsupported OS: $OS (only darwin and linux are supported)" ;;
esac

ARCH="$(uname -m)"
case "$ARCH" in
    arm64|aarch64) ARCH="arm64" ;;
    x86_64|amd64)  ARCH="x86_64" ;;
    *) die "unsupported arch: $ARCH (only arm64 and x86_64 are supported)" ;;
esac

info "Detected platform: ${C_BOLD}${OS}-${ARCH}${C_RESET}"

# --- Required tools ---
command -v curl >/dev/null 2>&1 || die "curl is required but not found"
command -v tar  >/dev/null 2>&1 || die "tar is required but not found"

# --- Resolve latest release tag ---
info "Fetching latest release metadata for ${REPO}"
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    RELEASE_JSON="$(gh api "repos/${REPO}/releases/latest" 2>/dev/null)" \
        || die "failed to fetch latest release via gh"
else
    RELEASE_JSON="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest")" \
        || die "failed to fetch latest release from GitHub API"
fi

# Extract tag_name (e.g. "v0.1.0") - portable grep/sed, no jq dependency
VERSION="$(printf '%s' "$RELEASE_JSON" \
    | grep -m1 '"tag_name"' \
    | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
[ -n "$VERSION" ] || die "could not parse release tag from API response"

ARTIFACT="${BIN_NAME}-${VERSION}-${OS}-${ARCH}.tar.gz"
CHECKSUM="${ARTIFACT}.sha256"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"

info "Installing ${C_BOLD}${BIN_NAME} ${VERSION}${C_RESET} from ${ARTIFACT}"

# --- Stage in a temp dir, clean up on exit ---
TMPDIR_X="$(mktemp -d 2>/dev/null || mktemp -d -t iris-code)"
trap 'rm -rf "$TMPDIR_X"' EXIT INT TERM

# --- Download artifact ---
info "Downloading ${BASE_URL}/${ARTIFACT}"
curl -fsSL -o "${TMPDIR_X}/${ARTIFACT}" "${BASE_URL}/${ARTIFACT}" \
    || die "failed to download ${ARTIFACT}"

# --- Verify SHA256 if sibling .sha256 exists ---
if curl -fsSL -o "${TMPDIR_X}/${CHECKSUM}" "${BASE_URL}/${CHECKSUM}" 2>/dev/null; then
    info "Verifying SHA256 checksum"
    EXPECTED="$(awk '{print $1}' "${TMPDIR_X}/${CHECKSUM}")"
    if command -v shasum >/dev/null 2>&1; then
        ACTUAL="$(shasum -a 256 "${TMPDIR_X}/${ARTIFACT}" | awk '{print $1}')"
    elif command -v sha256sum >/dev/null 2>&1; then
        ACTUAL="$(sha256sum "${TMPDIR_X}/${ARTIFACT}" | awk '{print $1}')"
    else
        warn "no shasum/sha256sum tool found - skipping checksum verification"
        ACTUAL="$EXPECTED"
    fi
    [ "$EXPECTED" = "$ACTUAL" ] || die "checksum mismatch (expected $EXPECTED, got $ACTUAL)"
    ok "checksum verified"
else
    warn "no .sha256 file published for this artifact - skipping verification"
fi

# --- Extract ---
info "Extracting archive"
tar -xzf "${TMPDIR_X}/${ARTIFACT}" -C "${TMPDIR_X}" || die "failed to extract ${ARTIFACT}"

# Locate the binary inside the extracted tree (handles flat or nested layouts)
SRC_BIN="$(find "${TMPDIR_X}" -type f -name "${BIN_NAME}" -perm -u+x 2>/dev/null | head -n1)"
[ -n "${SRC_BIN:-}" ] || SRC_BIN="$(find "${TMPDIR_X}" -type f -name "${BIN_NAME}" 2>/dev/null | head -n1)"
[ -n "${SRC_BIN:-}" ] && [ -f "$SRC_BIN" ] || die "binary '${BIN_NAME}' not found in archive"

# --- Install (no auto-sudo) ---
if ! mkdir -p "$INSTALL_DIR" 2>/dev/null; then
    die "cannot create ${INSTALL_DIR} - re-run with a writable IRIS_INSTALL_DIR or as the appropriate user"
fi
if [ ! -w "$INSTALL_DIR" ]; then
    die "${INSTALL_DIR} is not writable - re-run with a writable IRIS_INSTALL_DIR or as the appropriate user"
fi

DEST="${INSTALL_DIR}/${BIN_NAME}"
# Atomic-ish replace: cp to a temp sibling then mv
cp "$SRC_BIN" "${DEST}.new" || die "failed to copy binary into ${INSTALL_DIR}"
chmod +x "${DEST}.new" || die "failed to chmod binary"
mv -f "${DEST}.new" "$DEST" || die "failed to move binary into place"
ok "installed to ${C_BOLD}${DEST}${C_RESET}"

# --- PATH hint (do NOT modify shell rc files) ---
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        warn "${INSTALL_DIR} is not on your PATH"
        printf '  Add this line to your shell rc (e.g. ~/.zshrc or ~/.bashrc):\n'
        printf '    %sexport PATH="%s:$PATH"%s\n' "$C_BOLD" "$INSTALL_DIR" "$C_RESET"
        ;;
esac

# --- Done ---
printf '\n%s%s installed%s %s\n' "$C_GREEN$C_BOLD" "$BIN_NAME $VERSION" "$C_RESET" ""
printf 'Run %s%s --help%s to get started.\n' "$C_BOLD" "$BIN_NAME" "$C_RESET"
