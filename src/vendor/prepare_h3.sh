#!/usr/bin/env bash
# Regenerate the patched local h3 crate from the cargo registry cache,
# downloading it from crates.io if it isn't cached yet.
# Run this before the first `cargo build`/`check` on a fresh checkout.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCK="$ROOT/Cargo.lock"
# The patch lives in an untracked cargo config so neither Cargo.toml nor
# Renovate see it; we generate it here alongside vendor/h3.
CONFIG="$ROOT/.cargo/config.toml"

# Default to the exact version resolved in Cargo.lock — unlike Cargo.toml
# requirements (e.g. "^0.0.8"), a lockfile entry is always a literal version,
# so the cache lookup and download URL can't be built from a requirement string.
# Optionally overridden by $1.
VERSION="${1:-$(awk '
    /^\[\[package\]\]/ { found = 0 }
    /^name = "h3"$/    { found = 1; next }
    found && /^version = "/ {
        gsub(/version = "|"/, "")
        print
        exit
    }' "$LOCK" 2>/dev/null || true)}"
if [ -z "$VERSION" ]; then
    echo "error: no h3 entry in Cargo.lock; run 'cargo fetch' first" >&2
    exit 1
fi

SRC="$(ls -d "$HOME"/.cargo/registry/src/*/h3-"$VERSION" 2>/dev/null | head -n1 || true)"
if [ -z "$SRC" ]; then
    # Not in the registry cache yet (cargo never downloads path-dependencies).
    REGISTRY_SRC="$HOME"/.cargo/registry/src
    SRC_DIR="$(ls -d "$REGISTRY_SRC"/*/ 2>/dev/null | head -n1 || true)"
    SRC_DIR="${SRC_DIR:-$REGISTRY_SRC/index.crates.io-6f17d22bba15001f}"
    mkdir -p "$SRC_DIR"
    echo "h3-$VERSION not in cargo registry cache; downloading from crates.io" >&2
    curl -fsSL "https://static.crates.io/crates/h3/h3-$VERSION.crate" \
        | tar xz -C "$SRC_DIR"
    SRC="$SRC_DIR/h3-$VERSION"
fi

OUT="$ROOT/vendor/h3"
rm -rf "$OUT"
cp -r "$SRC" "$OUT"
patch -p1 -d "$OUT" < "$ROOT/vendor/h3.patch"

# Register the patch in cargo's config (kept out of the tracked files so
# that `cargo fetch`/Renovate work on fresh checkouts before vendoring).
if ! grep -qF 'path = "vendor/h3"' "$CONFIG" 2>/dev/null; then
    mkdir -p "$(dirname "$CONFIG")"
    printf '\n[patch.crates-io]\nh3 = {path = "vendor/h3"}\n' >> "$CONFIG"
fi

# Re-resolve so Cargo.lock records h3 as the path package; without this,
# a lockfile generated against crates.io (e.g. by Renovate) would make the
# subsequent `cargo build --locked` fail.
cargo metadata --format-version 1 >/dev/null

echo "patched h3 -> $OUT"
