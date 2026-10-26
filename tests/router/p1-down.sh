#!/bin/sh
# On-router: usque `--on-disconnect` hook during phase-1 tests.
set -u
DIR=/tmp/usque-test
mkdir -p "$DIR"
echo "event=$USQUE_EVENT iface=$USQUE_IFACE endpoint=$USQUE_ENDPOINT" > "$DIR/hook-disconnect.txt"
logger -t usque-test "disconnect iface=$USQUE_IFACE endpoint=$USQUE_ENDPOINT"
# Withdraw the tunnel default route; the supervisor reconnects in ~1s and the
# on-connect hook installs it again.
DEFAULT_ROUTE=$(ip route show default | grep " dev $USQUE_IFACE" | head -1)
[ -n "$DEFAULT_ROUTE" ] && ip route del default dev "$USQUE_IFACE" 2>/dev/null
logger -t usque-test "p1-down: default=$(ip route show default | tr '\n' ';')"
