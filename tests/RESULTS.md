# usque on OpenWrt 25.12 — end-to-end test results

Tester: teammate `tester`. Target: live router `root@192.168.1.1`.
Scripts: [`tests/`](.) (`phase1.sh`, `phase2.sh`, `snapshot.sh`, `fetch-artifacts.sh`,
`deploy.sh`, `cleanup.sh`, `restore.sh`). Raw evidence: `tests/artifacts/`, baselines:
`tests/baseline/`. Credentials from `reference/warp0.json` are never printed (only hashes).

## Environment (captured 2026-10-26 17:00 CET)

| Item | Value |
| --- | --- |
| Router | `router.example.net`, OpenWrt 25.12.6 (`r33311-981c978eff`), ipq40xx/generic, `arm_cortex-a7_neon-vfpv4` |
| Kernel / apk | 6.12.112 SMP armv7l / apk-tools 3.0.5-r3 (`apk`), firewall4 + nft |
| WAN | iPhone tether: `phy0-tether` 172.20.10.13/28, gw `172.20.10.1`, default metric 1, IPv4-only |
| Baseline egress | `curl https://www.cloudflare.com/cdn-cgi/trace` → `ip=<WAN_IP>`, `warp=off`, `colo=VIE` |
| Overlay | `/dev/ubi0_2` 18.6M total, 14.3M used, **3.3M free (81%)** — tight |
| TUN | `tun.ko` loaded (refcount 0, ready) |
| Pre-existing usque leftovers | `/etc/usque/{up.sh,down.sh,warp0.json}` (no init script), uci `network.warp0`, `network.warp0_v6`, firewall zone `wan` lists `warp0 warp0_v6`, no `device` zone yet |
| WARP config | `/etc/usque/warp0.json` == `reference/warp0.json`, sha256 `5f839021c36e2eca…`, 710 bytes, endpoint `162.159.198.2:443`, ipv4 `172.16.0.2`, ipv6 `<WARP_CLIENT_IPV6>` |

Baseline dumps: [`tests/baseline/snapshot.txt`](baseline/snapshot.txt),
[`uci-network.txt`](baseline/uci-network.txt), [`uci-firewall.txt`](baseline/uci-firewall.txt),
[`apk-packages.txt`](baseline/apk-packages.txt), [`df.txt`](baseline/df.txt),
[`routes.txt`](baseline/routes.txt).

## Harness self-test (before any real test)

Route-install/withdraw mechanics were validated on the live router **without**
touching the default route (harmless TEST-NET route `203.0.113.9/32`): added via
`phy0-tether`, removed, `emergency-cleanup.sh` ran idempotently, default route and WAN
egress unchanged. `warp9` was never created outside the trap-protected phase-1 run.

## Environment-control probe — upstream Go usque v4.2.1 (2026-10-26 17:09 CET)

Purpose: validate the router environment and the routing plan before the Rust binary exists.
Artifact `usque_4.2.1_linux_armv7.zip` downloaded from the upstream GitHub release,
sha256 `5e0025f6298d3822ef13b85960fc7c5845f8510e1ddd088ea87ea0f00d39cc88` matches upstream
`checksums.txt`; binary inside is 12,255,394 bytes, ELF 32-bit ARM EABI5 static, run from
`/tmp` only (overlay untouched; removed afterwards).

| Check | Result | Evidence |
| --- | --- | --- |
| `--help` / `nativetun --help` runs on armv7 | PASS | CLI identical to our contract |
| TUN `warp9` created, IPv4 172.16.0.2/32 + IPv6, MTU 1280 | PASS | `phase1-*-tunnel-state.txt` |
| MASQUE handshake over **UDP/443 through iPhone tether** | PASS | `Connected to MASQUE server` in 0–1 s |
| HTTPS through tunnel (HTTP 200) | PASS | `phase1-*-trace.txt` |
| DNS via router resolver and via 1.1.1.1 through tunnel | PASS | `phase1-*-dns-*.txt` |
| `warp=on` egress through tunnel | PASS **only with the mwan3 fix below** | egress `<WARP_EGRESS_IP>`, `colo=VIE`, `loc=AT` |
| Cleanup: routes removed, `warp=off`, `warp9` gone | PASS | `phase1-*-post-stop.txt` |

### Finding 1 — UDP/443 is NOT blocked by the iPhone tether

QUIC/MASQUE handshake completes in under a second, so `--http2` is not needed for
connectivity on this uplink. Keep `--http2` in the Rust binary as a fallback, but it is not
required by the environment.

