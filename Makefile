#
# Copyright (C) 2026 gooyjq
#
# 本软件为自由软件，遵循 GNU 通用公共许可证第 2 版。
#

include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-online-upgrade
PKG_VERSION:=2.4.1
PKG_RELEASE:=1

PKG_MAINTAINER:=gooyjq <gooyjq@users.noreply.github.com>
PKG_LICENSE:=GPL-2.0-only
PKG_LICENSE_FILES:=LICENSE

# Rust 后端自带 HTTP 栈（reqwest/rustls），无需依赖 curl。
# 打包前必须已存在预编译二进制 ./root/usr/bin/online-upgrade
# （参见 README 的“编译”一节：可用 cargo zigbuild、交叉 gcc 或 CI 构建）。
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

# Rust 后端由 CI 的 rust 作业使用 rustup/cargo 交叉编译后放到
# ./root/usr/bin/online-upgrade；OpenWrt SDK 只负责 LuCI 文件、
# 包结构与 .apk 生成，因此这里只校验预编译产物，不在 SDK 内编译 Rust。
define Build/Prepare
	@if [ ! -x ./root/usr/bin/online-upgrade ]; then \
		echo "ERROR: 缺少预编译的 Rust 二进制 ./root/usr/bin/online-upgrade"; \
		echo "       请先按 README 的交叉编译步骤生成，或使用 CI 的 rust 作业产物。"; \
		exit 1; \
	fi
endef

define Build/Configure
endef

# 刻意为空：Rust 已在 CI/本地用 cargo 交叉编译，SDK 内不再重复编译。
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
	# ucode 版 LuCI（26.x）：不再提供 /lib/functions/luci.sh 和 luci-reload，
	# 菜单或 ACL 变更后只需让调度器缓存失效即可。
	rm -f /tmp/luci-indexcache*
	exit 0
}
endef

$(eval $(call BuildPackage,luci-app-online-upgrade))
