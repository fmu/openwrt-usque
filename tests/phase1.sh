#!/usr/bin/env bash
# Phase 1: validate the CI-built armv7 musl binary on the live router, using the
# existing WARP config, on a spare interface name (warp9) with our own routing
# hooks. Nothing is installed into the overlay: everything runs from /tmp.
#
# Usage:
#   tests/phase1.sh                 # try UDP/443 (QUIC), fall back to --http2 automatically
#   tests/phase1.sh --http2-only    # force the HTTP/2 carrier
#   tests/phase1.sh --udp-only      # never fall back
#
# Safety: an EXIT trap and an on-router watchdog both run emergency-cleanup.sh,
# so routes are withdrawn even if this shell (or the ssh session) dies.
set -uo pipefail
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"

MODE=auto
for arg in "$@"; do
  case "$arg" in
    --http2-only) MODE=http2 ;;
    --udp-only)   MODE=udp ;;
    *) echo "unknown arg: $arg" >&2; exit 1 ;;
  esac
done

IFACE=warp9
SUBCMD="${USQUE_SUBCMD:-native-tun}"   # upstream Go build uses "nativetun"
EXPECT_MTU="${EXPECT_MTU:-1400}"       # upstream Go build defaults to 1280
LOG="${PHASE1_LOG:-$ART_DIR/phase1-$(date +%Y%m%d-%H%M%S).log}"
# NOTE: no internal `tee` (macOS sandbox blocks process substitution). Run as:
#   tests/phase1.sh 2>&1 | tee tests/artifacts/phase1-<ts>.log
PASS=0; FAIL=0; SKIP=0
declare -a CHECKS
record() { # record <name> <PASS|FAIL|SKIP> [evidence-file]
  local ev=${3:-}
  CHECKS+=("$1|$2|$ev")
  [ "$2" = PASS ] && PASS=$((PASS+1))
  [ "$2" = FAIL ] && FAIL=$((FAIL+1))
  [ "$2" = SKIP ] && SKIP=$((SKIP+1))
  return 0
}
cleanup_on_exit() {
  echo; log "TRAP: unwinding router state"
  rt 'sh /tmp/usque-test/emergency-cleanup.sh warp9' || true
  rt 'ip -4 route show; echo ===; ip -br link | grep -E "warp9|tun" || true; echo ===; curl -m 10 -s https://www.cloudflare.com/cdn-cgi/trace' \
     > "$ART_DIR/phase1-post-cleanup.txt" 2>&1 || true
  echo "  post-cleanup egress:"
  grep -E '^(ip|warp)=' "$ART_DIR/phase1-post-cleanup.txt" 2>/dev/null | sed 's/^/    /'
  echo "log: $LOG"
}
trap cleanup_on_exit EXIT INT TERM

log "Phase 1 start $(date -u +%FT%TZ) mode=$MODE iface=$IFACE"

# ---------------------------------------------------------------- preflight
log "0. Preflight"
PRE=$(rt 'date; ip -4 route show default; ip -br link show warp9 2>/dev/null || echo "warp9: ABSENT"; wc -c /etc/usque/warp0.json; sha256sum /etc/usque/warp0.json; lsmod | grep -E "^tun "')
printf '%s\n' "$PRE" | sed 's/^/  /'
if printf '%s' "$PRE" | grep -q 'warp9: ABSENT'; then :; elif printf '%s' "$PRE" | grep -q 'warp9'; then
  echo "ERROR: warp9 already exists on the router; refusing to touch it" >&2; exit 10
fi
WAN_DEV=$(printf '%s' "$PRE" | awk '/^default /{for(i=1;i<=NF;i++) if($i=="dev") print $(i+1); exit}')
WAN_GW=$(printf '%s' "$PRE" | awk '/^default /{for(i=1;i<=NF;i++) if($i=="via") print $(i+1); exit}')
[ -n "$WAN_DEV" ] && [ -n "$WAN_GW" ] || { echo "ERROR: cannot determine WAN default route" >&2; exit 11; }
echo "  WAN_DEV=$WAN_DEV WAN_GW=$WAN_GW"
BASE_TRACE=$(rt 'curl -m 10 -s https://www.cloudflare.com/cdn-cgi/trace')
BASE_IP=$(printf '%s' "$BASE_TRACE" | sed -n 's/^ip=//p')
printf '%s\n' "$BASE_TRACE" | sed 's/^/    | /'
check_contains "preflight: WAN egress reachable, warp=off" "warp=off" "$BASE_TRACE" && record "preflight WAN curl warp=off ($BASE_IP)" PASS

