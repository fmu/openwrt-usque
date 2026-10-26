#!/bin/sh
# On-router: installed as the usque `--on-connect` hook during phase-1 tests.
# Installs the tunnel default route + a host route that keeps the MASQUE
# endpoint itself reachable over the real WAN, then arms an emergency
# watchdog that unwinds everything if the test harness dies.
set -u
DIR=/tmp/usque-test
. "$DIR/wan.env" 2>/dev/null || { logger -t usque-test "p1-up: wan.env missing, refusing to touch routes"; exit 1; }

# USQUE_ENDPOINT is "ip:port" (v4 here). Strip the port.
EP=${USQUE_ENDPOINT%:*}
mkdir -p "$DIR"
echo "event=$USQUE_EVENT iface=$USQUE_IFACE endpoint=$USQUE_ENDPOINT ep=$EP" > "$DIR/hook-connect.txt"
logger -t usque-test "connect iface=$USQUE_IFACE endpoint=$USQUE_ENDPOINT"

# Keep the MASQUE endpoint on the physical WAN (more specific than default).
echo "$EP $WAN_GW $WAN_DEV" > "$DIR/bypass.txt"
ip route replace "$EP/32" via "$WAN_GW" dev "$WAN_DEV" \
  || logger -t usque-test "p1-up: endpoint bypass route failed"

# mwan3 (installed on this router) installs fwmark rules (priority 2001/...) that
# divert marked flows (e.g. TCP/443) to its WAN table, so a main-table default
# via the tunnel alone is silently bypassed. Pin policy lookup to main above
# mwan3's rules for the duration of the test.
ip rule show | grep -q "^1000:[[:space:]].*lookup main" || ip rule add priority 1000 from all lookup main \
  || logger -t usque-test "p1-up: could not install priority-1000 main rule"

# Send all other traffic through the tunnel.
ip route replace default dev "$USQUE_IFACE" metric 0 \
  || logger -t usque-test "p1-up: default route failed"

# Emergency unwind: if the orchestrating shell dies without cleanup, remove
# our routes after 150s. Cancel by deleting $DIR/armed.
: > "$DIR/armed"
( sleep 150; [ -f "$DIR/armed" ] && "$DIR/emergency-cleanup.sh" ) >/dev/null 2>&1 &
echo $! > "$DIR/watchdog.pid"

logger -t usque-test "p1-up: default=$(ip route show default | tr '\n' ';')"