### Finding 2 — mwan3 policy routing defeats a plain default route (critical)

`ip route replace default dev warp9 metric 0` alone is silently bypassed: mwan3 installs nft
marks (e.g. TCP/443 → mark `0x100/0x3f00`) and `ip rule 2001: from all fwmark 0x100/0x3f00
lookup 1` sends marked flows to table 1 (`default via 172.20.10.1`, the tether). Result: tunnel
is up, `ip route get` says `dev warp9`, but `curl` still egresses from the real WAN IP with
`warp=off`, and the tun TX counters do not move.

Fix verified on the live router — one rule above mwan3's rules pins lookup to `main`:

```sh
ip rule add priority 1000 from all lookup main   # remove on stop
```

With it: `curl https://www.cloudflare.com/cdn-cgi/trace` → `warp=on`,
`ip=<WARP_EGRESS_IP>`, `colo=VIE`; tun counters advance (4 → 60 packets).

The packaged init/hotplug must install/remove this rule (or an equivalent fwmark-safe
scheme), otherwise every user running mwan3 — including this router — sees `warp=off`
with a healthy tunnel. `tests/router/p1-up.sh` applies it for the test window and
`emergency-cleanup.sh` removes it.

## Rust probe (WIP build, `dist/usque-probe`, sha256 `11d3e55a…`, 4,239,712 bytes)

Runs on the router, creates the TUN device, then aborts before TLS:

```
INFO usque_cli::cmd::nativetun: Created TUN device: warp9
Error: failed to build key pair
Caused by:
    Key rejected by ring: InvalidEncoding
```

Root cause (diagnosed and fix verified locally with ring 0.17.14/rcgen 0.14.10 against the real
config key): in `src/crates/usque-crypto/src/keys.rs`, the SEC1→PKCS#8 re-wrap
(`build_pkcs8_v1`) uses (a) non-minimal DER lengths — `04 81 79` instead of `04 79` for the
121-byte ECPrivateKey, and an outer length that is one byte too large — and (b) the RFC 5915
`[0] parameters` field is emitted *before* the 32-byte scalar instead of after it. ring's strict
parser rejects both. The existing unit test only covers ring-generated keys, so it stays green.
A corrected 150-byte document was proven to parse in ring, build an rcgen `KeyPair`, and sign a
325-byte self-signed certificate embedding the config public key. Reported to builder with the
exact constants.

## Phase 1 — raw binary, existing config, spare interface `warp9` — **PASS**

Run: `tests/phase1.sh` with `USQUE_BIN=dist/usque` (fixed binary, locally cross-compiled
from builder's tree with `build/build-usque.sh` + zig 0.17.0/cargo-zigbuild; the CI artifact
`usque-armv7-musl` can be dropped in `tests/artifacts/build/` and the same command is used).

Artifact: **2,912,344 bytes (2.8 MiB)**, sha256 `a3c91f661f93e0fd2fd24cb4cab287834911a5562c51af507323d04c48eecd8c`.
Log: [`tests/artifacts/phase1-rust-local.log`](artifacts/phase1-rust-local.log) (gitignored),
summary [`tests/artifacts/phase1-summary.txt`](artifacts/phase1-summary.txt),
CLI [`tests/artifacts/usque-help.txt`](artifacts/usque-help.txt) (captured for the docs).

| Check | Result | Evidence |
| --- | --- | --- |
| Binary sha256 identical Mac↔router | PASS | `a3c91f66…` on both |
| `usque --help`, `native-tun --help` run on armv7 | PASS | full help captured; `--auto-register` present |
| MASQUE connected over UDP/443 through iPhone tether | PASS | `Connected to MASQUE server`, 1 s, no TLS/ring errors |
| first-attempt `warp=on` (no retry flapping) | PASS | attempt 1: `dev warp9`, `warp=on` |
| tun `warp9` UP, MTU 1400, 172.16.0.2/32 + IPv6 /128 | PASS | `phase1-udp-tunnel-state.txt` |
| Default route via `warp9`; endpoint /32 bypass via `phy0-tether` | PASS | route dump |
| HTTPS through tunnel | PASS | HTTP 200 |
| DNS via router resolver and via 1.1.1.1 through tunnel | PASS | `phase1-udp-dns-*.txt` |
| Egress IP changed `<WAN_IP> → <WARP_EGRESS_IP>`, `colo=VIE`, `loc=AT` | PASS | `phase1-udp-trace.txt` |
| IPv6 through tunnel | SKIP | `curl -6` no reply; no v6 uplink on this WAN, need builder guidance |
| RSS / VSZ / threads / idle CPU | PASS | 6,740 kB / 16.3 MB / 5 / ~1 % of one core |
| Stop → routes withdrawn, `warp=off`, `warp9` gone, no ip rules left | PASS | `phase1-udp-post-stop.txt` |