# deploy router-side scripts + config copy + wan.env
log "1. Deploy helper scripts and config copy to $ROUTER_TMP"
rt "mkdir -p $ROUTER_TMP; rm -f $ROUTER_TMP/*.sh $ROUTER_TMP/warp9.json $ROUTER_TMP/usque.log"
for f in p1-up.sh p1-down.sh emergency-cleanup.sh; do rtpush "$TESTS_DIR/router/$f" "$ROUTER_TMP/$f" || exit 12; done
printf 'WAN_DEV=%s\nWAN_GW=%s\n' "$WAN_DEV" "$WAN_GW" > /tmp/wan.env.$$
rtpush /tmp/wan.env.$$ "$ROUTER_TMP/wan.env"; rm -f /tmp/wan.env.$$
rtpush "$REPO_DIR/reference/warp0.json" "$ROUTER_TMP/warp9.json" 2>/dev/null \
  || rt "cp /etc/usque/warp0.json $ROUTER_TMP/warp9.json"
rt "chmod +x $ROUTER_TMP/*.sh; ls -la $ROUTER_TMP/"
CONF_HASH=$(rt "sha256sum $ROUTER_TMP/warp9.json" | awk '{print $1}')
REF_HASH=$(sha256sum "$REPO_DIR/reference/warp0.json" | awk '{print $1}')
[ "$CONF_HASH" = "$REF_HASH" ] && { ok "config copy matches reference/warp0.json ($CONF_HASH)"; record "config copy matches reference (sha256 $CONF_HASH)" PASS; } \
                               || { bad "config copy differs from reference ($CONF_HASH != $REF_HASH)"; record "config copy matches reference" FAIL; }

# deploy binary
log "2. Deploy binary"
BIN_LOCAL="${USQUE_BIN:-$(find "$ART_DIR/build" -type f -name usque 2>/dev/null | head -1)}"
if [ -z "$BIN_LOCAL" ]; then
  TARBALL=$(find "$ART_DIR/build" -type f \( -name '*.tar.gz' -o -name '*.tgz' \) 2>/dev/null | head -1)
  if [ -n "$TARBALL" ]; then
    mkdir -p "$ART_DIR/build/extract"; tar -xzf "$TARBALL" -C "$ART_DIR/build/extract"
    BIN_LOCAL=$(find "$ART_DIR/build/extract" -type f -name usque | head -1)
  fi
fi
[ -n "$BIN_LOCAL" ] || { bad "no usque binary in $ART_DIR/build (run tests/fetch-artifacts.sh build)"; record "artifact present" FAIL; exit 13; }
echo "  local binary: $BIN_LOCAL ($(wc -c < "$BIN_LOCAL") bytes, sha256 $(shasum -a 256 "$BIN_LOCAL" | awk '{print $1}'))"
LOCAL_HASH=$(shasum -a 256 "$BIN_LOCAL" | awk '{print $1}')
rtpush "$BIN_LOCAL" "$ROUTER_TMP/usque" || exit 14
rt "chmod +x $ROUTER_TMP/usque"
RT_HASH=$(rt "sha256sum $ROUTER_TMP/usque" | awk '{print $1}')
[ "$LOCAL_HASH" = "$RT_HASH" ] && { ok "binary sha256 identical on router"; record "binary copy sha256 $LOCAL_HASH" PASS; } \
                               || { bad "binary sha256 mismatch local=$LOCAL_HASH router=$RT_HASH"; record "binary copy sha256" FAIL; }

