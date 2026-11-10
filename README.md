<div align="center">

# luci-app-online-upgrade

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

## 简介

`luci-app-online-upgrade` 是专为 ImmortalWrt 与 OpenWrt 路由器打造的在线固件检测与一键升级插件。

本插件为 **v2 全面重构版**，彻底告别传统庞杂臃肿的 Lua 脚本与外部 curl 进程调用，采用：
- **前端**：原生纯净 JavaScript + 现代 CSS3 响应式布局 + 原生 SVG 科技插画，无任何第三方重型前端框架依赖。
- **中间层**：OpenWrt 官方现代标准的 `rpcd` ucode 插件桥接，轻量高效，原生融入 LuCI 会话鉴权体系。
- **后端内核**：基于 **Rust (Tokio 异步运行时)** 编写的静态 Musl 二进制程序，内置自研 **并发分段下载引擎**，支持多流并发提速、断点续传、智能重试与自动故障转移。

无论是在电脑大屏还是手机移动端，均可获得流畅、优雅且极速的固件升级与配置容灾体验。

---

## 核心特性

### 1. 极简纯白现代视觉设计
- **卡片式轻拟物层次**：采用清爽纯白（`#ffffff`）与柔和中性灰背景（`#f8fafc`），配合细腻高光与微阴影，摆脱传统路由界面的粗糙质感。
- **原生 SVG 矢量科技插图**：精心手绘 Hero 头部交互插图（智能路由中枢、云端固件通道、双向高速传输管道、安全防护盾牌）与全套功能状态图标，全分辨率清晰锐利。
- **四阶段升级流水线指示**：直观展示 `检测固件 → 准备就绪 → 极速下载 → 刷写重启` 四步流水线状态，每一步骤进度实时脉冲呈现。

### 2. 全端全场景响应式适配
- **手机触屏深度优化**：针对移动端屏幕尺寸，自动重构为单列大触摸卡片，操作按钮智能扩宽为易触控的弹性网格，彻底解决移动端横向溢出或操作别扭问题。
- **Hero 头部按视口自适应瘦身**：桌面端保持「文字 + SVG 插画」横向双栏并收紧留白；平板仅收窄插画、不再上下堆叠（避免横幅被撑到近 400px）；手机端隐藏纯装饰插画、把三个特性徽章由纵向堆叠改为横向紧凑排列，横幅高度压缩一半以上，首屏无需滚动即可看到「检查更新」与固件状态。说明文案完整保留，不做省略。
- **无截断文本排版**：采用专属全宽卡片展示「检测依据与规则」，固件正则规则、架构匹配、云端 Release 直达链接等关键信息多行自适应折行完整呈现，绝不模糊省略。

### 3. 智能架构与机型识别（防变砖安全决策引擎）
- **自动探查设备硬件特征**：全自动识别路由器当前硬件架构（如 `aarch64`、`x86_64`、`mips` 等）、芯片平台（如 `mediatek/filogic`、`rockchip` 等）以及设备具体板型（如 `Hiveton H5000M`、`H3C Magic NX30 Pro` 等）。
- **云端固件兼容性智能评估（该不该刷）**：全自动分析云端 Release 各固件产物，多维度交叉比对硬件架构与设备签名。若检测到架构或机型冲突（例如 ARM64 设备误选 x86_64 固件，或 H5000M 设备误选 NX30 Pro 固件），**系统将自动判定为「不兼容」并强制阻断升级**，从源头彻底杜绝刷错固件变砖的悲剧。
- **固件列表智能打标与置顶推荐**：探查固件列表时，自动将完美适配当前机型的固件置顶并标注 `[推荐]`，对不匹配的固件标注 `[不匹配]`，即便未配置固件匹配正则也能全自动智能匹配。
- **底层内核级预检 (`sysupgrade -T`) 双重安全屏障**：在固件下载完成后，插件不仅执行 SHA-256 完整性校验，更会调用系统底层 `sysupgrade -T` 及 `fwtool` 针对固件包头、分区表与硬件签名做二次无损预检，预检不通过坚决不刷入。

