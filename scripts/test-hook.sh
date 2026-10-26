#!/usr/bin/env bash
# Test the usque routing hook in an isolated network namespace.
#
# Requires Linux, root (or passwordless sudo) and iproute2:
#   sudo bash scripts/test-hook.sh openwrt/usque/files/usr/libexec/usque/hook
#
# The test creates:
#   wan0  (veth, 10.99.0.2/24, default via 10.99.0.1, fd00::2/64)
#   warp0 (dummy, 172.16.0.2/32 + fd00::99/128, stands in for the TUN device)
# plus a simulated mwan3 setup (table 1 WAN default + fwmark rule at priority
# 2001) and asserts that connect/disconnect install and clean up routes and
# the priority-1000 policy override without ever leaving the namespace
# without a default route.
set -euo pipefail

HOOK="${1:-openwrt/usque/files/usr/libexec/usque/hook}"
[ -f "$HOOK" ] || { echo "hook not found: $HOOK" >&2; exit 1; }
HOOK="$(cd "$(dirname "$HOOK")" && pwd)/$(basename "$HOOK")"
command -v ip >/dev/null || { echo "iproute2 required" >&2; exit 1; }
[ "$(id -u)" = 0 ] || { echo "must run as root (try: sudo bash $0 $HOOK)" >&2; exit 1; }

NS="usque-hook-test-$$"
STATE="$(mktemp -d)"
FAILS=0

cleanup() {
	ip netns del "$NS" 2>/dev/null || true
	ip link del wan-host 2>/dev/null || true
	rm -rf "$STATE"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }
pass() { echo "PASS: $*"; }
nsip() { ip netns exec "$NS" ip "$@"; }

run_hook() {
	local event="$1" endpoint="${2:-162.159.198.2:443}" v6="${3:-0}"
	ip netns exec "$NS" env \
		USQUE_EVENT="$event" \
		USQUE_IFACE=warp0 \
		USQUE_ENDPOINT="$endpoint" \
		USQUE_IPV4=172.16.0.2 \
		USQUE_IPV6=fd00::99 \
		USQUE_IPV6_ENABLED="$v6" \
		USQUE_STATE_DIR="$STATE" \
		"$HOOK"
}

