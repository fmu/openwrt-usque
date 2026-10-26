#!/bin/sh
# On-router functional test for the luci.usque rpcd backend.
#
# Run this ON the router (it needs jshn, uci, ip, logread) with a copy of the
# backend available as /tmp/luci.usque.src:
#
#   ssh root@192.168.1.1 'cat > /tmp/luci.usque.src' < root/usr/libexec/rpcd/luci.usque
#   ssh root@192.168.1.1 'sh -s' < host-tests/router-backend-test.sh
#
# It is non-destructive: the script works on a test copy of the backend with
# DEFAULT_CONFIG=/etc/usque/test-warp0.json, a fake `usque` binary and a fake
# init script (USQUE_BIN / INIT_SCRIPT env overrides), so the real
# /etc/config/usque, /etc/usque/warp0.json and /etc/init.d/usque are untouched.
# Everything it creates under /tmp and /etc/usque/test-warp0.json is removed at
# the end. Exit code 0 = all checks passed.
#
# Local (on-router) functional test for the luci.usque rpcd backend.
# Uses a test copy of the script with a temp config path + fake usque/init
# binaries, so it never touches the real /etc/config or /etc/usque/warp0.json.
set -u

T=/tmp/usquetest
SRC=/tmp/luci.usque.src
rm -rf "$T"
mkdir -p "$T"

echo "=================================================================="
echo "setup"
echo "=================================================================="

cat > "$T/usque" <<'FAKE'
#!/bin/sh
cfg=""; prev=""
for a in "$@"; do
	[ "$prev" = "--config" ] && cfg="$a"
	prev="$a"
done
echo "fake-usque: args=$*"
# emulate the real CLI printing the freshly created config (contains secrets)
printf '%s\n' '{"private_key":"<REDACTED_WARP_PRIVATE_KEY>","access_token":"<REDACTED_WARP_ACCESS_TOKEN>","id":"test-device-id"}'
sleep 2
if [ -n "$cfg" ]; then
	printf '%s\n' '{"private_key":"TESTKEYMATERIAL0123456789abcdef0123456789abcdef","access_token":"TESTTOKEN-1234","id":"test-device-id","ipv4":"172.16.0.2","ipv6":"2606:4700:110::2","endpoint_v4":"162.159.198.2","endpoint_v6":"2606:4700:103::2","endpoint_h2_v4":"162.159.198.2","endpoint_h2_v6":""}' > "$cfg"
fi
exit 0
FAKE
chmod +x "$T/usque"

cat > "$T/slowusque" <<'SLOW'
#!/bin/sh
echo "slow-usque starting"
sleep 30
SLOW
chmod +x "$T/slowusque"

cat > "$T/init.sh" <<'INIT'
#!/bin/sh
E=/tmp/usquetest/init.enabled
R=/tmp/usquetest/init.running
case "$1" in
	enabled)  [ "$(cat $E 2>/dev/null)" = "1" ] && exit 0 || exit 1 ;;
	running)  [ "$(cat $R 2>/dev/null)" = "1" ] && exit 0 || exit 1 ;;
	enable)   echo 1 > "$E"; exit 0 ;;
	disable)  echo 0 > "$E"; exit 0 ;;
	start)    echo 1 > "$R"; exit 0 ;;
	stop)     echo 0 > "$R"; exit 0 ;;
	restart)  echo 1 > "$R"; exit 0 ;;
	*) echo "bad action" >&2; exit 1 ;;
esac
INIT
chmod +x "$T/init.sh"

# test copy: temp config path, short registration watchdog
sed -e 's#^DEFAULT_CONFIG="/etc/usque/warp0.json"#DEFAULT_CONFIG="/etc/usque/test-warp0.json"#' \
    -e 's/^REG_TIMEOUT=90/REG_TIMEOUT=6/' "$SRC" > "$T/luci.usque"
chmod +x "$T/luci.usque"

export USQUE_BIN="$T/usque"
export INIT_SCRIPT="$T/init.sh"
CONF=/etc/usque/test-warp0.json
rm -f "$CONF" "$T/init.state"
rm -rf /tmp/luci-usque

