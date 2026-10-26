#!/bin/sh
# On-router functional test for the v2 luci.usque rpcd backend
# (multi-connection, per-section registration/import/control).
#
# Run it ON the router (needs jshn, ip, logread, curl) with a copy of the
# backend available as /tmp/luci.usque.src:
#
#   ssh root@192.168.1.1 'cat > /tmp/luci.usque.src' < root/usr/libexec/rpcd/luci.usque
#   ssh root@192.168.1.1 'sh -s' < host-tests/router-backend-test.sh
#
# It is non-destructive: the backend copy is driven with a fake `uci` (PATH),
# a fake `usque` binary (USQUE_BIN) and a fake init script (INIT_SCRIPT), so the
# real /etc/config/usque, /etc/usque/warp0.json and /etc/init.d/usque are never
# touched. Test config files are created under /etc/usque/ as
# test-warp0.json/test-custom.json and removed at the end; one log line is
# written to syslog to test the per-connection log filter. Exit code 0 = pass.
#
# On-router functional test for the v2 luci.usque backend (multi-connection).
# Uses a fake `uci`, fake usque binary and fake init script; never touches the
# real /etc/config/usque, /etc/usque or /etc/init.d/usque.
set -u

T=/tmp/usquetest2
SRC=/tmp/luci.usque.src
BIN="$T/luci.usque"
DB="$T/uci.db"
SEC="$T/uci.sections"

rm -rf "$T" /tmp/luci-usque
mkdir -p "$T"
[ -f "$SRC" ] || { echo "missing $SRC"; exit 2; }
cp "$SRC" "$BIN"
chmod +x "$BIN"
: > "$DB"
: > "$SEC"

# ---------------------------------------------------------------- fake uci
cat > "$T/uci" <<'FAKEUCI'
#!/bin/sh
DB=/tmp/usquetest2/uci.db
SEC=/tmp/usquetest2/uci.sections
[ "$1" = "-q" ] && shift
cmd="$1"; shift
case "$cmd" in
	get)
		key="$1"
		val="$(sed -n "s/^$key=//p" "$DB" 2>/dev/null | tail -n 1)"
		[ -n "$val" ] || exit 1
		printf '%s\n' "$val"
		;;
	show)
		[ "$1" = "usque" ] || exit 1
		while read -r sid type; do
			[ -n "$sid" ] || continue
			printf 'usque.%s=%s\n' "$sid" "$type"
			grep "^usque\.$sid\." "$DB" 2>/dev/null | while IFS='=' read -r k v; do
				printf "%s='%s'\n" "$k" "$v"
			done
		done < "$SEC"
		;;
	*)
		exit 1
		;;
esac
FAKEUCI
chmod +x "$T/uci"

# ------------------------------------------------------------ fake binaries
cat > "$T/usque" <<'FAKE'
#!/bin/sh
cfg=""; prev=""
for a in "$@"; do [ "$prev" = "--config" ] && cfg="$a"; prev="$a"; done
echo "fake-usque: args=$*"
printf '%s\n' '{"private_key":"MHcCAQEEIMC7SEKRITMATERIAL0123456789abcdef0123456789abcdef","access_token":"SECRETTOKEN-1234","id":"fake-device-id"}'
sleep 2
[ -n "$cfg" ] && printf '%s\n' '{"private_key":"FAKEKEYMATERIAL0123456789abcdef0123456789abcdef","access_token":"FAKETOKEN","id":"fake-device-id","ipv4":"172.16.0.2","ipv6":"2606:4700:110::2","endpoint_v4":"162.159.198.2","endpoint_v6":"2606:4700:103::2"}' > "$cfg"
exit 0
FAKE
chmod +x "$T/usque"

cat > "$T/init.sh" <<'INIT'
#!/bin/sh
R=/tmp/usquetest2/init.calls
printf '%s\n' "$*" >> "$R"
case "$1" in
	enable|disable|start|stop|restart) exit 0 ;;
	*) exit 1 ;;
esac
INIT
chmod +x "$T/init.sh"

export USQUE_BIN="$T/usque"
export INIT_SCRIPT="$T/init.sh"
export PATH="$T:$PATH"
export UCI_CONFIG=usque

