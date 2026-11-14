# 固件云编译预设配置向导（Firmware Builder Preset Guide）

> 适用版本：**luci-app-online-upgrade 2.6.7**
> 本文是编译期集成指南，不是 LuCI 界面里的独立向导。目标是把仓库地址、固件匹配规则等预设写进固件，刷入后打开页面即可直接「检查更新」。

---

## 0. 先明确边界

| 能做 | 不能做 |
| :--- | :--- |
| 在固件编译时预置 `/etc/config/online-upgrade` | 在 LuCI 里提供“一键生成预设”的界面向导 |
| 首次开机用 `uci-defaults` 动态写入配置 | 通过预设自动刷写固件 |
| 让页面「高级配置」回填预设值 | 绕过 SHA256 / `sysupgrade -T` 校验 |

运行时的等价入口是「在线升级」页面里的「高级配置」折叠区；预设只是把同样的值提前写进固件。

---

## 1. 生效链路

```text
编译期写入 /etc/config/online-upgrade
        │（若用 uci-defaults：首次开机执行一次，成功后脚本自删）
        ▼
插件读取 online-upgrade.settings
        ▼
页面「高级配置」回填 → 「检查更新」按预设仓库/规则检测
```

配置文件固定为 `/etc/config/online-upgrade`，section 名必须是 `settings`：

```uci
config online-upgrade 'settings'
```

---

## 2. 方案一：`files/` 目录直接覆盖（推荐）

OpenWrt 编译系统会把源码根目录下的 `files/` 树原样打包进固件，最轻量、无脚本。

### 2.1 建立目录

```bash
mkdir -p files/etc/config
```

### 2.2 写入配置

新建 `files/etc/config/online-upgrade`：

```uci
config online-upgrade 'settings'
	option repo 'your-username/your-firmware-repo'
	option tag 'latest'
	option firmware_pattern 'H5000M-.*-sysupgrade\.bin'
	option proxy 'https://gh.acg2.mom/'
	option keep_config '1'
	option min_size_mb '15'
	option dl_workers '6'
	option dl_chunk_mb '4'
	option dl_stall_sec '30'
	option dl_retries '3'
	option dl_min_speed_kb '32'
	option dl_slow_window_sec '15'
	option dl_slow_grace_sec '10'
	option api_via_proxy '0'
	option api_timeout '25'
	option ttl_actions '120'
	option ttl_releases '300'
	option ttl_artifacts '180'
	option ttl_compat '600'
	option ttl_network '60'
```

### 2.3 字段含义

| 键 | 默认 | 说明 |
| :--- | :--- | :--- |
| `repo` | 空 | `owner/repo`，例如 `LianXia233/OpenWRT-CI` |
| `tag` | `latest` | `latest`、`latest@^前缀` 或具体 tag |
| `firmware_pattern` | 空 | 固件文件名 ERE 正则，反斜杠写成 `\.` |
| `proxy` | `https://gh.acg2.mom/` | 下载加速前缀，留空直连 |
| `keep_config` | `1` | `1` 升级保留配置（`sysupgrade -f`），`0` 全新刷入 |
| `min_size_mb` | `15` | 过滤小于该体积的辅助文件，`0` 不限制 |
| `dl_workers` / `dl_chunk_mb` | `6` / `4` | 并发线程与分块大小 |
| `dl_stall_sec` / `dl_retries` | `30` / `3` | 分块停滞超时与重试次数 |
| `dl_min_speed_kb` / `dl_slow_window_sec` / `dl_slow_grace_sec` | `32` / `15` / `10` | 慢速保护阈值、窗口与启动宽限 |
| `api_via_proxy` | `0` | API 是否也走加速源（多数镜像不支持 `api.github.com`） |
| `api_timeout` | `25` | API 请求超时（秒），范围 5–120 |
| `ttl_actions` / `ttl_releases` / `ttl_artifacts` / `ttl_compat` / `ttl_network` | `120` / `300` / `180` / `600` / `60` | 各状态模型缓存 TTL（秒），范围 15–86400 |

> `enabled` 是内部开关（默认 `0`），界面不提供修改入口，**不要**写进预设，也不要当作“启用插件”的开关。

### 2.4 落地要求

- 固件内路径：`/etc/config/online-upgrade`
- 权限：`644 root:root`
- 编码：UTF-8；行尾：LF
- 文件由镜像构建打包，刷入后即为最终配置

---

## 3. 方案二：`uci-defaults` 首次开机脚本（适合多机型）

一套固件适配多种板型时，可按 `board_name` 动态生成规则。

### 3.1 建立脚本

