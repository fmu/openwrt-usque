#!/bin/sh
# On-router: idempotent unwind of every phase-1 test change. Safe to run at
# any time, from any state — used by the orchestrator's trap and by the
# watchdog that fires if the orchestrator dies unexpectedly.
DIR=/tmp/usque-test
IFACE=${1:-warp9}
logger -t usque-test "emergency-cleanup: starting (iface=$IFACE)"

# 1. Stop the test instance only (never a packaged/procd service).
#    Anchored pgrep: an unanchored pattern matches the ssh wrapper shell that
#    invoked this script and would kill our own session.
[ -f "$DIR/usque.pid" ] && kill "$(cat "$DIR/usque.pid")" 2>/dev/null
for pid in $(pgrep -f '^/tmp/usque-test/usque ' 2>/dev/null); do
  kill "$pid" 2>/dev/null
done
sleep 1
for pid in $(pgrep -f '^/tmp/usque-test/usque ' 2>/dev/null); do kill -9 "$pid" 2>/dev/null; done

# 2. Withdraw routes we installed.
DEF=$(ip route show default | grep " dev $IFACE" | head -1)
[ -n "$DEF" ] && ip route del default dev "$IFACE" 2>/dev/null
DEF6=$(ip -6 route show default | grep " dev $IFACE" | head -1)
[ -n "$DEF6" ] && ip -6 route del default dev "$IFACE" 2>/dev/null
if [ -f "$DIR/bypass.txt" ]; then
  set -- $(cat "$DIR/bypass.txt")
  [ -n "${1:-}" ] && ip route del "$1/32" via "$2" dev "$3" 2>/dev/null
  rm -f "$DIR/bypass.txt"
fi

# 2b. Remove the policy-routing bypass rule installed by p1-up.sh (if any).
ip rule show | grep -q "^1000:[[:space:]].*lookup main" && ip rule del priority 1000 2>/dev/null

# 3. Remove the tun device if the process is gone but the link lingers.
ip link show "$IFACE" >/dev/null 2>&1 && ip link del "$IFACE" 2>/dev/null

# 4. Disarm the watchdog.
rm -f "$DIR/armed"
[ -f "$DIR/watchdog.pid" ] && kill "$(cat "$DIR/watchdog.pid")" 2>/dev/null
rm -f "$DIR/watchdog.pid"

logger -t usque-test "emergency-cleanup: done default=$(ip route show default | tr '\n' ';')"
