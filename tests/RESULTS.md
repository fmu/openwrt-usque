# usque on OpenWrt 25.12 — end-to-end test results

> **Sanitized for publication:** private keys/tokens, internal hostnames, MAC addresses,
> DHCP/ULA device identifiers and personal/public IP addresses in this report and in
> `tests/baseline/` were replaced with `<REDACTED>`/example placeholders. Test outcomes
> and package versions are unchanged.


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

## Safety protocol (and one incident, 2026-10-26 ~18:00 CET)

Testing "does WARP actually carry traffic" necessarily means pointing the router's default
route at the tunnel for a few seconds. Phase-1 runs do exactly that and then revert it in an
`EXIT` trap plus an on-router watchdog. During manual debugging at ~17:59 the user noticed the
temporary takeover and restored connectivity manually (removed the route, killed the test
process). Verified afterwards: WAN default `via 172.20.10.1 dev phy0-tether metric 1` only, no
`warp9`, no test `ip rule`s, `curl trace → warp=off`, DNS + LAN fine. Thanks — that was a
test artifact, not a product behaviour.

From now on:

- Baseline/install/service tests (`tests/phase2.sh`) **never** touch the default route: with the
  v2 package defaults (`default_route=0`) the router's WAN default must be unchanged before and
  after start.
- The route-takeover part of `tests/phase1.sh` now requires an explicit
  `PHASE1_ALLOW_DEFAULT_ROUTE=1`; without it the harness only proves tunnel establishment and
  reports that the WAN default was untouched. Runs that install a default route print the
  revert result at the end.
- Opt-in routing (`default_route=1`) is tested only in a short, explicitly requested window,
  with `ip route` checked before/after, and reverted immediately.
- Every script keeps its trap + watchdog cleanup; every report includes the router's final state.

## IPv6 through the tunnel — **works**, needed a v6 policy rule

Earlier phase-1 runs recorded IPv6 as SKIP with `curl -6` timing out. Root cause found (Lead
proved it, I reproduced it here): **mwan3's IPv6 fwmark rules**. With a v6 default route via the
tunnel, mwan3's `ip -6 rule 2062 fwmark 0x3e00/0x3f00 unreachable` (and its nft marks) sends v6
traffic to a blackhole; `ping -6` returned `sendto: Network unreachable`, and outgoing v6
packets were dropped before reaching the tunnel.

Fix — same policy override as for IPv4, but for the v6 ruleset (verified live):

```sh
ip -6 rule add priority 1000 from all lookup main   # remove on disconnect
ip -6 route replace default dev warp9 metric 0      # v6_route=auto equivalent
```

Live result with the official CI binary (`usque` sha256 `492b89b3…`):

| Check | Result | Evidence |
| --- | --- | --- |
| `curl -6 https://www.cloudflare.com/cdn-cgi/trace` | PASS | `ip=<WARP_EGRESS_IP>`, `colo=VIE`, `loc=AT`, `warp=on`, HTTP 200 |
| `ping -6 2606:4700:4700::1111` through tunnel | PASS | 2/2 packets, 0% loss, ~116 ms |
| v4 control at the same time | PASS | `warp=off` with no v4 default takeover (v4 default untouched) |

So IPv6 needs no daemon change: the v2 hook must install a v6 policy rule (priority 1000,
`lookup main`) whenever it installs a v6 default route, and remove it on teardown — exactly what
packager is doing for `ipv6_route=auto`. LAN IPv6/NAT66 still to be validated with the v2 package.

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

Artifacts validated (all green, same behaviour):

| Artifact | Size | sha256 | Notes |
| --- | --- | --- | --- |
| local cross-build (builder tree 17:31) | 2,912,344 B | `a3c91f66…` | first proof of the ring/DER fix |
| CI run 40361577938 (cfbd6aa) | 2,898,952 B | `492b89b3…` | h2 removed; official acceptance run |
| CI run 40366592592 (9e6eac2) | 2,902,152 B | `7c1c5f69…` | **current**; adds `--authority`/`--protocol`; egress `<WARP_EGRESS_IP>`, `warp=on` |

