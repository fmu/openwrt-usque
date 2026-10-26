#!/usr/bin/env bash
# Verify a built usque apk feed with the SDK's host apk (apk-tools 3):
#  - index parses and lists the expected packages with the right arch,
#  - the index signature is actually trusted when the public key is installed
#    (and rejected without it),
#  - the packages install into a throw-away root (with dummy kmod-tun and
#    luci-base packages) and contain the expected files,
#  - compressed and installed sizes are reported.
#
# Required environment:
#   SDK_DIR   path to the extracted OpenWrt SDK
#   FEED_DIR  path to the feed produced by build-feed.sh
#   PUB_KEY   public key that must match $FEED_DIR/usque.pem
#   SIGN_KEY  private key used to sign the throw-away verification index
set -euo pipefail

die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
log() { printf '\n==> %s\n' "$*"; }

: "${SDK_DIR:?SDK_DIR is required}"
: "${FEED_DIR:?FEED_DIR is required}"
: "${PUB_KEY:?PUB_KEY is required}"
: "${SIGN_KEY:?SIGN_KEY is required}"

ARCH="${ARCH:-arm_cortex-a7_neon-vfpv4}"
APK="$SDK_DIR/staging_dir/host/bin/apk"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

[ -x "$APK" ] || die "host apk not found at $APK"
[ -d "$FEED_DIR" ] || die "feed dir not found: $FEED_DIR"

log "Feed files"
ls -l "$FEED_DIR"
for f in packages.adb index.json usque.pem; do
	[ -s "$FEED_DIR/$f" ] || die "missing feed file: $f"
done
cmp -s "$PUB_KEY" "$FEED_DIR/usque.pem" || die "feed usque.pem does not match $PUB_KEY"
printf 'public key sha256: %s\n' "$(sha256sum "$FEED_DIR/usque.pem" | cut -d' ' -f1)"

log "Index contents (apk adbdump)"
"$APK" adbdump "$FEED_DIR/packages.adb" > "$WORK/packages.txt"
grep -E '^\s+(name|version|arch|depends|provides):' "$WORK/packages.txt" || true

# adbdump prints one "- name: ..." block per package; extract name/arch pairs.
awk '
	/^  - name:/ { name = $3 }
	/^    arch:/ { print name, $2 }
' "$WORK/packages.txt" > "$WORK/archs.txt" || true
printf '\npackage architectures:\n'
cat "$WORK/archs.txt" || true
grep -q '^usque ' "$WORK/archs.txt" || die "usque missing from index"
grep -q "^usque $ARCH\$" "$WORK/archs.txt" || die "usque has wrong arch (expected $ARCH)"
grep -q '^luci-app-usque noarch$' "$WORK/archs.txt" || die "luci-app-usque has wrong arch (expected noarch)"
grep -q 'kmod-tun' "$WORK/packages.txt" || die "usque does not depend on kmod-tun"

log "index.json"
python3 - "$FEED_DIR/index.json" "$ARCH" <<'PY'
import json, sys
idx = json.load(open(sys.argv[1]))
assert idx["architecture"] == sys.argv[2], idx
assert idx["version"] == 2, idx
pkgs = idx["packages"]
assert "usque" in pkgs, pkgs
assert "luci-app-usque" in pkgs, pkgs
print("index.json packages:", json.dumps(pkgs, sort_keys=True))
PY

# ---------------------------------------------------------------------------
# Trust tests: an untrusted key set must be rejected, our key must be accepted.
# ---------------------------------------------------------------------------
VERIFY="$WORK/repo"
mkdir -p "$VERIFY"
cp "$FEED_DIR"/*.apk "$VERIFY/"

# Dummy packages for the dependencies that live in the target feed, so that a
# full install can be simulated inside the throw-away root.  They are only
# used for this verification, never published.
mkdir -p "$WORK/empty"
make_dummy() {
	local name="$1"
	"$APK" mkpkg \
		--info "name:$name" \
		--info "version:0.0.1-r0" \
		--info "arch:$ARCH" \
		--info "description:dummy $name for feed verification" \
		--files "$WORK/empty" \
		--output "$VERIFY/$name-0.0.1-r0.apk" >/dev/null
}
make_dummy kmod-tun
make_dummy luci-base

cd "$VERIFY"
"$APK" mkndx --allow-untrusted --sign-key "$SIGN_KEY" --output packages.adb ./*.apk

log "Negative test: index must be untrusted without the key"
NEG="$WORK/root-neg"
mkdir -p "$NEG/etc/apk/keys"
set +e
out="$("$APK" --root "$NEG" --force-no-chroot --arch "$ARCH" \
	--repositories-file /dev/null \
	--repository "file://$VERIFY/packages.adb" \
	--keys-dir etc/apk/keys \
	--no-scripts --no-cache --initdb add usque 2>&1)"
rc=$?
set -e
printf '%s\n' "$out" | tail -5
if [ "$rc" -eq 0 ]; then
	die "index without a trusted key was accepted"
fi
printf '%s\n' "$out" | grep -qi 'untrusted\|signature' || die "unexpected failure for untrusted index"
echo "OK: untrusted index rejected"

log "Positive test: install both packages into a throw-away root"
ROOT="$WORK/root"
mkdir -p "$ROOT/etc/apk/keys"
install -m 0644 "$FEED_DIR/usque.pem" "$ROOT/etc/apk/keys/usque.pem"
"$APK" --root "$ROOT" --force-no-chroot --arch "$ARCH" \
	--repositories-file /dev/null \
	--repository "file://$VERIFY/packages.adb" \
	--keys-dir etc/apk/keys \
	--no-scripts --no-cache --initdb add luci-app-usque
echo "OK: signed index trusted, packages installed"

log "Installed files"
(cd "$ROOT" && find . -type f | sort)
[ -x "$ROOT/usr/bin/usque" ] || die "installed root is missing /usr/bin/usque"
[ -x "$ROOT/etc/init.d/usque" ] || die "installed root is missing /etc/init.d/usque"
[ -f "$ROOT/etc/config/usque" ] || die "installed root is missing /etc/config/usque"
[ -x "$ROOT/usr/libexec/usque/hook" ] || die "installed root is missing /usr/libexec/usque/hook"
[ -x "$ROOT/usr/libexec/usque/firewall" ] || die "installed root is missing /usr/libexec/usque/firewall"

log "apk metadata"
"$APK" --root "$ROOT" --force-no-chroot --repositories-file /dev/null info || true
printf '\n'
"$APK" --root "$ROOT" --force-no-chroot --repositories-file /dev/null info -R usque || true

log "Sizes"
report_size() {
	local name="$1" root="$2"
	local files installed compressed
	files="$(mktemp)"
	"$APK" --root "$root" --force-no-chroot --repositories-file /dev/null info -L "$name" 2>/dev/null \
		| sed 's|^/||' > "$files" || true
	installed=0
	while IFS= read -r f; do
		[ -n "$f" ] || continue
		case "$f" in *containing:*|*contains:*) continue ;; esac
		[ -f "$root/$f" ] || continue
		installed=$((installed + $(stat -c '%s' "$root/$f")))
	done < "$files"
	compressed="$(stat -c '%s' "$VERIFY/$name"-*.apk 2>/dev/null | head -1 || echo 0)"
	printf '%-18s compressed=%8s bytes  installed=%8s bytes\n' "$name" "$compressed" "$installed"
	rm -f "$files"
}
for pkg in usque luci-app-usque; do
	report_size "$pkg" "$ROOT"
done

log "Verify OK"
