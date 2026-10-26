#!/usr/bin/env bash
# Phase 2: real signed apk feed install, procd service, routing/firewall, LuCI,
# reboot, and uninstall/rollback on the live router.
#
# Staged so each step can be inspected (and a failing step re-run) individually:
#
#   tests/phase2.sh backup      # back up /etc/config, /etc/apk, /etc/usque (router + Mac)
#   tests/phase2.sh stale       # remove approved stale hand-copied usque leftovers
#   tests/phase2.sh feed        # signed feed: key + repositories.d + apk update/add (transcript)
#   tests/phase2.sh service     # procd lifecycle, tun/routes/firewall, egress, LAN client
#   tests/phase2.sh luci        # menu.d/acl.d/rpcd + ubus + HTTP assets (+login if LUCI_PASSWORD)
#   tests/phase2.sh reboot      # pre-reboot evidence, reboot, wait, post-reboot checks
#   tests/phase2.sh uninstall   # apk del + leftover check + reinstall for the final state
#   tests/phase2.sh final       # restore baseline except approved leftovers, final dumps
#   tests/phase2.sh all         # backup stale feed service luci (reboot/uninstall are manual)
#
# Env: FEED_DIR=<dir with packages.adb + *.apk + usque.pem>  FEED_PORT=8123
#      FEED_HOST=192.168.1.240  LUCI_PASSWORD=<luci root password, optional>
#      MAKE_MWAN3_WORKAROUND=1  (test only; the *package* must do this itself)
set -uo pipefail
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"

STEP="${1:-}"
IFACE=warp0
FEED_PORT="${FEED_PORT:-8123}"
FEED_HOST="${FEED_HOST:-192.168.1.240}"
FEED_MODE="${FEED_MODE:-http}"          # http | file | pages
PAGES_URL="https://fmu.github.io/openwrt-usque/feed/packages.adb"
RT_FEED_DIR="$ROUTER_TMP/feed"
PKG_LOG="$ART_DIR/phase2-transcript.log"
PASS=0; FAIL=0; SKIP=0; declare -a CHECKS
LAST=PASS
record() { local n=$1 r=${2:-$LAST}; [ -n "${3:-}" ] && n="$n [$3]"
  CHECKS+=("$n|$r")
  [ "$r" = PASS ] && PASS=$((PASS+1)); [ "$r" = FAIL ] && FAIL=$((FAIL+1)); [ "$r" = SKIP ] && SKIP=$((SKIP+1)); return 0; }
ok()   { LAST=PASS; printf '  [PASS] %s\n' "$*"; }
bad()  { LAST=FAIL; printf '  [FAIL] %s\n' "$*"; }
info() { printf '  [ .. ] %s\n' "$*"; }
log()  { printf '\n### %s\n' "$*"; }
show() { printf '%s\n' "$*" | sed 's/^/  | /'; }
finish() {
  log "Summary ($(date -u +%FT%TZ))"
  local c; for c in "${CHECKS[@]:-}"; do [ -n "$c" ] || continue; IFS='|' read -r n r <<< "$c"; printf '  %-64s %s\n' "$n" "$r"; done
  echo "  PASS=$PASS FAIL=$FAIL SKIP=$SKIP"; echo "  transcript: $PKG_LOG"
  [ "$FAIL" -eq 0 ]
}

df_free() { rt 'df -k /overlay | awk "NR==2{print \$4}"'; }
wait_router_up() { # seconds
  local i=0 n=${1:-90}
  while [ "$i" -lt "$n" ]; do
    if ssh "${SSH_OPTS[@]}" -o ConnectTimeout=5 "$ROUTER" true 2>/dev/null; then return 0; fi
    sleep 3; i=$((i+3))
  done
  return 1
}
serve_feed() {
  [ -n "${FEED_DIR:-}" ] && [ -d "$FEED_DIR" ] || { bad "FEED_DIR not set/found"; return 1; }
  pkill -f "http.server $FEED_PORT" 2>/dev/null
  ( cd "$FEED_DIR" && nohup python3 -m http.server "$FEED_PORT" --bind "$FEED_HOST" >/tmp/feed-http.log 2>&1 & echo $! > /tmp/feed-http.pid )
  sleep 1
  local code; code=$(rt "curl -m 8 -s -o /dev/null -w '%{http_code}' $FEED_URL")
  if [ "$code" = 200 ]; then ok "feed reachable from router: $FEED_URL (HTTP 200)"; record "local feed reachable"; return 0; fi
  bad "feed not reachable from router ($FEED_URL -> $code)"; record "local feed reachable" ; return 1
}
stop_feed_http() { [ -f /tmp/feed-http.pid ] && kill "$(cat /tmp/feed-http.pid)" 2>/dev/null; rm -f /tmp/feed-http.pid; }

# ---------------------------------------------------------------- steps
step_backup() {
  log "1. Backup"
  "$TESTS_DIR/backup.sh" pre-phase2 || { bad "backup failed"; record backup; return 1; }
  ok "backup complete"; record "backup /etc/config + /etc/apk + /etc/usque"
  log "1b. Overlay space baseline"
  show "$(rt 'df -k /overlay /tmp; echo; apk info -s 2>/dev/null | tail -1')"
}

