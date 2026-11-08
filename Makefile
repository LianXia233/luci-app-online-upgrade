#
# Copyright (C) 2026 gooyjq
#
# This is free software, licensed under the GNU General Public License v2.
#

include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-online-upgrade
PKG_VERSION:=2.3.1
PKG_RELEASE:=1

PKG_MAINTAINER:=gooyjq <gooyjq@users.noreply.github.com>
PKG_LICENSE:=GPL-2.0-only
PKG_LICENSE_FILES:=LICENSE

# Rust backend ships its own HTTP stack (reqwest/rustls) — no curl needed.
# The prebuilt binary must exist at ./root/usr/bin/online-upgrade before
# packaging (see README "编译" — build via cargo zigbuild / cross-gcc, or CI).
# ca-bundle: uci-defaults 的自动重装脚本用 curl 走 HTTPS 拉取 release，
# 没有 CA 根证书时 curl 会直接因证书校验失败而无法下载。
LUCI_DEPENDS:=+rpcd-mod-ucode +ucode-mod-fs +ucode-mod-uci +ca-bundle

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/luci/luci.mk

define Package/luci-app-online-upgrade
	SECTION:=luci
	CATEGORY:=LuCI
	SUBMENU:=3. Applications
	TITLE:=Firmware online upgrade from GitHub (Rust backend)
	DEPENDS:=$(LUCI_DEPENDS)
endef

define Package/luci-app-online-upgrade/description
	Automatically check and upgrade ImmortalWrt/OpenWrt firmware
	from GitHub Releases. Tokio-powered segmented downloader,
	ucode rpcd backend, native JS/CSS frontend.
endef

define Build/Prepare
endef

define Build/Configure
endef

define Build/Compile
endef

define Package/luci-app-online-upgrade/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) ./root/usr/bin/online-upgrade $(1)/usr/bin/online-upgrade

	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) ./root/etc/config/online-upgrade $(1)/etc/config/online-upgrade

	$(INSTALL_DIR) $(1)/lib/upgrade/keep.d
	$(INSTALL_DATA) ./root/lib/upgrade/keep.d/online-upgrade $(1)/lib/upgrade/keep.d/online-upgrade

	$(INSTALL_DIR) $(1)/etc/uci-defaults
	$(INSTALL_BIN) ./root/etc/uci-defaults/99-online-upgrade $(1)/etc/uci-defaults/99-online-upgrade
	$(INSTALL_BIN) ./root/etc/uci-defaults/90-online-upgrade-auto $(1)/etc/uci-defaults/90-online-upgrade-auto

	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d
	$(INSTALL_DATA) ./root/usr/share/luci/menu.d/luci-app-online-upgrade.json \
		$(1)/usr/share/luci/menu.d/luci-app-online-upgrade.json

	$(INSTALL_DIR) $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) ./root/usr/share/rpcd/acl.d/luci-app-online-upgrade.json \
		$(1)/usr/share/rpcd/acl.d/luci-app-online-upgrade.json

	$(INSTALL_DIR) $(1)/usr/share/rpcd/ucode
	$(INSTALL_DATA) ./root/usr/share/rpcd/ucode/luci.online-upgrade \
		$(1)/usr/share/rpcd/ucode/luci.online-upgrade

	$(INSTALL_DIR) $(1)/usr/share/ucode/luci/template/online-upgrade
	$(INSTALL_DATA) ./root/usr/share/ucode/luci/template/online-upgrade/main.ut \
		$(1)/usr/share/ucode/luci/template/online-upgrade/main.ut

	$(INSTALL_DIR) $(1)/www/luci-static/resources/online-upgrade
	$(INSTALL_DATA) ./root/www/luci-static/resources/online-upgrade/app.js \
		$(1)/www/luci-static/resources/online-upgrade/app.js
	$(INSTALL_DATA) ./root/www/luci-static/resources/online-upgrade/style.css \
		$(1)/www/luci-static/resources/online-upgrade/style.css
endef

define Package/luci-app-online-upgrade/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] || {
	# ucode LuCI (26.x): no /lib/functions/luci.sh, no luci-reload —
	# invalidating the dispatcher cache is all a menu/acl change needs
	rm -f /tmp/luci-indexcache*
	exit 0
}
endef

$(eval $(call BuildPackage,luci-app-online-upgrade))
