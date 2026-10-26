# SPDX-License-Identifier: MIT
#
# usque - Cloudflare WARP (MASQUE / Connect-IP) tunnel client for OpenWrt
#
# The ARMv7 static binary is *not* built by this Makefile.  It is produced by
# build/build-usque.sh (in CI) and must be copied to $(CURDIR)/bin/usque
# before this package is compiled:
#
#     ./build/build-usque.sh
#     cp dist/usque openwrt/usque/bin/usque
#
# See README.md in this directory.

include $(TOPDIR)/rules.mk

PKG_NAME:=usque
PKG_VERSION:=0.1.0
PKG_RELEASE:=1

PKG_LICENSE:=MIT
PKG_LICENSE_FILES:=LICENSE.md
PKG_MAINTAINER:=fmu
# Target: OpenWrt 25.12 ipq40xx/generic (arm_cortex-a7_neon-vfpv4).
PKGARCH:=arm_cortex-a7_neon-vfpv4

include $(INCLUDE_DIR)/package.mk

# Path to the cross-compiled static binary.  Override on the make command line
# (e.g. `make package/usque/compile USQUE_BIN=/path/to/usque`).
USQUE_BIN?=$(CURDIR)/bin/usque

define Package/usque
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=VPN
  TITLE:=Cloudflare WARP (MASQUE/Connect-IP) tunnel client
  URL:=https://github.com/fmu/openwrt-usque
  DEPENDS:=+kmod-tun
endef

define Package/usque/description
 usque is a Cloudflare WARP client implementing the MASQUE/Connect-IP
 protocol (TUN mode only).  While connected it installs a default route
 through the WARP interface (metric 0) plus an endpoint bypass route via
 the current WAN, keeping the WAN default route as a fallback.  A dedicated
 firewall zone provides masquerading and MSS clamping.  Static ARMv7 build;
 no runtime dependencies besides kmod-tun.
endef

define Package/usque/conffiles
/etc/config/usque
endef

define Build/Compile
	@[ -f "$(USQUE_BIN)" ] || { \
		echo "ERROR: usque binary not found at $(USQUE_BIN)"; \
		echo "       run build/build-usque.sh, then copy dist/usque to openwrt/usque/bin/usque"; \
		exit 1; \
	}
	@magic="$$(od -An -tx1 -N4 "$(USQUE_BIN)" 2>/dev/null | tr -d ' \n')"; \
	if [ "$$magic" != "7f454c46" ]; then \
		echo "ERROR: $(USQUE_BIN) is not an ELF binary (magic=$$magic)"; \
		exit 1; \
	fi
	@echo "usque binary: $$(stat -c '%s' "$(USQUE_BIN)" 2>/dev/null || stat -f '%z' "$(USQUE_BIN)") bytes"

define Package/usque/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(USQUE_BIN) $(1)/usr/bin/usque
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/etc/init.d/usque $(1)/etc/init.d/usque
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_DATA) ./files/etc/config/usque $(1)/etc/config/usque
	$(INSTALL_DIR) $(1)/usr/libexec/usque
	$(INSTALL_BIN) ./files/usr/libexec/usque/hook $(1)/usr/libexec/usque/hook
	$(INSTALL_BIN) ./files/usr/libexec/usque/firewall $(1)/usr/libexec/usque/firewall
endef

define Package/usque/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
mkdir -p /etc/usque
chmod 700 /etc/usque 2>/dev/null
logger -t usque "installed: configure /etc/config/usque, then '/etc/init.d/usque enable && /etc/init.d/usque start'"
if /etc/init.d/usque enabled 2>/dev/null; then
	/etc/init.d/usque restart >/dev/null 2>&1 || true
fi
exit 0
endef

define Package/usque/prerm
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
/etc/init.d/usque stop >/dev/null 2>&1 || true
/etc/init.d/usque disable >/dev/null 2>&1 || true
if [ -x /usr/libexec/usque/firewall ]; then
	/usr/libexec/usque/firewall teardown >/dev/null 2>&1 || true
fi
exit 0
endef

define Package/usque/postrm
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
# Belt and braces: the zone helper may already be gone if prerm failed.
if uci -q get firewall.usque >/dev/null 2>&1; then
	uci -q delete firewall.usque_lan
	uci -q delete firewall.usque
	uci commit firewall
	/sbin/fw4 reload >/dev/null 2>&1 || true
fi
rm -f /var/run/usque/wan-default-v4 /var/run/usque/wan-default-v6 /var/run/usque/endpoints
exit 0
endef

$(eval $(call BuildPackage,usque))