Latest run (`PHASE1_ALLOW_DEFAULT_ROUTE=1`): MASQUE over UDP/443 in 1 s, first trace attempt
`warp=on` (egress `<WARP_EGRESS_IP>`, `colo=VIE`), MTU 1400, v4+v6 tun addresses, HTTPS 200, DNS
ok, RSS 6,768 kB, clean revert (`warp=off`, WAN default restored, `warp9` gone).
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

## Phase 2 — signed apk feed, procd service, LuCI, IPv6/NAT66, reboot — **PASS on final release**
Final verified versions: **usque 0.2.1-r1 + luci-app-usque 1.1.1-r1** (run 40373166952,
head 5139af6; binary 2,905,040 B; apks 1,466,405 B / 21,561 B).

Feed: **live GitHub Pages feed**, exactly as the docs say (no `--allow-untrusted`):

```sh
wget -O /etc/apk/keys/usque.pem https://fmu.github.io/openwrt-usque/feed/usque.pem
echo "https://fmu.github.io/openwrt-usque/feed/packages.adb" > /etc/apk/repositories.d/usque.list
apk update
apk add usque luci-app-usque
```

Feed provenance: run 40367747226 (apps) + 40369664094 (final luci 1.1.0-r1), Pages deploy
verified by downloading all five files (packages.adb 949 B, index.json 144 B, usque.pem 178 B
sha256 `f47c63b5…`, usque-0.2.0-r1.apk 1,463,960 B, luci-app-usque-1.1.0-r1.apk 21,472 B).

### Install (transcript: `tests/artifacts/phase2-transcript.log`)

`apk update` → 2 unavailable/stale (pre-existing unrelated `openwrt-spectral` feed warns
UNTRUSTED; our feed does not warn), 11213 packages. `apk add usque luci-app-usque` →
`Installing usque (0.2.0-r1)`, `Installing luci-app-usque (1.0.0-r1)`, both post-install
scripts OK, `OK: 43.2 MiB in 270 packages`. Signature trust is exercised (no
`--allow-untrusted`, no untrusted warning for our index).

| Size / space | Value |
| --- | --- |
| `apk info -s usque` | 2862 KiB installed |
| `apk info -s luci-app-usque` | 80 KiB installed (1.1.0-r1) |
| `/usr/bin/usque` on disk | 2,836 KiB (2,902,152 B) |
| overlay free before install | 3276 KiB |
| overlay free after install | 1592 KiB (then 1572 KiB after luci 1.1.0 upgrade) |
| installers | `du`: init 12K, config 4K, hook+firewall+lan_ipv6 28K, rpcd 32K, view 52K, menu/acl 8K |

⚠️ **Docs bug found**: `apk update && apk add luci-app-usque` does **not** upgrade an
already-installed luci-app-usque on this apk-tools 3.0.5 (`apk list` showed
"upgradable from: luci-app-usque-1.0.0-r1" but `apk add` was a no-op). Pinned
`apk add luci-app-usque=1.1.0-r1` performs the upgrade. Docs should use the pinned form or
`apk upgrade luci-app-usque`.

### Check matrix