### 4. 智能仓库识别与固件自动过滤
- **一键智能解析**：支持直接粘贴 GitHub Release 页面链接、仓库主页链接或简单的 `owner/repo`，全自动提取仓库与标签。
- **智能过滤小文件（≥15MB）**：点击「获取固件列表」时，自动探查目标 Release 附件产物，**全自动剔除小于 15MB 的非固件文件**（如 `.txt` 配置单、`.manifest` 清单、`.sha256` 校验和），仅保留真正可刷写的固件包供用户点选，一键自动生成固件匹配正则。
- **版本解析与升级方向判定**：版本号优先取自资产文件名，其次取自 Release tag；支持多段版本（`25.12.0.1`）、`-rc`/`-beta`/`-alpha` 后缀、有无 `v` 前缀、版本号不在首段的命名（如 `vendor-2.1.0-immortalwrt-24.10.0-...`），也不会把 SoC/架构里的数字当版本。比较结果分为「目标更高 / 同版本 / 目标更低 / 无法比较」四种，目标更低时默认拒绝降级（需强制模式）；目标版本与当前固件相同时不算「有更新」，界面不摆普通升级入口，重装只能手动点「强制升级」。

### 5. 多线程分段加速下载与任务管理
- **动态切片与 Work-Stealing**：自动探测 HTTP Range 支持，按 4MB 动态切片并分派给多达 6 个 Tokio 异步 Worker 并发拉取，快连接自动偷取慢连接分块，拉满带宽潜能。
- **断点续传与故障恢复**：内置 `.parts.json` 侧车机制，意外断网无缝续传；支持块级独立重试与停滞看门狗（Stall Watchdog，默认 30 秒），遇不支持分段的目标自动平滑降级单流传输。
- **开始 / 暂停 / 继续 / 取消**：下载全程由 Rust + Tokio 调度。控制指令写入 `/tmp/online-upgrade-control.json`，由下载进程的 Tokio 任务轮询后经 `tokio::sync::watch` 广播给每个分块 Worker；暂停在**分块边界**生效（不会把 HTTP 响应流悬挂在空闲状态），继续时从断点续传记录接着下；取消立即终止，且不会被误判为线路故障而换源重下。
- **删除历史任务**：任务记录持久化在 `/root/.online-upgrade-tasks.json`（最多 30 条，重启不丢），状态、进度、实测速度与停止原因都可查，支持单条删除。仍在运行/暂停的任务需先取消；删除时若已无「已完成」记录，会一并清理 `/tmp/firmware.img.gz` 及 `.ts` / `.parts.json` 侧车，避免残留占满 tmpfs。
- **慢速保护（自动停止并抛出原因）**：每秒采样共享字节计数器计算实测吞吐，连续低于 `dl_min_speed_kb`（默认 32 KB/s）达到 `dl_slow_window_sec`（默认 15 秒）即停止下载，并在状态文件、运行日志和任务历史中输出具体原因（窗口内平均速度、阈值、观测时长、当前线路），同时列出可切换的加速节点。TLS 握手与 TCP 慢启动由 `dl_slow_grace_sec`（默认 10 秒）宽限；`dl_min_speed_kb=0` 可关闭保护。慢速停止是**任务级终止**，不自动换线路重试，断点续传记录会保留。
- **已实测可用镜像代理**：预置 7 个快捷节点 `gh.acg2.mom`（默认）、`ghfast.top`、`ghproxy.net`、`gh-proxy.com`、`ghproxy.imciel.com`、`ghproxy.monkeyray.net`、`ghproxy.vip`，2026-11-10 逐个验证均能正确返回同一份文件内容；已移除不再解析的 `github.moeyy.xyz`。
- **下载线路自动回退**：优先使用所选镜像/直连的 Release 下载地址；当 `github.com` 网页前端返回 5xx 或超时时，自动回退到 GitHub API 资产直链（`api.github.com/repos/<owner>/<repo>/releases/assets/<id>` → 签名 CDN），网页前端故障期间仍能完成升级。**只有线路级网络失败才回退**，用户取消与慢速保护触发的停止不会回退。

#### 下载任务 CLI（SSH 排查用）

| 命令 | 作用 |
| :--- | :--- |
| `online-upgrade download` | 开始一个下载任务（只下载并校验，不刷机） |
| `online-upgrade pause` / `resume` | 暂停 / 继续当前下载 |
| `online-upgrade cancel` | 取消当前下载 |
| `online-upgrade tasks` | 输出历史任务 JSON（状态、进度、速度、原因） |
| `online-upgrade task-delete <id> [--force]` | 删除历史任务；`--force` 用于清理进程崩溃后残留的 `running` 记录 |

