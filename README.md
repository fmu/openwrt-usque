# usque for OpenWrt

Cloudflare WARP (MASQUE / Connect-IP) tunnel daemon for OpenWrt 25.12, with a LuCI
configuration interface. Built from [FireMasterK/usque-rs](https://github.com/FireMasterK/usque-rs),
TUN mode only, cross-compiled for `arm_cortex-a7_neon-vfpv4`.

The installable APK feed and instructions live at <https://fmu.github.io/openwrt-usque/>.

- `src/` – vendored + patched usque-rs source
- `openwrt/` – OpenWrt packages (`usque`, `luci-app-usque`)
- `build/` – cross-compile and packaging scripts used by CI
- `docs/` – GitHub Pages site

