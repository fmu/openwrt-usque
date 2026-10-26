#!/usr/bin/env bash
# Cross-compile usque for OpenWrt (armv7, musl) as a *static*, stripped binary.
#
# Usage (from anywhere; paths are resolved relative to the repo root):
#   build/build-usque.sh
#
# Output: <repo>/dist/usque  (overridable with OUT_DIR=...)
# Prints the stripped size and sha256, and exits non-zero if the binary is
# larger than MAX_SIZE_BYTES (default 3.2 MiB — the router overlay only has
# ~3.3 MB free).
#
# Toolchains, in order of preference:
#   1. zig + cargo-zigbuild   (used locally on macOS and in CI; no external
#      libc/toolchain downloads, zig supplies musl)
#   2. musl.cc cross gcc      (arm-linux-musleabihf-gcc, or $CROSS_COMPILE)
#
# Environment:
#   TARGET         rust target triple     (default armv7-unknown-linux-musleabihf)
#   OUT_DIR        output directory       (default <repo>/dist)
#   MAX_SIZE_BYTES size gate in bytes     (default 3355443 = 3.2 MiB)
#   SKIP_SIZE_CHECK=1  do not fail on size
#   FEATURES       cargo feature list     (default: crate defaults = quinn HTTP/3 only)
#   CARGO_PROFILE  cargo profile          (default release)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/src"
TARGET="${TARGET:-armv7-unknown-linux-musleabihf}"
OUT_DIR="${OUT_DIR:-$ROOT/dist}"
MAX_SIZE_BYTES="${MAX_SIZE_BYTES:-3355443}"
PROFILE="${CARGO_PROFILE:-release}"
FEATURES="${FEATURES:-}"

BIN_NAME="usque"
OUT_BIN="$OUT_DIR/$BIN_NAME"
BUILT_BIN="$SRC/target/$TARGET/$PROFILE/$BIN_NAME"

mkdir -p "$OUT_DIR"

echo "== usque cross build =="
echo "target:  $TARGET"
echo "profile: $PROFILE"
echo "out:     $OUT_BIN"

cd "$SRC"

# The h3 crate is patched locally (Cloudflare's `cf-connect-ip` protocol
# token); regenerate it from the cargo registry cache before building.
bash vendor/prepare_h3.sh >/dev/null

if [ -n "$FEATURES" ]; then
    FEATURE_ARGS="--no-default-features --features $FEATURES"
else
    FEATURE_ARGS=""
fi

if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
    echo "toolchain: zig $(zig version) + cargo-zigbuild"
    # shellcheck disable=SC2086
    cargo zigbuild --profile "$PROFILE" --target "$TARGET" -p usque-cli $FEATURE_ARGS
else
    # musl.cc / OpenWrt SDK style cross toolchain.
    CROSS="${CROSS_COMPILE:-arm-linux-musleabihf-}"
    if ! command -v "${CROSS}gcc" >/dev/null 2>&1; then
        echo "error: no zig+cargo-zigbuild and no ${CROSS}gcc cross compiler on PATH" >&2
        echo "       install zig (with cargo-zigbuild) or set CROSS_COMPILE" >&2
        exit 1
    fi
    echo "toolchain: $CROSS"
    t="$(printf '%s' "$TARGET" | tr '-' '_')"
    t_upper="$(printf '%s' "$t" | tr 'a-z' 'A-Z')"
    export "CC_$t=${CROSS}gcc"
    export "AR_$t=${CROSS}ar"
    export "RANLIB_$t=${CROSS}ranlib"
    export "CARGO_TARGET_${t_upper}_LINKER=${CROSS}gcc"
    # shellcheck disable=SC2086
    cargo build --profile "$PROFILE" --target "$TARGET" -p usque-cli $FEATURE_ARGS
fi

if [ ! -f "$BUILT_BIN" ]; then
    echo "error: expected build output not found: $BUILT_BIN" >&2
    exit 1
fi

cp "$BUILT_BIN" "$OUT_BIN"
UNSTRIPPED_SIZE=$(wc -c < "$OUT_BIN" | tr -d ' ')

# The release profile already sets `strip = "symbols"`, but run an explicit
# strip when a target-aware tool exists so the artifact is minimal no matter
# how cargo passes the flag through. macOS' `strip` cannot handle ELF, hence
# the explicit candidate list (and the ignored failure).
STRIP_TOOL=""
if [ -n "${CROSS_COMPILE:-}" ]; then
    STRIP_CANDIDATES="${CROSS_COMPILE}strip arm-linux-musleabihf-strip armv7-linux-musleabihf-strip llvm-strip"
else
    STRIP_CANDIDATES="arm-linux-musleabihf-strip armv7-linux-musleabihf-strip llvm-strip"
fi
for candidate in $STRIP_CANDIDATES; do
    if command -v "$candidate" >/dev/null 2>&1; then
        STRIP_TOOL="$candidate"
        break
    fi
done
if [ -n "$STRIP_TOOL" ]; then
    echo "strip:    $STRIP_TOOL --strip-all"
    "$STRIP_TOOL" --strip-all "$OUT_BIN" || echo "warning: $STRIP_TOOL failed; keeping cargo-stripped binary" >&2
else
    echo "strip:    (cargo profile strip only)"
fi

SIZE=$(wc -c < "$OUT_BIN" | tr -d ' ')
HUMAN=$(du -h "$OUT_BIN" | cut -f1)
if command -v sha256sum >/dev/null 2>&1; then
    SHA=$(sha256sum "$OUT_BIN" | cut -d' ' -f1)
else
    SHA=$(shasum -a 256 "$OUT_BIN" | cut -d' ' -f1)
fi

echo
echo "== result =="
if command -v file >/dev/null 2>&1; then
    file "$OUT_BIN"
fi
echo "unstripped: $UNSTRIPPED_SIZE bytes"
echo "stripped:   $SIZE bytes ($HUMAN)"
echo "sha256:     $SHA"

if [ "${SKIP_SIZE_CHECK:-0}" != "1" ] && [ "$SIZE" -gt "$MAX_SIZE_BYTES" ]; then
    echo "error: $OUT_BIN is $SIZE bytes, over the $MAX_SIZE_BYTES byte budget" >&2
    exit 1
fi
