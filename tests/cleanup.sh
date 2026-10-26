#!/usr/bin/env bash
# Safety net: unwind anything a test run may have left behind on the router.
# Never stops a procd-managed service that a *test* did not start, never
# removes a package — that is phase2/uninstall's job.
#
# Usage: tests/cleanup.sh [--purge]
#   --purge  also delete /tmp/usque-test (test files, logs, backups)
set -uo pipefail
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"
PURGE=0; [ "${1:-}" = "--purge" ] && PURGE=1

log "1. Run router-side emergency cleanup (routes, test process, tun warp9)"
rt 'sh /tmp/usque-test/emergency-cleanup.sh warp9' 2>/dev/null || echo "  (no emergency-cleanup.sh on router yet)"

log "2. Kill any stray /tmp/usque-test binary instances"
rt 'for p in $(pgrep -f "/tmp/usque-test/usque" 2>/dev/null); do echo "  kill $p"; kill -9 "$p"; done; true'

log "3. Current routing state"
rt 'ip -4 route show; echo ===; ip -6 route show; echo ===; ip -br link | grep -E "warp|tun" || echo "(no warp/tun links)"'

log "4. Connectivity check"
rt 'curl -m 12 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp|colo)="'

if [ "$PURGE" -eq 1 ]; then
  log "5. Purge test files"
  rt 'rm -rf /tmp/usque-test; echo purged'
fi
log "Cleanup done"
