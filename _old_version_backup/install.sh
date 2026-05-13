#!/usr/bin/env sh
# hippo-code installer
# Usage: curl -sSL https://raw.githubusercontent.com/daniel-farina/hippo-code/main/install.sh | sh
# Env:
#   HIPPO_INSTALL_DIR     Override install directory (default: ~/.local/bin)
#   HIPPO_VERSION         Pin a specific version like "v0.1.0" (default: latest)
#   NO_COLOR              Disable colored output if set to any value
#
#   Test/CI overrides (used by tools/install-tests/*; safe to ignore):
#   HIPPO_API_BASE_URL    Override the GitHub REST host (default https://api.github.com)
#   HIPPO_DOWNLOAD_BASE_URL  Override the release-download host (default https://github.com)
#   HIPPO_MOCK_OS         Override `uname -s` output (e.g. "Darwin" or "Linux")
#   HIPPO_MOCK_ARCH       Override `uname -m` output (e.g. "arm64" or "x86_64")
#   HIPPO_TEST_DEFINE_ONLY=1   Source-only mode: define functions, skip procedural body.
#                              Used by unit tests to exercise individual functions.
#
# Native dependencies: a downloader (curl OR wget), tar, mkdir, chmod, uname,
# sed, grep, awk, mktemp, find, head. All POSIX-standard on macOS and Linux.
# Does NOT depend on `gh`, `jq`, or `python`.

set -eu

REPO="daniel-farina/hippo-code"
BIN_NAME="hip"

# Resolve overridable URL bases up front so unit tests can swap them.
: "${HIPPO_API_BASE_URL:=https://api.github.com}"
: "${HIPPO_DOWNLOAD_BASE_URL:=https://github.com}"

# --- Color helpers (respect NO_COLOR) ---
init_colors() {
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
}

info()  { printf '%s==>%s %s\n' "${C_BLUE:-}"   "${C_RESET:-}" "$1"; }
ok()    { printf '%s OK%s %s\n' "${C_GREEN:-}"  "${C_RESET:-}" "$1"; }
warn()  { printf '%swarn%s %s\n' "${C_YELLOW:-}" "${C_RESET:-}" "$1" >&2; }
die()   { printf '%serror%s %s\n' "${C_RED:-}"  "${C_RESET:-}" "$1" >&2; exit 1; }

# --- Pick a downloader: prefer curl, fall back to wget ---
pick_downloader() {
    if command -v curl >/dev/null 2>&1; then
        echo "curl"
    elif command -v wget >/dev/null 2>&1; then
        echo "wget"
    else
        return 1
    fi
}

# fetch_to <url> <dest>: download into a file, exit non-zero on HTTP error.
# Uses ${DOWNLOADER} that init_downloader() set up.
fetch_to() {
    if [ "${DOWNLOADER:-curl}" = "curl" ]; then
        curl -fsSL -o "$2" "$1"
    else
        wget -q -O "$2" "$1"
    fi
}

# fetch_stdout <url>: print to stdout, exit non-zero on HTTP error.
fetch_stdout() {
    if [ "${DOWNLOADER:-curl}" = "curl" ]; then
        curl -fsSL "$1"
    else
        wget -q -O - "$1"
    fi
}

# detect_os: print normalized os name ("darwin" / "linux") or die.
# Honors HIPPO_MOCK_OS env override for tests.
detect_os() {
    raw="${HIPPO_MOCK_OS:-$(uname -s)}"
    raw="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]')"
    case "$raw" in
        darwin) echo "darwin" ;;
        linux)  echo "linux" ;;
        *) return 1 ;;
    esac
}

# detect_arch: print normalized arch ("arm64" / "x86_64") or die.
# Honors HIPPO_MOCK_ARCH env override.
detect_arch() {
    raw="${HIPPO_MOCK_ARCH:-$(uname -m)}"
    case "$raw" in
        arm64|aarch64) echo "arm64" ;;
        x86_64|amd64)  echo "x86_64" ;;
        *) return 1 ;;
    esac
}

