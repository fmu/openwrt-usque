#!/usr/bin/env bash
# Build the usque OpenWrt packages inside an extracted OpenWrt SDK and
# assemble a signed apk (apk-tools 3) feed.
#
# Required environment:
#   SDK_DIR   path to the extracted OpenWrt SDK (contains rules.mk)
#   REPO_DIR  path to this repository checkout
#   OUT_DIR   output directory for the feed (created/emptied)
#   USQUE_BIN path to the static armv7 usque binary
#   SIGN_KEY  EC P-256 private key (PEM) used to sign the feed index
#   PUB_KEY   matching public key (PEM) shipped in the feed as usque.pem
#
# Optional environment:
#   ARCH  package architecture (default: arm_cortex-a7_neon-vfpv4)
#   JOBS  parallel make jobs (default: nproc)
#
# The feed directory contains:
#   usque-<version>.apk, luci-app-usque-<version>.apk (if available),
#   packages.adb, index.json, usque.pem
set -euo pipefail

die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
log() { printf '\n==> %s\n' "$*"; }

: "${SDK_DIR:?SDK_DIR is required}"
: "${REPO_DIR:?REPO_DIR is required}"
: "${OUT_DIR:?OUT_DIR is required}"
: "${USQUE_BIN:?USQUE_BIN is required}"
: "${SIGN_KEY:?SIGN_KEY is required}"
: "${PUB_KEY:?PUB_KEY is required}"

ARCH="${ARCH:-arm_cortex-a7_neon-vfpv4}"
JOBS="${JOBS:-$(nproc)}"
APK="$SDK_DIR/staging_dir/host/bin/apk"

[ -f "$SDK_DIR/rules.mk" ] || die "SDK_DIR ($SDK_DIR) does not look like an OpenWrt SDK"
[ -x "$APK" ] || die "host apk not found at $APK"
[ -f "$USQUE_BIN" ] || die "usque binary not found at $USQUE_BIN"
[ -f "$SIGN_KEY" ] || die "signing key not found at $SIGN_KEY"
[ -f "$PUB_KEY" ] || die "public key not found at $PUB_KEY"
[ -d "$REPO_DIR/openwrt/usque" ] || die "missing $REPO_DIR/openwrt/usque"

log "SDK: $SDK_DIR"
"$APK" --version || true
printf 'host arch: %s\n' "$(uname -m)"

# ---------------------------------------------------------------------------
# 1. Copy the package sources into the SDK and place the binary.
# ---------------------------------------------------------------------------
log "Copying packages into the SDK"
rm -rf "$SDK_DIR/package/usque" "$SDK_DIR/package/luci-app-usque"
mkdir -p "$SDK_DIR/package/usque"
cp -a "$REPO_DIR/openwrt/usque/." "$SDK_DIR/package/usque/"
mkdir -p "$SDK_DIR/package/usque/bin"
install -m 0755 "$USQUE_BIN" "$SDK_DIR/package/usque/bin/usque"

BUILD_LUCI=0
if [ -f "$REPO_DIR/openwrt/luci-app-usque/Makefile" ]; then
	mkdir -p "$SDK_DIR/package/luci-app-usque"
	cp -a "$REPO_DIR/openwrt/luci-app-usque/." "$SDK_DIR/package/luci-app-usque/"
	BUILD_LUCI=1
else
	printf 'WARNING: openwrt/luci-app-usque/Makefile not present yet; building the usque package only\n'
fi

# ---------------------------------------------------------------------------
# 2. Configure the SDK (no feeds are required: both packages are local).
# ---------------------------------------------------------------------------
cd "$SDK_DIR"
if [ ! -f .config ]; then
	: > .config
fi
{
	echo "CONFIG_PACKAGE_usque=y"
	[ "$BUILD_LUCI" = 1 ] && echo "CONFIG_PACKAGE_luci-app-usque=y"
} >> .config

log "Running make defconfig"
# DEVELOPER=1 makes the package compile targets work even when kconfig drops
# the selection because a dependency (kmod-tun / luci-base) is not part of
# this SDK; the dependency is still recorded in the apk metadata.
export DEVELOPER=1
make defconfig

TARGETS="package/usque/compile"
[ "$BUILD_LUCI" = 1 ] && TARGETS="$TARGETS package/luci-app-usque/compile"

log "Building: $TARGETS"
make -j"$JOBS" $TARGETS V=s

# ---------------------------------------------------------------------------
# 3. Collect the .apk files.
# ---------------------------------------------------------------------------
log "Collecting packages"
APKS=()
while IFS= read -r f; do
	APKS+=("$f")
done < <(find "$SDK_DIR/bin" -type f -name '*.apk' | sort)
[ "${#APKS[@]}" -gt 0 ] || die "no .apk files were produced under $SDK_DIR/bin"

for f in "${APKS[@]}"; do
	printf 'built: %s (%s bytes)\n' "$(basename "$f")" "$(stat -c '%s' "$f")"
done

# ---------------------------------------------------------------------------
# 4. Assemble and sign the feed.
# ---------------------------------------------------------------------------
log "Assembling feed in $OUT_DIR"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
for f in "${APKS[@]}"; do
	cp "$f" "$OUT_DIR/"
done
install -m 0644 "$PUB_KEY" "$OUT_DIR/usque.pem"

cd "$OUT_DIR"
shopt -s nullglob
PKG_FILES=(*.apk)
[ "${#PKG_FILES[@]}" -gt 0 ] || die "feed has no packages"
"$APK" mkndx \
	--allow-untrusted \
	--sign-key "$(cd "$(dirname "$SIGN_KEY")" && pwd)/$(basename "$SIGN_KEY")" \
	--output packages.adb \
	"${PKG_FILES[@]}"

"$APK" adbdump --format json packages.adb \
	| python3 "$SDK_DIR/scripts/make-index-json.py" -f apk -a "$ARCH" - > index.json

# ---------------------------------------------------------------------------
# 5. Report.
# ---------------------------------------------------------------------------
log "Feed contents"
ls -l "$OUT_DIR"
printf '\n'
printf 'packages.adb packages:\n'
"$APK" adbdump packages.adb | grep -E '^\s+(name|version|arch):' || true
printf '\n'
for f in "${PKG_FILES[@]}"; do
	printf '%-40s %8s bytes compressed\n' "$f" "$(stat -c '%s' "$f")"
done

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
	{
		echo "## usque package feed"
		echo
		echo "| file | size (bytes) |"
		echo "| --- | ---: |"
		for f in "$OUT_DIR"/*; do
			echo "| $(basename "$f") | $(stat -c '%s' "$f") |"
		done
	} >> "$GITHUB_STEP_SUMMARY"
fi

log "Done"
