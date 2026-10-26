# usque for OpenWrt

Cloudflare WARP (MASQUE / Connect-IP, RFC 9484) tunnel daemon for **OpenWrt 25.12**
with a LuCI interface and one-click WARP device registration. Built from
[FireMasterK/usque-rs](https://github.com/FireMasterK/usque-rs), TUN mode only.

**Install instructions: <https://fmu.github.io/openwrt-usque/>**

```sh
wget -O /etc/apk/keys/usque.pem https://fmu.github.io/openwrt-usque/feed/usque.pem
echo "https://fmu.github.io/openwrt-usque/feed/packages.adb" >> /etc/apk/repositories.d/customfeeds.list
apk update
apk add luci-app-usque
```

Then open LuCI → *Services → usque*, generate a WARP configuration and start the
service. The feed is signed with an ECDSA P-256 key; the repository index and
packages are served from GitHub Pages.

## Repository layout

| Path | Contents |
| --- | --- |
| `src/` | vendored + patched usque-rs source (upstream revision in `UPSTREAM.md`) |
| `build/` | reproducible armv7 cross-compile script used by CI |
| `openwrt/usque/` | OpenWrt package: binary, procd init, UCI config, routing/firewall hooks |
| `openwrt/luci-app-usque/` | LuCI JS view + rpcd backend (status, registration, diagnostics) |
| `scripts/` | feed assembly/signing |
| `docs/` | static GitHub Pages site |
| `tests/` | live-router verification scripts and results |

Cross-compilation and package builds run in GitHub Actions (no local OpenWrt
toolchain required). Target platform: `ipq40xx/generic`, `arm_cortex-a7_neon-vfpv4`
(32-bit ARM musl).

## License

MIT, see [LICENSE.md](LICENSE.md). Not affiliated with Cloudflare.