A() { sh "$BIN" call "$1" </dev/null; }
call() { m="$1"; shift; sh "$BIN" call "$m"; }
ask() { # ask <method> <json>
	printf '%s' "$2" | sh "$BIN" call "$1"
}

setsec() { # setsec <sid> <type> <k=v> ...
	sid="$1"; type="$2"; shift 2
	grep -q "^$sid " "$SEC" 2>/dev/null || printf '%s %s\n' "$sid" "$type" >> "$SEC"
	for kv in "$@"; do printf 'usque.%s.%s\n' "$sid" "$kv" >> "$DB"; done
}
setglob() { printf '%s\n' "usque.settings.$1" >> "$DB"; }
dbkey() { printf '%s=%s\n' "$1" "$2" >> "$DB"; }

pass=0; fail=0
check() { if [ "$2" = 0 ]; then pass=$((pass+1)); printf 'PASS  %s\n' "$1"; else fail=$((fail+1)); printf 'FAIL  %s  (%s)\n' "$1" "$3"; fi; }
isjson() { printf '%s' "$1" | jsonfilter -e '@' >/dev/null 2>&1; }
has() { printf '%s' "$1" | grep -q -- "$2"; }

echo "=================================================================="
echo "1) empty configuration"
echo "=================================================================="
r=$(A status)
isjson "$r" && check "status is valid JSON" 0 || check "status is valid JSON" 1 "$r"
has "$r" '"connection_count": 0' && check "status: 0 connections" 0 || check "status: 0 connections" 1 "$r"
has "$r" '"init_script": true' && check "status: init script detected" 0 || check "status: init script detected" 1 "$r"
has "$r" '"connections": \[' && check "status: connections array" 0 || check "status: connections array" 1 "$r"

r=$(ask getConfig '{"section":"nope"}')
has "$r" 'Unknown section' && check "getConfig: unknown section rejected" 0 || check "getConfig: unknown section rejected" 1 "$r"
r=$(ask getConfig '{}')
has "$r" 'No connections are configured' && check "getConfig: no default section without connections" 0 || check "getConfig: no default section without connections" 1 "$r"

echo
echo "=================================================================="
echo "2) two connections: cloudflare (main) + custom (cnt2)"
echo "=================================================================="
mkdir -p /etc/usque
CONF=/etc/usque/test-warp0.json
CONF2=/etc/usque/test-custom.json
rm -f "$CONF" "$CONF2"
printf '%s\n' '{"private_key":"MAINKEYMATERIAL0123456789abcdef0123456789abcdef","access_token":"MAINTOKEN","id":"main-device","ipv4":"172.16.0.3","ipv6":"2606:4700:110:1::3","endpoint_v4":"162.159.198.2","endpoint_v6":"2606:4700:103::2"}' > "$CONF"
chmod 600 "$CONF"

setsec main usque enabled=1 mode=cloudflare config_file=$CONF interface=warp0 mtu=1400 ipv6=1 ipv6_route=auto default_route=0 metric=0 endpoint_port=443 always_reconnect=1 auto_register=1 device_name='' log_level=info
setsec cnt2 usque enabled=1 mode=custom config_file=$CONF2 interface=warp9 mtu=1300 ipv6=1 ipv6_route=always default_route=1 metric=10 endpoint_port=443 always_reconnect=0 auto_register=0 device_name='' log_level=debug sni=example.com
dbkey usque.settings.lan_ipv6 0
dbkey usque.settings.lan_ipv6_prefix ''
dbkey network.globals.ula_prefix fd12:3456:789a::/48
dbkey network.lan.device br-lan
dbkey dhcp.lan.ra server