default_dev() { nsip route get 1.1.1.1 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p'; }
default6_dev() { nsip -6 route get 2001:db8::1 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p'; }
marked_dev() { nsip route get 1.1.1.1 mark 0x100 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p'; }
rule1000() { nsip rule show 2>/dev/null | grep -c '^1000:.*lookup main' || true; }
wan_default_count() { nsip route show default | grep -c "dev wan0" || true; }

echo "== setting up namespace $NS"
ip netns add "$NS"
ip netns exec "$NS" ip link set lo up

ip link add wan-host type veth peer name wan0
ip link set wan0 netns "$NS"
ip addr add 10.99.0.1/24 dev wan-host
ip addr add fd00::1/64 dev wan-host
ip link set wan-host up
nsip link set wan0 up
nsip addr add 10.99.0.2/24 dev wan0
nsip addr add fd00::2/64 dev wan0
nsip link add warp0 type dummy
nsip link set warp0 up
nsip addr add 172.16.0.2/32 dev warp0
nsip addr add fd00::99/128 dev warp0
nsip route add default via 10.99.0.1 dev wan0 metric 0
nsip -6 route add default via fd00::1 dev wan0 metric 0

# Simulated mwan3: marked flows (TCP/443) are sent to table 1 (WAN) by a
# policy rule that is evaluated before the main table.
nsip route add table 1 default via 10.99.0.1 dev wan0
nsip rule add priority 2001 fwmark 0x100/0x3f00 lookup 1

echo
echo "== test 0: baseline, mwan3-style policy routing sends marked flows to wan0"
[ "$(marked_dev)" = "wan0" ] && pass "marked flow uses wan0 before connect" || fail "marked flow does not use wan0 before connect (got $(marked_dev))"
[ "$(rule1000)" -eq 0 ] && pass "no priority-1000 rule before connect" || fail "priority-1000 rule present before connect"

echo
echo "== test 1: connect installs endpoint bypass, warp0 default and policy override"
run_hook connect
[ "$(default_dev)" = "warp0" ] && pass "IPv4 default route uses warp0" || fail "IPv4 default route does not use warp0 (got $(default_dev))"
[ "$(marked_dev)" = "warp0" ] && pass "mwan3-marked flow now uses warp0 (priority 1000 rule wins)" || fail "marked flow still bypasses the tunnel (got $(marked_dev))"
[ "$(rule1000)" -eq 1 ] && pass "priority-1000 lookup main rule installed" || fail "priority-1000 rule missing"
nsip route show 162.159.198.2/32 | grep -q 'dev wan0' && pass "endpoint bypass route via wan0 present" || fail "endpoint bypass route missing"
[ "$(default6_dev)" = "wan0" ] && pass "IPv6 default untouched when ipv6 disabled" || fail "IPv6 default changed although ipv6 disabled (got $(default6_dev))"

echo
echo "== test 2: connect is idempotent"
run_hook connect
[ "$(default_dev)" = "warp0" ] && pass "second connect keeps warp0 default" || fail "second connect broke IPv4 default"
[ "$(rule1000)" -eq 1 ] && pass "no duplicate priority-1000 rules" || fail "priority-1000 rules duplicated ($(rule1000))"
[ "$(wan_default_count)" -le 1 ] && pass "no duplicate endpoint bypass entries" || fail "duplicate endpoint bypass entries"

echo
echo "== test 3: disconnect restores WAN routing and cleans up"
run_hook disconnect
[ "$(default_dev)" = "wan0" ] && pass "IPv4 default restored to wan0" || fail "IPv4 default not restored (got $(default_dev))"
[ "$(marked_dev)" = "wan0" ] && pass "mwan3 policy routing works again" || fail "marked flow still diverted after disconnect (got $(marked_dev))"
[ "$(rule1000)" -eq 0 ] && pass "priority-1000 rule removed" || fail "priority-1000 rule left behind"
[ -z "$(nsip route show 162.159.198.2/32)" ] && pass "endpoint bypass removed" || fail "endpoint bypass still present"
[ "$(nsip route show default | grep -c 'dev warp0')" -eq 0 ] && pass "no warp0 default left" || fail "warp0 default left behind"
[ "$(nsip route show default | grep -c 'dev wan0')" -eq 1 ] && pass "exactly one WAN default route" || fail "unexpected WAN default count"

echo
echo "== test 4: disconnect is idempotent"
run_hook disconnect
[ "$(default_dev)" = "wan0" ] && pass "second disconnect keeps WAN default" || fail "second disconnect broke IPv4 default"

echo
echo "== test 5: connect/disconnect with IPv6 enabled"
run_hook connect "162.159.198.2:443" 1
[ "$(default6_dev)" = "warp0" ] && pass "IPv6 default route uses warp0 when enabled" || fail "IPv6 default route not using warp0 (got $(default6_dev))"
[ "$(default_dev)" = "warp0" ] && pass "IPv4 default route uses warp0" || fail "IPv4 default route not using warp0"
nsip -6 rule show 2>/dev/null | grep -q '^1000:.*lookup main' && pass "IPv6 priority-1000 rule installed" || fail "IPv6 priority-1000 rule missing"
run_hook disconnect "" 1
[ "$(default6_dev)" = "wan0" ] && pass "IPv6 default restored to wan0" || fail "IPv6 default not restored (got $(default6_dev))"
[ "$(default_dev)" = "wan0" ] && pass "IPv4 default restored to wan0" || fail "IPv4 default not restored"

echo
echo "== test 6: IPv6 endpoint (bracketed) does not break IPv4"
run_hook connect "[2606:4700:103::2]:443" 0
nsip -6 route show 2606:4700:103::2/128 | grep -q 'dev wan0' && pass "IPv6 endpoint bypass via wan0 present" || fail "IPv6 endpoint bypass missing"
[ "$(default_dev)" = "warp0" ] && pass "IPv4 default route uses warp0" || fail "IPv4 default route not using warp0"
run_hook disconnect "" 0
[ "$(default_dev)" = "wan0" ] && pass "IPv4 default restored" || fail "IPv4 default not restored"

echo
if [ "$FAILS" -ne 0 ]; then
	echo "RESULT: $FAILS test(s) FAILED"
	exit 1
fi
echo "RESULT: all hook route tests passed"