# parse_tag_name <release-json>: extract tag_name from a GitHub releases JSON
# payload via grep+sed. Prints the tag (e.g. "v0.1.0") or returns 1 on miss.
parse_tag_name() {
    printf '%s' "$1" \
        | grep -m1 '"tag_name"' \
        | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/'
}

# verify_sha256 <tarball> <sha256-file>: compute the tarball's SHA256 and
# compare to the first whitespace-token in <sha256-file>. Returns 0 on
# match, 1 on mismatch, 2 if no tool available.
verify_sha256() {
    expected="$(awk '{print $1}' "$2")"
    if command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$1" | awk '{print $1}')"
    elif command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$1" | awk '{print $1}')"
    else
        return 2
    fi
    [ "$expected" = "$actual" ]
}

# locate_binary <root-dir> <name>: print path to a binary named <name>
# inside <root-dir>, preferring executable matches. Returns 1 if missing.
locate_binary() {
    found="$(find "$1" -type f -name "$2" -perm -u+x 2>/dev/null | head -n1)"
    [ -z "$found" ] && found="$(find "$1" -type f -name "$2" 2>/dev/null | head -n1)"
    [ -n "$found" ] && [ -f "$found" ] || return 1
    printf '%s\n' "$found"
}

# install_atomic <src> <install-dir>: cp+chmod+mv to atomic-replace at
# <install-dir>/<basename>. Refuses to use sudo.
install_atomic() {
    src="$1"
    dest_dir="$2"
    name="$(basename "$src")"
    if ! mkdir -p "$dest_dir" 2>/dev/null; then
        return 1
    fi
    if [ ! -w "$dest_dir" ]; then
        return 1
    fi
    dest="${dest_dir}/${name}"
    cp "$src" "${dest}.new" || return 1
    chmod +x "${dest}.new" || return 1
    mv -f "${dest}.new" "$dest" || return 1
    printf '%s\n' "$dest"
}

