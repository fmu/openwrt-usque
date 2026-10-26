#!/usr/bin/env bash
# Back up every piece of router state the tests can touch.
#
# Saves to BOTH /tmp/usque-test/backup-<ts> on the router and tests/backups/<ts>
# on the Mac (/tmp is tmpfs, so it does not survive the reboot test).
#
# Usage: tests/backup.sh [label]
set -uo pipefail
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"
LABEL="${1:-pre-phase2}"
TS=$(date +%Y%m%d-%H%M%S)
NAME="$LABEL-$TS"
LOCAL_DIR="$TESTS_DIR/backups/$NAME"
RT_DIR="$ROUTER_TMP/backup-$TS"
mkdir -p "$LOCAL_DIR"

log "Backing up router state -> $RT_DIR (mirror: $LOCAL_DIR)"
rt "mkdir -p $RT_DIR/config $RT_DIR/apk/keys $RT_DIR/apk/repositories.d $RT_DIR/usque
    cp -a /etc/config/. $RT_DIR/config/
    cp -a /etc/apk/keys/. $RT_DIR/apk/keys/ 2>/dev/null
    cp -a /etc/apk/repositories.d/. $RT_DIR/apk/repositories.d/ 2>/dev/null
    cp -a /etc/usque/. $RT_DIR/usque/ 2>/dev/null
    uci show network > $RT_DIR/uci-network.txt
    uci show firewall > $RT_DIR/uci-firewall.txt
    ip -4 route show > $RT_DIR/ip-route4.txt
    ip -6 route show > $RT_DIR/ip-route6.txt
    ip -br link > $RT_DIR/ip-link.txt
    df -k > $RT_DIR/df.txt
    apk info -v | sort > $RT_DIR/apk-world.txt
    ls -la /etc/init.d/usque /etc/config/usque 2>/dev/null > $RT_DIR/usque-presence.txt
    errexit=0; true" || { echo "router backup failed" >&2; exit 1; }

# mirror to Mac (excluding nothing; the whole backup dir is small)
for f in $(rt "cd $RT_DIR && find . -type f | sed 's|^\./||'"); do
  mkdir -p "$LOCAL_DIR/$(dirname "$f")"
  rtget "$RT_DIR/$f" "$LOCAL_DIR/$f"
done
echo "$NAME" > "$TESTS_DIR/backups/.last"
echo "$RT_DIR" > "$LOCAL_DIR/ROUTER_PATH.txt"

log "Backup summary"
find "$LOCAL_DIR" -type f | sed 's/^/  /'
( cd "$LOCAL_DIR" && shasum -a 256 config/network config/firewall usque/warp0.json 2>/dev/null )
echo "  local:  $LOCAL_DIR"
echo "  router: $RT_DIR"