| Check | Result | Evidence |
| --- | --- | --- |
| Signed feed install from Pages (key + repositories.d + apk update + apk add) | PASS | transcript; no `--allow-untrusted` |
| Package file layout (init, config, hook, firewall, lan_ipv6, rpcd, menu.d, acl.d, view) | PASS | `apk info -L`, `phase2-service.log` |
| Fresh defaults `enabled=0, default_route=0, ipv6=1, ipv6_route=auto, mode=cloudflare` | PASS | `uci show usque` |
| Install makes **zero routing changes** (WAN v4 default only, no rule/bypass/tunnel route) | PASS | pre-start dumps |
| `usque.warp0.enabled=1` + start → tunnel up in 6 s, MASQUE connected | PASS | `phase2-service.log` |
| With `default_route=0`: v4 WAN default untouched, no v4 rule, no v4 route via warp0, no endpoint bypass | PASS | same |
| `ipv6_route=auto` on v4-only WAN: v6 default via warp0 + IPv6 priority-1000 rule | PASS | by design, single expected route change |
| Router egress with defaults: v4 `warp=off`, v6 `warp=on` (`ip=<WARP_EGRESS_IP>`, colo VIE) | PASS | trace files |
| fw4 zone `usque`, `lan→usque` and `guest→usque` forwarding | PASS | nft `forward_lan`/`forward_guest` have `accept_to_usque` |
| Restart | PASS | tun returns; 3× restart loop later also fine (no EEXIST in that window) |
| Stop: v6 route+rule removed, tun gone, WAN v4 untouched, v4 egress OK | PASS | `phase2-service.log` |
| LAN IPv6 / NAT66 (`settings.lan_ipv6=1`): Mac (ULA `fd12:…`) `curl -6` via router/WARP | PASS | `ip=<WARP_EGRESS_IP>`, `warp=on`, colo VIE; revert clean |
| Opt-in `default_route=1`: v4 default via warp0 + rule 1000 + `warp=on` | PASS | `phase2-optin.log` |
| Opt-in revert to 0: WAN v4 default restored exactly, v4 egress `warp=off` | PARTIAL | route restored; **v4 rule 1000 + stale owner state left** (see Known issues) |
| Multi-instance: second section `warp1` (custom mode) up simultaneously with warp0 | PASS | both links up, both hooks ran, warp1 `default_route=0` made no routing changes |
| Custom mode: `warp2` section, no auto-register, `sni`/`authority`/`protocol` reach the daemon | PASS | proc cmdline: `-s example.org --authority example.org --protocol connect-ip` |
| LuCI 1.1.0-r1: menu.d, acl.d, view asset (HTTP 200), `luci.usque` ubus object | PASS | `phase2-luci-1.1.0.log` |
| `luci.usque status/getConfig/test/logs` per section, no secrets | PASS | status has `connections[]` with `route4_installed=false, route6_installed=true`, iface addrs, pid/uptime; getConfig has no `private_key`/`access_token`; `test` → `warp=on` |
| Unauthenticated LuCI page → 403 (login required) | PASS | no password used per Lead's instruction |
| Reboot: service ENabled/auto-start, warp0 up, WAN v4 default only, v6 via WARP, LuCI reachable | PASS 7/7 | `phase2-reboot.log` |

### 0.2.1-r1 re-test (both earlier issues FIXED — verified on the live router)

Upgrade command used (important, see docs note above):
`apk update && apk add usque=0.2.1-r1 luci-app-usque=1.1.1-r1`
→ upgraded both packages, `OK: 43.2 MiB in 270 packages`, overlay 1584 KiB free, service kept
running, all defaults preserved (`default_route=0`, `ipv6_route=auto`, `enabled=1`).

| Re-test | Result | Evidence |
| --- | --- | --- |
| **Restart ×5, EEXIST fix** — every restart spawns a fresh pid, no `failed to add IPv4 address`, no 10 s respawn delay | **PASS** | pids 27118 → 27962 → 28490 → 29001 → 29472 → 29959, each `status=running`, EEXIST delta 0, link up immediately |
| **Stop cleanliness** — stop leaves no instance/process, no tun, no v4/v6 defaults or rules, empty state dir | **PASS** | ubus pid `[]`, `pgrep` 0, `ip -br link show warp0` → absent, only WAN v4 default, `[v6def empty]`, no rules, `/var/run/usque/shared/` empty |
| **Start after stop** | **PASS** | new pid 30499, running, WAN v4 default only, v6 default back |
| **default_route 1→0 revert (state-driven teardown fix)** — no v4 rule 1000, no warp0 v4 default, v4 owner state purged, v6 auto route+rule unaffected | **PASS** | after revert: `ip rule show` has no 1000; `ip -4 route show default` = WAN only; `/var/run/usque/shared/` contains only `iprule-v6`, `iprule-v6.added`, `v6-default`; v6 default via warp0 + v6 rule 1000 present; egress v4 `warp=off`, v6 `warp=on` |
| Opt-in `default_route=1` still works on 0.2.1 | **PASS** | v4 default via warp0 + rule 1000, `ip route get` → `dev warp0 src 172.16.0.2`, 3/3 curl traces `warp=on ip=<WARP_EGRESS_IP>` |
| LAN IPv6/NAT66 on 0.2.1 (packaged helper, Mac ULA `fd12:…`) | **PASS** | Mac `curl -6` → `warp=on ip=<WARP_EGRESS_IP> colo=VIE`; revert clean |
| Multi-instance on 0.2.1 (`warp1` second section, custom mode) | **PASS** | both links UP, both hooks ran; warp1 `default_route=0/ipv6_route=off` → "no routing changes requested" |
| Custom mode on 0.2.1 (`warp2`/`custom0`) | **PASS** | instance up, no auto-register, hook made no routing changes; `sni`/`authority`/`protocol` reach the daemon (see proc cmdline in 0.2.0 section) |
| LuCI 1.1.1-r1 ubus: `status/getConfig/test/logs` per section | **PASS** | status `connections[]` + `globals`; getConfig no secrets; `test` → `warp=on ip=<WARP_EGRESS_IP> colo=VIE`, `method: curl --interface warp0`; no `private_key`/`access_token` in any output |
| Reboot on the final release | not re-run (no need) | 0.2.0-r1 reboot was PASS 7/7; boot order noted as known behaviour |