### 6. 固件哈希校验与配置容灾安全升级
- **固件 SHA-256 完整性校验（来源链）**：按 `expected_sha256`（配置）→ Release 校验单（`sha256sums` / `sha256sums.txt` / `<firmware>.sha256`）→ Release 说明正文的顺序取期望哈希；哈希不符立即终止刷写并清理固件。上游三种来源都拿不到哈希时不会静默跳过：界面与日志明确标注「无校验依据，仅记录本地 SHA256」，此时不做比对校验，也不宣称“已校验”。
- **刷前自动全备份**：每次启动刷写前，全自动为路由器当前配置创建压缩归档存于 `/root/`。
- **三维恢复通道**：在 Web 界面即可随时「下载备份包到电脑」、「一键自动恢复现有备份」或「手动上传历史备份包恢复」。
- **升级自举无损保留**：与 `sysupgrade -f` 及 `keep.d` 深度集成，升级完成后自动触发 `uci-defaults` 恢复缺失配置。自动重装脚本同时支持 `apk` 与 `opkg`，按本机真实架构筛选安装包（不再“取第一个”），缺 `jsonfilter` 时用等效文本提取兜底，任何失败都会写入 syslog 并以非零码退出。

### 7. 真实进度条与实时控制台终端
- **真实字节级进度**：精准展示下载百分比、实时下载速度、已下体积与总大小，非虚假动画估算。
- **内嵌实时日志控制台**：支持折叠/展开的黑客风格暗色终端，实时捕获输出升级日志；支持一键「清空日志」；**内置内存保护机制，严格限制 tmpfs 内存上限 ≤ 2MB**（超额自动截断保留最新），绝不膨胀占用路由器宝贵的物理内存。

---

## 架构拓扑

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
│  ├─ upgrade  6线程并发分段下载 → 校验 → 刷机          │
│  ├─ backup   升级前全自动配置打包 (/root/backup-*.tar.gz)│
│  └─ restore  自动/手动恢复备份配置                     │
└──────────────┬──────────────────────────┬──────────────┘
               │ 写入状态 JSON             │ 执行升级
               ▼                          ▼
   /tmp/online-upgrade-status.json   sysupgrade -f (保留配置刷机)
