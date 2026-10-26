#!/usr/bin/env bash
# Restore router config from a backup made by tests/backup.sh, then (optionally)
# remove only the stale hand-copied usque leftovers the user approved removing.
#
# Usage:
#   tests/restore.sh                 # restore newest local backup
#   tests/restore.sh <backup-dir>    # restore a specific tests/backups/<name>
#   tests/restore.sh --prune-leftovers [dir]
#
# --prune-leftovers removes: uci network.warp0 + network.warp0_v6, the
# 'warp0'/'warp0_v6' entries in firewall zone wan's network list, and
# /etc/usque/up.sh + /etc/usque/down.sh. It keeps /etc/usque/warp0.json.
set -uo pipefail
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"

PRUNE=0
if [ "${1:-}" = "--prune-leftovers" ]; then PRUNE=1; shift; fi
if [ -n "${1:-}" ]; then BAK="$1"; else BAK=$(cat "$TESTS_DIR/backups/.last" 2>/dev/null); BAK="$TESTS_DIR/backups/$BAK"; fi
[ -d "$BAK" ] || { echo "no such backup dir: $BAK" >&2; exit 1; }
echo "restoring from $BAK"

log "1. Restore /etc/config files"
for f in "$BAK"/config/*; do
  base=$(basename "$f")
  ssh "${SSH_OPTS[@]}" "$ROUTER" "cat > /etc/config/$base" < "$f" && echo "  restored /etc/config/$base"
done
if [ -d "$BAK/usque" ]; then
  rt "mkdir -p /etc/usque"
  for f in "$BAK"/usque/*; do
    base=$(basename "$f")
    ssh "${SSH_OPTS[@]}" "$ROUTER" "cat > /etc/usque/$base; chmod 600 /etc/usque/$base" < "$f" && echo "  restored /etc/usque/$base"
  done
fi
rt "uci commit network; uci commit firewall; uci commit dhcp 2>/dev/null; true"

log "2. Apply config"
rt "/etc/init.d/network reload; sleep 2; fw4 reload 2>&1 | tail -3; echo rc=\$?"
sleep 2

if [ "$PRUNE" -eq 1 ]; then
  log "3. Prune stale hand-copied usque leftovers"
  rt 'set -x
      uci -q delete network.warp0
      uci -q delete network.warp0_v6
      uci commit network
      uci -q del_list firewall.@zone[1].network=warp0
      uci -q del_list firewall.@zone[1].network=warp0_v6
      uci commit firewall
      rm -f /etc/usque/up.sh /etc/usque/down.sh
      fw4 reload 2>&1 | tail -2
      true'
  log "3b. Verify leftovers are gone"
  rt 'uci show network | grep -c warp0 || echo "network warp0 sections: 0"
      uci show firewall | grep -c warp0 || echo "firewall warp0 refs: 0"
      ls /etc/usque/'
fi

log "4. Post-restore state"
rt 'ip -4 route show; echo ===; ip -6 route show; echo ===; df -k /overlay; echo ===; curl -m 10 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp)="'