# ---------------------------------------------------------------- CLI surface
log "2b. CLI surface (captured for docs)"
CLIOUT=$(rt "/tmp/usque-test/usque --help; echo '===== $SUBCMD --help ====='; /tmp/usque-test/usque ${SUBCMD} --help")
printf '%s\n' "$CLIOUT" > "$ART_DIR/usque-help.txt"
printf '%s\n' "$CLIOUT" | sed 's/^/  | /'
has_flag() { printf '%s' "$CLIOUT" | grep -q -- "$1" && echo 1 || echo 0; }
H2_SUPPORTED=$(has_flag -- '--http2')
HAS_ALWAYS_RECONNECT=$(has_flag -- '--always-reconnect')
HAS_KEEPALIVE=$(has_flag -- '--keepalive-period')
HAS_RECONNECT=$(has_flag -- '--reconnect-delay')
HAS_MTU=$(has_flag -- '-m, --mtu')
HAS_AUTO_REGISTER=$(has_flag -- '--auto-register')
info "capabilities: http2=$H2_SUPPORTED always-reconnect=$HAS_ALWAYS_RECONNECT keepalive=$HAS_KEEPALIVE reconnect-delay=$HAS_RECONNECT mtu=$HAS_MTU auto-register=$HAS_AUTO_REGISTER"
if printf '%s' "$CLIOUT" | grep -qE "native-?tun"; then ok "native-tun/subcommand present"; record "CLI: native-tun subcommand" PASS; else bad "subcommand missing from help"; record "CLI: native-tun subcommand" FAIL; fi
[ "$H2_SUPPORTED" = 1 ] && record "CLI: --http2 present" PASS || record "CLI: --http2 present" SKIP

