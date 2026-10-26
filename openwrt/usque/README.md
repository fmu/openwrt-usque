# usque OpenWrt package

OpenWrt 25.12 package for [usque](https://github.com/FireMasterK/usque-rs), a
Cloudflare WARP (MASQUE / Connect-IP) client, TUN mode only.

The package does not build the Rust binary itself: the static ARMv7 binary is
produced by `build/build-usque.sh` at the repository root and copied to
`openwrt/usque/bin/usque` (gitignored) before packaging.

## Files installed by the package

| Path | Purpose |
| --- | --- |
| `/usr/bin/usque` | static ARMv7 (musl) binary |
| `/etc/init.d/usque` | procd init script (`start`/`stop`/`restart`/`reload`/`enable`/`status`) |
| `/etc/config/usque` | UCI configuration (`usque.main.*`), see below |
| `/usr/libexec/usque/hook` | routing hook called by usque on connect/disconnect |
| `/usr/libexec/usque/firewall` | idempotent fw4 zone helper (zone `usque`, `lan` -> `usque` forwarding) |

## UCI options (`/etc/config/usque`, section `main`)

| Option | Default | Meaning |
| --- | --- | --- |
| `enabled` | `0` | start the service (also `enable` the init script) |
| `config_file` | `/etc/usque/warp0.json` | WARP credentials/config file (mode 600) |
| `interface` | `warp0` | TUN interface name |
| `mtu` | `1400` | TUN MTU |
| `ipv6` | `0` | enable IPv6 on the tunnel and install an IPv6 default route |
| `http2` | `0` | use HTTP/2 transport instead of QUIC/MASQUE |
| `endpoint_port` | `443` | WARP endpoint port |
| `always_reconnect` | `1` | keep reconnecting in the background |
| `auto_register` | `1` | register a new WARP device if `config_file` is missing |
| `device_name` | *(empty)* | device name passed to `usque register -n` |
| `log_level` | `info` | `RUST_LOG` filter for the daemon |

If `config_file` is missing and `auto_register=1`, the init script runs
`usque --config <config_file> register --accept-tos --force` before starting the
daemon (Cloudflare WARP ToS are accepted implicitly by `auto_register=1`).

## Routing behaviour

On connect the hook (`/usr/libexec/usque/hook`):

1. remembers the current WAN default route(s),
2. adds a host route for the WARP endpoint via the current WAN (endpoint
   bypass, prevents a routing loop),
3. installs `default dev <interface> metric 0` for IPv4 (and IPv6 when
   enabled).

On disconnect it removes the endpoint bypass and tunnel default route(s) and
restores the saved WAN default route if none is left, so the router always has
a working default route (SSH does not break).

The firewall helper creates a dedicated fw4 zone (`firewall.usque`, device
`<interface>`, `input REJECT`, `output ACCEPT`, `forward REJECT`, `masq`,
`masq6`, `mtu_fix`) plus `firewall.usque_lan` forwarding `lan -> usque`. The
existing WAN zone is never modified. The zone is removed by the package
`prerm` script.

## Building

In an extracted OpenWrt 25.12 SDK:

```sh
./build/build-usque.sh                       # writes dist/usque
cp dist/usque openwrt/usque/bin/usque
cp -r openwrt/usque "$SDK_DIR/package/usque"
cd "$SDK_DIR"
echo 'CONFIG_PACKAGE_usque=y' >> .config
make defconfig
DEVELOPER=1 make package/usque/compile V=s
```

`DEVELOPER=1` is only needed when the `kmod-tun` dependency is not part of the
SDK's package metadata (the dependency is still recorded in the apk metadata).

The CI pipeline (`.github/workflows/package.yml`) performs these steps and
assembles a signed apk feed with `scripts/build-feed.sh`.