```

---

## 使用向导

### 步骤一：填入或识别仓库
1. 进入 LuCI 菜单：**系统** → **在线升级**。
2. 在「**1. 固件仓库来源**」中直接粘贴您的 GitHub Release 网址、仓库网址或 `owner/repo`（如 `LianXia233/OpenWRT-CI`），点击「**识别**」。系统将自动解析仓库名并设定 Release 追踪规则。

### 步骤二：选择目标固件文件（自动过滤小文件）
1. 点击「**获取固件列表**」。
2. 系统会自动请求该仓库最新的 Release 产物附件，**并自动过滤掉所有小于 15MB 的配置文件和辅助文本**。
3. 从下拉菜单中点选您设备对应的固件型号（如 `xxx-sysupgrade.bin`），点击「**使用此文件 → 生成规则**」，插件将自动生成精准的匹配规则。

### 步骤三：选择加速代理并保存
1. 在「**3. 下载加速代理**」中，可直接点击已实测可用的节点按钮（`gh.acg2.mom`、`ghfast.top`、`ghproxy.net` 等 7 个）或「直连 GitHub」快速填入代理，国内网络环境强烈推荐启用。
2. 下载任务的开始 / 暂停 / 继续 / 取消与历史删除在「**下载任务管理**」卡片操作；慢速保护阈值可在「高级配置」中调整（默认低于 32 KB/s 持续 15 秒自动停止）。
3. 点击「**保存配置**」，此时当前配置已被持久化至路由器的 UCI 配置文件中。

### 步骤四：一键检测与极速升级
1. 点击「**检查更新**」按钮，系统开始向 GitHub 查询最新版本。
2. 查看「**检测依据与规则**」全宽卡片，确认目标版本、发布日期与本地固件比对信息。
3. 点击「**一键在线升级**」：
   - 系统自动为当前路由器的网络、密码、插件等配置创建备份压缩包；
   - 启动多线程分段极速下载，在页面可实时观测下载速度与进度条；
   - 下载校验完成后自动调用 `sysupgrade -f` 刷写固件并重启路由器。

---

## 识别与判定矩阵（机型 × 架构 × 版本 × 自用构建）

插件不硬编码任何仓库、组织名、资产命名规则、机型标识或版本格式：仓库、标签规则、文件名正则、期望哈希全部来自 UCI 配置，识别逻辑只依赖「本机真实身份」与「资产声明信息」的结构化比对。默认值（例如默认镜像前缀）可以被配置覆盖，覆盖后逻辑不再依赖默认值。

### 1. 判定输入

| 维度 | 本机侧（真实身份） | 资产侧（声明信息） |
| :--- | :--- | :--- |
| 机型 | `/tmp/sysinfo/board_name`、`/tmp/sysinfo/model`、`/etc/board.json` 的 `model.id`，缺失时才执行 `ubus call system board` | 文件名里的机型令牌，与上面这些标识逐段比对（归一化后比较；平台词、SoC 词、架构词、发行版词、镜像类型词一律**不**作为机型证据） |
| 架构 | `DISTRIB_ARCH`（`/etc/openwrt_release`）→ `apk --print-arch` → `opkg print-architecture` → `uname -m` | 文件名里的架构/SoC 词表（`aarch64/arm64/cortex-a53/mt7981/rk3568`…、`x86/x86-64/x86_64/amd64`、`mipsel/mt7621`…、`armv7/armhf`…） |
| 版本 | 按可信度依次尝试：构建写入的版本文件（`/etc/online-upgrade-version`、`/etc/build-version`、`/etc/firmware-version`、`/etc/version`）→ `DISTRIB_VERSION` / `DISTRIB_RELEASE` / os-release 的 `VERSION_ID` → `DISTRIB_DESCRIPTION` / `DISTRIB_REVISION` / `BUILD_ID` / `/etc/openwrt_version` → `base-files` 包版本 → 固件构建时间（`/etc/openwrt_release` 的修改时间）。命中的来源会随结果一起返回并在界面显示，详见「3.1 本机版本从哪来」 | 文件名版本（优先）或 Release tag；通用解析：多段数字 + 可选 `-rc/-beta/-alpha` 后缀 + 可选 `v` 前缀，取最后一个候选避免把厂商/SOC 数字当版本 |

### 2. 机型 × 架构判定矩阵

| 架构比对 | 机型比对 | 判定级别 | 是否允许升级 | 界面呈现 |
| :--- | :--- | :--- | :--- | :--- |
| `ok`（架构一致） | `exact`（完整标识或专属令牌命中） | Compatible | 允许 | 「机型匹配」+ 命中标识 |
| `ok` | `rule`（命中配置的正则） | Compatible | 允许 | 「命中配置规则」+ 规则原文 |
| `ok` | `partial`（只命中通用词，如厂商名） | Warning | 阻止，需强制模式 | 「机型部分匹配，需人工核对」 |
| `ok` | `unmatched`（本机标识都没出现） | Unknown | 阻止 | 「无法确认是否适配」 |
| `undeclared`（资产名没有架构词） | 任意（无规则） | Warning / Unknown | 阻止 | 「资产未声明架构」，不猜成“通用” |
| `undeclared` | `rule` | Compatible | 允许 | 「配置规则匹配·架构未声明」并在依据里写明 |
| `unknown`（本机架构读不出来） | 任意（无规则） | Warning | 阻止 | 「本机架构未知」 |
| `conflict`（资产声明的架构与本机不同） | 任意 | Incompatible | 阻止，规则也不能覆盖 | 「架构冲突，严禁刷入」 |
| 文件名是 `factory` / `initramfs` 镜像 | 任意 | Warning | 阻止 | 提示应选 `sysupgrade` 产物 |

`can_upgrade=false` 时后端直接拒绝执行升级；只有用户显式点击「强制更新」（`--force`）才会绕过，且绕过行为会写进日志。

### 3. 版本判定矩阵

#### 3.1 本机版本从哪来

自用构建的 SNAPSHOT 固件通常没有 `DISTRIB_RELEASE=24.10.0` 这类版本号（`DISTRIB_RELEASE` 字面就是 `SNAPSHOT`）。直接放弃会让版本方向永远落到「无法比较」，所以插件按可信度从高到低依次尝试，并把命中的来源一并返回（`version.local` / `version.local_source` / `version.local_source_text` / `version.local_confidence`）：

| 顺序 | 来源 | 可信度 | 典型值 |
| :--- | :--- | :--- | :--- |
| 1 | 构建写入的版本文件：`/etc/online-upgrade-version`、`/etc/build-version`、`/etc/firmware-version`、`/etc/version` | 可信 | CI 打进去的自用 tag，如 `26.11.08` |
| 2 | `/etc/openwrt_release` 的 `DISTRIB_VERSION`、`DISTRIB_RELEASE`；`/etc/os-release` 的 `VERSION_ID` | 可信 | `24.10.0` |
| 3 | `DISTRIB_DESCRIPTION`（内嵌版本号时） | 可信 | `ImmortalWrt 24.10.0 r28000-…` |
| 4 | `DISTRIB_REVISION`、`BUILD_ID`、`/etc/openwrt_version`、`base-files` 包版本 | 弱 | `r0-23a1cd3`、`0~23a1cd3` |
| 5 | 固件构建时间（`/etc/openwrt_release` 的修改时间，UTC+8） | 弱 | `26.11.07`（只与日期型标签（`YY.MM.DD` / `YYYY.MM.DD`，月日合法）比较，并按目标年份位数对齐；目标不是日期型标签时**不比较**） |

`SNAPSHOT` 这类非版本文本不会被当成版本号；拿不到任何来源时，结果里明确写「无法比较」，不会伪造版本。

#### 3.2 判定状态

版本判定分三种状态，只有降级才需要用户二次确认，不确定本身不构成阻断：

| 本机版本 | 目标版本 | 关系 | 状态 | 行为 |
| :--- | :--- | :--- | :--- | :--- |
| 可解析（可信来源） | 高于当前 | `newer` | `decidable` | 允许升级 |
| 可解析（可信来源） | 等于当前 | `same` | `decidable` | 提示「已是最新（同版本）」；**不出现「有新版本」引导，也不提供普通升级按钮**，只保留显式的「强制升级」（同版本重装）。Release 更新时间变化（重新构建）时同样只走强制升级 |
| 可解析（可信来源） | 低于当前 | `older` | `decidable` | 拒绝（`confirm_required=true`），需强制模式并记录原因 |
| 只有弱依据（构建时间、修订号、包版本） | 高于当前 | `newer` | `suspect` | 允许升级，界面标注「依据较弱」并给出取到的版本与来源 |
| 只有弱依据 | 等于 / 低于当前 | `same` / `older` | `suspect` | 同版本提示已是最新（同版本重装只能强制升级）；降级仍拒绝并需强制确认 |
| 确实取不到（如 `SNAPSHOT` 且无任何其他来源） | 任意 | `unknown` | `incomparable` | **放行**：按「将升级到该发布版本」处理，界面说明原因；机型匹配、架构匹配与 SHA-256 校验全部照旧生效 |
| 只有构建时间，但目标不是日期型标签（如 `24.10.0`、`25.12.0.1`） | 任意 | `unknown` | `incomparable` | **不比较、放行**：构建时间与普通发布号不是同一坐标系（`26.11.07` 对 `24.10.0` 只会得出错误方向），按「将升级到该发布版本」处理，其余校验照旧 |
| 任意 | 无法解析 | `unknown` | `incomparable` | 按 Release 发布时间提示 |

界面上除了关系结论，还会显示「本地版本」「依据」「判定状态」三个标签，用户能看出这个结论是基于哪个来源、哪一次比较得出的（`version.confirm_required` 同时提供给 API 调用方）。

同版本（`same`）不视为「有更新」：后端给出 `has_update=false`，前端不显示「有新版本」角标、不摆出普通升级按钮，只保留手动点击的「同版本重装（强制升级）」；直接调用 `upgrade` 而未加 `--force` 也会被拒绝，重装必须显式强制。降级与无法比较两种情形的处理不受影响。

### 4. 「自用构建」兜底路径

1. 先用架构比对：资产名声明了与本机冲突的架构 → 直接 Incompatible（有正面证据，不能被规则覆盖）。
2. 再看配置规则：`firmware_pattern` 命中 → 命中即视为适配，`matched_rule` 与 `basis` 同时写入日志和界面（含「资产名未声明架构」这类前提）。
3. 再看结构化机型比对：命中完整标识或专属（字母+数字）令牌 → 匹配；只命中厂商/通用词 → 部分匹配。
4. 都判定不了 → `unknown`，界面显示「无法确认是否适配」，**不会**自动当成可升级；用户需要显式使用强制模式。自动选择也**不会**退化成「取第一个」。

### 5. 资产选择在常见情况下如何表现

| 情况 | 表现 |
| :--- | :--- |
| 资产名不含机型 | 有匹配正则时按规则命中（视为配置声明）；无规则时结构化比对为 `unmatched`/`partial` → 报「无法确认」并列出候选文件名，不自动选择 |
| 同一 Release 有多个 sysupgrade 变体 | 按架构/机型/规则打分排序，只接受 Compatible 的最高分；最高分并列多个时**报歧义错误**并列出候选，要求你配置更精确的正则或手动指定，不会取第一个 |
| tag 不是 `vX.Y.Z`（如 `自用构建`、`20261107`、`latest`） | tag 只用于定位 Release；版本优先从资产文件名解析。tag 无法解析且文件名也无版本时，判定为 `unknown`，按 Release 发布时间提示，不做降级/升级的强行判断 |

## 并发保护与撞锁行为

- 后端进程互斥用的是 `/tmp/online-upgrade.lock`，内容为 `<pid> <owner> <时间戳>`。
- 判定顺序：先按 PID 做存活探测（`/proc/<pid>`），进程已不存在或 PID 就是自己时才回收；PID 存活且不是自己则直接返回「已有任务正在运行 (pid N, owner)」。
- **锁的新鲜度**：如果锁文件存在但读不出 PID（典型情况是另一个进程刚 `create_new` 建好文件、还没来得及写入 PID，或首行只写了一半），在 **2 秒宽限期内一律按“忙”处理**，超过 2 秒仍不可读才当作崩溃残留回收。这样既能避免两个进程同时持锁去写同一份固件与状态文件，也不会让“建锁后立即崩溃”的残留永久占锁。
- rpcd 层还有一次快速失败探测（读锁文件首个字段做 `/proc` 探测），第二次点击立即拿到明确错误，而不是排队等待。
- **前端表现**：`check` / `start_upgrade` 拿到 `{ok:false, error}`（撞锁）时立即在面板上显示后端返回的错误、恢复两个升级按钮、**不进入轮询**，也不会显示上一次任务的进度卡片；只有真正启动成功才隐藏按钮并开始轮询。

---

## 安装与部署

### 支持架构
- **aarch64**（如 MediaTek Filogic MT798x / RK3568 / RK3588 等）
- **x86_64**（软路由、工控机、虚拟机）

### 方式 A：通过 Release 安装包（推荐）
从本仓库的 [Releases 页面](https://github.com/LianXia233/luci-app-online-upgrade/releases) 下载对应架构的最新 `.apk` 安装包，上传至路由器并执行：

```bash
# 适用于 ImmortalWrt 25.12+ / OpenWrt 25+ (apk 包管理器)
apk add --allow-untrusted luci-app-online-upgrade_*.apk
```

> **版本号与标签规则**：Release 标签固定为 `v<PKG_VERSION>`（如 `v2.4.0`），安装包文件名为 `luci-app-online-upgrade-<PKG_VERSION>-r<PKG_RELEASE>_<arch>.apk`。三者由 `Makefile` 统一派生，`rust/Cargo.toml` 与模板中的 `app.js?v=` / `style.css?v=` 由 CI 一致性校验锁定；发布前 CI 会校验安装包名与版本相符，Release 正文自动取自 `CHANGELOG.md` 对应版本段落，因此标签、产物版本与更新日志始终同步。

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

## UCI 配置详解与参数说明

插件的核心配置文件存储于路由器的 `/etc/config/online-upgrade`。支持通过 Web 界面图形化配置，亦可直接通过 SSH 命令行或 UCI 指令修改。

### 1. 配置项参考表

| 配置项 | 类型 | 默认值 | 说明与示例 |
| :--- | :--- | :--- | :--- |
| `enabled` | bool | `0` | 是否启用在线升级配置：`1` 为启用，`0` 为禁用 |
| `repo` | string | 空 | GitHub 仓库路径，格式为 `owner/repo`（如 `LianXia233/OpenWRT-CI`） |
| `tag` | string | `latest` | Release 追踪策略：<br>• `latest`：追踪最新发布的 Release；<br>• `latest@^前缀`：按前缀过滤目标机型最新 Release（如 `latest@^H5000M`）；正则写错会**直接报错并中止检测**（不会退化成“不过滤”而在全部 Release 里挑）；<br>• 具体 Tag 名（如 `v2026.11`） |
| `firmware_pattern` | string | 空 | 目标固件文件名匹配的正则表达式（如 `immortalwrt-.*-sysupgrade\.bin`） |
| `proxy` | string | `https://gh.acg2.mom/` | GitHub 文件与 API 加速反代节点，留空则直连 GitHub |
| `min_size_mb` | integer | `15` | **最小固件体积阈值 (MB)**：获取列表与检测匹配时自动忽略小于此体积的文件（过滤 metadata/txt/sha256），设为 `0` 则不限制 |
| `expected_sha256` | string | 空 | 固件 SHA-256 校验和（64 位十六进制）：<br>• **留空（推荐）**：依次尝试 Release 附件 `sha256sums` / `sha256sums.txt` / `<固件>.sha256` 与 Release 说明正文；<br>• **手动填写**：强制使用该值校验，不一致则自动阻断刷写。<br>三种来源都没有哈希时界面会标注「无校验依据，仅记录本地哈希」，不假装校验通过 |
| `keep_config` | bool | `1` | 升级时是否调用 `sysupgrade -f` 保留现有配置（`1`=保留配置，`0`=完全全新刷入） |
| `dl_workers` | integer | `6` | 并发下载线程数（范围 1~16，建议 4~8；不支持 Range 的节点自动平滑降级为单线程） |
| `dl_chunk_mb` | integer | `4` | 动态分段切片大小（MB），默认 4MB |
| `dl_stall_sec` | integer | `30` | 分块传输停滞超时看门狗阈值（秒），分片停滞时自动重调度与故障转移 |
| `dl_retries` | integer | `3` | 单个分块下载失败后允许的最大重试次数 |
| `dl_min_speed_kb` | integer | `32` | **慢速保护阈值 (KB/s)**：实测速度持续低于该值达到判定窗口即自动停止下载并输出原因；设为 `0` 关闭保护 |
| `dl_slow_window_sec` | integer | `15` | 慢速判定窗口（秒，范围 3~300）：速度连续低于阈值达到该时长才判为异常 |
| `dl_slow_grace_sec` | integer | `10` | 启动宽限期（秒，范围 0~300）：TLS 握手与 TCP 慢启动期间不做慢速判定 |
| `github_token` | string | 空 | 可选。GitHub Personal Access Token（只读权限即可），用于防止局域网内公网 IP 触发 GitHub API 403 限流。令牌只会发给 `api.github.com`，经镜像前缀转发或回退到镜像下载时一律不带 Authorization 头 |