# main: the top-level installer flow.
main() {
    init_colors

    INSTALL_DIR="${HIPPO_INSTALL_DIR:-$HOME/.local/bin}"

    DOWNLOADER="$(pick_downloader)" || die "neither curl nor wget is installed. Install one and retry."

    OS="$(detect_os)" || die "unsupported OS: ${HIPPO_MOCK_OS:-$(uname -s)}"
    ARCH="$(detect_arch)" || die "unsupported arch: ${HIPPO_MOCK_ARCH:-$(uname -m)}"

    # hip only ships pre-built binaries for Apple Silicon. MTPLX (the server
    # hip talks to) is built on MLX which is Apple-Silicon-only, so a Linux
    # or Intel-Mac install would have nothing to chat with.
    if [ "${OS}-${ARCH}" != "darwin-arm64" ]; then
        printf '\n'
        printf '%ship only ships pre-built binaries for darwin-arm64 (Apple Silicon).%s\n' "${C_RED}" "${C_RESET}" >&2
        printf 'Detected: %s-%s\n' "${OS}" "${ARCH}" >&2
        printf '\n'
        printf 'Why: MTPLX (the local model server hip talks to) requires MLX,\n' >&2
        printf 'which only runs on Apple Silicon (M1, M2, M3, ...). A non-Apple-\n' >&2
        printf 'Silicon hip binary would have no server to connect to locally.\n' >&2
        printf '\n'
        printf 'If you intend to run hip against a remote MTPLX, build from source:\n' >&2
        printf '  %scargo install --git https://github.com/%s --tag <ver>%s\n' "${C_BOLD}" "${REPO}" "${C_RESET}" >&2
        printf '\n'
        exit 2
    fi

    info "Detected platform: ${C_BOLD}${OS}-${ARCH}${C_RESET} (using ${DOWNLOADER})"

    command -v tar  >/dev/null 2>&1 || die "tar is required but not found"

    if [ -n "${HIPPO_VERSION:-}" ]; then
        VERSION="$HIPPO_VERSION"
        info "Pinned version: ${C_BOLD}${VERSION}${C_RESET}"
    else
        info "Fetching latest release metadata for ${REPO}"
        RELEASE_JSON="$(fetch_stdout "${HIPPO_API_BASE_URL}/repos/${REPO}/releases/latest")" \
            || die "failed to fetch latest release from GitHub API"
        VERSION="$(parse_tag_name "$RELEASE_JSON")"
        [ -n "$VERSION" ] || die "could not parse release tag from API response"
    fi

    ARTIFACT="${BIN_NAME}-${VERSION}-${OS}-${ARCH}.tar.gz"
    CHECKSUM="${ARTIFACT}.sha256"
    BASE_URL="${HIPPO_DOWNLOAD_BASE_URL}/${REPO}/releases/download/${VERSION}"

    info "Installing ${C_BOLD}${BIN_NAME} ${VERSION}${C_RESET} from ${ARTIFACT}"

    TMPDIR_X="$(mktemp -d 2>/dev/null || mktemp -d -t hippo-code)"
    # shellcheck disable=SC2064
    trap "rm -rf '$TMPDIR_X'" EXIT INT TERM

    info "Downloading ${BASE_URL}/${ARTIFACT}"
    fetch_to "${BASE_URL}/${ARTIFACT}" "${TMPDIR_X}/${ARTIFACT}" \
        || die "failed to download ${ARTIFACT}"

    if fetch_to "${BASE_URL}/${CHECKSUM}" "${TMPDIR_X}/${CHECKSUM}" 2>/dev/null; then
        info "Verifying SHA256 checksum"
        if verify_sha256 "${TMPDIR_X}/${ARTIFACT}" "${TMPDIR_X}/${CHECKSUM}"; then
            ok "checksum verified"
        else
            rc=$?
            if [ "$rc" = "2" ]; then
                warn "no shasum/sha256sum tool found - skipping checksum verification"
            else
                die "checksum mismatch"
            fi
        fi
    else
        warn "no .sha256 file published for this artifact - skipping verification"
    fi

    info "Extracting archive"
    tar -xzf "${TMPDIR_X}/${ARTIFACT}" -C "${TMPDIR_X}" || die "failed to extract ${ARTIFACT}"

    SRC_BIN="$(locate_binary "$TMPDIR_X" "$BIN_NAME")" \
        || die "binary '${BIN_NAME}' not found in archive"

    DEST="$(install_atomic "$SRC_BIN" "$INSTALL_DIR")" \
        || die "${INSTALL_DIR} is not writable - re-run with a writable HIPPO_INSTALL_DIR or as the appropriate user"
    ok "installed to ${C_BOLD}${DEST}${C_RESET}"

    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            warn "${INSTALL_DIR} is not on your PATH"
            printf '  Add this line to your shell rc (e.g. ~/.zshrc or ~/.bashrc):\n'
            printf '    %sexport PATH="%s:$PATH"%s\n' "$C_BOLD" "$INSTALL_DIR" "$C_RESET"
            ;;
    esac

    printf '\n%s%s installed%s\n' "$C_GREEN$C_BOLD" "$BIN_NAME $VERSION" "$C_RESET"
    printf 'Run %s%s --help%s to get started, or %s%s%s for the chat REPL.\n' \
        "$C_BOLD" "$BIN_NAME" "$C_RESET" \
        "$C_BOLD" "$BIN_NAME" "$C_RESET"
}

# --- Entry point ---
# When sourced for testing (`HIPPO_TEST_DEFINE_ONLY=1 . install.sh`), skip the
# main() invocation so unit tests can call individual functions directly.
if [ "${HIPPO_TEST_DEFINE_ONLY:-0}" = "1" ]; then
    return 0 2>/dev/null || true
fi

main