r=$(A status)
isjson "$r" && check "status JSON with 2 connections" 0 || check "status JSON with 2 connections" 1 "$r"
has "$r" '"connection_count": 2' && check "status: 2 connections" 0 || check "status: 2 connections" 1 "$r"
has "$r" '"section": "main"' && check "status: main present" 0 || check "status: main present" 1 "$r"
has "$r" '"section": "cnt2"' && check "status: cnt2 present" 0 || check "status: cnt2 present" 1 "$r"
has "$r" '"mode": "custom"' && check "status: custom mode parsed" 0 || check "status: custom mode parsed" 1 "$r"
has "$r" '"default_route": 1\|"default_route": true' && check "status: default_route flag present" 0 || check "status: default_route flag present" 1 "$r"
has "$r" '"ipv6_route": "always"' && check "status: ipv6_route parsed" 0 || check "status: ipv6_route parsed" 1 "$r"
has "$r" '"ipv6": true' && check "status: ipv6 enabled" 0 || check "status: ipv6 enabled" 1 "$r"
has "$r" '"iface": "warp0"' && check "status: iface parsed" 0 || check "status: iface parsed" 1 "$r"
has "$r" '"config_exists": true' && check "status: main config exists" 0 || check "status: main config exists" 1 "$r"
has "$r" 'main-device' && check "status: device id parsed" 0 || check "status: device id parsed" 1 "$r"
has "$r" '"lan_ula": "fd12:3456:789a' && check "globals: ULA reported" 0 || check "globals: ULA reported" 1 "$r"
has "$r" '"lan_ipv6": false' && check "globals: lan_ipv6 off by default" 0 || check "globals: lan_ipv6 off by default" 1 "$r"
has "$r" '"route4_installed"' && check "status: route installed flag" 0 || check "status: route installed flag" 1 "$r"

# secrets must never leak
if has "$r" 'MAINKEYMATERIAL\|MAINTOKEN\|private_key\|access_token'; then
	check "status: no secrets leaked" 1 "LEAK"
else
	check "status: no secrets leaked" 0
fi

r=$(ask getConfig '{}')
has "$r" '"section": "main"' && check "getConfig without section uses the first connection" 0 || check "getConfig without section uses the first connection" 1 "$r"

r=$(ask getConfig '{"section":"main"}')
isjson "$r" && check "getConfig main JSON" 0 || check "getConfig main JSON" 1 "$r"
has "$r" '"device_id": "main-device"' && check "getConfig: device id" 0 || check "getConfig: device id" 1 "$r"
has "$r" '"warp_ipv4": "172.16.0.3"' && check "getConfig: warp ipv4" 0 || check "getConfig: warp ipv4" 1 "$r"
has "$r" '"secure": true' && check "getConfig: mode 600 secure" 0 || check "getConfig: mode 600 secure" 1 "$r"
if has "$r" 'private_key\|MAINKEY'; then check "getConfig: no secrets" 1 "LEAK"; else check "getConfig: no secrets" 0; fi

echo
echo "=================================================================="
echo "3) importConfig validation + success (custom mode)"
echo "=================================================================="
r=$(ask importConfig '{"section":"main","json":"{\"private_key\":\"x\"}"}')
has "$r" 'cloudflare mode' && check "import rejected in cloudflare mode" 0 || check "import rejected in cloudflare mode" 1 "$r"

r=$(ask importConfig '{"section":"cnt2","json":"not json"}')
has "$r" 'JSON object' && check "import rejects non-JSON" 0 || check "import rejects non-JSON" 1 "$r"

r=$(ask importConfig '{"section":"cnt2","json":"{bad"}')
has "$r" 'not valid JSON' && check "import rejects broken JSON" 0 || check "import rejects broken JSON" 1 "$r"

r=$(ask importConfig '{"section":"cnt2","json":"{\"ipv4\":\"172.16.0.9\",\"endpoint_v4\":\"192.0.2.1\",\"endpoint_pub_key\":\"-----BEGIN PUBLIC KEY-----\\nabc\\n-----END PUBLIC KEY-----\"}"}')
has "$r" 'private_key' && check "import requires private_key" 0 || check "import requires private_key" 1 "$r"

r=$(ask importConfig '{"section":"cnt2","json":"{\"private_key\":\"abcdefghijklmnopqrstuvwxyz0123456789\",\"ipv4\":\"172.16.0.9\",\"endpoint_v4\":\"192.0.2.1\",\"endpoint_pub_key\":\"nope\"}"}')
has "$r" 'PEM' && check "import requires PEM endpoint_pub_key" 0 || check "import requires PEM endpoint_pub_key" 1 "$r"

