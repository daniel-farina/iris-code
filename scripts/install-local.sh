#!/usr/bin/env sh
# Install the current dev tree as the system-wide `hip` binary.
#
# The release installer (install.sh) downloads a pinned tarball from
# GitHub Releases into ~/.local/hippo-code/<VERSION>/ and symlinks
# ~/.local/bin/hip → that bin/hip launcher. This script does the same
# thing locally: it rsyncs the WORKING DIRECTORY into the installed
# tree at the version listed in package.json, so a fresh `hip` invocation
# picks up the latest source. No tarball, no GitHub round-trip.
#
# Usage:
#   ./scripts/install-local.sh
#
# Idempotent. Safe to re-run after every change.

set -eu

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR"

VERSION="$(node -p "require('./package.json').version" 2>/dev/null || true)"
if [ -z "$VERSION" ]; then
    # Fall back to a grep parse so we don't require node on PATH.
    VERSION="$(grep -m1 '"version"' package.json | sed -E 's/.*"version":[[:space:]]*"([^"]+)".*/\1/')"
fi
if [ -z "$VERSION" ]; then
    echo "error: could not resolve version from package.json" >&2
    exit 1
fi

INSTALL_ROOT="${HIPPO_INSTALL_DIR:-$HOME/.local}"
INSTALL_DIR="${INSTALL_ROOT}/hippo-code/${VERSION}"
BIN_LINK="${INSTALL_ROOT}/bin/hip"

mkdir -p "$INSTALL_DIR"
mkdir -p "$(dirname "$BIN_LINK")"

echo "[install-local] syncing $REPO_DIR -> $INSTALL_DIR (version $VERSION)"
# rsync the subset that hip actually runs from. node_modules is
# big - reuse what's already in the install dir if it exists, otherwise
# we'll fall back to copying it.
rsync -a --delete \
    --exclude '_old_version_backup' \
    --exclude 'tests' \
    --exclude '.git' \
    --exclude '*.prev' \
    "$REPO_DIR/src/" "$INSTALL_DIR/src/"
rsync -a --delete "$REPO_DIR/bin/" "$INSTALL_DIR/bin/"
rsync -a --delete "$REPO_DIR/assets/" "$INSTALL_DIR/assets/"
cp "$REPO_DIR/package.json" "$INSTALL_DIR/package.json"
cp "$REPO_DIR/tsconfig.json" "$INSTALL_DIR/tsconfig.json"
cp "$REPO_DIR/biome.json" "$INSTALL_DIR/biome.json" 2>/dev/null || true
cp "$REPO_DIR/bun.lock" "$INSTALL_DIR/bun.lock" 2>/dev/null || true

# node_modules: if the install dir has them, leave them; otherwise copy
# from the working tree. bun install creates platform-specific links so
# a straight rsync is safer than re-running `bun install` in the
# install dir.
if [ ! -d "$INSTALL_DIR/node_modules" ] && [ -d "$REPO_DIR/node_modules" ]; then
    echo "[install-local] copying node_modules (first time only)"
    rsync -a "$REPO_DIR/node_modules/" "$INSTALL_DIR/node_modules/"
fi

# (Re-)point the symlink at the new install dir's bin/hip.
TARGET="$INSTALL_DIR/bin/hip"
chmod +x "$TARGET"
if [ -L "$BIN_LINK" ] || [ -e "$BIN_LINK" ]; then
    rm -f "$BIN_LINK"
fi
ln -s "$TARGET" "$BIN_LINK"

echo "[install-local] installed: $BIN_LINK -> $TARGET"
echo "[install-local] run: hip --version    (or just: hip)"