### 2. 标准 UCI 配置文件示例 (`/etc/config/online-upgrade`)

```uci
config online-upgrade 'settings'
	option enabled '1'
	option repo 'your-username/your-firmware-repo'
	option tag 'latest'
	option firmware_pattern 'immortalwrt-.*-sysupgrade\.bin'
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
```

---

## 固件云编译预设配置向导 (Firmware Builder Preset Guide)

在自编译 OpenWrt / ImmortalWrt 固件时，将本插件编译进固件并**预设好您的 GitHub 仓库与机型规则**，即可实现固件刷入后**开箱即用**：用户无需手动输入仓库地址，进入后台点击「检查更新」即可一键在线平滑升级。

### 方案一：`files/` 目录直接覆盖（推荐，最轻量纯净）

OpenWrt 编译系统原生支持 `files` 目录树自动覆盖打包进根文件系统，这是最规范、无需执行脚本的方法。

1. 在固件源码根目录下创建配置目录：
   ```bash
   mkdir -p files/etc/config
   ```
2. 创建 `files/etc/config/online-upgrade` 文件并填入您的预设参数：
   ```uci
   config online-upgrade 'settings'
   	option enabled '1'
   	option repo 'your-username/your-firmware-repo'
   	option tag 'latest'
   	option firmware_pattern 'immortalwrt-.*-sysupgrade\.bin'
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
   set online-upgrade.settings.enabled='1'
   set online-upgrade.settings.repo='your-username/your-firmware-repo'
   set online-upgrade.settings.tag='latest'
   set online-upgrade.settings.proxy='https://gh.acg2.mom/'
   set online-upgrade.settings.keep_config='1'
   set online-upgrade.settings.min_size_mb='15'
   set online-upgrade.settings.dl_workers='6'
   set online-upgrade.settings.dl_chunk_mb='4'
   set online-upgrade.settings.dl_stall_sec='30'
   set online-upgrade.settings.dl_retries='3'

   # 可根据机型动态设定不同的匹配正则
   case "\$BOARD_NAME" in
       *"h5000m"*)
           set online-upgrade.settings.firmware_pattern='H5000M-.*-sysupgrade\.bin'
           ;;
       *"x86"*)
           set online-upgrade.settings.firmware_pattern='.*-x86-64-.*-combined-efi\.img\.gz'
           ;;
       *)
           set online-upgrade.settings.firmware_pattern='immortalwrt-.*-sysupgrade\.bin'
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
set online-upgrade.settings.enabled='1'
set online-upgrade.settings.repo='your-username/your-firmware-repo'
set online-upgrade.settings.tag='latest'
set online-upgrade.settings.firmware_pattern='immortalwrt-.*-sysupgrade\.bin'
set online-upgrade.settings.proxy='https://gh.acg2.mom/'
set online-upgrade.settings.keep_config='1'
set online-upgrade.settings.min_size_mb='15'
set online-upgrade.settings.dl_workers='6'
set online-upgrade.settings.dl_chunk_mb='4'
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

## 命令行与 Ubus 调试

无需打开 Web 页面，您也可以通过 SSH 终端直接运行命令行进行调试与自动化任务：

### 命令行工具 (`online-upgrade`)
```bash
# 检查更新（输出格式化 JSON 到状态文件，详细日志写入 /tmp/online-upgrade.log）
online-upgrade check

