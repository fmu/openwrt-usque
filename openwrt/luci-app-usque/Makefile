#
# Copyright (C) 2026 OpenWrt usque packaging contributors
#
# SPDX-License-Identifier: MIT
#
# Plain OpenWrt package: intentionally does NOT include ../../luci.mk so it
# builds with the stock OpenWrt SDK, without adding the LuCI feed.
#

include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-usque
PKG_VERSION:=1.0.0
PKG_RELEASE:=1

PKG_LICENSE:=MIT
PKG_MAINTAINER:=usque packaging contributors

include $(INCLUDE_DIR)/package.mk

define Package/luci-app-usque
  SECTION:=luci
  CATEGORY:=LuCI
  SUBMENU:=3. Applications
  TITLE:=LuCI support for usque (Cloudflare WARP)
  PKGARCH:=all
  DEPENDS:=+luci-base +usque
endef

define Package/luci-app-usque/description
 Web interface for the usque Cloudflare WARP (MASQUE) client: one-click WARP
 account registration, service control, status and connection diagnostics.
endef

define Build/Compile
endef

define Package/luci-app-usque/install
	$(INSTALL_DIR) $(1)/www/luci-static/resources/view/usque
	$(INSTALL_DATA) ./htdocs/luci-static/resources/view/usque/overview.js \
		$(1)/www/luci-static/resources/view/usque/overview.js

	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d
	$(INSTALL_DATA) ./root/usr/share/luci/menu.d/luci-app-usque.json \
		$(1)/usr/share/luci/menu.d/luci-app-usque.json

	$(INSTALL_DIR) $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) ./root/usr/share/rpcd/acl.d/luci-app-usque.json \
		$(1)/usr/share/rpcd/acl.d/luci-app-usque.json

	$(INSTALL_DIR) $(1)/usr/libexec/rpcd
	$(INSTALL_BIN) ./root/usr/libexec/rpcd/luci.usque \
		$(1)/usr/libexec/rpcd/luci.usque
endef

define Package/luci-app-usque/postinst
[ -n "$${IPKG_INSTROOT}" ] || { \
	rm -f /tmp/luci-indexcache.*; \
	rm -rf /tmp/luci-modulecache/; \
	/etc/init.d/rpcd reload 2>/dev/null; \
	exit 0; \
}
endef

define Package/luci-app-usque/postrm
[ -n "$${IPKG_INSTROOT}" ] || { \
	rm -f /tmp/luci-indexcache.*; \
	rm -rf /tmp/luci-modulecache/; \
	/etc/init.d/rpcd reload 2>/dev/null; \
	exit 0; \
}
endef

$(eval $(call BuildPackage,luci-app-usque))