pass=0
fail=0
check() { # check <name> <expr-result> <detail>
	if [ "$2" = 0 ]; then
		pass=$((pass+1)); printf 'PASS  %s\n' "$1"
	else
		fail=$((fail+1)); printf 'FAIL  %s  (%s)\n' "$1" "$3"
	fi
}

echo
echo "=================================================================="
echo "1) list / status / getConfig without config file"
echo "=================================================================="
sh "$T/luci.usque" list | grep -q '"registerStatus"' && check "list exposes registerStatus" 0 || check "list exposes registerStatus" 1 "missing"
sh "$T/luci.usque" call status </dev/null | grep -q '"config_exists": false' && check "status: config_exists=false" 0 || check "status: config_exists=false" 1 "bad json"
sh "$T/luci.usque" call getConfig </dev/null | grep -q '"exists": false' && check "getConfig: exists=false" 0 || check "getConfig: exists=false" 1 "bad json"

echo
echo "=================================================================="
echo "2) register rejects missing ToS / missing binary / bad name"
echo "=================================================================="
r=$(sh "$T/luci.usque" call register <<'J'
{"name":"x","accept_tos":false}
J
)
echo "$r" | grep -q 'Terms of Service' && check "register without ToS rejected" 0 || check "register without ToS rejected" 1 "$r"

r=$(USQUE_BIN=/nonexistent sh "$T/luci.usque" call register <<'J'
{"accept_tos":true}
J
)
echo "$r" | grep -q 'binary not found' && check "register without binary rejected" 0 || check "register without binary rejected" 1 "$r"

r=$(sh "$T/luci.usque" call register <<'J'
{"name":"bad;name$(id)","accept_tos":true}
J
)
echo "$r" | grep -q 'invalid characters' && check "register rejects shell metacharacters in name" 0 || check "register rejects shell metacharacters in name" 1 "$r"

echo
echo "=================================================================="
echo "3) successful registration (async) + status polling + redaction"
echo "=================================================================="
r=$(sh "$T/luci.usque" call register <<'J'
{"name":"test router","accept_tos":true}
J
)
echo "$r" | grep -q '"started": true' && check "register returns started" 0 || check "register returns started" 1 "$r"
echo "$r" | grep -q '"name": "test router"' && check "register echoes device name" 0 || check "register echoes device name" 1 "$r"

r2=$(sh "$T/luci.usque" call register <<'J'
{"accept_tos":true}
J
)
echo "$r2" | grep -q 'already running' && check "second register while running rejected" 0 || check "second register while running rejected" 1 "$r2"

s1=$(sh "$T/luci.usque" call registerStatus </dev/null)
echo "$s1" | grep -q '"running": true' && check "registerStatus: running" 0 || check "registerStatus: running" 1 "$s1"
echo "$s1" | grep -q '"done": false' && check "registerStatus: not done" 0 || check "registerStatus: not done" 1 "$s1"
echo "$s1" | grep -q '\[redacted\]' && check "registerStatus output is redacted" 0 || check "registerStatus output is redacted" 1 "$s1"
echo "$s1" | grep -qi 'TESTKEYMATERIAL\|TESTTOKEN\|c8087d44' && check "no secrets in registerStatus output" 1 "secret leaked!" || check "no secrets in registerStatus output" 0