GOOD='{"private_key":"abcdefghijklmnopqrstuvwxyz0123456789ABCDEF","ipv4":"172.16.0.9","ipv6":"2606:4700:110:1::9","endpoint_v4":"192.0.2.1","endpoint_v6":"2001:db8::1","endpoint_pub_key":"-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----\n"}'
r=$(ask importConfig "{\"section\":\"cnt2\",\"json\":\"$(printf '%s' "$GOOD" | sed 's/"/\\"/g' | sed 's/\\n/\\\\n/g')\"}")
has "$r" '"imported": true' && check "import valid custom config" 0 || check "import valid custom config" 1 "$r"
[ -s "$CONF2" ] && check "import wrote config file" 0 || check "import wrote config file" 1 "missing"
mode=$(ls -ld "$CONF2" | cut -c1-10)
[ "$mode" = "-rw-------" ] && check "imported config mode 600" 0 || check "imported config mode 600" 1 "$mode"
if has "$r" 'private_key\|abcdefghij'; then check "import reply has no secrets" 1 "LEAK"; else check "import reply has no secrets" 0; fi

echo
echo "=================================================================="
echo "4) registration (cloudflare only, async)"
echo "=================================================================="
r=$(ask register '{"section":"cnt2","accept_tos":true}')
has "$r" 'custom mode' && check "register rejected for custom mode" 0 || check "register rejected for custom mode" 1 "$r"
r=$(ask register '{"section":"main","accept_tos":false}')
has "$r" 'Terms of Service' && check "register requires ToS" 0 || check "register requires ToS" 1 "$r"
rm -f "$CONF"
r=$(ask register '{"section":"main","name":"test router","accept_tos":true}')
has "$r" '"started": true' && check "register started" 0 || check "register started" 1 "$r"
r=$(ask register '{"section":"main","accept_tos":true}')
has "$r" 'already running' && check "second register rejected" 0 || check "second register rejected" 1 "$r"
r=$(ask registerStatus '{"section":"main"}')
has "$r" '"running": true' && check "registerStatus running" 0 || check "registerStatus running" 1 "$r"
if has "$r" 'FAKEKEYMATERIAL\|FAKETOKEN\|SECRETTOKEN'; then check "register output redacted" 1 "LEAK"; else check "register output redacted" 0; fi
sleep 4
r=$(ask registerStatus '{"section":"main"}')
has "$r" '"done": true' && check "registerStatus done" 0 || check "registerStatus done" 1 "$r"
has "$r" '"rc": 0' && check "registerStatus rc=0" 0 || check "registerStatus rc=0" 1 "$r"
[ -s "$CONF" ] && check "register created config" 0 || check "register created config" 1 "missing"
mode=$(ls -ld "$CONF" | cut -c1-10)
[ "$mode" = "-rw-------" ] && check "registered config mode 600" 0 || check "registered config mode 600" 1 "$mode"

echo
echo "=================================================================="
echo "5) per-section init actions"
echo "=================================================================="
: > "$T/init.calls"
r=$(ask setInitAction '{"action":"start","section":"main"}')
has "$r" '"ok": true' && check "start main ok" 0 || check "start main ok" 1 "$r"
grep -qx 'start main' "$T/init.calls" && check "init got section argument" 0 || check "init got section argument" 1 "$(cat $T/init.calls)"
r=$(ask setInitAction '{"action":"restart"}')
grep -qx 'restart' "$T/init.calls" && check "global restart without section" 0 || check "global restart without section" 1 "$(cat $T/init.calls)"
r=$(ask setInitAction '{"action":"enable"}')
grep -qx 'enable' "$T/init.calls" && check "enable is global" 0 || check "enable is global" 1 "$(cat $T/init.calls)"
r=$(ask setInitAction '{"action":"bogus"}')
has "$r" '"ok": false' && check "invalid action rejected" 0 || check "invalid action rejected" 1 "$r"
r=$(ask setInitAction '{"action":"start","section":"ghost"}')
has "$r" 'Unknown section' && check "unknown section rejected" 0 || check "unknown section rejected" 1 "$r"