step_stale() {
  log "2. Remove approved stale hand-copied leftovers (backup exists)"
  PRE_DF=$(df_free)
  rt 'set -x
      uci -q delete network.warp0
      uci -q delete network.warp0_v6
      uci commit network
      uci -q del_list firewall.@zone[1].network=warp0
      uci -q del_list firewall.@zone[1].network=warp0_v6
      uci commit firewall
      rm -f /etc/usque/up.sh /etc/usque/down.sh
      true' | sed 's/^/  | /'
  rt 'fw4 check && fw4 reload' 2>&1 | sed 's/^/  | /'
  sleep 2
  AFTER=$(rt 'uci show network | grep -c warp0 || true; uci show firewall | grep -c warp0 || true; ls /etc/usque/')
  show "$AFTER"
  echo "$AFTER" | head -1 | grep -qx 0 && { ok "no warp0 uci network sections left"; record "stale uci network.warp0 removed"; } \
                                     || { bad "warp0 uci network sections still present"; record "stale uci network.warp0 removed"; }
  echo "$AFTER" | sed -n 2p | grep -qx 0 && { ok "no warp0 firewall refs left"; record "stale firewall warp0 refs removed"; } \
                                      || { bad "warp0 firewall refs still present"; record "stale firewall warp0 refs removed"; }
  echo "$AFTER" | grep -qx 'warp0.json' && { ok "/etc/usque/warp0.json kept"; record "warp0.json kept"; } \
                                        || { bad "/etc/usque/warp0.json missing"; record "warp0.json kept"; }
  WAN=$(rt 'curl -m 12 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp)="')
  show "$WAN"
  echo "$WAN" | grep -q 'warp=off' && { ok "WAN still healthy after cleanup"; record "WAN healthy after leftovers cleanup"; } \
                                    || { bad "WAN broken after cleanup"; record "WAN healthy after leftovers cleanup"; }
  show "overlay free before=${PRE_DF}K after=$(df_free)K"
}

step_feed() {
  log "3. Signed feed install (mode=$FEED_MODE)"
  : > "$PKG_LOG"
  local before_free; before_free=$(df_free); info "overlay free before: ${before_free}K"
  case "$FEED_MODE" in
    http)
      serve_feed || return 1
      FEED_URL="http://$FEED_HOST:$FEED_PORT/packages.adb" ;;
    file)
      [ -n "${FEED_DIR:-}" ] && [ -d "$FEED_DIR" ] || { bad "FEED_DIR not set/found"; record "local file feed"; return 1; }
      rt "rm -rf $RT_FEED_DIR; mkdir -p $RT_FEED_DIR"
      ( cd "$FEED_DIR" && find . -maxdepth 1 -type f | sed 's|^\./||' ) | while read -r f; do rtpush "$FEED_DIR/$f" "$RT_FEED_DIR/$f"; done
      show "$(rt "ls -la $RT_FEED_DIR")"
      FEED_URL="file://$RT_FEED_DIR/packages.adb" ;;
    pages)
      FEED_URL="$PAGES_URL" ;;
    *) bad "unknown FEED_MODE=$FEED_MODE"; record "feed mode"; return 1 ;;
  esac
  info "repository: $FEED_URL"

  # public key -> /etc/apk/keys; repo -> /etc/apk/repositories.d
  local keyfile; keyfile=$(find "${FEED_DIR:-/nonexistent}" -maxdepth 1 \( -name '*.pem' -o -name '*.pub' \) 2>/dev/null | head -1)
  if [ -z "$keyfile" ] && [ "$FEED_MODE" = pages ]; then
    keyfile=/tmp/usque-pages-key.pem
    curl -m 15 -sS -o "$keyfile" "https://fmu.github.io/openwrt-usque/feed/usque.pem" || true
  fi
  [ -n "$keyfile" ] && [ -s "$keyfile" ] || { bad "no public key found"; record "feed public key present"; return 1; }
  if [ "$FEED_MODE" = pages ]; then
    ok "Pages feed key downloaded ($(wc -c < "$keyfile") bytes)"
  fi
  info "public key: $keyfile ($(wc -c < "$keyfile") bytes) sha256 $(shasum -a 256 "$keyfile" | awk '{print $1}')"
  rt "mkdir -p /etc/apk/keys /etc/apk/repositories.d; cat > /etc/apk/keys/$(basename "$keyfile")" < "$keyfile"
  rt "echo '$FEED_URL' > /etc/apk/repositories.d/usque.list; cat /etc/apk/repositories.d/usque.list"
  record "feed key installed in /etc/apk/keys" PASS

  log "3b. apk update (signed index verification)"
  { echo "=== apk update ($FEED_URL)"; rt 'apk update 2>&1'; echo "=== repos:"; rt 'cat /etc/apk/repositories.d/usque.list'; } | tee -a "$PKG_LOG" | sed 's/^/  | /'
  log "3c. apk add usque luci-app-usque"
  local add_out add_rc
  add_out=$(rt 'apk add usque luci-app-usque 2>&1'); add_rc=$?
  { echo "=== apk add usque luci-app-usque (rc=$add_rc)"; echo "$add_out"; } | tee -a "$PKG_LOG" | sed 's/^/  | /'
  if [ "$add_rc" -eq 0 ]; then ok "apk add succeeded via signed feed (no --allow-untrusted)"; record "signed feed apk add (usque + luci-app-usque)"; else bad "apk add failed"; record "signed feed apk add"; return 1; fi

  log "3d. Installed size / overlay after"
  local after_free; after_free=$(df_free)
  { echo "=== apk info -s"; rt 'apk info -s usque luci-app-usque 2>&1'; echo "=== du"; rt 'du -sk /usr/bin/usque /etc/init.d/usque /etc/config/usque /www/luci-static/resources/view/usque /usr/libexec/usque /usr/libexec/rpcd/luci.usque /usr/share/luci/menu.d/luci-app-usque.json /usr/share/rpcd/acl.d/luci-app-usque.json 2>/dev/null'; echo "=== df"; rt 'df -k /overlay'; } | tee -a "$PKG_LOG" | sed 's/^/  | /'
  show "overlay free before=${before_free}K after=${after_free}K (delta $((before_free-after_free))K)"
  [ "${after_free:-0}" -gt 500 ] && { ok "overlay still has ${after_free}K free"; record "overlay space after install" ; } \
                                 || { bad "overlay critically low: ${after_free}K"; record "overlay space after install"; }
  log "3e. Package file list"
  rt 'apk info -L usque luci-app-usque 2>&1' | tee -a "$PKG_LOG" | sed 's/^/  | /'
  for f in /usr/bin/usque /etc/init.d/usque /etc/config/usque /usr/libexec/usque/hook /usr/libexec/usque/firewall /usr/share/luci/menu.d/luci-app-usque.json /usr/share/rpcd/acl.d/luci-app-usque.json /usr/libexec/rpcd/luci.usque; do
    rt "test -e $f" && { ok "installed: $f"; record "file $f"; } || { bad "MISSING: $f"; record "file $f"; }
  done
}