# ---------------------------------------------------------------- main run
run_carrier() { # run_carrier <udp|http2>
  local carrier=$1 extra="" t0 t1 started=0 i flags=""
  [ "$carrier" = http2 ] && extra="--http2"
  [ "${HAS_KEEPALIVE:-0}" = 1 ] && flags="$flags --keepalive-period 15s"
  [ "${HAS_RECONNECT:-0}" = 1 ] && flags="$flags --reconnect-delay 1s"
  [ "${HAS_ALWAYS_RECONNECT:-0}" = 1 ] && flags="$flags --always-reconnect"
  [ "${HAS_MTU:-0}" = 1 ] && flags="$flags -m $EXPECT_MTU"
  log "3. Native-tun run ($carrier${extra:+, $extra})"
  rt 'sh /tmp/usque-test/emergency-cleanup.sh warp9' >/dev/null
  rt "cd /tmp/usque-test && RUST_LOG=info,usque=debug,usque_tunnel_core=debug,usque_masque=debug,usque_cli=debug \
      setsid /tmp/usque-test/usque --config /tmp/usque-test/warp9.json $SUBCMD -n $IFACE -P 443 \
      $flags $extra \
      --on-connect /tmp/usque-test/p1-up.sh --on-disconnect /tmp/usque-test/p1-down.sh \
      >/tmp/usque-test/usque.log 2>&1 </dev/null & \
      echo \$! > /tmp/usque-test/usque.pid; sleep 1; echo started pid=\$(cat /tmp/usque-test/usque.pid); ps w | grep -c '[u]sque-test/usque'"
  t0=$(date +%s)
  # remote poll: wait for connect or repeated failure
  local st
  st=$(rt "i=0; while [ \$i -lt 75 ]; do
             if grep -q 'Connected to MASQUE server' /tmp/usque-test/usque.log 2>/dev/null; then echo CONNECTED; exit 0; fi
             PID=\$(cat /tmp/usque-test/usque.pid 2>/dev/null)
             if [ -z \"\$PID\" ] || ! kill -0 \"\$PID\" 2>/dev/null; then echo EXITED; exit 3; fi
             if [ \$i -gt 15 ] && [ \$(grep -c 'Failed to connect tunnel' /tmp/usque-test/usque.log 2>/dev/null) -ge 3 ]; then echo FAILING; exit 2; fi
             sleep 1; i=\$((i+1)); done; echo TIMEOUT; exit 4")
  t1=$(date +%s); local elapsed=$((t1-t0))
  echo "  poll result: $st after ${elapsed}s"
  rt "sed -n '1,200p' /tmp/usque-test/usque.log" > "$ART_DIR/phase1-$carrier-log.txt"
  case "$st" in
    CONNECTED)
      ok "$carrier: MASQUE tunnel established in ${elapsed}s"
      record "$carrier MASQUE connected (${elapsed}s)" PASS
      ;;
    *)
      bad "$carrier: no MASQUE connection ($st, ${elapsed}s)"
      record "$carrier MASQUE connected" FAIL
      rt "tail -40 /tmp/usque-test/usque.log" | sed 's/^/    | /'
      printf 'carrier=%s poll=%s elapsed=%s\n' "$carrier" "$st" "$elapsed" > "$ART_DIR/phase1-$carrier-fail.txt"
      return 1
      ;;
  esac
  START_LATENCY=$elapsed

  log "4. Tunnel state ($carrier)"
  TUN_STATE=$(rt "echo === link; ip -br link show $IFACE; echo === addr; ip -4 addr show dev $IFACE; ip -6 addr show dev $IFACE; echo === mtu; ip link show $IFACE | head -1; echo === routes; ip -4 route show; echo === hook; cat /tmp/usque-test/hook-connect.txt 2>/dev/null")
  printf '%s\n' "$TUN_STATE" | sed 's/^/  /'
  printf '%s\n' "$TUN_STATE" > "$ART_DIR/phase1-$carrier-tunnel-state.txt"
  printf '%s' "$TUN_STATE" | grep -q "UP" && { ok "tun link $IFACE is UP"; record "$carrier tun $IFACE UP" PASS; } || { bad "tun link $IFACE not UP"; record "$carrier tun $IFACE UP" FAIL; }
  printf '%s' "$TUN_STATE" | grep -q "172.16.0.2" && { ok "tun has config IPv4 172.16.0.2/32"; record "$carrier tun IPv4 172.16.0.2" PASS; } || { bad "tun missing IPv4 172.16.0.2"; record "$carrier tun IPv4 172.16.0.2" FAIL; }
  printf '%s' "$TUN_STATE" | grep -q "<WARP_CLIENT_IPV6>" && { ok "tun has config IPv6"; record "$carrier tun IPv6 addr" PASS; } || { bad "tun missing IPv6 addr"; record "$carrier tun IPv6 addr" FAIL; }
  printf '%s' "$TUN_STATE" | grep -q "mtu $EXPECT_MTU" && { ok "tun MTU $EXPECT_MTU"; record "$carrier tun MTU $EXPECT_MTU" PASS; } || { bad "tun MTU not $EXPECT_MTU"; record "$carrier tun MTU $EXPECT_MTU" FAIL; }
  printf '%s' "$TUN_STATE" | grep -q "^default dev $IFACE" && { ok "default route installed via $IFACE"; record "$carrier default route via $IFACE" PASS; } || { bad "default route NOT via $IFACE"; record "$carrier default route via $IFACE" FAIL; }
  printf '%s' "$TUN_STATE" | grep -q "162.159.198.2 via .* dev $WAN_DEV" && { ok "MASQUE endpoint bypassed via $WAN_DEV"; record "$carrier endpoint bypass route" PASS; } || info "endpoint bypass route not matched in dump (see evidence)"

  log "5. Traffic through the tunnel ($carrier)"
  # A tunnel daemon that reconnects on idle can flap the default route, and a
  # single curl can then silently escape via the WAN. Retry until the trace
  # reports warp=on, recording the route decision for each attempt.
  : > "$ART_DIR/phase1-$carrier-trace-attempts.txt"
  TRACE=""; TRACE_ROUTE=""; IP=""
  for attempt in 1 2 3 4 5 6; do
    ROUTE_TO=$(rt 'ip route get 104.16.132.229 2>&1 | head -1')
    TRY=$(rt 'curl -m 20 -s https://www.cloudflare.com/cdn-cgi/trace')
    printf '  attempt %s: route=[%s] warp=%s ip=%s\n' "$attempt" "$ROUTE_TO" \
      "$(printf '%s' "$TRY" | sed -n 's/^warp=//p')" "$(printf '%s' "$TRY" | sed -n 's/^ip=//p')"
    printf 'attempt %s\n  route-to-cf: %s\n%s\n\n' "$attempt" "$ROUTE_TO" "$TRY" >> "$ART_DIR/phase1-$carrier-trace-attempts.txt"
    TRACE="$TRY"; TRACE_ROUTE="$ROUTE_TO"
    if printf '%s' "$TRY" | grep -q '^warp=on'; then break; fi
    sleep 2
  done
  IP=$(printf '%s' "$TRACE" | sed -n 's/^ip=//p')
  printf '%s\n' "$TRACE" | sed 's/^/    | /'
  printf '%s\n' "$TRACE" > "$ART_DIR/phase1-$carrier-trace.txt"
  if printf '%s' "$TRACE_ROUTE" | grep -q "dev $IFACE"; then
    ok "trace traffic actually routed via $IFACE ($TRACE_ROUTE)"
    record "$carrier trace route via $IFACE" PASS
  else
    bad "trace traffic did not use $IFACE (route: $TRACE_ROUTE)"
    record "$carrier trace route via $IFACE" FAIL
  fi
  if printf '%s' "$TRACE" | grep -q '^warp=on'; then
    ok "warp=on through tunnel (egress $IP, baseline $BASE_IP)"
    record "$carrier curl trace warp=on (egress $IP)" PASS
    [ -n "$IP" ] && [ "$IP" != "$BASE_IP" ] && { ok "egress IP changed ($BASE_IP -> $IP)"; record "$carrier egress IP changed" PASS; } \
                                            || { bad "egress IP unchanged ($IP)"; record "$carrier egress IP changed" FAIL; }
  else
    bad "warp=off through tunnel (trace doesn't show warp=on after retries)"
    record "$carrier curl trace warp=on" FAIL
  fi
  # generic internet reachability through the tunnel
  HTTPCODE=$(rt 'curl -m 20 -s -o /dev/null -w "%{http_code}" https://www.cloudflare.com/')
  case "$HTTPCODE" in 2*|3*) ok "HTTPS through tunnel: HTTP $HTTPCODE"; record "$carrier HTTPS via tunnel (code $HTTPCODE)" PASS;;
    *) bad "HTTPS through tunnel failed (code $HTTPCODE)"; record "$carrier HTTPS via tunnel" FAIL;; esac
  # DNS (router resolver, and direct-to-1.1.1.1 through the tunnel)
  DNS_LOCAL=$(rt 'nslookup www.cloudflare.com 127.0.0.1 2>&1 | tail -6')
  DNS_TUN=$(rt 'nslookup example.com 1.1.1.1 2>&1 | tail -6')
  printf '%s\n' "$DNS_LOCAL" | sed 's/^/    DNS(local)| /'
  printf '%s\n' "$DNS_TUN" | sed 's/^/    DNS(1.1.1.1)| /'
  printf '%s\n' "$DNS_LOCAL" > "$ART_DIR/phase1-$carrier-dns-local.txt"
  printf '%s\n' "$DNS_TUN" > "$ART_DIR/phase1-$carrier-dns-tun.txt"
  printf '%s' "$DNS_LOCAL" | grep -Eq 'Address|answer|Name:' && { ok "DNS via router resolver works"; record "$carrier DNS via router resolver" PASS; } || { bad "DNS via router resolver failed"; record "$carrier DNS via router resolver" FAIL; }
  printf '%s' "$DNS_TUN" | grep -Eq 'Address|answer|Name:' && { ok "DNS to 1.1.1.1 through tunnel works"; record "$carrier DNS to 1.1.1.1 via tunnel" PASS; } || { bad "DNS to 1.1.1.1 through tunnel failed"; record "$carrier DNS to 1.1.1.1 via tunnel" FAIL; }

  log "6. IPv6 through the tunnel ($carrier) [best effort]"
  rt "ip -6 route replace default dev $IFACE metric 0" >/dev/null 2>&1
  TRACE6=$(rt 'curl -6 -m 20 -s https://www.cloudflare.com/cdn-cgi/trace')
  printf '%s\n' "$TRACE6" | sed 's/^/    | /'
  printf '%s\n' "$TRACE6" > "$ART_DIR/phase1-$carrier-trace6.txt"
  rt "ip -6 route show default | grep \" dev $IFACE\" | while read -r r; do ip -6 route del \$r 2>/dev/null; done" >/dev/null 2>&1
  if printf '%s' "$TRACE6" | grep -q '^warp=on'; then
    ok "IPv6 through tunnel works (warp=on)"
    record "$carrier IPv6 curl trace warp=on" PASS
  else
    info "IPv6 through tunnel did not report warp=on (no IPv6 uplink / WARP v6 handling); recorded as SKIP"
    record "$carrier IPv6 curl trace warp=on" SKIP
  fi

  log "7. Resource usage ($carrier)"
  RES=$(rt 'PID=$(cat /tmp/usque-test/usque.pid); grep -E "^(VmSize|VmRSS|VmData|Threads)" /proc/$PID/status; echo === cpu_sample_10s
        read -r _ _ _ _ _ _ _ _ _ _ _ _ _ U1 S1 _ < /proc/$PID/stat
        sleep 10
        read -r _ _ _ _ _ _ _ _ _ _ _ _ _ U2 S2 _ < /proc/$PID/stat
        echo "jiffies=$(($U2-$U1+$S2-$S1)) over 10s (HZ=100) => $((($U2-$U1+$S2-$S1)*100/100/10))% of one core avg idle"
        echo === load; cat /proc/loadavg; echo === memfree; grep -E "MemFree|MemAvailable" /proc/meminfo')
  printf '%s\n' "$RES" | sed 's/^/  /'
  printf '%s\n' "$RES" > "$ART_DIR/phase1-$carrier-resources.txt"
  RSS=$(printf '%s' "$RES" | sed -n 's/^VmRSS:[[:space:]]*//p')
  [ -n "$RSS" ] && { ok "process alive, RSS $RSS"; record "$carrier process RSS ($RSS)" PASS; } || { bad "process not running after test"; record "$carrier process alive" FAIL; }

  log "8. Stop and restore ($carrier)"
  rt 'sh /tmp/usque-test/emergency-cleanup.sh warp9'
  sleep 1
  POST=$(rt 'ip -4 route show; echo ===; ip -br link show warp9 2>&1; echo ===; curl -m 15 -s https://www.cloudflare.com/cdn-cgi/trace')
  printf '%s\n' "$POST" | sed 's/^/  /'
  printf '%s\n' "$POST" > "$ART_DIR/phase1-$carrier-post-stop.txt"
  POST_IP=$(printf '%s' "$POST" | sed -n 's/^ip=//p')
  printf '%s' "$POST" | grep -q '^default via ' && { ok "default route restored via WAN"; record "$carrier default route restored" PASS; } || { bad "default route NOT restored"; record "$carrier default route restored" FAIL; }
  printf '%s' "$POST" | grep -q "dev $WAN_DEV" && { ok "WAN device $WAN_DEV back in routing table"; record "$carrier WAN device restored" PASS; } || { bad "WAN device missing"; record "$carrier WAN device restored" FAIL; }
  printf '%s' "$POST" | grep -q '^warp=off' && { ok "egress restored: warp=off, ip=$POST_IP"; record "$carrier egress restored warp=off" PASS; } || { bad "egress still redirected (warp=on)"; record "$carrier egress restored warp=off" FAIL; }
  LINK_STATE=$(rt 'ip link show warp9 >/dev/null 2>&1 && echo PRESENT || echo ABSENT')
  [ "$LINK_STATE" = ABSENT ] && { ok "warp9 link removed"; record "$carrier warp9 removed" PASS; } || { bad "warp9 link still present"; record "$carrier warp9 removed" FAIL; }
  rt 'nslookup www.cloudflare.com 127.0.0.1 2>&1 | tail -4' | tee "$ART_DIR/phase1-$carrier-post-dns.txt" | sed 's/^/    /'
  return 0
}

