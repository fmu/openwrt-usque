#!/usr/bin/env bash
# Test the usque routing hook (v2) in an isolated network namespace.
#
# Requires Linux, root (or passwordless sudo) and iproute2:
#   sudo bash scripts/test-hook.sh openwrt/usque/files/usr/libexec/usque/hook
#
# The test creates:
#   wan0   (veth, 10.99.0.2/24 + fd00:1::2/64, defaults via 10.99.0.1 / fd00:1::1)
#   warp0  (dummy, 172.16.0.2/32 + fd00::99/128, stands in for a TUN device)
#   warp1  (dummy, 172.17.0.2/32, second connection)
# plus a simulated mwan3 setup:
#   - IPv4: table 1 WAN default + `ip rule 2001 fwmark 0x100/0x3f00 lookup 1`
#   - IPv6: `ip -6 rule 2062 fwmark 0x3e00/0x3f00 unreachable`
# and asserts:
#   - default_route=0 / ipv6_route=off: the hook makes NO routing changes,
#   - default_route=1: v4 default + endpoint bypass + priority-1000 rule,
#     marked mwan3 flows now use the tunnel; revert restores the WAN,
#   - ipv6_route=auto: untouched while a native v6 default exists, installs
#     when there is none; ipv6_route=always installs; both add the v6 rule,
#   - two instances are ref-counted: the rule and the WAN default survive
#     until the last owner disconnects.
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

# run_hook <event> <section> <iface> <endpoint> <v6enabled> <v6route> <defroute> <metric>
run_hook() {
	local event="$1" section="$2" iface="$3" endpoint="$4" v6="$5" v6route="$6" defroute="$7" metric="$8"
	ip netns exec "$NS" env \
		USQUE_EVENT="$event" \
		USQUE_SECTION="$section" \
		USQUE_IFACE="$iface" \
		USQUE_ENDPOINT="$endpoint" \
		USQUE_IPV4=172.16.0.2 \
		USQUE_IPV6=fd00::99 \
		USQUE_STATE_ROOT="$STATE" \
		USQUE_STATE_DIR="$STATE/$section" \
		USQUE_IPV6_ENABLED="$v6" \
		USQUE_IPV6_ROUTE="$v6route" \
		USQUE_DEFAULT_ROUTE="$defroute" \
		USQUE_METRIC="$metric" \
		"$HOOK"
}