```bash
mkdir -p files/etc/uci-defaults
cat > files/etc/uci-defaults/99-default-online-upgrade <<'EOF'
#!/bin/sh
BOARD_NAME="$(cat /tmp/sysinfo/board_name 2>/dev/null)"

case "$BOARD_NAME" in
    *h5000m*) PAT='H5000M-.*-sysupgrade\.bin' ;;
    *x86*)    PAT='.*-x86-64-.*-combined-efi\.img\.gz' ;;
    *)        PAT='immortalwrt-.*-sysupgrade\.bin' ;;
esac

uci -q batch <<EOC
set online-upgrade.settings.repo='your-username/your-firmware-repo'
set online-upgrade.settings.tag='latest'
set online-upgrade.settings.firmware_pattern='$PAT'
set online-upgrade.settings.proxy='https://gh.acg2.mom/'
set online-upgrade.settings.keep_config='1'
commit online-upgrade
EOC
exit 0
EOF
chmod 755 files/etc/uci-defaults/99-default-online-upgrade
```

### 3.2 要点

- section 必须是 `settings`（`online-upgrade.settings.xxx`），不是 `main`。
- 脚本在首次开机运行一次，成功后系统自动删除。
- 必须 `755` 权限、LF 行尾，否则 BusyBox ash 可能拒绝执行。
- 配置最终仍落在 `/etc/config/online-upgrade`。

---

## 4. 方案三：GitHub Actions 云编译注入

在云编译模板的 `diy-part1.sh` / `diy-part2.sh` 中追加。不同模板的根目录不同：P3TERX 模板用 `files/`，部分模板用 `package/base-files/files/`，按实际模板选择。

```bash
mkdir -p files/etc/uci-defaults
cat > files/etc/uci-defaults/99-default-online-upgrade <<'EOF'
#!/bin/sh
uci -q batch <<EOC
set online-upgrade.settings.repo='your-username/your-firmware-repo'
set online-upgrade.settings.tag='latest'
set online-upgrade.settings.firmware_pattern='immortalwrt-.*-sysupgrade\.bin'
set online-upgrade.settings.proxy='https://gh.acg2.mom/'
set online-upgrade.settings.keep_config='1'
commit online-upgrade
EOC
exit 0
EOF
chmod 755 files/etc/uci-defaults/99-default-online-upgrade
```

---

## 5. 方案四：把插件加入编译源码树

### 5.1 添加 feed

在 `feeds.conf.default` 追加：

```text
src-git onlineupgrade https://github.com/LianXia233/luci-app-online-upgrade.git;main
```

### 5.2 更新并安装

```bash
./scripts/feeds update onlineupgrade
./scripts/feeds install -a -p onlineupgrade
```

### 5.3 选中插件

`make menuconfig` → `LuCI` → `3. Applications` → `<*>` `luci-app-online-upgrade`。

云编译可直接写 `.config`：

```bash
echo "CONFIG_PACKAGE_luci-app-online-upgrade=y" >> .config
```

---

## 6. 刷入后验证

```bash
uci show online-upgrade | grep -E 'repo|tag|firmware_pattern|keep_config|api_|ttl_'
ls -l /etc/config/online-upgrade                       # 应为 644 root:root
ls /etc/uci-defaults/99-default-online-upgrade 2>/dev/null   # 已执行则不存在
```

页面「高级配置」应看到同样的值；点击「检查更新」应直接命中预设仓库与规则。若为空，先检查文件路径与 section 名。

---

## 7. 常见坑

| 现象 | 原因 | 处理 |
| :--- | :--- | :--- |
| 页面配置为空 | 文件路径不是 `/etc/config/online-upgrade` | 确认 `files/etc/config/` 层级 |
| `uci show` 报错 | section 写成了 `main` | 改为 `online-upgrade.settings.*` |
| 规则匹配不到固件 | 正则未转义 | `sysupgrade.bin` 写成 `sysupgrade\.bin` |
| uci-defaults 没生效 | 脚本无执行权限或非首次启动 | `chmod 755`；重新刷机或手动执行 |
| 升级后配置丢了 | `keep_config='0'` | 改回 `1` |
| API 一直失败 | `api_via_proxy='1'` 但镜像不支持 API | 改回 `0` 直连 |

---

## 8. 复制即用模板

### 8.1 `files/etc/config/online-upgrade`

见 2.2 节；把 `repo` 与 `firmware_pattern` 替换为你的仓库和文件名即可。

### 8.2 `files/etc/uci-defaults/99-default-online-upgrade`

见 3.1 节；适合多机型或需要按板型切换正则的场景。

---

## 9. 与界面/文档的关系

- 本文件是 README「使用向导 → 步骤五」的独立版，内容与 2.6.7 代码一致。
- 界面行为、任务清理规则见 README「使用向导」与 FAQ。
- 字段默认值以 `/etc/config/online-upgrade` 与 `luci.online-upgrade` 的 `set_config` 校验范围为准。
