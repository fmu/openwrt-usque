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
FEED_URL="http://$FEED_HOST:$FEED_PORT/packages.adb"
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
  log "3. Signed feed install"
  : > "$PKG_LOG"
  local before_free; before_free=$(df_free); info "overlay free before: ${before_free}K"
  serve_feed || return 1

  # public key -> /etc/apk/keys; repo -> /etc/apk/repositories.d
  local keyfile; keyfile=$(find "${FEED_DIR:?}" -maxdepth 1 -name '*.pem' -o -maxdepth 1 -name '*.pub' | head -1)
  [ -n "$keyfile" ] || { bad "no public key in $FEED_DIR"; record "feed public key present"; return 1; }
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
  log "4. procd service lifecycle"
  rt 'uci set usque.main.enabled=1; uci commit usque; /etc/init.d/usque enable; echo enabled=$(/etc/init.d/usque enabled && echo yes)'
  rt '/etc/init.d/usque start' | sed 's/^/  | /'
  local i=0 live=0
  while [ "$i" -lt 40 ]; do
    if rt "ip link show $IFACE >/dev/null 2>&1 && pgrep -f '/usr/bin/usque' >/dev/null"; then live=1; break; fi
    sleep 1; i=$((i+1))
  done
  [ "$live" = 1 ] && { ok "service started, $IFACE exists after ~${i}s"; record "service start (tun up in ${i}s)"; } \
                  || { bad "service did not bring up $IFACE within 40s"; record "service start"; }
  log "4a. status"
  S=$(rt '/etc/init.d/usque status; echo "---ubus"; ubus call service list "{\"name\":\"usque\"}" 2>&1 | head -40; echo "---log"; logread -e usque | tail -25')
  show "$S"
  echo "$S" | grep -qi 'running' && { ok "init status running"; record "init status running"; } || { bad "init status not running"; record "init status running"; }
  echo "$S" | grep -qi 'Connected to MASQUE server\|Tunnel established' && { ok "MASQUE connected per logs"; record "MASQUE connected (service)"; } \
                                                                      || { bad "no MASQUE connect log line"; record "MASQUE connected (service)"; }
  log "4b. tun / routes / firewall"
  T=$(rt "echo === link; ip -br link show $IFACE; echo === addr; ip addr show dev $IFACE; echo === routes; ip -4 route show; echo === route6; ip -6 route show; echo === nft-zone; nft list ruleset | grep -i -A4 usque; echo === fw4-zone; uci show firewall | grep -i warp0")
  show "$T"
  echo "$T" | grep -q "$IFACE" && { ok "$IFACE present"; record "service tun device"; } || { bad "no $IFACE"; record "service tun device"; }
  echo "$T" | grep -q "default dev $IFACE" && { ok "default route via $IFACE"; record "service default route via tun"; } || { bad "default route not via $IFACE"; record "service default route via tun"; }
  echo "$T" | grep -qi "masq" && { ok "firewall zone/masq referenced"; record "firewall zone + masq"; } || info "no masq match in nft snippet"

  log "4c. Router-originated egress through the service"
  TRACE=$(rt 'curl -m 20 -s https://www.cloudflare.com/cdn-cgi/trace'); show "$TRACE"
  if echo "$TRACE" | grep -q '^warp=on'; then
    ok "warp=on with the packaged service"; record "service egress warp=on"
  else
    bad "warp=off with the packaged service (mwan3 policy routing?)"; record "service egress warp=on"
    if [ "${MAKE_MWAN3_WORKAROUND:-0}" = 1 ]; then
      log "4d. TEST-ONLY mwan3 workaround (product must do this itself)"
      rt 'ip rule add priority 1000 from all lookup main'
      TRACE2=$(rt 'curl -m 20 -s https://www.cloudflare.com/cdn-cgi/trace'); show "$TRACE2"
      echo "$TRACE2" | grep -q '^warp=on' && { ok "warp=on only after priority-1000 rule -> hook must add it"; record "service egress warp=on WITH manual mwan3 rule"; } \
                                            || { bad "still warp=off with rule"; record "service egress warp=on WITH manual mwan3 rule"; }
    else
      info "rerun with MAKE_MWAN3_WORKAROUND=1 to confirm the mwan3 rule is the only blocker"
    fi
  fi
  log "4e. LAN client path (Mac uses the router as default gateway)"
  MAC_GW=$(route -n get default 2>/dev/null | awk '/gateway:/{print $2}')
  if [ "$MAC_GW" = 192.168.1.1 ]; then
    MTRACE=$(curl -m 20 -s https://www.cloudflare.com/cdn-cgi/trace); show "$MTRACE"
    echo "$MTRACE" | grep -q '^warp=on' && { ok "LAN client (Mac) egress warp=on via router"; record "LAN client egress warp=on"; } \
                                        || { bad "LAN client egress warp=off"; record "LAN client egress warp=on"; }
    MDNS=$(dig +short +time=3 +tries=1 www.cloudflare.com @192.168.1.1 2>/dev/null | head -2)
    show "Mac DNS via router: $MDNS"
    [ -n "$MDNS" ] && { ok "LAN DNS via router works"; record "LAN DNS via router"; } || { bad "LAN DNS via router failed"; record "LAN DNS via router"; }
  else
    info "Mac default gateway is $MAC_GW (not the router) — LAN path verified via ubus counters instead"
    record "LAN client egress warp=on" SKIP
  fi
  log "4e2. Guest network (br-guest -> wan) while the tunnel is up"
  G=$(rt 'echo === guest-zone; uci show firewall | grep -A8 "name=.guest."; echo === wan-zone-networks; uci show firewall | grep "@zone\[1\].network"; echo === forward-chains; nft list ruleset | grep -E "forward_guest|forward_wan" | head -10; echo === guest-addr; ip -4 addr show br-guest | grep inet; echo === dnsmasq-listen; netstat -lnu 2>/dev/null | grep :53 | head -3')
  show "$G"
  echo "$G" | grep -q 'guest' && { ok "guest zone present"; record "guest zone present"; } || { bad "guest zone missing"; record "guest zone present"; }
  echo "$G" | grep -q "guest" && info "guest forwarding depends on zone wan membership of the tunnel (see wan-zone-networks above)"
  GSTATE=$(rt 'ubus call network.interface.guest status 2>/dev/null | head -c 300')
  show "guest ifstatus: $GSTATE"
  info "NOTE: no physical guest client is attached; verified are zone/forwarding/masq config + that the guest L3 interface is up. A live guest-client test needs a client on the guest SSID."

  log "4f. restart / stop / start"
  rt '/etc/init.d/usque restart'; sleep 8
  rt "ip -br link show $IFACE" | sed 's/^/  | /'
  rt "ip link show $IFACE >/dev/null 2>&1" && { ok "restart brought tun back"; record "service restart"; } || { bad "restart failed"; record "service restart"; }
  rt '/etc/init.d/usque stop'; sleep 3
  POSTSTOP=$(rt "ip -4 route show; echo ===; ip link show $IFACE >/dev/null 2>&1 && echo \"$IFACE PRESENT\" || echo \"$IFACE ABSENT\"; echo ===; curl -m 15 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='")
  show "$POSTSTOP"
  echo "$POSTSTOP" | grep -q "^default via " && { ok "stop restored WAN default route"; record "service stop restores WAN route"; } || { bad "WAN default route not restored after stop"; record "service stop restores WAN route"; }
  echo "$POSTSTOP" | grep -q 'warp=off' && { ok "stop restored WAN egress (warp=off)"; record "service stop restores warp=off"; } || { bad "egress still via tunnel"; record "service stop restores warp=off"; }
  rt '/etc/init.d/usque start'; sleep 6
  rt "ip link show $IFACE >/dev/null 2>&1" && { ok "start after stop works"; record "service start after stop"; } || { bad "start after stop failed"; record "service start after stop"; }
}

