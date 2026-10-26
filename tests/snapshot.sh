#!/usr/bin/env bash
# Capture the router's baseline state (also useful for before/after diffing).
# Usage: tests/snapshot.sh [outdir]   (default: tests/baseline)
set -uo pipefail
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"
OUT="${1:-$BASE_DIR}"
mkdir -p "$OUT"

log "Snapshot router $ROUTER -> $OUT"
rt 'date; echo === release; cat /etc/openwrt_release; echo === uname; uname -a
    echo === routes4; ip -4 route show
    echo === routes6; ip -6 route show
    echo === rule; ip -4 rule show
    echo === link; ip -br link
    echo === addr; ip -br addr
    echo === df; df -h
    echo === memory; cat /proc/meminfo | head -3
    echo === resolv; cat /etc/resolv.conf
    echo === tun_wg_modules; lsmod | grep -E "^(tun|wireguard) "
    echo === usque_leftovers; ls -la /etc/usque/ 2>/dev/null; ls -la /etc/init.d/ | grep -i usque
    echo === config_hash; sha256sum /etc/usque/warp0.json 2>/dev/null
    echo === processes; ps w | grep -i -E "usque|warp" | grep -v grep
    echo === logread_usque; logread | grep -i usque | tail -20
    echo === wan_egress; curl -m 10 -s https://www.cloudflare.com/cdn-cgi/trace' > "$OUT/snapshot.txt"

rt 'uci show network' > "$OUT/uci-network.txt"
rt 'uci show firewall' > "$OUT/uci-firewall.txt"
rt 'apk info -v 2>/dev/null | sort' > "$OUT/apk-packages.txt"
rt 'df -k' > "$OUT/df.txt"
wc -l "$OUT"/*.txt