sleep 4
s2=$(sh "$T/luci.usque" call registerStatus </dev/null)
echo "$s2" | grep -q '"done": true' && check "registerStatus: done" 0 || check "registerStatus: done" 1 "$s2"
echo "$s2" | grep -q '"rc": 0' && check "registerStatus: rc=0" 0 || check "registerStatus: rc=0" 1 "$s2"
[ -s "$CONF" ] && check "config file created" 0 || check "config file created" 1 "missing"
mode=$(ls -ld "$CONF" | cut -c1-10)
[ "$mode" = "-rw-------" ] && check "config file mode 600 (chmod by backend)" 0 || check "config file mode 600 (chmod by backend)" 1 "$mode"
s3=$(sh "$T/luci.usque" call status </dev/null)
echo "$s3" | grep -q '"config_exists": true' && check "status: config_exists=true" 0 || check "status: config_exists=true" 1 "$s3"
echo "$s3" | grep -q '"config_secure": true' && check "status: config_secure=true" 0 || check "status: config_secure=true" 1 "$s3"
echo "$s3" | grep -q '"device_id": "test-device-id"' && check "status: device id parsed" 0 || check "status: device id parsed" 1 "$s3"
echo "$s3" | grep -qi 'private_key\|access_token\|TESTKEYMATERIAL' && check "status never leaks secrets" 1 "leak!" || check "status never leaks secrets" 0
g=$(sh "$T/luci.usque" call getConfig </dev/null)
echo "$g" | grep -qi 'private_key\|access_token\|TESTKEYMATERIAL' && check "getConfig never leaks secrets" 1 "leak!" || check "getConfig never leaks secrets" 0

echo
echo "=================================================================="
echo "4) registration failure output is redacted"
echo "=================================================================="
cat > "$T/failusque" <<'FAIL'
#!/bin/sh
echo 'Error: {"private_key":"LEAKME-abcdefabcdefabcdefabcdefabcdef","access_token":"LEAKTOKEN"}' >&2
exit 1
FAIL
chmod +x "$T/failusque"
rm -f "$CONF" /tmp/luci-usque/register.done
USQUE_BIN="$T/failusque" sh "$T/luci.usque" call register <<'J' >/dev/null
{"accept_tos":true}
J
sleep 2
sf=$(sh "$T/luci.usque" call registerStatus </dev/null)
echo "$sf" | grep -q '"rc": 1' && check "failed registration rc=1 reported" 0 || check "failed registration rc=1 reported" 1 "$sf"
echo "$sf" | grep -qi 'LEAKME\|LEAKTOKEN' && check "failed registration output redacted" 1 "leak!" || check "failed registration output redacted" 0

echo
echo "=================================================================="
echo "5) timeout watchdog"
echo "=================================================================="
rm -f "$CONF" /tmp/luci-usque/register.done
USQUE_BIN="$T/slowusque" sh "$T/luci.usque" call register <<'J' >/dev/null
{"accept_tos":true}
J
sleep 9
st=$(sh "$T/luci.usque" call registerStatus </dev/null)
echo "$st" | grep -q '"timed_out": true' && check "hung registration marked timed out" 0 || check "hung registration marked timed out" 1 "$st"
echo "$st" | grep -q '"done": true' && check "hung registration finished (killed)" 0 || check "hung registration finished (killed)" 1 "$st"
pkill -f slowusque 2>/dev/null

echo
echo "=================================================================="
echo "6) setInitAction + enable/disable state"
echo "=================================================================="
echo 0 > "$T/init.enabled"; echo 0 > "$T/init.running"
r=$(sh "$T/luci.usque" call setInitAction <<'J'
{"action":"enable"}
J
)
echo "$r" | grep -q '"ok": true' && check "setInitAction enable ok" 0 || check "setInitAction enable ok" 1 "$r"
[ "$(cat "$T/init.enabled")" = 1 ] && check "enable switched init state" 0 || check "enable switched init state" 1 "enabled=$(cat $T/init.enabled)"
r=$(sh "$T/luci.usque" call setInitAction <<'J'
{"action":"restart"}
J
)
echo "$r" | grep -q '"ok": true' && check "setInitAction restart ok" 0 || check "setInitAction restart ok" 1 "$r"
s=$(sh "$T/luci.usque" call status </dev/null)
# usque.main.enabled comes from UCI (absent in this test env -> false); the
# /etc/rc.d boot symlink is reported separately as init_enabled
echo "$s" | grep -q '"enabled": false' && check "status: enabled follows UCI flag" 0 || check "status: enabled follows UCI flag" 1 "$s"
echo "$s" | grep -q '"init_enabled": true' && check "status: init_enabled=true after enable" 0 || check "status: init_enabled=true after enable" 1 "$s"
echo "$s" | grep -q '"running": true' && check "status: running=true after restart" 0 || check "status: running=true after restart" 1 "$s"
r=$(sh "$T/luci.usque" call setInitAction <<'J'
{"action":"bogus"}
J
)
echo "$r" | grep -q '"ok": false' && check "setInitAction rejects invalid action" 0 || check "setInitAction rejects invalid action" 1 "$r"
r=$(INIT_SCRIPT=/nonexistent sh "$T/luci.usque" call setInitAction <<'J'
{"action":"start"}
J
)
echo "$r" | grep -q 'not found' && check "setInitAction reports missing init script" 0 || check "setInitAction reports missing init script" 1 "$r"