# 探查指定仓库最新 Release 产物列表
online-upgrade probe --repo owner/repo --tag latest

# 清空运行日志文件（内存占用归零）
online-upgrade clear-log

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

# 清空运行日志
ubus call luci.online-upgrade clear_log

# 列出 /root/ 目录下的历史备份列表
ubus call luci.online-upgrade list_backups

# 探查仓库资产文件
ubus call luci.online-upgrade probe_assets '{"repo":"owner/repo","tag":"latest"}'
```

---

## 常见问题 (FAQ)

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
1. 可以在插件的「第 3 步」选用已实测可用的反代加速节点（<code>gh.acg2.mom</code>、<code>ghfast.top</code>、<code>ghproxy.net</code>、<code>gh-proxy.com</code> 等共 7 个）或直连；<br>
2. 如果提示 API 限速（403 Rate Limit），可在「高级配置」中填入您自己的 GitHub Token（仅需 <code>public_repo</code> 只读权限）；<br>
3. 下载持续过慢时插件会<b>自动停止并输出原因</b>（实测速度、阈值、观测窗口、当前线路），可换一个节点后点「开始下载」从断点继续；阈值与窗口在「高级配置」中可调，<code>dl_min_speed_kb=0</code> 可关闭该保护。
</details>

<details>
<summary><b>Q4: 能检查到新版本，但一点「立即升级」就下载失败（如 HTTP 504）？</b></summary>
这通常是 <code>github.com</code> 网页前端自身故障——Release 下载地址就由它提供。插件会先用你选择的镜像/直连地址，失败后自动回退到 GitHub API 资产直链（走 <code>release-assets.githubusercontent.com</code> 签名 CDN），两条线路都失败才报错。若回退也失败，说明 GitHub API 同时不可用，稍后重试即可。可先在 SSH 里执行 <code>online-upgrade download</code> 单独验证下载链路——它只下载与校验，<b>不会刷机</b>；状态文件里的 <code>dry_run="true"</code> 与 <code>verified</code> 字段可以确认走到了哪一步。
</details>

---

## 许可证

本项目基于 **GNU General Public License v2 (GPL-2.0)** 协议开源。欢迎提交 Issue 与 Pull Request！