echo
echo "=================================================================="
echo "6) test / logs / fixConfigPermissions / deleteConfig"
echo "=================================================================="
r=$(ask test '{"section":"main"}')
has "$r" '"warp":' && check "test returns warp state" 0 || check "test returns warp state" 1 "$r"
r=$(ask test '{}')
has "$r" '"warp":' && check "test without section uses the first connection" 0 || check "test without section uses the first connection" 1 "$r"
has "$r" '"section": "main"' && check "test reports the defaulted section" 0 || check "test reports the defaulted section" 1 "$r"
has "$r" '"method": "curl' && check "test used curl" 0 || check "test used curl" 1 "$r"
r=$(ask test '{"section":"ghost"}')
has "$r" 'Unknown section' && check "test: unknown section rejected" 0 || check "test: unknown section rejected" 1 "$r"

logger -t usque "main: functional test log line" 2>/dev/null
r=$(ask logs '{"section":"main"}')
has "$r" 'functional test log line' && check "logs filter finds section line" 0 || check "logs filter finds section line" 1 "$r"
r=$(ask logs '{}')
has "$r" '"ok": true' && check "logs without section ok" 0 || check "logs without section ok" 1 "$r"

chmod 644 "$CONF"
r=$(ask fixConfigPermissions '{"section":"main"}')
has "$r" '"ok": true' && check "fixConfigPermissions ok" 0 || check "fixConfigPermissions ok" 1 "$r"
mode=$(ls -ld "$CONF" | cut -c1-10)
[ "$mode" = "-rw-------" ] && check "fixConfigPermissions sets 600" 0 || check "fixConfigPermissions sets 600" 1 "$mode"

dbkey usque.ghost.config_file /tmp/outside.json
printf 'ghost usque\n' >> "$SEC"
r=$(ask deleteConfig '{"section":"ghost"}')
has "$r" 'outside' && check "deleteConfig refuses outside /etc/usque" 0 || check "deleteConfig refuses outside /etc/usque" 1 "$r"

r=$(ask deleteConfig '{"section":"cnt2"}')
has "$r" '"deleted": true' && check "deleteConfig cnt2 ok" 0 || check "deleteConfig cnt2 ok" 1 "$r"
[ -f "$CONF2" ] && check "custom config removed" 1 "still there" || check "custom config removed" 0
r=$(ask deleteConfig '{"section":"cnt2"}')
has "$r" '"ok": true' && check "deleteConfig idempotent" 0 || check "deleteConfig idempotent" 1 "$r"

echo
echo "=================================================================="
echo "7) LAN IPv6 globals"
echo "=================================================================="
dbkey usque.settings.lan_ipv6 1
dbkey usque.settings.lan_ipv6_prefix fdd0:1234:5678:1::/64
r=$(A status)
has "$r" '"lan_ipv6": true' && check "globals: lan_ipv6 on" 0 || check "globals: lan_ipv6 on" 1 "$r"
has "$r" '"lan_ipv6_prefix": "fdd0:1234:5678:1::' && check "globals: prefix reported" 0 || check "globals: prefix reported" 1 "$r"

echo
echo "=================================================================="
echo "8) malformed input"
echo "=================================================================="
r=$(sh "$BIN" call status </dev/null 2>/dev/null)
isjson "$r" && check "status tolerates empty stdin" 0 || check "status tolerates empty stdin" 1 "$r"
r=$(printf 'garbage' | sh "$BIN" call importConfig 2>/dev/null)
has "$r" '"ok": false' && check "import tolerates invalid JSON input" 0 || check "import tolerates invalid JSON input" 1 "$r"
r=$(sh "$BIN" call bogus 2>/dev/null)
has "$r" 'Unknown method' && check "unknown method answered" 0 || check "unknown method answered" 1 "$r"
r=$(sh "$BIN" list)
isjson "$r" && check "list is valid JSON" 0 || check "list is valid JSON" 1 "$r"
has "$r" '"importConfig"' && check "list exposes importConfig" 0 || check "list exposes importConfig" 1 "$r"

echo
echo "=================================================================="
echo "cleanup"
echo "=================================================================="
pkill -f "$T/usque" 2>/dev/null
rm -rf /tmp/luci-usque "$CONF" "$CONF2"
sed -i '/^ghost /d' "$SEC" 2>/dev/null
echo
printf 'RESULT: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ] || exit 1