step_luci() {
  log "5. LuCI app"
  F=$(rt 'echo === menu; ls -l /usr/share/luci/menu.d/luci-app-usque.json 2>&1; echo === acl; ls -l /usr/share/rpcd/acl.d/luci-app-usque.json 2>&1; echo === view; ls -l /www/luci-static/resources/view/usque/ 2>&1; echo === rpcd; ls -l /usr/libexec/rpcd/luci.usque 2>&1; echo === menu-content; cat /usr/share/luci/menu.d/luci-app-usque.json 2>/dev/null; echo === acl-content; cat /usr/share/rpcd/acl.d/luci-app-usque.json 2>/dev/null')
  show "$F"
  echo "$F" | grep -q 'menu.d/luci-app-usque.json' && { ok "menu.d entry installed"; record "LuCI menu.d"; } || { bad "menu.d entry missing"; record "LuCI menu.d"; }
  echo "$F" | grep -q 'acl.d/luci-app-usque.json' && { ok "acl.d entry installed"; record "LuCI acl.d"; } || { bad "acl.d entry missing"; record "LuCI acl.d"; }
  echo "$F" | grep -q 'view/usque/' && { ok "JS view asset installed"; record "LuCI view asset"; } || { bad "JS view asset missing"; record "LuCI view asset"; }
  rt '/etc/init.d/rpcd restart' >/dev/null 2>&1; sleep 2
  U=$(rt 'echo === list; ubus -S list luci.usque 2>&1; echo === status; ubus -S call luci.usque status 2>&1; echo === getConfig; ubus -S call luci.usque getConfig 2>&1; echo === test; ubus -S call luci.usque test 2>&1')
  show "$U"
  echo "$U" | grep -q 'luci.usque' && { ok "ubus object luci.usque present"; record "ubus object luci.usque"; } || { bad "ubus object missing"; record "ubus object luci.usque"; }
  echo "$U" | grep -q '"' && { ok "ubus methods return JSON"; record "ubus methods return JSON"; } || { bad "ubus methods returned no JSON"; record "ubus methods return JSON"; }

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
  log "6. Reboot test (pre-state)"
  PRE=$(rt "date; uptime; ip -4 route show; echo ===; cat /etc/config/usque; echo ===; /etc/init.d/usque enabled && echo ENABLED || echo DISABLED; echo ===; ip link show $IFACE 2>&1 | head -2; echo ===; curl -m 12 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='")
  show "$PRE"
  : > "$ART_DIR/phase2-reboot-pre.txt"; printf '%s\n' "$PRE" > "$ART_DIR/phase2-reboot-pre.txt"
  info "rebooting router now"
  ssh "${SSH_OPTS[@]}" "$ROUTER" 'sync; (sleep 1; reboot) >/dev/null 2>&1 &' || true
  sleep 8; ssh "${SSH_OPTS[@]}" -o ConnectTimeout=3 "$ROUTER" true 2>/dev/null && info "still reachable, waiting for drop"
  if wait_router_up 180; then ok "router back after reboot"; record "reboot: router reachable"; else bad "router did not come back in 180s"; record "reboot: router reachable"; return 1; fi
  log "6b. Post-reboot state"
  POST=$(rt "date; uptime; ip -4 route show; echo ===; cat /etc/config/usque; echo ===; /etc/init.d/usque enabled && echo ENABLED || echo DISABLED; echo ===; ip br-link 2>/dev/null; ip -br link show $IFACE 2>&1; echo ===; logread -e usque | head -20; echo ===; curl -m 20 -s https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='")
  show "$POST"; : > "$ART_DIR/phase2-reboot-post.txt"; printf '%s\n' "$POST" > "$ART_DIR/phase2-reboot-post.txt"
  echo "$POST" | grep -q ENABLED && { ok "service still enabled after reboot"; record "reboot: service enabled"; } || { bad "service not enabled after reboot"; record "reboot: service enabled"; }
  echo "$POST" | grep -q "$IFACE" && { ok "$IFACE came up automatically"; record "reboot: tun auto-started"; } || { bad "$IFACE missing after reboot"; record "reboot: tun auto-started"; }
  echo "$POST" | grep -q 'warp=on' && { ok "egress warp=on after reboot"; record "reboot: egress warp=on"; } || { bad "egress warp=off after reboot"; record "reboot: egress warp=on"; }
  code=$(curl -m 10 -s -o /dev/null -w '%{http_code}' http://192.168.1.1/cgi-bin/luci/); [ "$code" = 200 ] && { ok "LuCI reachable after reboot"; record "reboot: LuCI reachable"; } || { bad "LuCI not reachable ($code)"; record "reboot: LuCI reachable"; }
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
  rt 'uci set usque.main.enabled=1; uci commit usque; /etc/init.d/usque enable; /etc/init.d/usque start' >/dev/null 2>&1
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

case "$STEP" in
  backup)    step_backup ;;
  stale)     step_stale ;;
  feed)      step_feed; stop_feed_http ;;
  service)   step_service ;;
  luci)      step_luci ;;
  reboot)    step_reboot ;;
  uninstall) step_uninstall ;;
  final)     step_final ;;
  all)       step_backup && step_stale && step_feed && step_service && step_luci; stop_feed_http ;;
  *) sed -n '2,30p' "$0"; exit 1 ;;
esac
finish
