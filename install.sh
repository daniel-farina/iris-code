#!/usr/bin/env sh
# hippo-code installer
# Usage: curl -sSL https://raw.githubusercontent.com/daniel-farina/hippo-code/main/install.sh | sh
# Env:
#   HIPPO_INSTALL_DIR  Override install directory (default: ~/.local/bin)
#   HIPPO_VERSION      Pin a specific version like "v0.1.0" (default: latest)
#   NO_COLOR          Disable colored output if set to any value
#
# Native dependencies: a downloader (curl OR wget), tar, mkdir, chmod, uname,
# sed, grep, awk, mktemp, find, head. All POSIX-standard on macOS and Linux.
# Does NOT depend on `gh`, `jq`, or `python`.

set -eu

REPO="daniel-farina/hippo-code"
BIN_NAME="hip"
INSTALL_DIR="${HIPPO_INSTALL_DIR:-$HOME/.local/bin}"

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

# --- Pick a downloader: prefer curl, fall back to wget ---
if command -v curl >/dev/null 2>&1; then
    DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
    DOWNLOADER="wget"
else
    die "neither curl nor wget is installed. Install one and retry."
fi

# fetch_to <url> <dest>: download into a file, exit non-zero on HTTP error
fetch_to() {
    if [ "$DOWNLOADER" = "curl" ]; then
        curl -fsSL -o "$2" "$1"
    else
        wget -q -O "$2" "$1"
    fi
}

# fetch_stdout <url>: print to stdout, exit non-zero on HTTP error
fetch_stdout() {
    if [ "$DOWNLOADER" = "curl" ]; then
        curl -fsSL "$1"
    else
        wget -q -O - "$1"
    fi
}

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

info "Detected platform: ${C_BOLD}${OS}-${ARCH}${C_RESET} (using ${DOWNLOADER})"

# --- Required tools ---
command -v tar  >/dev/null 2>&1 || die "tar is required but not found"

# --- Resolve version: HIPPO_VERSION pin, or latest from the GitHub REST API ---
if [ -n "${HIPPO_VERSION:-}" ]; then
    VERSION="$HIPPO_VERSION"
    info "Pinned version: ${C_BOLD}${VERSION}${C_RESET}"
else
    info "Fetching latest release metadata for ${REPO}"
    RELEASE_JSON="$(fetch_stdout "https://api.github.com/repos/${REPO}/releases/latest")" \
        || die "failed to fetch latest release from GitHub API"
    # Extract tag_name (e.g. "v0.1.0") - portable grep/sed, no jq dependency
    VERSION="$(printf '%s' "$RELEASE_JSON" \
        | grep -m1 '"tag_name"' \
        | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
    [ -n "$VERSION" ] || die "could not parse release tag from API response"
fi

ARTIFACT="${BIN_NAME}-${VERSION}-${OS}-${ARCH}.tar.gz"
CHECKSUM="${ARTIFACT}.sha256"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"

info "Installing ${C_BOLD}${BIN_NAME} ${VERSION}${C_RESET} from ${ARTIFACT}"

# --- Stage in a temp dir, clean up on exit ---
TMPDIR_X="$(mktemp -d 2>/dev/null || mktemp -d -t hippo-code)"
trap 'rm -rf "$TMPDIR_X"' EXIT INT TERM

# --- Download artifact ---
info "Downloading ${BASE_URL}/${ARTIFACT}"
fetch_to "${BASE_URL}/${ARTIFACT}" "${TMPDIR_X}/${ARTIFACT}" \
    || die "failed to download ${ARTIFACT}"

# --- Verify SHA256 if sibling .sha256 exists ---
if fetch_to "${BASE_URL}/${CHECKSUM}" "${TMPDIR_X}/${CHECKSUM}" 2>/dev/null; then
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
    die "cannot create ${INSTALL_DIR} - re-run with a writable HIPPO_INSTALL_DIR or as the appropriate user"
fi
if [ ! -w "$INSTALL_DIR" ]; then
    die "${INSTALL_DIR} is not writable - re-run with a writable HIPPO_INSTALL_DIR or as the appropriate user"
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
printf '\n%s%s installed%s\n' "$C_GREEN$C_BOLD" "$BIN_NAME $VERSION" "$C_RESET"
printf 'Run %s%s --help%s to get started, or %s%s%s for the chat REPL.\n' \
    "$C_BOLD" "$BIN_NAME" "$C_RESET" \
    "$C_BOLD" "$BIN_NAME" "$C_RESET"