Two implementation notes from this run:
- The machine-readable CLI is: `native-tun` with `-c/--config`, `-n/--interface-name`,
  `-m/--mtu` (default 1400), `-P/--connect-port`, `-k/--keepalive-period`,
  `-r/--reconnect-delay`, `--always-reconnect`, `--insecure`, `--on-connect`,
  `--on-disconnect`, `-I/--no-iproute2`, `--persist`, `--auto-register`. In this build
  `--http2` still existed; it is being removed (QUIC-only) and the harness then skips that
  carrier automatically.
- The phase-1 hook (`tests/router/p1-up.sh`) installs the mwan3 workaround rule; the packaged
  hook must do the same for phase 2 to show `warp=on` (see Finding 2 above).

## Phase 2 — signed apk feed, procd service, LuCI, reboot, uninstall

Status: **backups + stale-leftover cleanup done (steps 1–2); feed steps pending a
successful `package.yml` run and a working binary** (package builds currently fail; see
`gh run list --workflow package.yml`).

### Step 1–2 evidence (already executed on the live router)

Backup (`tests/backup.sh pre-phase2`, local + router copies):
`tests/backups/pre-phase2-20261026-172340/` (full `/etc/config`, `/etc/apk/{keys,repositories.d}`,
`/etc/usque/`, `uci show network|firewall`, routes, df, apk world) and
`/tmp/usque-test/backup-20261026-172340` on the router. Key hashes: `config/network`
`2f12711b…`, `config/firewall` `ad0d12c3…`, `usque/warp0.json` `5f839021…`.

Stale hand-copied leftovers removed (approved by user/Lead), exact commands:

```sh
uci -q delete network.warp0; uci -q delete network.warp0_v6; uci commit network
uci -q del_list firewall.@zone[1].network=warp0
uci -q del_list firewall.@zone[1].network=warp0_v6
uci commit firewall
rm -f /etc/usque/up.sh /etc/usque/down.sh
fw4 check && fw4 reload
```

Verified after: `uci show network | grep -c warp0` → 0, `uci show firewall | grep -c warp0` → 0,
`/etc/usque/` contains only `warp0.json` (kept, so no new Cloudflare device is registered),
default route `via 172.20.10.1 dev phy0-tether`, egress `warp=off ip=<WAN_IP>`,
overlay free unchanged at 3396K.

| Check | Result | Evidence |
| --- | --- | --- |
| Backups of `/etc/config/{network,firewall}` + `/etc/usque/` | – | – |
| Stale leftovers removed (uci `warp0`/`warp0_v6`, `up.sh`/`down.sh`) | – | – |
| `apk add` from signed feed succeeds | – | – |
| Installed size / overlay free after install | – | – |
| `service usque` enable/start/status/restart/stop | – | – |
| TUN device/address/routes/firewall zone+masq | – | – |
| Router-originated egress `warp=on` with service | – | – |
| LAN client path through router while tunnel up | – | – |
| LuCI page + ubus methods + menu.d/acl.d | – | – |
| Reboot → auto-start, config persists, tunnel returns | – | – |
| Disable/stop restores WAN default routing | – | – |
| Uninstall + restore → router as found | – | – |

## Router left in state (as of 2026-10-26 17:24 CET, after phase-2 step 2)

- Default route `via 172.20.10.1 dev phy0-tether metric 1`, egress `warp=off`,
  `ip=<WAN_IP>`, DNS/dnsmasq working.
- **Removed** the approved stale hand-copied leftovers: uci `network.warp0`,
  `network.warp0_v6`, their entries in firewall zone `wan`, `/etc/usque/up.sh`,
  `/etc/usque/down.sh`. Kept `/etc/usque/warp0.json`. `fw4` reloaded, WAN verified after.
- No test devices (`warp9`/`warp0`) and no test `ip rule` entries remain; nothing installed
  from the tests; overlay free unchanged (3396K). Backups exist locally and on the router
  (see step 1–2 evidence above) so everything is revertible.
- Final state after phase 2 will be recorded here once the feed test completes.
