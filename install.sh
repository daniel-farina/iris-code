#!/usr/bin/env sh
# hippo-code installer (TypeScript port).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/daniel-farina/hippo-code/main/install.sh | sh
#
# Env overrides:
#   HIPPO_VERSION       Pin a specific version like "v0.4.0" (default: latest)
#   HIPPO_INSTALL_DIR   Override install directory (default: ~/.local/bin)
#   PREFIX              Alternate to HIPPO_INSTALL_DIR; PREFIX/bin/hip becomes the install path
#
# MLX/MTPLX is Apple-Silicon-only, so this installer only supports darwin-arm64.

set -eu

REPO="daniel-farina/hippo-code"
BIN_NAME="hip"

# --- platform check ---
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
case "$ARCH" in
    arm64|aarch64) ARCH="arm64" ;;
    *) ARCH="other" ;;
esac
if [ "${OS}-${ARCH}" != "darwin-arm64" ]; then
    echo "error: hip only ships pre-built tarballs for darwin-arm64 (Apple Silicon)." >&2
    echo "  detected: ${OS}-${ARCH}" >&2
    echo "  reason:   MTPLX (the model server hip talks to) is built on MLX, which is Apple-Silicon-only." >&2
    echo "  workaround: run from source — 'git clone https://github.com/${REPO} && cd hippo-code && bun install && ./bin/hip'" >&2
    exit 1
fi

# --- bun dependency ---
if ! command -v bun >/dev/null 2>&1; then
    echo "error: 'bun' not found on PATH." >&2
    echo "  install with: curl -fsSL https://bun.sh/install | bash" >&2
    exit 1
fi

# --- pick a downloader ---
if command -v curl >/dev/null 2>&1; then
    download() { curl -fsSL "$1" -o "$2"; }
    fetch_text() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
    download() { wget -qO "$2" "$1"; }
    fetch_text() { wget -qO- "$1"; }
else
    echo "error: need curl or wget on PATH." >&2
    exit 1
fi

# --- resolve version ---
VERSION="${HIPPO_VERSION:-}"
if [ -z "$VERSION" ]; then
    # latest release tag from GitHub API. Fallback grep avoids jq dep.
    VERSION="$(fetch_text "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' | head -n1 | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
    if [ -z "$VERSION" ]; then
        echo "error: could not resolve latest version from GitHub API." >&2
        exit 1
    fi
fi
VERSION="${VERSION#v}"  # strip leading v if present

# --- download + verify ---
TARBALL="${BIN_NAME}-${VERSION}-darwin-arm64.tar.gz"
SHAFILE="${TARBALL}.sha256"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/hip-install.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

echo "downloading ${TARBALL}..."
BASE="https://github.com/${REPO}/releases/download/v${VERSION}"
download "${BASE}/${TARBALL}" "${TMP}/${TARBALL}"
if download "${BASE}/${SHAFILE}" "${TMP}/${SHAFILE}" 2>/dev/null; then
    expected="$(awk '{print $1}' "${TMP}/${SHAFILE}")"
    actual="$(shasum -a 256 "${TMP}/${TARBALL}" | awk '{print $1}')"
    if [ "$expected" != "$actual" ]; then
        echo "error: sha256 mismatch (expected ${expected}, got ${actual})" >&2
        exit 1
    fi
    echo "sha256 ok"
fi

# --- extract + symlink ---
tar -xzf "${TMP}/${TARBALL}" -C "${TMP}"
SRC_DIR="${TMP}/${BIN_NAME}-${VERSION}"
HOME_INSTALL="${HOME}/.local/hippo-code/${VERSION}"
mkdir -p "${HOME_INSTALL}"
cp -R "${SRC_DIR}/." "${HOME_INSTALL}/"

# Make sure the install has its node_modules.
(cd "${HOME_INSTALL}" && bun install >/dev/null 2>&1) || {
    echo "warning: 'bun install' inside ${HOME_INSTALL} failed; hip may not run until you re-run it manually." >&2
}

# Symlink target.
if [ -n "${PREFIX:-}" ]; then
    BIN_DIR="${PREFIX}/bin"
else
    BIN_DIR="${HIPPO_INSTALL_DIR:-${HOME}/.local/bin}"
fi
mkdir -p "${BIN_DIR}"
ln -sf "${HOME_INSTALL}/bin/${BIN_NAME}" "${BIN_DIR}/${BIN_NAME}"

echo
echo "installed hip v${VERSION}"
echo "  binary:   ${BIN_DIR}/${BIN_NAME} -> ${HOME_INSTALL}/bin/${BIN_NAME}"
echo "  install:  ${HOME_INSTALL}"
case ":${PATH}:" in
    *":${BIN_DIR}:"*) ;;
    *)
        echo
        echo "note: ${BIN_DIR} is not on your PATH."
        echo "      add this to your shell profile: export PATH=\"${BIN_DIR}:\$PATH\""
        ;;
esac
echo
echo "run 'hip --version' to verify. 'hip --update' will update in place."