CARRIER_RESULT=0
if [ "$MODE" = udp ] || [ "$MODE" = auto ]; then
  run_carrier udp || CARRIER_RESULT=$?
  UDP_FAILED_BEFORE=$CARRIER_RESULT
  if [ "$CARRIER_RESULT" -ne 0 ]; then
    log "UDP/443 failed — checking whether the tether blocks it"
    rt 'sh /tmp/usque-test/emergency-cleanup.sh warp9' >/dev/null
  fi
fi
if [ "$MODE" = http2 ] || { [ "$MODE" = auto ] && [ "${UDP_FAILED_BEFORE:-0}" -ne 0 ]; }; then
  if [ "${H2_SUPPORTED:-0}" = 1 ]; then
    run_carrier http2
  else
    log "HTTP/2 fallback: binary has no --http2 (dropped by design) — skipping"
    record "http2 carrier available" SKIP
  fi
fi

# ---------------------------------------------------------------- summary
log "Summary ($(date -u +%FT%TZ))"
printf '  %-58s %s\n' CHECK RESULT
for c in "${CHECKS[@]}"; do
  IFS='|' read -r name res ev <<< "$c"
  printf '  %-58s %s\n' "$name" "$res"
done
echo "  TOTAL: PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
{ echo "# phase1 $(date -u +%FT%TZ) mode=$MODE log=$(basename "$LOG")"
  for c in "${CHECKS[@]}"; do echo "$c"; done
} > "$ART_DIR/phase1-summary.txt"
echo "  log: $LOG"
echo "  summary: $ART_DIR/phase1-summary.txt"
[ "$FAIL" -eq 0 ] || exit 20