echo
echo "=================================================================="
echo "7) fixConfigPermissions + deleteConfig"
echo "=================================================================="
printf '{"id":"x"}\n' > "$CONF"
chmod 644 "$CONF"
r=$(sh "$T/luci.usque" call fixConfigPermissions </dev/null)
echo "$r" | grep -q '"ok": true' && check "fixConfigPermissions ok" 0 || check "fixConfigPermissions ok" 1 "$r"
mode=$(ls -ld "$CONF" | cut -c1-10)
[ "$mode" = "-rw-------" ] && check "fixConfigPermissions sets 600" 0 || check "fixConfigPermissions sets 600" 1 "$mode"
echo 1 > "$T/init.running"
r=$(sh "$T/luci.usque" call deleteConfig </dev/null)
echo "$r" | grep -q '"deleted": true' && check "deleteConfig ok" 0 || check "deleteConfig ok" 1 "$r"
[ -f "$CONF" ] && check "config file removed" 1 "still there" || check "config file removed" 0
[ "$(cat "$T/init.running")" = 0 ] && check "deleteConfig stops the service" 0 || check "deleteConfig stops the service" 1 "running=$(cat $T/init.running)"
r=$(sh "$T/luci.usque" call deleteConfig </dev/null)
echo "$r" | grep -q '"ok": true' && check "deleteConfig on missing file is idempotent" 0 || check "deleteConfig on missing file is idempotent" 1 "$r"

echo
echo "=================================================================="
echo "8) test method (egress trace parsing)"
echo "=================================================================="
r=$(sh "$T/luci.usque" call test </dev/null)
echo "$r" | grep -q '"warp":' && check "test returns warp state" 0 || check "test returns warp state" 1 "$r"
echo "$r" | grep -q '"colo":' && check "test returns colo" 0 || check "test returns colo" 1 "$r"
echo "$r" | grep -q '"method": "curl' && check "test used curl" 0 || check "test used curl" 1 "$r"
echo "$r" | grep -q '"bound": false' && check "test reports bound=false without tunnel" 0 || check "test reports bound=false without tunnel" 1 "$r"

echo
echo "=================================================================="
echo "9) logs"
echo "=================================================================="
r=$(sh "$T/luci.usque" call logs </dev/null)
echo "$r" | grep -q '"ok": true' && check "logs ok" 0 || check "logs ok" 1 "$r"
echo "$r" | grep -q '"lines"' && check "logs has lines" 0 || check "logs has lines" 1 "$r"

echo
echo "=================================================================="
echo "10) unknown method / bad input"
echo "=================================================================="
r=$(sh "$T/luci.usque" call bogusMethod </dev/null)
echo "$r" | grep -q 'Unknown method' && check "unknown method handled" 0 || check "unknown method handled" 1 "$r"
r=$(printf 'not json' | sh "$T/luci.usque" call register)
echo "$r" | grep -q '"ok": false' && check "invalid JSON input handled" 0 || check "invalid JSON input handled" 1 "$r"

echo
echo "=================================================================="
echo "cleanup"
echo "=================================================================="
pkill -f "$T/usque" 2>/dev/null
rm -rf /tmp/luci-usque "$CONF" "$T/init.enabled" "$T/init.running"
echo
printf 'RESULT: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ] || exit 1
