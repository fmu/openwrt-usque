# usque OpenWrt package

OpenWrt 25.12 package for [usque](https://github.com/FireMasterK/usque-rs), a
Cloudflare WARP / MASQUE (Connect-IP) client, TUN mode only.

The package does not build the Rust binary itself: the static ARMv7 binary is
produced by `build/build-usque.sh` at the repository root and copied to
`openwrt/usque/bin/usque` (gitignored) before packaging.

## Files installed by the package

| Path | Purpose |
| --- | --- |
| `/usr/bin/usque` | static ARMv7 (musl) binary |
| `/etc/init.d/usque` | procd init script, one instance per `config usque` section |
| `/etc/config/usque` | UCI configuration (see below) |
| `/usr/libexec/usque/hook` | routing hook called by usque on connect/disconnect |
| `/usr/libexec/usque/firewall` | idempotent fw4 zone helper (zone `usque`, forwardings for lan/guest/...) |
| `/usr/libexec/usque/lan_ipv6` | optional LAN IPv6 (ULA + RA + NAT66) helper |

## Configuration model (v2)

`/etc/config/usque` has one global `settings` section and one `usque` section
per tunnel connection. Each enabled section runs as its own procd instance
(instance name = section name), with its own config file, interface and state
directory (`/var/run/usque/<section>/`).

```
config settings 'settings'
	option lan_ipv6 '0'
	option lan_ipv6_prefix ''

config usque 'warp0'
	option enabled '0'
	option mode 'cloudflare'
	option config_file '/etc/usque/warp0.json'
	option interface 'warp0'
	option mtu '1400'
	option ipv6 '1'
	option endpoint_ipv6 '0'
	option ipv6_route 'auto'
	option default_route '0'
	option metric '0'
	option endpoint_port '443'
	option always_reconnect '1'
	option auto_register '1'
	option device_name ''
	option log_level 'info'
	option sni ''
```

Global `settings`:

| Option | Default | Meaning |
| --- | --- | --- |
| `lan_ipv6` | `0` | give LAN clients IPv6 through the tunnel (ULA + RA + NAT66) |
| `lan_ipv6_prefix` | *(empty)* | ULA prefix to use (`/48` or `/64`); derived from the LAN MAC when empty |

Per-connection section options:

| Option | Default | Meaning |
| --- | --- | --- |
| `enabled` | `0` | start this connection (also `enable` the init script) |
| `mode` | `cloudflare` | `cloudflare` (auto-register, WARP defaults) or `custom` (user supplied MASQUE/Connect-IP config; never registers) |
| `config_file` | `/etc/usque/<section>.json` | credentials/config file (mode 600) |
| `interface` | `warp0` | TUN interface name (must be unique per section) |
| `mtu` | `1400` | TUN MTU |
| `ipv6` | `1` | tunnel IPv6 address (passes `--no-tunnel-ipv6` when 0) |
| `endpoint_ipv6` | `0` | connect to the MASQUE endpoint over IPv6 (`--ipv6`); only for native-v6/v6-only networks |
| `ipv6_route` | `auto` | install an IPv6 default route: `auto` (only when no native IPv6 default exists), `always`, `off` |
| `default_route` | `0` | install an IPv4 default route through the tunnel (off by default: **no routing changes at all**) |
| `metric` | `0` | metric for the tunnel default route(s) |
| `endpoint_port` | `443` | WARP endpoint port |
| `always_reconnect` | `1` | keep reconnecting in the background |
| `auto_register` | `1` | register a new WARP device if `config_file` is missing (cloudflare mode only) |
| `device_name` | *(empty)* | device name passed to `usque register -n` |
| `log_level` | `info` | `RUST_LOG` filter for the daemon |
| `sni` | *(empty)* | TLS SNI for custom endpoints (`-s <sni>`) |
| `authority` | *(empty)* | CONNECT request authority for custom endpoints (`--authority <host>`) |

## Routing behaviour

**With the default configuration the package makes no routing changes**: no
default route, no endpoint bypass, no policy rule, neither on start nor on
stop. This is deliberate: the package must never take over routing unless
asked to.

When `default_route=1` (IPv4) and/or `ipv6_route` installs an IPv6 default
route, the hook (`/usr/libexec/usque/hook`):

1. installs an endpoint bypass host route via the current WAN for the address
   family whose default route is being changed (prevents a routing loop),
2. saves the current WAN default route once (shared across sections),
3. installs `default dev <interface> metric <metric>` for that family,
4. installs `ip rule priority 1000 from all lookup main` for that family, so
   policy routing (`mwan3` fwmark rules) cannot divert traffic away from the
   main table. **This rule is required on mwan3 routers, for IPv4 and IPv6.**

Ownership is ref-counted across sections: the policy rule and the saved WAN
default route are only removed when the last owning section disconnects. The
WAN default route is never removed while the tunnel is up, so failover stays
automatic. `ipv6_route=auto` never treats a route installed by usque itself as
a "native" IPv6 default.

The firewall helper creates a dedicated fw4 zone (`firewall.usque`, device
list of all enabled tunnel interfaces, `input REJECT`, `output ACCEPT`,
`forward REJECT`, `masq`, `masq6`, `mtu_fix`) plus a `firewall.usque_<zone>`
forwarding rule for every internal zone that forwards to `wan` (always `lan`,
and e.g. `guest` when present), so LAN/guest clients keep internet access
while a tunnel default route is active. The existing WAN zone is never
modified. The zone and its forwardings are removed by the package `prerm`.

With `settings.lan_ipv6=1` the `lan_ipv6` helper additionally ensures a ULA
prefix (`network.globals.ula_prefix`, derived from the LAN MAC unless
`lan_ipv6_prefix` is set), an IPv6 prefix on `lan` (`ip6assign=60` if neither
`ip6assign` nor `ip6addr` is set) and the RA/DHCPv6 server
(`dhcp.lan.ra=server`), then calls `reload_config`. Existing settings are never
replaced. Disabling `lan_ipv6` leaves the configuration in place
(non-destructive); revert manually if needed.

## Usage

```
/etc/init.d/usque enable
/etc/init.d/usque start            # all enabled sections
/etc/init.d/usque start warp0      # a single section
/etc/init.d/usque restart warp0
/etc/init.d/usque stop
```

If `config_file` is missing, `mode=cloudflare` and `auto_register=1`, the init
script runs `usque --config <config_file> register --accept-tos --force` for
that section before starting the daemon (Cloudflare WARP ToS are accepted
implicitly by `auto_register=1`).

## Building

In an extracted OpenWrt 25.12 SDK:

```sh
./build/build-usque.sh                       # writes dist/usque
cp dist/usque openwrt/usque/bin/usque
cp -r openwrt/usque "$SDK_DIR/package/usque"
cd "$SDK_DIR"
echo 'CONFIG_PACKAGE_usque=y' >> .config
make defconfig
DEVELOPER=1 make -C package/usque compile V=s \
    USQUE_BINARY="$SDK_DIR/package/usque/bin/usque"
```

`DEVELOPER=1` is only needed when the `kmod-tun` dependency is not part of the
SDK's package metadata (the dependency is still recorded in the apk metadata).
The CI pipeline (`.github/workflows/package.yml`) performs these steps via
`scripts/build-feed.sh` and assembles a signed apk feed.