default_dev() { nsip route get 1.1.1.1 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p'; }
default6_dev() { nsip -6 route get 2001:db8::1 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p'; }
marked_dev() { nsip route get 1.1.1.1 mark 0x100 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p'; }
marked6_out() { nsip -6 route get 2001:db8::1 mark 0x3e00 2>&1 || true; }
rule4() { nsip rule show 2>/dev/null | grep -c '^1000:.*lookup main' || true; }
rule6() { nsip -6 rule show 2>/dev/null | grep -c '^1000:.*lookup main' || true; }
wan_default_count() { nsip route show default | grep -c "dev wan0" || true; }

echo "== setting up namespace $NS"
ip netns add "$NS"
ip netns exec "$NS" ip link set lo up

ip link add wan-host type veth peer name wan0
ip link set wan0 netns "$NS"
ip addr add 10.99.0.1/24 dev wan-host
ip addr add fd00:1::1/64 dev wan-host
ip link set wan-host up
nsip link set wan0 up
nsip addr add 10.99.0.2/24 dev wan0
nsip addr add fd00:1::2/64 dev wan0
for iface in warp0 warp1; do
	nsip link add "$iface" type dummy
	nsip link set "$iface" up
done
nsip addr add 172.16.0.2/32 dev warp0
nsip addr add fd00::99/128 dev warp0
nsip addr add 172.17.0.2/32 dev warp1
nsip route add default via 10.99.0.1 dev wan0 metric 0
nsip -6 route add default via fd00:1::1 dev wan0 metric 0

# Simulated mwan3 policy routing.
nsip route add table 1 default via 10.99.0.1 dev wan0
nsip rule add priority 2001 fwmark 0x100/0x3f00 lookup 1
nsip -6 rule add priority 2062 fwmark 0x3e00/0x3f00 unreachable

echo
echo "== test 0: baseline (mwan3 diverts marked flows)"
[ "$(marked_dev)" = "wan0" ] && pass "marked IPv4 flow uses wan0 before connect" || fail "marked IPv4 flow not via wan0 (got $(marked_dev))"
[ "$(default_dev)" = "wan0" ] && pass "IPv4 default via wan0" || fail "unexpected IPv4 default ($(default_dev))"
case "$(marked6_out)" in *unreachable*) pass "marked IPv6 flow is unreachable before connect" ;; *) fail "marked IPv6 flow not unreachable: $(marked6_out)" ;; esac

echo
echo "== test 1: default install (default_route=0, ipv6_route=off) changes nothing"
run_hook connect warp0 warp0 "162.159.198.2:443" 0 off 0 0
[ "$(default_dev)" = "wan0" ] && pass "IPv4 default still via wan0" || fail "IPv4 default changed ($(default_dev))"
[ "$(default6_dev)" = "wan0" ] && pass "IPv6 default still via wan0" || fail "IPv6 default changed ($(default6_dev))"
[ "$(rule4)" -eq 0 ] && pass "no IPv4 policy rule installed" || fail "IPv4 policy rule installed"
[ "$(rule6)" -eq 0 ] && pass "no IPv6 policy rule installed" || fail "IPv6 policy rule installed"
[ -z "$(nsip route show 162.159.198.2/32)" ] && pass "no endpoint bypass installed" || fail "endpoint bypass installed"
[ -z "$(nsip route show default | grep 'dev warp0')" ] && pass "no tunnel default route installed" || fail "tunnel default route installed"

echo
echo "== test 2: default_route=1 installs v4 routing (and only v4)"
run_hook connect warp0 warp0 "162.159.198.2:443" 0 off 1 0
[ "$(default_dev)" = "warp0" ] && pass "IPv4 default via warp0" || fail "IPv4 default not via warp0 ($(default_dev))"
[ "$(marked_dev)" = "warp0" ] && pass "mwan3-marked IPv4 flow now uses warp0 (priority-1000 rule wins)" || fail "marked IPv4 flow still diverted ($(marked_dev))"
[ "$(rule4)" -eq 1 ] && pass "IPv4 priority-1000 rule installed" || fail "IPv4 priority-1000 rule missing"
[ "$(rule6)" -eq 0 ] && pass "no IPv6 policy rule (v6 untouched)" || fail "unexpected IPv6 policy rule"
[ "$(default6_dev)" = "wan0" ] && pass "IPv6 default untouched" || fail "IPv6 default changed ($(default6_dev))"
nsip route show 162.159.198.2/32 | grep -q 'dev wan0' && pass "endpoint bypass via wan0 present" || fail "endpoint bypass missing"

echo
echo "== test 3: second instance (warp1) and ref-counted teardown"
run_hook connect warp1 warp1 "162.159.198.2:443" 0 off 1 1
[ "$(nsip route show default | grep -c 'dev warp1')" -eq 1 ] && pass "warp1 default route installed" || fail "warp1 default route missing"
[ "$(rule4)" -eq 1 ] && pass "single IPv4 priority-1000 rule for two owners" || fail "unexpected IPv4 rule count ($(rule4))"
run_hook disconnect warp0 warp0 "" 0 off 1 0
[ -z "$(nsip route show default | grep 'dev warp0')" ] && pass "warp0 default route removed" || fail "warp0 default route left"
[ "$(rule4)" -eq 1 ] && pass "IPv4 rule kept while warp1 still owns a tunnel default" || fail "IPv4 rule removed too early"
[ -n "$(nsip route show default | grep 'dev warp1')" ] && pass "warp1 default route still present" || fail "warp1 default route removed"
run_hook disconnect warp1 warp1 "" 0 off 1 1
[ "$(default_dev)" = "wan0" ] && pass "WAN IPv4 default restored after last owner left" || fail "WAN default not restored ($(default_dev))"
[ "$(rule4)" -eq 0 ] && pass "IPv4 rule removed after last owner left" || fail "IPv4 rule left behind"
[ "$(wan_default_count)" -eq 1 ] && pass "exactly one WAN default route" || fail "unexpected WAN default count"

echo
echo "== test 4: ipv6_route=auto leaves a native IPv6 default alone"
run_hook connect warp0 warp0 "162.159.198.2:443" 1 auto 0 0
[ "$(default6_dev)" = "wan0" ] && pass "IPv6 default untouched with native v6" || fail "IPv6 default changed ($(default6_dev))"
[ "$(rule6)" -eq 0 ] && pass "no IPv6 policy rule with native v6" || fail "unexpected IPv6 policy rule"
run_hook disconnect warp0 warp0 "" 1 auto 0 0

echo
echo "== test 5: ipv6_route=auto installs IPv6 when there is no native default"
nsip -6 route del default via fd00:1::1 dev wan0
run_hook connect warp0 warp0 "162.159.198.2:443" 1 auto 0 0
[ "$(default6_dev)" = "warp0" ] && pass "IPv6 default via warp0 installed" || fail "IPv6 default not installed ($(default6_dev))"
[ "$(rule6)" -eq 1 ] && pass "IPv6 priority-1000 rule installed" || fail "IPv6 priority-1000 rule missing"
case "$(marked6_out)" in *warp0*) pass "marked IPv6 flow now uses warp0" ;; *) fail "marked IPv6 flow not via warp0: $(marked6_out)" ;; esac
run_hook disconnect warp0 warp0 "" 1 auto 0 0
[ -z "$(nsip -6 route show default)" ] && pass "IPv6 tunnel default removed" || fail "IPv6 tunnel default left"
[ "$(rule6)" -eq 0 ] && pass "IPv6 priority-1000 rule removed" || fail "IPv6 rule left behind"

echo
echo "== test 6: ipv6_route=always replaces an existing IPv6 default and reverts"
nsip -6 route add default via fd00:1::1 dev wan0 metric 0
run_hook connect warp0 warp0 "162.159.198.2:443" 1 always 0 0
[ "$(default6_dev)" = "warp0" ] && pass "IPv6 default via warp0 (always)" || fail "IPv6 default not via warp0 ($(default6_dev))"
run_hook disconnect warp0 warp0 "" 1 always 0 0
[ "$(default6_dev)" = "wan0" ] && pass "IPv6 WAN default restored" || fail "IPv6 WAN default not restored ($(default6_dev))"
[ "$(rule6)" -eq 0 ] && pass "IPv6 rule removed" || fail "IPv6 rule left behind"

echo
echo "== test 7: disconnect is idempotent and never breaks IPv4"
run_hook disconnect warp0 warp0 "" 0 off 0 0
run_hook disconnect warp0 warp0 "" 1 always 0 0
[ "$(default_dev)" = "wan0" ] && pass "IPv4 default still via wan0" || fail "IPv4 default broken ($(default_dev))"

echo
if [ "$FAILS" -ne 0 ]; then
	echo "RESULT: $FAILS test(s) FAILED"
	exit 1
fi
echo "RESULT: all hook route tests passed"
