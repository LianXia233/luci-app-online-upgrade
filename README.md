<div align="center">

# 🚀 luci-app-online-upgrade

**适用于 ImmortalWrt / OpenWrt 的下一代现代化 GitHub 在线固件升级插件**

[![License: GPL v2](https://img.shields.io/badge/License-GPL%20v2-blue.svg)](LICENSE)
[![LuCI Version](https://img.shields.io/badge/LuCI-25.12%2B-0052cc.svg)](https://github.com/openwrt/luci)
[![OpenWrt/ImmortalWrt](https://img.shields.io/badge/OpenWrt%20%2F%20ImmortalWrt-25.x%2B-brightgreen.svg)](https://immortalwrt.org/)
[![Rust Tokio](https://img.shields.io/badge/Rust-Tokio%20Async-orange.svg?logo=rust)](https://www.rust-lang.org/)
[![UI Style](https://img.shields.io/badge/UI-Modern%20Pure%20White-6366f1.svg)](#-界面美学与设计)
[![Architecture](https://img.shields.io/badge/Arch-aarch64%20%7C%20x86__64-success.svg)](#-支持架构)

<p align="center">
  <b>极简纯白现代美学 · 原生 SVG 科技矢量插画 · 全端响应式适配 · Rust Tokio 多线程分段加速 · 零 Lua 轻量架构</b>
</p>

<p align="center">
  <img src="screenshot.png" alt="luci-app-online-upgrade 界面预览" width="880" style="border-radius: 8px; box-shadow: 0 4px 20px rgba(0,0,0,0.08);">
</p>

</div>

---

## 📖 简介

`luci-app-online-upgrade` 是专为 ImmortalWrt 与 OpenWrt 路由器打造的在线固件检测与一键升级插件。

本插件为 **v2 全面重构版**，彻底告别传统庞杂臃肿的 Lua 脚本与外部 curl 进程调用，采用：
- **前端**：原生纯净 JavaScript + 现代 CSS3 响应式布局 + 原生 SVG 科技插画，无任何第三方重型前端框架依赖。
- **中间层**：OpenWrt 官方现代标准的 `rpcd` ucode 插件桥接，轻量高效，原生融入 LuCI 会话鉴权体系。
- **后端内核**：基于 **Rust (Tokio 异步运行时)** 编写的静态 Musl 二进制程序，内置自研 **FluxDown 并发分段下载引擎**，支持多流并发提速、断点续传、智能重试与自动故障转移。

无论是在电脑大屏还是手机移动端，均可获得流畅、优雅且极速的固件升级与配置容灾体验。

---

## 🌟 核心特性

### 🎨 1. 极简纯白现代视觉设计
- **卡片式轻拟物层次**：采用清爽纯白（`#ffffff`）与柔和中性灰背景（`#f8fafc`），配合细腻高光与微阴影，摆脱传统路由界面的粗糙质感。
- **原生 SVG 矢量科技插图**：精心手绘 Hero 头部交互插图（智能路由中枢、云端固件通道、双向高速传输管道、安全防护盾牌）与全套功能状态图标，全分辨率清晰锐利。
- **四阶段升级流水线指示**：直观展示 `检测固件 → 准备就绪 → 极速下载 → 刷写重启` 四步流水线状态，每一步骤进度实时脉冲呈现。

### 📱 2. 全端全场景响应式适配
- **手机触屏深度优化**：针对移动端屏幕尺寸，自动重构为单列大触摸卡片，操作按钮智能扩宽为易触控的弹性网格，彻底解决移动端横向溢出或操作别扭问题。
- **无截断文本排版**：采用专属全宽卡片展示「检测依据与规则」，固件正则规则、架构匹配、云端 Release 直达链接等关键信息多行自适应折行完整呈现，绝不模糊省略。

### 🔍 3. 智能架构与机型识别（防变砖安全决策引擎）
- **自动探查设备硬件特征**：全自动识别路由器当前硬件架构（如 `aarch64`、`x86_64`、`mips` 等）、芯片平台（如 `mediatek/filogic`、`rockchip` 等）以及设备具体板型（如 `Hiveton H5000M`、`H3C Magic NX30 Pro` 等）。
- **云端固件兼容性智能评估（该不该刷）**：全自动分析云端 Release 各固件产物，多维度交叉比对硬件架构与设备签名。若检测到架构或机型冲突（例如 ARM64 设备误选 x86_64 固件，或 H5000M 设备误选 NX30 Pro 固件），**系统将自动判定为「不兼容」并强制阻断升级**，从源头彻底杜绝刷错固件变砖的悲剧。
- **固件列表智能打标与置顶推荐**：探查固件列表时，自动将完美适配当前机型的固件置顶并标注 🟢 `[推荐]`，对不匹配的固件标注 🔴 `[不匹配]`，即便未配置固件匹配正则也能全自动智能匹配。
- **底层内核级预检 (`sysupgrade -T`) 双重安全屏障**：在固件下载完成后，插件不仅执行 SHA-256 完整性校验，更会调用系统底层 `sysupgrade -T` 及 `fwtool` 针对固件包头、分区表与硬件签名做二次无损预检，预检不通过坚决不刷入。

### 📦 4. 智能仓库识别与固件自动过滤
- **一键智能解析**：支持直接粘贴 GitHub Release 页面链接、仓库主页链接或简单的 `owner/repo`，全自动提取仓库与标签。
- **智能过滤小文件（≥15MB）**：点击「获取固件列表」时，自动探查目标 Release 附件产物，**全自动剔除小于 15MB 的非固件文件**（如 `.txt` 配置单、`.manifest` 清单、`.sha256` 校验和），仅保留真正可刷写的固件包供用户点选，一键自动生成固件匹配正则。
- **版本依据双重核验**：结合固件文件名版本号与 Release 发布时间戳双重判定，智能识别增量更新与升级方向，杜绝误判降级。

### ⚡ 5. FluxDown 架构多线程分段加速下载
- **动态切片与 Work-Stealing**：自动探测 HTTP Range 支持，按 4MB 动态切片并分派给多达 6 个 Tokio 异步 Worker 并发拉取，快连接自动偷取慢连接分块，拉满带宽潜能。
- **断点续传与故障恢复**：内置 `.parts.json` 侧车机制，意外断网无缝续传；支持块级独立重试与 30 秒停滞看门狗（Stall Watchdog），遇不支持分段的目标自动平滑降级单流传输。
- **内置常用镜像代理**：预置快捷镜像节点（`gh.acg2.mom`、`ghfast.top`、`moeyy.xyz` 等），一键切换 GitHub 加速或直连模式。

### 🛡️ 6. 固件哈希校验与配置容灾安全升级
- **固件 SHA-256 完整性校验**：自动探查 Release 产物中的校验单（`sha256sums` / `sha256sums.txt` / `<firmware>.sha256` 等）或 Release 页面说明提取 SHA256 哈希值进行比对校验；亦支持在高级配置中自定义期望哈希。下载后自动核验，若哈希不符立即终止刷写并清理固件，确保绝不刷入损坏镜像。
- **刷前自动全备份**：每次启动刷写前，全自动为路由器当前配置创建压缩归档存于 `/root/`。
- **三维恢复通道**：在 Web 界面即可随时「下载备份包到电脑」、「一键自动恢复现有备份」或「手动上传历史备份包恢复」。
- **升级自举无损保留**：与 `sysupgrade -f` 及 `keep.d` 深度集成，升级完成后自动触发 `uci-defaults` 恢复缺失配置，安全可靠。

### 📊 7. 真实进度条与实时控制台终端
- **真实字节级进度**：精准展示下载百分比、实时下载速度、已下体积与总大小，非虚假动画估算。
- **内嵌实时日志控制台**：支持折叠/展开的黑客风格暗色终端，实时捕获输出升级日志，排查一目了然。

---

## 🏗️ 架构拓扑

```
┌────────────────────────────────────────────────────────┐
│                   Web 浏览器 UI 客户端                  │
│       原生 JS (app.js) + 现代响应式 CSS (style.css)      │
│     SVG 矢量图标 · 4阶流水线 · 纯白卡片 · 移动端/桌面适配    │
└───────────────────────────┬────────────────────────────┘
                            │ fetch /ubus/ (JSON-RPC)
                            │ 带 LuCI Web 会话鉴权
                            ▼
┌────────────────────────────────────────────────────────┐
│            rpcd ucode 桥接服务 (luci.online-upgrade)    │
│    /usr/share/rpcd/ucode/luci.online-upgrade           │
│    API: get_status, get_sysinfo, get_config, probe...  │
└───────────────────────────┬────────────────────────────┘
                            │ popen / setsid
                            ▼
┌────────────────────────────────────────────────────────┐
│             Rust 静态二进制核心 (/usr/bin/online-upgrade) │
│           Tokio 异步引擎 · reqwest + rustls (静态 musl) │
├────────────────────────────────────────────────────────┤
│  ├─ probe    探测 GitHub Release 产物附件列表与体积     │
│  ├─ check    双重比对版本与构建时间，解析匹配目标固件    │
│  ├─ upgrade  FluxDown 6线程并发分段下载 → 校验 → 刷机   │
│  ├─ backup   升级前全自动配置打包 (/root/backup-*.tar.gz)│
│  └─ restore  自动/手动恢复备份配置                     │
└──────────────┬──────────────────────────┬──────────────┘
               │ 写入状态 JSON             │ 执行升级
               ▼                          ▼
   /tmp/online-upgrade-status.json   sysupgrade -f (保留配置刷机)
```

---

## 🧭 使用向导

### 步骤一：填入或识别仓库
1. 进入 LuCI 菜单：**系统** → **在线升级**。
2. 在「**1. 固件仓库来源**」中直接粘贴您的 GitHub Release 网址、仓库网址或 `owner/repo`（如 `LianXia233/OpenWRT-CI`），点击「**识别**」。系统将自动解析仓库名并设定 Release 追踪规则。

### 步骤二：选择目标固件文件（自动过滤小文件）
1. 点击「**获取固件列表**」。
2. 系统会自动请求该仓库最新的 Release 产物附件，**并自动过滤掉所有小于 15MB 的配置文件和辅助文本**。
3. 从下拉菜单中点选您设备对应的固件型号（如 `xxx-sysupgrade.bin`），点击「**使用此文件 → 生成规则**」，插件将自动生成精准的匹配规则。

### 步骤三：选择加速代理并保存
1. 在「**3. 下载加速代理**」中，可直接点击常用推荐按钮（如 `gh.acg2.mom`、`ghfast.top` 等）快速填入代理，国内网络环境强烈推荐启用。
2. 点击「**保存配置**」，此时当前配置已被持久化至路由器的 UCI 配置文件中。

### 步骤四：一键检测与极速升级
1. 点击「**检查更新**」按钮，系统开始向 GitHub 查询最新版本。
2. 查看「**检测依据与规则**」全宽卡片，确认目标版本、发布日期与本地固件比对信息。
3. 点击「**一键在线升级**」：
   - 系统自动为当前路由器的网络、密码、插件等配置创建备份压缩包；
   - 启动多线程分段极速下载，在页面可实时观测下载速度与进度条；
   - 下载校验完成后自动调用 `sysupgrade -f` 刷写固件并重启路由器。

---

## 📦 安装与部署

### 支持架构
- **aarch64**（如 MediaTek Filogic MT798x / RK3568 / RK3588 等）
- **x86_64**（软路由、工控机、虚拟机）

### 方式 A：通过 Release 安装包（推荐）
从本仓库的 [Releases 页面](https://github.com/LianXia233/luci-app-online-upgrade/releases) 下载对应架构的最新 `.apk` 安装包，上传至路由器并执行：

```bash
# 适用于 ImmortalWrt 25.12+ / OpenWrt 25+ (apk 包管理器)
apk add --allow-untrusted luci-app-online-upgrade_*.apk
```

### 方式 B：从源码编译打包

#### 1. 编译 Rust 后端内核（宿主机交叉编译，无需 OpenWrt 全量环境）
```bash
# 安装 cargo-zigbuild 工具链
cargo install cargo-zigbuild

# 进入 Rust 源码目录
cd rust

# 交叉编译 aarch64 静态二进制
cargo zigbuild --release --target aarch64-unknown-linux-musl
cp target/aarch64-unknown-linux-musl/release/online-upgrade ../root/usr/bin/online-upgrade

# 或交叉编译 x86_64 静态二进制
cargo zigbuild --release --target x86_64-unknown-linux-musl
cp target/x86_64-unknown-linux-musl/release/online-upgrade ../root/usr/bin/online-upgrade
```

#### 2. OpenWrt / ImmortalWrt 源码树打包
将仓库放入 SDK 或源码树的 `package/` 目录：
```bash
cd openwrt
make package/luci-app-online-upgrade/compile V=s
```

---

## ⚙️ UCI 配置详解与参数说明

插件的核心配置文件存储于路由器的 `/etc/config/online-upgrade`。支持通过 Web 界面图形化配置，亦可直接通过 SSH 命令行或 UCI 指令修改。

### 1. 配置项参考表

| 配置项 | 类型 | 默认值 | 说明与示例 |
| :--- | :--- | :--- | :--- |
| `enabled` | bool | `0` | 是否启用在线升级配置：`1` 为启用，`0` 为禁用 |
| `repo` | string | 空 | GitHub 仓库路径，格式为 `owner/repo`（如 `LianXia233/OpenWRT-CI`） |
| `tag` | string | `latest` | Release 追踪策略：<br>• `latest`：追踪最新发布的 Release；<br>• `latest@^前缀`：按前缀过滤目标机型最新 Release（如 `latest@^H5000M`）；<br>• 具体 Tag 名（如 `v2026.11`） |
| `firmware_pattern` | string | 空 | 目标固件文件名匹配的正则表达式（如 `immortalwrt-.*-sysupgrade\.bin`） |
| `proxy` | string | `https://gh.acg2.mom/` | GitHub 文件与 API 加速反代节点，留空则直连 GitHub |
| `min_size_mb` | integer | `15` | **最小固件体积阈值 (MB)**：获取列表与检测匹配时自动忽略小于此体积的文件（过滤 metadata/txt/sha256），设为 `0` 则不限制 |
| `expected_sha256` | string | 空 | 固件 SHA-256 校验和（64 位十六进制）：<br>• **留空（推荐）**：自动探查 Release 附件 `sha256sums` / `sha256sums.txt` / `<固件>.sha256` 或 Release 说明正文中的哈希；<br>• **手动填写**：强制使用该 SHA-256 值校验，不一致则自动阻断刷写 |
| `keep_config` | bool | `1` | 升级时是否调用 `sysupgrade -f` 保留现有配置（`1`=保留配置，`0`=完全全新刷入） |
| `dl_workers` | integer | `6` | FluxDown 并发下载线程数（范围 1~16，建议 4~8；不支持 Range 的节点自动平滑降级为单线程） |
| `dl_chunk_mb` | integer | `4` | 动态分段切片大小（MB），默认 4MB |
| `dl_stall_sec` | integer | `30` | 分块传输停滞超时看门狗阈值（秒），分片停滞时自动重调度与故障转移 |
| `dl_retries` | integer | `3` | 单个分块下载失败后允许的最大重试次数 |
| `github_token` | string | 空 | 可选。GitHub Personal Access Token（只读权限即可），用于防止局域网内公网 IP 触发 GitHub API 403 限流 |

### 2. 标准 UCI 配置文件示例 (`/etc/config/online-upgrade`)

```uci
config online-upgrade 'main'
	option enabled '1'
	option repo 'your-username/your-firmware-repo'
	option tag 'latest'
	option firmware_pattern 'immortalwrt-.*-sysupgrade\\.bin'
	option proxy 'https://gh.acg2.mom/'
	option keep_config '1'
	option min_size_mb '15'
	option dl_workers '6'
	option dl_chunk_mb '4'
	option dl_stall_sec '30'
	option dl_retries '3'
```

---

## 🛠️ 固件云编译预设配置向导 (Firmware Builder Preset Guide)

在自编译 OpenWrt / ImmortalWrt 固件时，将本插件编译进固件并**预设好您的 GitHub 仓库与机型规则**，即可实现固件刷入后**开箱即用**：用户无需手动输入仓库地址，进入后台点击「检查更新」即可一键在线平滑升级。

### 方案一：`files/` 目录直接覆盖（推荐，最轻量纯净）

OpenWrt 编译系统原生支持 `files` 目录树自动覆盖打包进根文件系统，这是最规范、无需执行脚本的方法。

1. 在固件源码根目录下创建配置目录：
   ```bash
   mkdir -p files/etc/config
   ```
2. 创建 `files/etc/config/online-upgrade` 文件并填入您的预设参数：
   ```uci
   config online-upgrade 'main'
   	option enabled '1'
   	option repo 'your-username/your-firmware-repo'
   	option tag 'latest'
   	option firmware_pattern 'immortalwrt-.*-sysupgrade\\.bin'
   	option proxy 'https://gh.acg2.mom/'
   	option keep_config '1'
   	option min_size_mb '15'
   	option dl_workers '6'
   	option dl_chunk_mb '4'
   	option dl_stall_sec '30'
   	option dl_retries '3'
   ```
   > [!TIP]
   > 将 `your-username/your-firmware-repo` 替换为存放固件 Release 的 GitHub 仓库；将 `firmware_pattern` 替换为您编译生成的固件文件名正则。

---

### 方案二：`uci-defaults` 首次开机自启脚本（适合多机型动态适配）

如果您一套固件源码同时适配多种不同板型，需要根据路由器的实际机型名称（如 `/tmp/sysinfo/model`）动态匹配不同的 Release 文件，推荐使用 `uci-defaults` 脚本。

1. 在固件源码根目录下创建脚本：
   ```bash
   mkdir -p files/etc/uci-defaults
   touch files/etc/uci-defaults/99-default-online-upgrade
   chmod +x files/etc/uci-defaults/99-default-online-upgrade
   ```
2. 编辑 `files/etc/uci-defaults/99-default-online-upgrade`：
   ```sh
   #!/bin/sh

   # 获取当前设备板型标识（例如 mediatek,mt7981,h5000m）
   BOARD_NAME="$(cat /tmp/sysinfo/board_name 2>/dev/null)"

   uci -q batch <<EOF
   set online-upgrade.main=online-upgrade
   set online-upgrade.main.enabled='1'
   set online-upgrade.main.repo='your-username/your-firmware-repo'
   set online-upgrade.main.tag='latest'
   set online-upgrade.main.proxy='https://gh.acg2.mom/'
   set online-upgrade.main.keep_config='1'
   set online-upgrade.main.min_size_mb='15'
   set online-upgrade.main.dl_workers='6'
   set online-upgrade.main.dl_chunk_mb='4'
   set online-upgrade.main.dl_stall_sec='30'
   set online-upgrade.main.dl_retries='3'

   # 可根据机型动态设定不同的匹配正则
   case "\$BOARD_NAME" in
       *"h5000m"*)
           set online-upgrade.main.firmware_pattern='H5000M-.*-sysupgrade\.bin'
           ;;
       *"x86"*)
           set online-upgrade.main.firmware_pattern='.*-x86-64-.*-combined-efi\.img\.gz'
           ;;
       *)
           set online-upgrade.main.firmware_pattern='immortalwrt-.*-sysupgrade\.bin'
           ;;
   esac

   commit online-upgrade
   EOF

   exit 0
   ```
   > [!NOTE]
   > `uci-defaults` 脚本会在路由器初次开机（Firstboot）阶段全自动运行一次，执行成功后系统会自动删除该脚本，配置将永久固化在 `/etc/config/online-upgrade`。

---

### 方案三：GitHub Actions 云编译流水线脚本注入（如 P3TERX 模板）

如果您使用 GitHub Actions 自动云编译（例如基于 P3TERX 等主流开源工作流），只需在工作流调用的 `diy-part2.sh`（或自定义执行脚本）末尾追加一段代码即可完成预设注入：

```bash
# === 在 diy-part2.sh 末尾追加以下内容 ===
mkdir -p package/base-files/files/etc/uci-defaults

cat << 'EOF' > package/base-files/files/etc/uci-defaults/99-default-online-upgrade
#!/bin/sh
uci -q batch << 'EOC'
set online-upgrade.main=online-upgrade
set online-upgrade.main.enabled='1'
set online-upgrade.main.repo='your-username/your-firmware-repo'
set online-upgrade.main.tag='latest'
set online-upgrade.main.firmware_pattern='immortalwrt-.*-sysupgrade\.bin'
set online-upgrade.main.proxy='https://gh.acg2.mom/'
set online-upgrade.main.keep_config='1'
set online-upgrade.main.min_size_mb='15'
set online-upgrade.main.dl_workers='6'
set online-upgrade.main.dl_chunk_mb='4'
commit online-upgrade
EOC
exit 0
EOF

chmod +x package/base-files/files/etc/uci-defaults/99-default-online-upgrade
```

---

### 方案四：如何将插件集成到云编译源码中

在编译固件前，需要确保固件源码树中包含本插件。

1. **添加到 feeds.conf.default**：
   在源码根目录的 `feeds.conf.default`（或云编译的 `diy-part1.sh`）中追加本插件仓库源：
   ```text
   src-git onlineupgrade https://github.com/LianXia233/luci-app-online-upgrade.git;main
   ```

2. **更新与安装 feeds**：
   ```bash
   ./scripts/feeds update onlineupgrade
   ./scripts/feeds install -a -p onlineupgrade
   ```

3. **在 menuconfig 中勾选插件**：
   运行 `make menuconfig`，依次进入：
   `LuCI` ➔ `3. Applications` ➔ 选择 `<*>` `luci-app-online-upgrade`。
   若希望直接注入 `.config` 文件（适合云编译），在 `diy-part2.sh` 中追加：
   ```bash
   echo "CONFIG_PACKAGE_luci-app-online-upgrade=y" >> .config
   ```

---

## 🛠️ 命令行与 Ubus 调试

无需打开 Web 页面，您也可以通过 SSH 终端直接运行命令行进行调试与自动化任务：

### 命令行工具 (`online-upgrade`)
```bash
# 检查更新（输出格式化 JSON 到状态文件，详细日志写入 /tmp/online-upgrade.log）
online-upgrade check

# 探查指定仓库最新 Release 产物列表
online-upgrade probe --repo owner/repo --tag latest

# 手动备份当前路由器配置
online-upgrade backup

# 触发在线升级流程
online-upgrade upgrade

# 查看当前下载与执行状态 JSON
online-upgrade status
```

### Ubus JSON-RPC 接口 (`luci.online-upgrade`)
```bash
# 获取当前路由器运行状态及固件信息
ubus call luci.online-upgrade get_sysinfo

# 读取插件配置
ubus call luci.online-upgrade get_config

# 列出 /root/ 目录下的历史备份列表
ubus call luci.online-upgrade list_backups

# 探查仓库资产文件
ubus call luci.online-upgrade probe_assets '{"repo":"owner/repo","tag":"latest"}'
```

---

## ❓ 常见问题 (FAQ)

<details>
<summary><b>Q1: 为什么点击「获取固件列表」时，没有看到 Release 里的文本配置单或说明文件？</b></summary>
这是插件的预期保护特性。Release 中往往混杂了 <code>Config.txt</code>、<code>.manifest</code> 或 <code>.sha256</code> 等小于几百 KB 的辅助文本。插件默认开启了 <b>≥15MB</b> 过滤机制，避免误将文本文件选为固件导致刷机失败。如需调整，可在「高级配置」中修改「最小固件体积 (MB)」。
</details>

<details>
<summary><b>Q2: 下载过程中如果遇到断网或路由器突然断电，会变砖吗？</b></summary>
不会变砖。本插件采用严格的安全流水线：固件下载期间文件暂存在内存临时目录（<code>/tmp</code>），且必须完整通过分段合并与体积校验后，才会调用系统底层的 <code>sysupgrade</code> 开始真正写入 Flash。在正式刷写前，断电或断网仅相当于下载中断，不会对路由器现有系统造成破坏。
</details>

<details>
<summary><b>Q3: 国内网络访问 GitHub Releases API 偶尔超时怎么办？</b></summary>
1. 可以在插件的「第 3 步」选用预置的国内反代加速节点（如 <code>gh.acg2.mom</code>、<code>ghfast.top</code> 等）；<br>
2. 如果提示 API 限速（403 Rate Limit），可在「高级配置」中填入您自己的 GitHub Token（仅需 <code>public_repo</code> 只读权限）。
</details>

---

## 📄 许可证

本项目基于 **GNU General Public License v2 (GPL-2.0)** 协议开源。欢迎提交 Issue 与 Pull Request！