Both previously reported defects are **FIXED and verified**:

1. ~~stop→start EEXIST race~~ — fixed by daemon EEXIST tolerance + init waiting for/deleting a
   stale interface. Verified by 5× restart with a fresh pid each time and zero
   `failed to add IPv4 address` lines, plus a full stop→start cycle.
2. ~~default_route 1→0 revert leaves the IPv4 priority-1000 rule + stale state~~ — fixed by
   state-driven teardown (ownership records + kernel presence). Verified with the exact repro:
   rule gone, no warp0 v4 default, v4 owner state purged, legitimate v6 auto state retained.

Known minor behaviour (documented, not fixed in this release): after a reboot the daemon
starts before the tether's default route exists (log: `sendmsg error: Network unreachable`) and
reconnects when WAN comes up (~83 s in the 0.2.0-r1 reboot). Self-recovers; IPv4/IPv6 for LAN
clients follows once connected.

## Router left in state (final, verified 2026-10-26 19:04 CET)

Full dump: [`tests/artifacts/phase2-final-state-0.2.1.txt`](artifacts/phase2-final-state-0.2.1.txt).

```
apk: usque-0.2.1-r1 + luci-app-usque-1.1.1-r1 (from the live Pages feed)
df -k /overlay: 19036 total, 16436 used, 1584 available (91%)
/etc/init.d/usque status: running        boot_enabled: yes (ubus instance pid 30499)
usque.warp0: enabled=1 mode=cloudflare interface=warp0 mtu=1400 ipv6=1 endpoint_ipv6=0
             ipv6_route=auto default_route=0 metric=0 endpoint_port=443 always_reconnect=1
usque.settings: lan_ipv6=0 lan_ipv6_prefix=""
ip -4 route show default: default via 172.20.10.1 dev phy0-tether proto static src 172.20.10.13 metric 1   ← only v4 default
ip -6 route show default: default dev warp0 metric 1024 pref medium                                        ← ipv6_route=auto
ip rule show:   no priority-1000 rule (only mwan3's 1001/2001/2061/2062/3001 + main)
ip -6 rule show: 1000: from all lookup main   ← required for IPv6 over mwan3
egress v4: ip=<WAN_IP> warp=off colo=VIE
egress v6: ip=<WARP_EGRESS_IP> warp=on colo=VIE
link: warp0 UP; addrs 172.16.0.2/32 + <WARP_CLIENT_IPV6>/128
```

Cleanup state: no test sections left (`uci show usque` has only `settings` + `warp0`); no
`warp9`/`warp1`/`warp2`/`warp3`/`custom*` interfaces or uci sections; `/tmp/usque-test`
removed from the router; `/etc/apk/keys/usque.pem` + `/etc/apk/repositories.d/usque.list`
(Pages) kept so `apk` upgrades keep working; stale hand-copied `network.warp0`/`warp0_v6`
leftovers remain removed; `/etc/usque/warp0.json` kept (no new device registered).
Revert material: `tests/backups/pre-phase2-20261026-172340/` (Mac).