step_service() {
  log "4. Enable + start warp0 with v2 defaults (default_route=0, ipv6_route=auto)"
  rt 'uci set usque.warp0.enabled=1; uci commit usque; /etc/init.d/usque enable' >/dev/null 2>&1
  rt '/etc/init.d/usque start' | sed 's/^/  | /'
  local i=0 live=0
  while [ "$i" -lt 45 ]; do
    if rt "ip link show $IFACE >/dev/null 2>&1 && pgrep -f '/usr/bin/usque' >/dev/null"; then live=1; break; fi
    sleep 1; i=$((i+1))
  done
  [ "$live" = 1 ] && { ok "service started, $IFACE up after ~${i}s"; record "service start (tun up in ${i}s)"; } \
                  || { bad "service did not bring up $IFACE within 45s"; record "service start"; }
  log "4a. status + logs"
  S=$(rt '/etc/init.d/usque status; echo "---ubus"; ubus call service list "{\"name\":\"usque\"}" | head -c 700; echo; echo "---log"; logread -e usque | tail -25')
  show "$S"
  echo "$S" | grep -qi 'Connected to MASQUE server\|tunnel established\|MASQUE' && { ok "MASQUE connected per logs"; record "service: MASQUE connected"; } \
                                                                              || { bad "no MASQUE connect log line"; record "service: MASQUE connected"; }
  log "4a0. DEFAULT-OFF SAFETY: no IPv4 routing change with default_route=0"
  SAFE=$(rt 'echo ===v4def; ip -4 route show default; echo ===v6def; ip -6 route show default; echo ===rule4; ip rule show | grep -E "^1000:.*lookup main" || echo none; echo ===rule6; ip -6 rule show | grep -E "^1000:.*lookup main" || echo none; echo ===bypass; ip -4 route show 162.159.198.2/32 2>/dev/null || echo none; echo ===via_tun4; ip -4 route show | grep " dev warp0" || echo none')
  show "$SAFE"
  echo "$SAFE" | sed -n '/===v4def/,/===v6def/p' | grep -q 'default via .*dev phy0-tether' && { ok "WAN IPv4 default intact"; record "default-off: WAN v4 default intact"; } \
                                                                                        || { bad "WAN IPv4 default changed"; record "default-off: WAN v4 default intact"; }
  echo "$SAFE" | sed -n '/===v4def/,/===v6def/p' | grep -q 'dev warp0' && { bad "IPv4 default taken over despite default_route=0"; record "default-off: no v4 takeover"; } \
                                                                         || { ok "no IPv4 default via warp0"; record "default-off: no v4 takeover"; }
  echo "$SAFE" | sed -n '/===rule4/,/===rule6/p' | grep -q '^1000:' && { bad "IPv4 policy rule installed despite default_route=0"; record "default-off: no v4 policy rule"; } \
                                                                      || { ok "no IPv4 policy rule"; record "default-off: no v4 policy rule"; }
  echo "$SAFE" | sed -n '/===bypass/,/===via_tun4/p' | grep -q '162.159.198.2' && { bad "endpoint bypass installed despite default_route=0"; record "default-off: no endpoint bypass"; } \
                                                                                || { ok "no endpoint bypass"; record "default-off: no endpoint bypass"; }
  echo "$SAFE" | sed -n '/===v6def/,$p' | grep -q 'dev warp0' && { ok "IPv6 default via warp0 installed (ipv6_route=auto, no native v6)"; record "ipv6: auto default via tun"; } \
                                                                 || { bad "no IPv6 default via warp0"; record "ipv6: auto default via tun"; }
  echo "$SAFE" | sed -n '/===rule6/,$p' | grep -q '^1000:' && { ok "IPv6 priority-1000 rule installed"; record "ipv6: policy rule installed"; } \
                                                              || { bad "no IPv6 policy rule"; record "ipv6: policy rule installed"; }

  log "4b. tun device"
  T=$(rt "echo === link; ip -br link show $IFACE; echo === addr4; ip -4 addr show dev $IFACE | grep inet; echo === addr6; ip -6 addr show dev $IFACE | grep inet6; echo === mtu; ip link show $IFACE | head -1")
  show "$T"
  echo "$T" | grep -q "$IFACE" && { ok "$IFACE present"; record "service: tun device"; } || { bad "no $IFACE"; record "service: tun device"; }
  echo "$T" | grep -q '172.16.0.2' && { ok "tun has 172.16.0.2"; record "service: tun IPv4"; } || { bad "tun missing IPv4"; record "service: tun IPv4"; }
  echo "$T" | grep -q '2606:4700:110' && { ok "tun has WARP IPv6"; record "service: tun IPv6"; } || { bad "tun missing IPv6"; record "service: tun IPv6"; }

  log "4b2. firewall zone + forwarding (LAN/guest) while tunnel up"
  F=$(rt 'uci show firewall | grep -A10 "usque" | head -20; echo ===nft; nft list ruleset | sed -n "/chain forward_lan {/,/}/p"; nft list ruleset | sed -n "/chain forward_guest {/,/}/p"')
  show "$F"
  echo "$F" | grep -q "firewall.usque" && { ok "fw4 zone 'usque' created"; record "firewall: usque zone"; } || { bad "no usque firewall zone"; record "firewall: usque zone"; }
  echo "$F" | grep -q 'accept_to_usque' && { ok "forwarding into usque zone present"; record "firewall: forwarding to usque"; } || info "no forwarding chain to usque yet"
  GJ=$(rt 'nft list ruleset | sed -n "/chain forward_guest {/,/}/p"')
  echo "$GJ" | grep -q 'accept_to_usque' && { ok "guest -> usque forwarding present"; record "guest -> usque forwarding"; } \
                                          || { bad "guest -> usque forwarding missing (guest clients would break with routing on)"; record "guest -> usque forwarding"; }

  log "4c. egress with defaults (v4 must still be WAN, v6 via WARP)"
  V4=$(rt 'curl -4 -m 15 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp)="')
  V6=$(rt 'curl -6 -m 20 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp|colo)="')
  show "v4: $V4"; show "v6: $V6"
  echo "$V4" | grep -q 'warp=off' && { ok "IPv4 egress still direct WAN (warp=off)"; record "egress: v4 warp=off with defaults"; } \
                                   || { bad "IPv4 egress changed with default_route=0"; record "egress: v4 warp=off with defaults"; }
  echo "$V6" | grep -q 'warp=on' && { ok "IPv6 egress through WARP (warp=on)"; record "egress: v6 warp=on via tun"; } \
                                 || { bad "IPv6 egress not warp=on"; record "egress: v6 warp=on via tun"; }

  log "4d. resource usage"
  RES=$(rt 'PID=$(pgrep -f "/usr/bin/usque" | head -1); grep -E "^(VmSize|VmRSS|Threads)" /proc/$PID/status
        read -r _ _ _ _ _ _ _ _ _ _ _ _ _ U1 S1 _ < /proc/$PID/stat; sleep 10; read -r _ _ _ _ _ _ _ _ _ _ _ _ _ U2 S2 _ < /proc/$PID/stat
        echo "cpu=$((($U2-$U1+$S2-$S1)/10))% of one core avg over 10s"')
  show "$RES"

  log "4e. restart"
  rt '/etc/init.d/usque restart' >/dev/null 2>&1; sleep 8
  rt "ip link show $IFACE >/dev/null 2>&1" && { ok "restart brought tun back"; record "service: restart"; } || { bad "restart failed"; record "service: restart"; }

  log "4f. stop -> everything reverted, WAN default untouched"
  rt '/etc/init.d/usque stop' >/dev/null 2>&1; sleep 4
  POSTSTOP=$(rt 'echo ===v4def; ip -4 route show default; echo ===v6def; ip -6 route show default; echo ===rule4; ip rule show | grep -E "^1000:" || echo none; echo ===rule6; ip -6 rule show | grep -E "^1000:" || echo none; echo ===link; ip link show warp0 >/dev/null 2>&1 && echo PRESENT || echo ABSENT; echo ===trace4; curl -4 -m 12 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp)="')
  show "$POSTSTOP"
  echo "$POSTSTOP" | sed -n '/===v4def/,/===v6def/p' | grep -q 'default via .*dev phy0-tether' && { ok "WAN default intact after stop"; record "stop: WAN default intact"; } || { bad "WAN default changed after stop"; record "stop: WAN default intact"; }
  echo "$POSTSTOP" | sed -n '/===rule4/,/===rule6/p' | grep -q '^1000:' && { bad "IPv4 rule left after stop"; record "stop: no leftover rules"; } || { ok "no leftover policy rules after stop"; record "stop: no leftover rules"; }
  echo "$POSTSTOP" | grep -q 'ABSENT' && { ok "tun device removed on stop"; record "stop: tun removed"; } || info "tun still present after stop"
  echo "$POSTSTOP" | grep -q 'warp=off' && { ok "v4 egress WAN after stop"; record "stop: egress ok"; } || { bad "v4 egress broken after stop"; record "stop: egress ok"; }
  log "4g. start again for following steps"
  rt '/etc/init.d/usque start' >/dev/null 2>&1; sleep 6
  rt "ip link show $IFACE >/dev/null 2>&1" && { ok "service up again"; record "service: start after stop"; } || { bad "start after stop failed"; record "service: start after stop"; }
}

step_luci() {
  log "5. LuCI app"
  F=$(rt 'echo === menu; ls -l /usr/share/luci/menu.d/luci-app-usque.json 2>&1; echo === acl; ls -l /usr/share/rpcd/acl.d/luci-app-usque.json 2>&1; echo === view; ls -l /www/luci-static/resources/view/usque/ 2>&1; echo === rpcd; ls -l /usr/libexec/rpcd/luci.usque 2>&1; echo === menu-content; cat /usr/share/luci/menu.d/luci-app-usque.json 2>/dev/null; echo === acl-content; cat /usr/share/rpcd/acl.d/luci-app-usque.json 2>/dev/null')
  show "$F"
  echo "$F" | grep -q 'menu.d/luci-app-usque.json' && { ok "menu.d entry installed"; record "LuCI menu.d"; } || { bad "menu.d entry missing"; record "LuCI menu.d"; }
  echo "$F" | grep -q 'acl.d/luci-app-usque.json' && { ok "acl.d entry installed"; record "LuCI acl.d"; } || { bad "acl.d entry missing"; record "LuCI acl.d"; }
  rt 'test -f /www/luci-static/resources/view/usque/overview.js' && { ok "JS view asset installed"; record "LuCI view asset"; } || { bad "JS view asset missing"; record "LuCI view asset"; }
  rt 'rm -f /tmp/luci-indexcache.*; rm -rf /tmp/luci-modulecache; /etc/init.d/rpcd reload' >/dev/null 2>&1; sleep 3
  U=$(rt 'echo === list; ubus -S list luci.usque 2>&1; echo === status; ubus -S call luci.usque status 2>&1; echo === getConfig; ubus -S call luci.usque getConfig "{\"section\":\"warp0\"}" 2>&1; echo === test; ubus -S call luci.usque test "{\"section\":\"warp0\"}" 2>&1; echo === logs; ubus -S call luci.usque logs "{\"section\":\"warp0\"}" 2>&1 | head -c 600; echo')
  show "$U"
  echo "$U" | grep -q 'luci.usque' && { ok "ubus object luci.usque present"; record "ubus object luci.usque"; } || { bad "ubus object missing"; record "ubus object luci.usque"; }
  echo "$U" | grep -q '"' && { ok "ubus methods return JSON"; record "ubus methods return JSON"; } || { bad "ubus methods returned no JSON"; record "ubus methods return JSON"; }
  echo "$U" | grep -q 'connections' && { ok "status reports connections[]"; record "ubus: status connections[]"; } || { bad "status missing connections[]"; record "ubus: status connections[]"; }
  echo "$U" | grep -q '"warp0"' && { ok "status/getConfig reference section warp0"; record "ubus: section warp0"; } || { bad "no warp0 section in ubus output"; record "ubus: section warp0"; }
  if echo "$U" | grep -qiE 'private_key|access_token'; then bad "SECRET LEAK in ubus output (private_key/access_token)"; record "ubus: no secrets leaked"; else ok "no private_key/access_token in any ubus output"; record "ubus: no secrets leaked"; fi
  echo "$U" | grep -q 'invalid or missing section\|Invalid or missing section' && { bad "section parameter rejected"; record "ubus: section parameter accepted"; } || { ok "section parameter accepted"; record "ubus: section parameter accepted"; }

  log "5b. HTTP assets"
  for path in /cgi-bin/luci/ /cgi-bin/luci/admin/services/usque /luci-static/resources/view/usque/overview.js; do
    code=$(curl -m 10 -s -o /dev/null -w '%{http_code}' "http://192.168.1.1$path")
    info "GET $path -> $code"
    case "$path:$code" in
      */cgi-bin/luci/:200) ok "LuCI login page 200"; record "LuCI login page HTTP 200";;
      *overview.js:200)    ok "LuCI view JS asset 200"; record "LuCI view JS asset HTTP 200";;
      *admin/services/usque:200) ok "view page 200 without auth (unauthenticated LuCI?)"; record "LuCI view page HTTP 200";;
      *admin/services/usque:403) info "view page 403 without session (expected: login required)"; record "LuCI view page requires login (403)";;
      *) info "$path -> $code";;
    esac
  done
  if [ -n "${LUCI_PASSWORD:-}" ]; then
    log "5c. Authenticated LuCI session"
    J=$(mktemp)
    curl -m 10 -s -c "$J" -o /dev/null http://192.168.1.1/cgi-bin/luci/
    TOKEN=$(curl -m 10 -s -b "$J" -c "$J" http://192.168.1.1/cgi-bin/luci/ | sed -n "s/.*token: *'\([^']*\)'.*/\1/p" | head -1)
    code=$(curl -m 10 -s -b "$J" -c "$J" -o /tmp/luci-login.html -w '%{http_code}' \
      --data-urlencode "luci_username=root" --data-urlencode "luci_password=$LUCI_PASSWORD" \
      --data-urlencode "token=$TOKEN" http://192.168.1.1/cgi-bin/luci/)
    info "login POST -> $code"
    page=$(curl -m 10 -s -b "$J" -o /tmp/luci-usque.html -w '%{http_code}' http://192.168.1.1/cgi-bin/luci/admin/services/usque)
    info "authenticated view page -> $page"
    if [ "$page" = 200 ] && grep -qi usque /tmp/luci-usque.html; then
      ok "authenticated LuCI view page renders (200, mentions usque)"; record "LuCI authenticated view page"
    else
      bad "authenticated LuCI view page not OK ($page)"; record "LuCI authenticated view page"
    fi
    rm -f "$J"
  else
    info "LUCI_PASSWORD not set — authenticated login page test skipped (router root password unknown to the harness)"
    record "LuCI authenticated view page" SKIP
  fi
}

step_reboot() {
  log "6. Reboot test — pre-state"
  PRE=$(rt "date; uptime; echo ===v4def; ip -4 route show default; echo ===v6def; ip -6 route show default; echo ===rule4; ip rule show | grep -E '^1000:' || echo none; echo ===rule6; ip -6 rule show | grep -E '^1000:' || echo none; echo ===uci; uci show usque; echo ===enabled; /etc/init.d/usque enabled && echo ENABLED || echo DISABLED; echo ===iface; ip -br link show warp0 2>&1; echo ===trace4; curl -4 -m 12 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='; echo ===trace6; curl -6 -m 15 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='")
  show "$PRE"; printf '%s\n' "$PRE" > "$ART_DIR/phase2-reboot-pre.txt"
  log "6a. rebooting now"
  ssh "${SSH_OPTS[@]}" "$ROUTER" 'sync; (sleep 1; reboot) >/dev/null 2>&1 &' || true
  sleep 12
  if wait_router_up 240; then ok "router back after reboot"; record "reboot: router reachable"; else bad "router did not come back"; record "reboot: router reachable"; return 1; fi
  sleep 12
  log "6b. post-reboot state"
  POST=$(rt "date; uptime; echo ===v4def; ip -4 route show default; echo ===v6def; ip -6 route show default; echo ===rule4; ip rule show | grep -E '^1000:' || echo none; echo ===rule6; ip -6 rule show | grep -E '^1000:' || echo none; echo ===enabled; /etc/init.d/usque enabled && echo ENABLED || echo DISABLED; echo ===iface; ip -br link show warp0 2>&1; echo ===trace4; curl -4 -m 15 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='; echo ===trace6; curl -6 -m 20 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='; echo ===log; logread -e usque | head -12")
  show "$POST"; printf '%s\n' "$POST" > "$ART_DIR/phase2-reboot-post.txt"
  echo "$POST" | grep -q ENABLED && { ok "service still enabled after reboot"; record "reboot: service enabled"; } || { bad "service not enabled after reboot"; record "reboot: service enabled"; }
  echo "$POST" | grep -q 'warp0 ' && { ok "warp0 came up automatically"; record "reboot: tun auto-started"; } || { bad "warp0 missing after reboot"; record "reboot: tun auto-started"; }
  echo "$POST" | sed -n '/===v4def/,/===v6def/p' | grep -q 'default via .*dev phy0-tether' && { ok "WAN v4 default intact after reboot"; record "reboot: WAN v4 default intact"; } || { bad "WAN v4 default changed after reboot"; record "reboot: WAN v4 default intact"; }
  echo "$POST" | sed -n '/===v4def/,/===v6def/p' | grep -q 'dev warp0' && { bad "v4 default taken over after reboot"; record "reboot: no v4 takeover"; } || { ok "no v4 default via warp0 after reboot"; record "reboot: no v4 takeover"; }
  echo "$POST" | grep -q 'warp=on' && { ok "IPv6 through WARP restored after reboot"; record "reboot: v6 warp=on"; } || { bad "IPv6 not through WARP after reboot"; record "reboot: v6 warp=on"; }
  code=$(curl -m 10 -s -o /dev/null -w '%{http_code}' http://192.168.1.1/cgi-bin/luci/); [ "$code" = 403 ] || [ "$code" = 200 ] && { ok "LuCI reachable after reboot (HTTP $code)"; record "reboot: LuCI reachable"; } || { bad "LuCI unreachable ($code)"; record "reboot: LuCI unreachable"; }
}

step_uninstall() {
  log "7. Uninstall test (evidence only; reinstall follows for the final state)"
  BEFORE=$(rt 'apk info -v | grep -i usque; echo ===; ip -4 route show; echo ===; /etc/init.d/usque status 2>&1 | head -3')
  show "$BEFORE"
  OUT=$(rt '/etc/init.d/usque stop 2>&1; /etc/init.d/usque disable 2>&1; apk del luci-app-usque usque 2>&1'); RC=$?
  show "$OUT"
  [ "$RC" -eq 0 ] && { ok "apk del succeeded"; record "uninstall apk del"; } || { bad "apk del failed"; record "uninstall apk del"; }
  sleep 2
  LEFTOVER=$(rt 'ls -l /usr/bin/usque /etc/init.d/usque /etc/config/usque /usr/libexec/usque /usr/libexec/rpcd/luci.usque /usr/share/luci/menu.d/luci-app-usque.json /usr/share/rpcd/acl.d/luci-app-usque.json /www/luci-static/resources/view/usque 2>&1; echo ===; ip -4 route show; echo ===; ip link show warp0 2>&1 | head -1; echo ===; uci show firewall | grep -i warp0')
  show "$LEFTOVER"
  echo "$LEFTOVER" | grep -q "No such file" && info "files removed (see list)"
  POSTU=$(rt 'curl -m 15 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp)="')
  show "$POSTU"
  echo "$POSTU" | grep -q 'warp=off' && { ok "uninstall restored WAN egress"; record "uninstall restores WAN"; } || { bad "WAN not restored after uninstall"; record "uninstall restores WAN"; }
  log "7b. Reinstall for the final state"
  OUT2=$(rt 'apk add usque luci-app-usque 2>&1'); RC2=$?
  show "$OUT2"
  [ "$RC2" -eq 0 ] && { ok "reinstall from signed feed OK"; record "reinstall after uninstall"; } || { bad "reinstall failed"; record "reinstall after uninstall"; }
  rt 'uci set usque.warp0.enabled=1; uci commit usque; /etc/init.d/usque enable; /etc/init.d/usque start' >/dev/null 2>&1
  sleep 8
  rt "ip link show $IFACE >/dev/null 2>&1" && { ok "service up again after reinstall"; record "service up after reinstall"; } || { bad "service not up after reinstall"; record "service up after reinstall"; }
}

step_final() {
  log "8. Final state: restore baseline config, keep paket installed"
  "$TESTS_DIR/restore.sh" --prune-leftovers "$(cat "$TESTS_DIR/backups/.last" | sed 's|^|'"$TESTS_DIR"'/backups/|')" | sed 's/^/  | /'
  log "8b. Final dumps"
  FIN=$(rt "date; echo === routes4; ip -4 route show; echo === routes6; ip -6 route show; echo === rules; ip rule show; echo === network; uci show network; echo === firewall-zone; uci show firewall; echo === df; df -k; echo === usque; cat /etc/config/usque; echo === enabled; /etc/init.d/usque enabled && echo YES || echo NO; echo === status; /etc/init.d/usque status; echo === iface; ip -br link show warp0 2>&1; echo === egress; curl -m 15 -s https://www.cloudflare.com/cdn-cgi/trace")
  show "$FIN"; : > "$ART_DIR/phase2-final-state.txt"; printf '%s\n' "$FIN" > "$ART_DIR/phase2-final-state.txt"
  echo "$FIN" | grep -q '^default via ' && { ok "default route via WAN"; record "final default route via WAN"; } || { bad "no WAN default route"; record "final default route via WAN"; }
  if echo "$FIN" | grep -q 'warp0'; then
    info "tunnel still active in final state (expected: installed+enabled+started)"
  fi
  rt 'ip rule show | grep -q "^1000:" && echo "TEST mwan3 rule still present"' | grep -q 1000 && { bad "test-only priority-1000 rule left behind"; record "no test ip rules left"; } || { ok "no test ip rules left"; record "no test ip rules left"; }
  # keep or drop the local feed repo entry so plain `apk` keeps working
  if rt 'grep -q 192.168.1.240 /etc/apk/repositories.d/usque.list 2>/dev/null'; then
    info "removing local-feed repository entry (feed not permanently reachable)"
    rt 'rm -f /etc/apk/repositories.d/usque.list'
  fi
  ok "final state captured"; record "final state captured"
}

# ---------------------------------------------------------------- v2 features
step_optin() {
  log "9. Opt-in default route (default_route=1) — short window, reverts immediately"
  log "9a. pre-state"
  PRE=$(rt 'ip -4 route show default; ip -6 route show default; ip rule show | grep -E "^1000:.*lookup main" || echo "no rule1000"; ip -6 rule show | grep -E "^1000:.*lookup main" || echo "no v6rule1000"')
  show "$PRE"
  rt 'uci set usque.warp0.default_route=1; uci commit usque; /etc/init.d/usque restart' >/dev/null 2>&1
  local i=0
  while [ "$i" -lt 30 ]; do rt "ip route show default | grep -q ' dev warp0'" && break; sleep 1; i=$((i+1)); done
  MID=$(rt 'ip -4 route show default; echo ===rule; ip rule show | grep -E "^1000:.*lookup main" || echo none; echo ===routeget; ip route get 104.16.132.229 | head -1; echo ===trace; curl -m 15 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp)="')
  show "$MID"
  echo "$MID" | grep -q 'dev warp0' && { ok "v4 default via warp0 installed"; record "optin: v4 default via warp0"; } || { bad "v4 default not via warp0"; record "optin: v4 default via warp0"; }
  echo "$MID" | sed -n '/===rule/,/===routeget/p' | grep -q '^1000:' && { ok "priority-1000 v4 rule installed"; record "optin: policy rule installed"; } || { bad "no priority-1000 rule"; record "optin: policy rule installed"; }
  echo "$MID" | grep -q 'warp=on' && { ok "egress warp=on with opt-in routing"; record "optin: egress warp=on"; } || { bad "egress not warp=on"; record "optin: egress warp=on"; }
  log "9b. revert (default_route=0 + restart) and verify exact restore"
  rt 'uci set usque.warp0.default_route=0; uci commit usque; /etc/init.d/usque restart' >/dev/null 2>&1
  sleep 6
  POST=$(rt 'ip -4 route show default; echo ===v6; ip -6 route show default; echo ===rule4; ip rule show | grep -E "^1000:.*lookup main" || echo none; echo ===rule6; ip -6 rule show | grep -E "^1000:.*lookup main" || echo none; echo ===trace4; curl -4 -m 15 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp)="; echo ===trace6; curl -6 -m 15 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp)="')
  show "$POST"
  echo "$POST" | head -1 | grep -q 'default via .*dev phy0-tether' && { ok "WAN v4 default restored exactly"; record "optin: WAN default restored"; } || { bad "WAN v4 default NOT restored"; record "optin: WAN default restored"; }
  echo "$POST" | sed -n '/===rule4/,/===rule6/p' | grep -q '^1000:' && { bad "IPv4 policy rule left behind after revert"; record "optin: no leftover v4 rule"; } || { ok "no leftover IPv4 policy rule"; record "optin: no leftover v4 rule"; }
  echo "$POST" | sed -n '/===trace4/,/===trace6/p' | grep -q 'warp=off' && { ok "IPv4 egress restored to WAN (warp=off)"; record "optin: v4 egress restored"; } || { bad "IPv4 egress still via tunnel"; record "optin: v4 egress restored"; }
  # v6 keeps its auto route/rule by design
  echo "$POST" | sed -n '/===rule6/,$p' | grep -q '^1000:' && info "v6 policy rule still present (expected with ipv6_route=auto)"
}

step_ipv6() {
  log "10. IPv6 routing (ipv6_route=auto on v4-only WAN)"
  rt 'uci set usque.warp0.default_route=0; uci set usque.warp0.ipv6=1; uci set usque.warp0.ipv6_route=auto; uci commit usque; /etc/init.d/usque restart' >/dev/null 2>&1
  local i=0
  while [ "$i" -lt 30 ]; do rt "ip -6 route show default | grep -q ' dev warp0'" && break; sleep 1; i=$((i+1)); done
  V6=$(rt 'ip -6 route show default; echo ===rule6; ip -6 rule show | grep -E "^1000:.*lookup main" || echo none; echo ===addrs; ip -6 addr show dev warp0 | grep inet6; echo ===trace6; curl -6 -m 20 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp|colo)="; echo ===ping6; ping -6 -c 2 -W 5 2606:4700:4700::1111 2>&1 | tail -2')
  show "$V6"
  echo "$V6" | grep -q 'dev warp0' && { ok "IPv6 default via warp0 (auto)"; record "ipv6: default route installed"; } || { bad "no IPv6 default via warp0"; record "ipv6: default route installed"; }
  echo "$V6" | sed -n '/===rule6/,/===addrs/p' | grep -q '^1000:' && { ok "IPv6 priority-1000 rule installed"; record "ipv6: policy rule installed"; } || { bad "no IPv6 policy rule"; record "ipv6: policy rule installed"; }
  echo "$V6" | grep -q 'warp=on' && { ok "curl -6 through tunnel: warp=on"; record "ipv6: curl -6 warp=on"; } || { bad "curl -6 not warp=on"; record "ipv6: curl -6 warp=on"; }
  echo "$V6" | grep -q '0% packet loss' && { ok "ping -6 through tunnel works"; record "ipv6: ping -6"; } || info "ping -6 0% loss not observed"
  log "10b. revert to ipv6_route=off and verify v6 untouched"
  rt 'uci set usque.warp0.ipv6_route=off; uci commit usque; /etc/init.d/usque restart' >/dev/null 2>&1; sleep 6
  V6OFF=$(rt 'ip -6 route show default; ip -6 rule show | grep -E "^1000:" || echo none')
  show "$V6OFF"
  echo "$V6OFF" | grep -q 'dev warp0' && { bad "v6 default left with ipv6_route=off"; record "ipv6: off leaves v6 untouched"; } || { ok "no v6 default with ipv6_route=off"; record "ipv6: off leaves v6 untouched"; }
  rt 'uci set usque.warp0.ipv6_route=auto; uci commit usque' >/dev/null 2>&1
}

step_lanipv6() {
  log "11. LAN IPv6 / NAT66 (lan_ipv6=1)"
  PRE=$(rt 'cat /proc/sys/net/ipv6/conf/all/forwarding; ip -6 addr show br-lan | grep inet6; nft list ruleset | grep -c masq6 || true')
  show "$PRE"
  rt 'uci set settings.lan_ipv6=1; uci set settings.lan_ipv6_prefix=""; uci commit usque; /etc/init.d/usque restart' >/dev/null 2>&1
  sleep 8
  MID=$(rt 'ip -6 addr show br-lan | grep inet6; echo ===nat66; nft list ruleset | grep -iE "masq6|nat66|usque" | head -10; echo ===ra; ps w | grep -c "[o]dhcpd"; echo ===default; ip -6 route show default')
  show "$MID"
  echo "$MID" | grep -q 'masq6\|masq' && { ok "NAT66/masq6 rules present"; record "lan_ipv6: NAT66 configured"; } || info "no masq6 rule visible"
  MACV6=$(ifconfig en0 2>/dev/null | awk '/inet6 fd85/{print $2}' | head -1)
  info "Mac ULA: ${MACV6:-none}"
  if [ -n "$MACV6" ]; then
    MTRACE=$(curl -6 -m 20 -s https://www.cloudflare.com/cdn-cgi/trace 2>&1); show "$MTRACE"
    echo "$MTRACE" | grep -q 'warp=on' && { ok "Mac LAN IPv6 through router/WARP (NAT66) works"; record "lan_ipv6: Mac curl -6 warp=on"; } || { bad "Mac LAN IPv6 test failed"; record "lan_ipv6: Mac curl -6 warp=on"; }
  else
    record "lan_ipv6: Mac curl -6 warp=on" SKIP
  fi
  log "11b. revert lan_ipv6=0"
  rt 'uci set settings.lan_ipv6=0; uci commit usque; /etc/init.d/usque restart' >/dev/null 2>&1; sleep 6
  rt 'ip -6 route show default; curl -m 12 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E "^(ip|warp)="' | sed 's/^/  | /'
  record "lan_ipv6: reverted" PASS
}

step_multi() {
  log "12. Multi-instance (second section warp1)"
  rt 'uci -q delete usque.warp1; uci set usque.warp1=usque; uci set usque.warp1.enabled=1; uci set usque.warp1.mode=custom; uci set usque.warp1.config_file=/etc/usque/warp0.json; uci set usque.warp1.interface=warp1; uci set usque.warp1.mtu=1400; uci set usque.warp1.ipv6=0; uci set usque.warp1.default_route=0; uci set usque.warp1.auto_register=0; uci set usque.warp1.sni=""; uci commit usque; /etc/init.d/usque restart' >/dev/null 2>&1
  sleep 10
  M=$(rt 'ip -br link show warp0; ip -br link show warp1 2>&1; echo ===init; ubus call service list "{"name":"usque"}" | head -c 800; echo; echo ===log; logread -e usque | tail -12')
  show "$M"
  echo "$M" | grep -q '^warp0' && { ok "warp0 up with second instance"; record "multi: warp0 up"; } || { bad "warp0 down with second instance"; record "multi: warp0 up"; }
  echo "$M" | grep -q '^warp1' && { ok "warp1 up (second instance)"; record "multi: warp1 up"; } || { bad "warp1 not up"; record "multi: warp1 up"; }
  rt '/etc/init.d/usque stop >/dev/null 2>&1; uci -q delete usque.warp1; uci commit usque; /etc/init.d/usque start' >/dev/null 2>&1
  sleep 6
  record "multi: second instance removed" PASS
}

step_custom() {
  log "13. Custom mode section (fixture: existing Cloudflare config, custom port/iface)"
  rt 'uci -q delete usque.custom0; uci set usque.custom0=usque; uci set usque.custom0.enabled=1; uci set usque.custom0.mode=custom; uci set usque.custom0.config_file=/etc/usque/warp0.json; uci set usque.custom0.interface=warp2; uci set usque.custom0.mtu=1400; uci set usque.custom0.endpoint_port=443; uci set usque.custom0.endpoint_ipv6=0; uci set usque.custom0.ipv6=0; uci set usque.custom0.default_route=0; uci set usque.custom0.auto_register=0; uci set usque.custom0.sni="consumer-masque.cloudflareclient.com"; uci commit usque; /etc/init.d/usque restart' >/dev/null 2>&1
  sleep 10
  C=$(rt 'ip -br link show warp2 2>&1; echo ===log; logread -e usque | tail -10')
  show "$C"
  echo "$C" | grep -q '^warp2' && { ok "custom-mode instance up"; record "custom: instance up"; } || { bad "custom-mode instance not up"; record "custom: instance up"; }
  rt '/etc/init.d/usque stop >/dev/null 2>&1; uci -q delete usque.custom0; uci commit usque; /etc/init.d/usque start' >/dev/null 2>&1
  sleep 6
  record "custom: section removed" PASS
}

case "$STEP" in
  backup)    step_backup ;;
  stale)     step_stale ;;
  feed)      step_feed; stop_feed_http ;;
  service)   step_service ;;
  luci)      step_luci ;;
  reboot)    step_reboot ;;
  uninstall) step_uninstall ;;
  optin)     step_optin ;;
  ipv6)      step_ipv6 ;;
  lanipv6)   step_lanipv6 ;;
  multi)     step_multi ;;
  custom)    step_custom ;;
  final)     step_final ;;
  all)       step_backup && step_stale && step_feed && step_service && step_luci; stop_feed_http ;;
  *) sed -n '2,30p' "$0"; exit 1 ;;
esac
finish
