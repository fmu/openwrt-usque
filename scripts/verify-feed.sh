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

log "Index contents (apk adbdump --format json)"
"$APK" adbdump --format json "$FEED_DIR/packages.adb" > "$WORK/packages.json"
python3 - "$WORK/packages.json" "$ARCH" <<'PY'
import json
import sys

data = json.load(open(sys.argv[1]))
pkgs = data["packages"] if isinstance(data, dict) and "packages" in data else data
assert isinstance(pkgs, list) and pkgs, pkgs


def depnames(pkg):
    out = []
    for dep in pkg.get("depends", []):
        out.append(dep if isinstance(dep, str) else dep.get("name", ""))
    return out


by_name = {pkg["name"]: pkg for pkg in pkgs}
for name in sorted(by_name):
    pkg = by_name[name]
    print("%-18s version=%-10s arch=%-26s depends=%s"
          % (name, pkg.get("version"), pkg.get("arch"), " ".join(depnames(pkg))))

assert "usque" in by_name, "usque missing from index"
usque = by_name["usque"]
assert usque.get("arch") == sys.argv[2], \
    "usque arch %r != %r" % (usque.get("arch"), sys.argv[2])
assert "kmod-tun" in depnames(usque), \
    "usque does not depend on kmod-tun: %r" % depnames(usque)
assert str(usque.get("version", "")).startswith("0.2.0"), usque.get("version")

assert "luci-app-usque" in by_name, "luci-app-usque missing from index"
luci = by_name["luci-app-usque"]
assert luci.get("arch") == "noarch", "luci-app-usque arch %r" % luci.get("arch")
assert "luci-base" in depnames(luci), depnames(luci)
assert "usque" in depnames(luci), depnames(luci)

print("OK: package metadata is sane")
PY

log "index.json"
python3 - "$FEED_DIR/index.json" "$ARCH" <<'PY'
import json
import sys

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
# OpenWrt adds an implicit libc dependency to every package; the target
# feed supplies it, so provide a dummy here as well.
make_dummy libc
make_dummy libgcc
make_dummy kmod-tun
make_dummy luci-base

cd "$VERIFY"
"$APK" mkndx --allow-untrusted --sign-key "$SIGN_KEY" --output packages.adb ./*.apk

log "Negative test: index must be untrusted without the key"
NEG="$WORK/root-neg"
mkdir -p "$NEG/etc/apk/keys"
set +e
out="$("$APK" --root "$NEG" --force-no-chroot --usermode --arch "$ARCH" \
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
INSTALL_PKGS="luci-app-usque"
"$APK" --root "$ROOT" --force-no-chroot --usermode --arch "$ARCH" \
	--repositories-file /dev/null \
	--repository "file://$VERIFY/packages.adb" \
	--keys-dir etc/apk/keys \
	--no-scripts --no-cache --initdb add $INSTALL_PKGS
echo "OK: signed index trusted, packages installed"

log "Installed files"
(cd "$ROOT" && find . -type f | sort)
[ -x "$ROOT/usr/bin/usque" ] || die "installed root is missing /usr/bin/usque"
[ -x "$ROOT/etc/init.d/usque" ] || die "installed root is missing /etc/init.d/usque"
[ -f "$ROOT/etc/config/usque" ] || die "installed root is missing /etc/config/usque"
[ -x "$ROOT/usr/libexec/usque/hook" ] || die "installed root is missing /usr/libexec/usque/hook"
[ -x "$ROOT/usr/libexec/usque/firewall" ] || die "installed root is missing /usr/libexec/usque/firewall"
[ -x "$ROOT/usr/libexec/usque/lan_ipv6" ] || die "installed root is missing /usr/libexec/usque/lan_ipv6"
[ -f "$ROOT/www/luci-static/resources/view/usque/overview.js" ] || die "installed root is missing the LuCI view"
[ -f "$ROOT/usr/share/luci/menu.d/luci-app-usque.json" ] || die "installed root is missing the LuCI menu entry"
[ -f "$ROOT/usr/share/rpcd/acl.d/luci-app-usque.json" ] || die "installed root is missing the LuCI ACL"
[ -x "$ROOT/usr/libexec/rpcd/luci.usque" ] || die "installed root is missing the rpcd backend"

log "apk metadata"
"$APK" --root "$ROOT" --force-no-chroot --usermode --repositories-file /dev/null info || true
printf '\n'
"$APK" --root "$ROOT" --force-no-chroot --usermode --repositories-file /dev/null info -R usque || true

log "Sizes"
report_size() {
	local name="$1" root="$2"
	local files installed compressed
	files="$(mktemp)"
	"$APK" --root "$root" --force-no-chroot --usermode --repositories-file /dev/null info -L "$name" 2>/dev/null \
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
for pkg in $INSTALL_PKGS; do
	report_size "$pkg" "$ROOT"
done

log "Verify OK"
