# Changelog

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Changed

- **2026-11-12 全仓复核**：`build.yml` 中 `actions/checkout@v5` → `v7`、
  `actions/cache@v5` → `v6`（共 5 处），上方版本号已同步为当前最新大版本。GitHub 已于 2026-09-23 从托管 runner 移除 Node 20 运行时，仍声明 `node20` 的 action 会被强制跑在 Node 24 上并打印 `Node.js 20 is deprecated` 告警，属于未验证状态；本次按各 action 的 `action.yml` 逐一核对 `runs.using` 后统一升级。
- **仅改 CI 配置**：项目源码、`node-version` / `python-version` / `go-version` 等工具链版本、产物命名与 Release 流程均未改动。告警指的是 action 自身声明的运行时，不是项目构建所用的 Node 版本。
  升级后全仓复扫 `node20` 引用残留为 0。
- **CI 发布后自动清理旧 Release**：`Build Package` 上传当前版本后，通过 GitHub API 删除除当前 tag 外的所有 Release 对象，并校验清理后数量必须为 1；仅删除 Release，不删除 Git tag，保留历史源码引用。
- **CI 构建提速与 Node 24 迁移**：Rust 后端保持在 `rust` 作业中用 rustup/cargo 直接交叉编译（`aarch64-unknown-linux-musl` / `x86_64-unknown-linux-musl`），新增 `Swatinem/rust-cache` 缓存 Cargo registry 与 `target`；OpenWrt SDK 仅负责 LuCI 文件、包结构与 `.apk` 生成，新增 `actions/cache` 复用 SDK 压缩包，避免重复下载。所有 action 统一升级到当前最新大版本（`checkout@v7`、`upload-artifact@v7`、`download-artifact@v8`、`cache@v6`、`setup-node@v7`、`action-gh-release@v3`），消除 Node 20 弃用告警；`Makefile` 增加预编译二进制校验，明确不在 SDK 内编译 Rust。

## [2.6.1] - 2026-11-14

### Fixed

- **修复「远程状态与统一诊断」卡片未加载 / `RPC error -32002`（Access denied）**：升级安装后 `rpcd` 仍持有旧的 ucode 插件与 ACL 白名单，新增的 `get_states` 等 RPC 被判为权限不足。现在 `postinst` 会重载 `rpcd`，使新 ACL 与新方法立即生效；已登录会话的 ACL 快照在重新登录 LuCI 后刷新。
- 前端把 `-32002` 转成可操作的中文指引（重新登录 / `/etc/init.d/rpcd restart` / 确认已装最新版），并在四套面板内显示明确的失败状态，不再留空白卡片。
- 附带修掉 `states-hint` 用 `textContent` 写入却先 `esc()` 导致的 HTML 实体明文显示（如 `&amp;`）。

## [2.6.0] - 2026-11-14

### Added

- **四套完全独立的状态模型**：新增 `actions` / `releases` / `artifacts` / `compat` 四个独立 Rust 模块，各自拥有独立数据结构、状态枚举、缓存文件（`/tmp/online-upgrade-cache/*.json`）、独立 TTL、独立刷新与独立错误处理。任一模型刷新失败或异常都不会覆盖其他模型的真实状态。新增统一诊断层 `diagnostics`，只读汇总四套模型并输出中文诊断结论。
- **GitHub Actions 匿名状态检测**：新增 `ghapi` 通用 API 层，支持仓库地址解析（owner/repo 或完整 URL）、公开信息/默认分支/Workflow 自动发现、最近多条运行记录、分支与 Workflow 筛选；严格区分 `status` 与 `conclusion`，覆盖 queued/requested/waiting/pending/in_progress 与 success/failure/cancelled/timed_out/action_required/neutral/skipped；只有 `status=completed` 才依据 conclusion 判定结果。
- **GitHub Artifacts 状态检测**：独立识别可用/多个/过期/已删除/匿名无法下载/尚未生成等状态，展示过期时间与关联构建任务；明确区分 Artifact 与 Release Asset，且不凭名称判定内部固件。
- **通用设备信息标准化**：新增 `device` 模块，采集型号、board_name、Device Tree compatible、Target/Subtarget、CPU 架构、Package Arch/ABI、固件版本、发行版、内核、系统标识、升级方式与固件平台；多来源逐级兜底，识别失败保留未知、绝不推测。
- **固件自动发现与识别**：新增 `firmware` 模块，支持 Release Assets/Artifacts 多来源、常见镜像与压缩格式、文件用途分类（sysupgrade/factory/uboot/efi/rootfs/kernel/packages/sdk/toolchain/debug/校验文件/日志等）、ZIP 中央目录安全解析（防路径穿越与解压炸弹）、多维度匹配与置信度分级。
- **五层兼容性判定**：新增 `compatstate` 模块，按文件名初筛 → 内部元数据 → 设备维度比对 → 下载后完整性 → `sysupgrade -T` 本机验证分层记录；严格区分「无法确认兼容」与「明确不兼容」，未下载绝不宣称确认兼容或允许自动刷写。
- **缓存 / 限流 / 并发控制**：新增 `cache` 与 `network` 模块；读取 `X-RateLimit-*`/`Retry-After`，区分 403 限流与权限、429/5xx/DNS/TCP/TLS/超时分类，请求去重与并发限制，失败退避，缓存数据显式标记 stale 与「上次成功查询时间」，绝不把缓存伪装成实时数据；加速源优先、失败可靠回退官方直连，API 与下载策略分离，不关闭 TLS 校验。
- **RPC / WebUI 扩展**：新增 `get_states`、`get_device`、`get_diagnostics`、`probe_repo`、`netcheck`、`refresh_states`、`clear_states_cache` 七个 RPC 并同步 ACL；新增「远程状态与统一诊断」卡片（设备信息 / Actions / Releases / Artifacts / 固件兼容性 / 诊断结果 / 网络策略），状态均用文字 + 色标表达，含 760px / 380px 移动端适配。
- **通用 UCI 配置**：新增 `repo_url`、`build_branch`、`workflow_filter`、`api_via_proxy`、`api_timeout` 及五套独立 `ttl_*`，全部可选且带中性默认值；老配置升级时逐项补齐，绝不覆盖用户已有值。

### Changed

- 保留全部原有子命令、RPC 方法、返回字段与 UCI 字段；`check` / `download` / `upgrade` 现有流程与刷写安全标准一字未改。
- 新增只读状态子命令 `states` / `device` / `probe-repo` / `netcheck` / `clear-cache` 与独立刷新子命令 `refresh`；它们不写全局 `status.json`、不占用升级流程操作锁。
- 完成官方 OpenWrt **25.12.0 x86/64** 真实 QEMU 虚拟机验证（GRUB → Linux 6.12.71 → procd → rpcd/ubus → 真实 GitHub API → `sysupgrade -T`），证据与限制见 `docs/TEST_REPORT.md` 第 5 节。

### Fixed

- 设备信息采集的 `KEY='value'` 逐行解析：早期草稿在首行不匹配时提前返回，会导致后续字段全部丢失（单元测试覆盖）。
- 固件用途分类改用 token 精确匹配：避免短词裸子串匹配把普通文件名误判为 EFI/SDK 镜像。
- `ghapi::api_url()` 空 suffix 产生尾斜杠导致 GitHub 返回 404：真实匿名 API 冒烟发现「公开仓库被误判资源不存在」，已改为不带尾斜杠并加回归单测。
- `device.rs` ABI 误推导：真实 OpenWrt 25.12.0 的 `DISTRIB_ARCH='x86_64'` 曾被 `split_once('_')` 拆成 `abi="64"`，把 CPU 位宽当成 ABI。改为 `derive_abi()` 只在后缀确含字母（CPU 变体 / FPU / 浮点 ABI）时拆分，纯数字位宽、空后缀、无分隔符一律保留 `unknown`；新增 10 组回归单测。

### Security

- GitHub Token 全程只从 UCI 读取、仅在 `api.github.com` 作为请求头发送；所有日志、错误消息、返回值、前端输出均经脱敏，绝不落盘到交付文件。
- ZIP 解析拒绝绝对路径、`..`、反斜杠与 NUL 路径，限制条目数、单条目与总未压缩体积、异常压缩比；不落盘解压。

## [2.5.6] - 2026-11-10

### Changed

- **插件卡位置调整**：将「插件版本与自我更新」移动到「仓库与更新源配置」之后、运行日志之前。
- **插件卡默认折叠**：PC 与手机均默认收起，只显示标题、状态与「展开插件更新」入口；展开后显示检查/更新按钮、版本元信息、进度与发布规范。
- **手机下载任务不再默认折叠**：下载任务管理从手机默认折叠列表移除，进入页面即可看到任务列表与详情入口；备份、仓库配置、运行日志仍默认收起。

## [2.5.5] - 2026-11-10

### Added

- **插件包分块下载**：插件自更新不再使用独立单流下载，改为复用固件下载器的 worker/分块/重试/停滞/慢速保护；小包会按 worker 数强制拆分（分块不小于 64KB），因此任务详情中可看到真实分块与线程。
- **分块/线程任务详情**：任务记录新增 `workers`、`segments`、`chunk_size`、`chunks_total`、`chunks_done`、`chunks_running`、`chunk_states`；任务列表显示线程数与分块进度，详情弹窗展示统计卡片与分块状态网格（完成/传输中/等待）。
- **下载历史自动清理**：插件更新成功后按任务类型自动保留最近一条终态记录，清理更早的完成/失败任务；运行中任务不受影响。
- **代理优先回退**：配置加速代理后，插件包、校验单/清单与固件下载均优先走代理；代理失败或内容大小/SHA 校验不通过时再回退直连。

### Fixed

- 修复仅提示弹窗仍显示“取消”按钮的问题：改用 `is-hidden` 类，避免被按钮 `display:inline-flex !important` 覆盖。

## [2.5.4] - 2026-11-10

### Changed

- **统一弹窗系统**：新增 `ouDialog` 组件，替代全部浏览器原生 `confirm/alert`。手机端（≤760px）使用从屏幕底部滑出的 Bottom Sheet，桌面端使用居中 Modal Dialog；支持遮罩点击、ESC、焦点循环与按钮焦点恢复。
- **弹窗视觉规范**：白底、18px 桌面圆角、20px 手机顶部圆角、柔和阴影与毛玻璃遮罩；标题/说明/详情/操作区分层，详情使用等宽字体；主操作、危险操作与警告操作分别使用蓝、红、琥珀色按钮。
- **重启状态弹窗**：手机端改为底部抽屉形态，桌面端保持居中卡片；刷新按钮补齐蓝底主按钮样式。
- **弹窗调用迁移**：固件升级/强制升级、删除任务、删除备份、自动恢复、手动恢复、清空日志、插件更新等确认与失败提示全部切换到新弹窗。

### Fixed

- 弹窗按钮位于 `.ou-container` 之外，原先无法继承主按钮样式，已补齐独立的按钮基类与主/危险/警告配色。
- 桌面弹窗打开动画不再依赖 `requestAnimationFrame` 添加 `is-open`，改为同步提交初始状态后触发过渡，兼容部分 WebView/无头环境。

## [2.5.3] - 2026-11-10

### Added

- **插件更新下载任务**：插件自更新现在会在 `/root/.online-upgrade-tasks.json` 创建 `kind=plugin` 的任务，复用下载任务列表展示插件包名称、插件更新标签、下载/校验/安装阶段、进度、速度、原因与终态；任务完成或失败会写入 `done` / `failed`。
- **任务类型字段**：任务 JSON 新增 `kind`（`firmware` / `plugin`），旧任务缺少该字段时按 `firmware` 兼容；插件任务不会被固件的暂停/继续/取消按钮误操作。
- **插件状态同步**：插件更新完成（或失败）后，前端自动刷新插件版本、运行环境、任务列表与状态标识；下载完成不再停留在“发现新版本/下载中”。
- **资源缓存版本戳**：`plugin_info.cache_version` 返回安装后的版本号；前端读取当前模板 `style.css?v=` / `app.js?v=` 的静态版本戳，二者不一致时提示刷新页面。自更新安装后清理 `/tmp/luci-indexcache*` 与 `/tmp/luci-modulecache*`，确保 LuCI 模块缓存及时失效。

### Changed

- 任务表表头由“固件文件”改为“文件 / 任务”，插件任务在文件名旁显示“插件更新”标签，状态列按阶段显示下载中/校验中/安装中。

## [2.5.2] - 2026-11-10

### Changed

- **手机端排版精简**：Hero 区在手机上隐藏说明与特性标签，只保留标题、版本胶囊；固件状态卡隐藏重复说明，三张统计卡改为紧凑单行（标签 + 值），隐藏统计副标题。
- **判定详情按需展开**：手机端默认只显示结论、适配标签与「展开判定详情」入口；本机身份、机型/架构、版本、校验、发布信息与证据链在展开后显示，避免首屏被 8 个事实卡占满。
- **插件卡压缩**：手机上隐藏副标题，四行元信息改为 2×2 紧凑网格，资产说明最多两行；保留「检查插件更新 / 更新插件」主操作。
- **次要工具默认收起**：下载任务、配置备份、仓库配置、运行日志在手机上只保留标题与「展开」按钮，按需展开；配置卡展开后隐藏与上方重复的摘要条。
- **重复提示合并**：手机端成功的绿色提示条不再重复展示，结论统一由判定面板给出；判定面板的英文式小标题在手机上隐藏，减少标题层级。
- **字号与触控密度**：统一缩小手机端卡片内边距、标题/正文/标签字号、按钮高度与终端高度，同时保留可读行高与触控尺寸。

## [2.5.1] - 2026-11-10

### Fixed

- **插件自更新架构识别不精确**：`apk-tools 3` 的 `apk --print-arch` 只返回架构家族（如 `aarch64`），而 OpenWrt 的包架构是 `DISTRIB_ARCH`（如 `aarch64_cortex-a53`）。自更新此前因此显示 `pkg_arch=aarch64`，只能用架构家族匹配安装包；同家族存在多个 CPU 变体时会无法安全选择。现在优先使用 `DISTRIB_ARCH`，恢复精确匹配；仅在 `DISTRIB_ARCH` 缺失时才回退到包管理器架构。

## [2.5.0] - 2026-11-10

> 发布标签 `v2.5.0`；安装包 `luci-app-online-upgrade_2.5.0-r1_<arch>.apk`，并附带固定名称的 `online-upgrade-manifest.json` 与 `SHA256SUMS`。

### Added

- **插件自我更新**：新增 `plugin-info` / `plugin-check` / `plugin-update` 子命令，以及 rpcd 的 `plugin_info` / `start_plugin_check` / `start_plugin_update` / `get_plugin_status` 方法。自更新固定读取本插件仓库的稳定 Release，按 `online-upgrade-manifest.json` 选择与本机包架构匹配的安装包，校验 SHA256 与文件大小后调用 `apk` / `opkg` 原地安装，并在安装后重新读取版本确认到位。清单缺失时退回 `SHA256SUMS`；两者都没有校验值时拒绝安装，不提供“无校验更新”路径。
- **独立插件更新卡片**：页面新增「插件版本与自我更新」区块，展示当前版本、最新版本、运行环境、最近检查时间、目标安装包与 SHA256 摘要，提供「检查插件更新」「更新插件」「刷新页面」三个动作；检查/更新状态写入独立的 `/tmp/online-upgrade-plugin-status.json`，不会覆盖固件任务进度。
- **发布清单与校验单**：CI 在发布前自动生成 `online-upgrade-manifest.json`（`schema` / `version` / `pkg_release` / `tag` / 每个架构的 `file`、`sha256`、`size`）与 `SHA256SUMS`，随安装包一起上传；发布标签严格为 `vMAJOR.MINOR.PATCH`，自更新忽略 draft、prerelease 与日期型标签。
- **安装版本事实来源**：`Makefile` 安装时写入 `/usr/share/online-upgrade/version`（内容为 `<PKG_VERSION>-r<PKG_RELEASE>`），避免只依赖运行中二进制或用户手工配置来判断本地版本。

### Changed

- **检测依据排版重构**：原先挤在一行里的判定文案与标签流改为「结论 → 短标签 → 关键事实网格 → 可展开证据链」四层结构；本机身份、机型通道、架构通道、版本通道、本地版本依据、校验依据、发布信息分别成格，长正则、板型与哈希允许任意换行，不再把标题挤成参差的胶囊行。Release 链接移到面板右上角，证据链默认折叠并显示条数。
- **全局排版统一**：统一卡片标题区间距、说明文字行高、按钮字号与内边距、表头字号/字距、标签芯片圆角与换行规则；桌面端保持三列统计卡，平板两列、手机单列，插件卡片在窄屏自动改为上下布局。
- **Release 产物命名统一**：安装包从 `luci-app-online-upgrade-<版本>-r<发布号>_<架构>.apk` 改为 `luci-app-online-upgrade_<版本>-r<发布号>_<架构>.apk`；CI 的版本校验、Release 说明与 README 同步更新，避免自更新解析时混淆包名、版本与架构分隔符。


### Fixed

- **插件页面不显示 LuCI 菜单栏**：`main.ut` 直接包含 `themes/<theme>/header`，绕过了 LuCI 基础 `header.ut` 中加载 `luci.js` 并初始化全局 `L` 的步骤；主题 footer 里的菜单脚本（如 Proton2025 的 `L.require('menu-proton2025')`）因此无法执行，`#mainmenu` 始终为空，页面只剩主题顶栏。现改为标准写法 `{% include('header') %}`，由基础模板负责包含主题 header、加载 `luci.js` 与初始化 `L`，主题侧边栏/顶部菜单恢复正常，`L.env.sessionid` 也能作为真实设备 ubus 会话的兜底。
- **点击「立即升级」报 `ubus error 2`，下载从未开始**：rpcd 依据 `args` 中每个提示值的**类型**（而非类型名）推导 blobmsg 策略，`args: { force: 'boolean' }` 中的字符串会落到 `BLOBMSG_TYPE_STRING`，前端传入的 JSON 布尔值因而被 `rpc_ucode_validate_call_args()` 判为类型不符并返回 `UBUS_STATUS_INVALID_ARGUMENT`（2）。改用字面量 `false` 后策略注册为 `Boolean`，`--force` 参数恢复正常。
- **「清空日志」被 ACL 拒绝**：`clear_log` 未写入 `luci-app-online-upgrade` 的 write 授权，LuCI 会话实测 `access:false`，前端调用会得到 `ubus error 6`（Permission denied）。已补齐授权。
- **复用旧固件只比对长度与时间戳**：`/tmp/firmware.img.gz` 残留时，旧实现只要「文件长度不为 0 且 `.ts` 里的 `updated` 与本次资产相同」就直接跳过下载并宣告成功——这与续传空洞是同源问题，长度正确并不等于内容正确。现在续传标记写入「`updated` 时间戳 + 完成文件的本地 SHA256」两行，复用前要求长度等于资产大小、标记两行齐备、且标记哈希与磁盘当前哈希一致；任何一项不满足就丢弃重下。标记只在文件通过校验后才写，因此「有标记」即代表「曾通过哈希闸门」。`download` 的复用分支同样执行完整校验并输出 `verified` / `hash_source` / `dry_run`，不再用一条精简状态绕过。
- **中断后无法真正续传**：每次开始下载前都会先 `cleanup_download()` 删掉固件与 `.parts.json`，于是上一进程留下的续传记录（哪怕安全）永远用不上，重启后续传只能从 0 开始；下载失败分支同样删除了可续传状态。现在开始下载前不再清理，失败时尊重下载器自身的判断（长度不一致才删），只有校验失败才整套清理。真机实测：`kill -9` 中断后复跑日志出现「续传 12 块」，最终 SHA256 与完整下载完全一致。
- **续传后固件出现“空洞”且仍被判为下载成功**：断点续传只校验文件长度，而长度恰好等于总大小并不能证明每个分块都写过——被 `set_len` 预分配过的分块是零填充的，最终得到一份长度正确但内容损坏的固件。现在恢复 sidecar 时要求数据文件存在且长度与总大小一致、分块大小与当前配置一致、且所有已完成分块的索引落在本次分块范围内；下载成功前要求**全部分块都为 done**，否则删除固件与 sidecar 并报错。
- **sidecar 与文件内容不匹配时被静默复用**：`.parts.json` 里记录的 `chunk` 与本次分块不一致、或残留已越界的分块索引时，旧实现会直接沿用，导致后续合并错位。现在这类 sidecar 一律不接受（丢弃并重新下载），失败中断时也只在长度正确的前提下保留 sidecar。
- **镜像前缀会收到 GitHub 令牌**：开启镜像加速时，`Authorization: Bearer <token>` 会被拼到镜像域名请求上，等于把令牌交给第三方。现在只有目标主机是 `api.github.com` 时才附加令牌；经镜像转发或回退到镜像下载的请求一律不带 Authorization。
- **校验单在双线路下只走一条**：读取 `sha256sums` 一类的校验文件时（直连/镜像两条线路）现在都校验 HTTP 状态码为 2xx，避免把镜像的 404/5xx HTML 当成校验单解析。
- **并发点击检测/升级互相踩状态**：`check` 与 `upgrade` 共用同一份状态文件，rpcd 又是 detached 启动，两次点击会同时写同一个临时固件与同一个状态文件。现在后端用 `/tmp/online-upgrade.lock`（`create_new` + `/proc/<pid>` 存活探测 + 崩溃后回收）做互斥，抢不到锁的进程只写日志并退出，**不覆盖**正在运行进程的进度；rpcd 层再做一次快速失败探测（`busy_pid`），第二次点击立即得到明确错误。真机复测又发现两处：主进程用 `process::exit` 结束，析构被跳过，锁文件每次运行后都留在 `/tmp`（一旦 pid 被复用就会被误判为仍在运行）；`busy_pid()` 用 `/^[0-9]+$/` 匹配整行、而锁文件是三字段格式，导致快速失败恒不触发。现在显式释放锁，且快速失败按首个字段取 pid。
- **前端在点击后立刻读到上一次运行的终态**：状态文件里留有上一次的 `failed` / `check_done`，首次轮询就把旧结果当成新结果（表现为点“检查更新”瞬间显示旧结论、点“升级”瞬间报失败）。状态文件现在写入 `pid`，前端在发起操作前快照旧 `pid`，只接受 `pid` 变化后的状态（不依赖浏览器与路由器时钟是否一致）；20 秒内拿不到新状态则提示“可能有另一个操作正在进行”。启动请求期间按钮立即禁用。
- **机型误判（把平台词当机型）**：旧实现按资产名里是否出现 `mediatek`、`mt7621` 这类平台/SoC 词判断机型，导致同平台不同板型被判为匹配。现在机型只由本机真实身份（`/tmp/sysinfo/board_name`、`/tmp/sysinfo/model`、`/etc/board.json` 的 `model.id`，缺失时才用 `ubus call system board`）与文件名里的机型令牌做结构化比对，平台/SoC/架构/发行版/镜像类型词被明确排除在机型证据之外；对不上时如实给出「机型未匹配 / 部分匹配」，不降级为匹配。
- **产物名不含架构时被当成“通用”放行**：现在架构未知只记为 `undeclared`（资产未声明），不再视为通用并放行；无架构证据的文件只有在用户配置的正则命中时才可用，且界面与日志会写明“资产未声明架构”。
- **版本解析不可靠**：改为通用解析（多段版本、`-rc`/`-beta`/`-alpha` 后缀、有无 `v` 前缀、版本号不在首段、日期/构建号 tag），取最后一个候选以避免把厂商版本号与 SoC/架构数字当成固件版本；解析不到就明确走「无法比较」分支，不编造版本。
- **期望哈希缺失时“假装校验通过”**：上游既无校验单也无正文哈希时，旧文案仍宣称已完成 SHA-256 校验。现在区分 `expected_sha256` / `release_asset` / `release_body` / `none` 四种来源，`none` 时明确标注“无校验依据，仅记录本地哈希”，不宣称校验通过。
- **代理前缀拼接错误**：`https://host` 这类没有结尾斜杠的前缀会被拼成 `https://hosthttps://...`。现在统一归一化（去尾斜杠后补一个 `/`）并校验主机非空；ucode 层与 uci-defaults 自动重装脚本使用同一规则，非法代理降级为直连并记日志。
- **校验单出现多个哈希时取第一个**：多条目校验单里取的应该是与固件 basename 匹配的那一行；只有全文仅存在一个去重哈希时才接受单哈希解析，避免拿别的文件的哈希做比对而永久不匹配。
- **备份失败后留下错误的“已升级”标记**：现在先记录版本再备份，备份失败则回滚 `last_upgrade_ts` / `last_upgrade_version` 并中止，不再让下一次检测误判成“已是最新”；`/root` 备份复制失败也从“静默忽略”改为报错退出。
- **自动重装脚本吞掉错误且只支持 apk**：`90-online-upgrade-auto` 现在同时支持 `apk` 与 `opkg`，按本机真实包管理器架构筛选安装包（不再“取第一个”），缺 `jsonfilter` 时用等效文本提取兜底，缺 `curl`、下载失败、安装失败都会写 syslog 并以非零码退出；代理前缀同样做归一化。真机复测又发现：`apk --print-arch` 在 apk-tools 3 上返回的是家族名 `aarch64`，而包名用的是 `aarch64_cortex-a53`，按家族名筛会把 cortex-a53 与 cortex-a72 的包一起命中、再取第一个就可能装错；现在优先读 `/etc/openwrt_release` 的 `DISTRIB_ARCH` 做完整架构匹配，家族名只作兜底，同架构多候选时把全部候选写进日志。
- **`app.js` 缓存版本与包版本漂移**：`app.js?v=` / `style.css?v=` 曾写死为 `2.7.0` 而 `PKG_VERSION` 为 `2.2.0`，导致升级后浏览器仍用旧脚本。现在统一以 `PKG_VERSION` 为准（本版为 `2.3.1`），并在 CI 增加一致性校验（Makefile / Cargo.toml / 模板）。
- **README 与真实配置键不一致**：示例里的 `config online-upgrade 'main'` / `online-upgrade.main.*` 与代码实际使用的 `settings` 段不符，照抄会写坏配置；已全部改为 `settings`，并修正示例正则的重复转义。
- **非法 `latest@` 正则被静默忽略**：`Regex::new` 的编译错误被 `.ok()` 吞成 `None`，`release_candidates` 因此不做任何过滤，等于在全部最近 Release 里挑，可能选中非预期渠道的产物。现在 `check` 与 `probe` 两处都走同一个 `parse_tag_filter`，非法正则按配置错误明确报错并中止，不再退化为“不过滤”。
- **空锁或半写锁被当陈旧锁回收**：锁文件先 `create_new` 建好、后写入 PID，并发进程在两步之间读到空锁时，旧逻辑直接删锁重试，两个进程于是同时持锁写同一份固件与状态文件。现在按锁文件 mtime 判定新鲜度：2 秒宽限期内不可读一律按“忙”返回，超过宽限期才回收，避免崩溃残留永久占锁。
- **撞锁时前端把 `ok:false` 当成“已启动”**：ubus 在 `result[0]==0` 时直接 resolve 业务 payload，`{ok:false,error}` 不会 reject。原实现继续进入轮询并隐藏两个升级按钮（`check` 则白等 20 秒给泛化提示），用户只能刷新页面。现在两处都先判 `ok`，立即显示后端错误、恢复按钮、不进入轮询。

### Added

- **界面展示判定依据**：检测结果卡片现在逐条显示命中哪条规则、比对了哪个标识（本机标识 / 架构 / 资产声明架构）、版本关系与哈希来源；判定失败时能看出是哪一步没通过（架构冲突 / 资产未声明架构 / 机型未匹配）。
- **识别与判定矩阵文档**：README 新增「识别与判定矩阵（机型 × 架构 × 版本 × 自用构建）」章节，说明三通道输入来源、九种判定组合、版本四种关系与「自用构建」兜底路径。
- **固件下载线路自动回退**：优先使用所选镜像/直连的 Release 下载地址，失败后自动回退到 GitHub API 资产直链（`api.github.com/repos/<owner>/<repo>/releases/assets/<id>` + `Accept: application/octet-stream` → 签名 CDN）。`github.com` 网页前端返回 5xx / 超时（Release 下载地址由该域名提供）时，升级流程仍可完成。下载请求统一携带 `Accept: application/octet-stream`，该头是 API 资产端点重定向到文件（而非返回 JSON 元数据）的必要条件。
- **`download` 子命令**：只下载并校验固件、不写 flash，状态文件带 `dry_run="true"` 与 `verified` 字段，可用于在设备上验证下载/续传/校验链路。
- **版本方向判定**：检测结果新增 `version.relation`（`newer` / `same` / `older` / `unknown`），目标版本更低时默认拒绝执行升级，需 `--force` 才会继续。

## [2.4.0] - 2026-11-10

> 发布标签 `v2.4.0`，安装包 `luci-app-online-upgrade-2.4.0-r1_<arch>.apk`。标签、Release 标题与产物版本均由 `Makefile` 的 `PKG_VERSION` / `PKG_RELEASE` 派生，Release 正文自动同步自本条目；CI 在发布前校验安装包文件名版本一致。

### Added

- **下载任务管理（开始 / 暂停 / 继续 / 取消 / 删除历史任务）**：新增 `taskctl` 模块，提供跨进程控制文件 `/tmp/online-upgrade-control.json` 与持久化历史 `/root/.online-upgrade-tasks.json`（最多 30 条，重启不丢）。控制指令由下载进程的 Tokio 任务轮询后经 `tokio::sync::watch` 广播给每个分块 Worker；暂停在分块边界生效，不悬挂 HTTP 连接，继续时从 `.parts.json` 断点续传。新增 `download / pause / resume / cancel / tasks / task-delete <id> [--force]` 子命令，rpcd 增加 `start_download` 等 6 个方法与 LuCI「下载任务管理」卡片。删除历史任务时若已无「已完成」记录，会一并清理 `/tmp/firmware.img.gz` 及 `.ts` / `.parts.json`。
- **慢速保护（自动停止并抛出原因）**：每秒采样实测吞吐，连续低于 `dl_min_speed_kb`（默认 32 KB/s）达 `dl_slow_window_sec`（默认 15 秒）即终止下载，状态文件、日志、任务历史三处输出具体原因（平均速度、阈值、观测窗口、当前线路）并列出可切换节点；`dl_slow_grace_sec`（默认 10 秒）覆盖 TLS 握手 / TCP 慢启动，`dl_min_speed_kb=0` 可关闭保护。停止属于任务级终止，**不触发**线路回退，断点续传记录保留。进度文案同时显示实时速度。
- **加速节点实测更新**：`github.rs` 新增 `MIRRORS`，前端快捷按钮由 3 个扩充到 7 个已实测可用节点（`gh.acg2.mom`、`ghfast.top`、`ghproxy.net`、`gh-proxy.com`、`ghproxy.imciel.com`、`ghproxy.monkeyray.net`、`ghproxy.vip`），移除不再解析的 `github.moeyy.xyz`。2026-11-10 逐个验证均返回同一份 709 字节文件内容。

### Changed

- **下载错误分类**：`downloader::download` 现在返回 `DownloadError`（`Stopped(StopReason)` / `Other(String)`），区分用户取消、慢速保护与线路级网络失败；只有后者才会回退到 GitHub API 资产直链，避免把「用户点了取消」当成线路故障静默换源重下。
- **Tokio 原生并发控制**：Cargo 启用 `tokio/sync`，数据块循环用 `tokio::select!` 同时等待响应流与 `watch::changed()`（cancel-safe），停滞看门狗统一使用 UCI 的 `dl_stall_sec`，`Mutex` 中毒改用 `into_inner()` 恢复而非 `unwrap()`。
- **静态资源缓存版本**：`PKG_VERSION` / `Cargo.toml` / `Cargo.lock` / `main.ut` 的 `?v=` 同步到 `2.4.0`。
- **Release 标签与产物版本统一**：CI 不再用 `v2.0.<github.run_number>` 作为标签（曾出现标签 `v2.0.17` 却挂载 `2.3.2-r1` 安装包的情况），改由 `verify` 任务输出 `PKG_VERSION` / `PKG_RELEASE`，发布标签固定为 `v<PKG_VERSION>`、标题同步；发布前逐个校验安装包文件名前缀，重复构建时先清理同名旧产物再上传，Release 正文由 `CHANGELOG.md` 对应版本段落自动生成。

### Fixed

- **watcher 初始化竞态**：控制文件轮询的基线 seq 若在异步任务内部读取，可能与启动后立即写入的暂停 / 取消命令竞争，导致该命令被当成基线而永不生效。现在在 `spawn` 之前同步读取基线；`taskctl::tests::watcher_publishes_commands` 覆盖该场景。
- **预分配文件被截断**：`File::options().create(true)` 未声明 truncate 行为，clippy 要求显式指定；断点续传必须保留已写入分块，现显式 `.truncate(false)`。
- **新版 Rust 1.93 clippy 门禁**：顺带修复基线代码在新 lint 下的告警（`explicit_auto_deref`、`useless_format`、`doc_lazy_continuation`、`vec_init_then_push`、`manual_is_multiple_of`、`invalid_regex` 测试），`cargo clippy --all-targets -- -D warnings` 恢复全绿。

## [2.3.2] - 2026-11-09

### Changed

- **Hero 头部横幅全端瘦身**：PC 端压缩卡片内边距（`28px 36px` → `16px 28px`）、元素间距、标题/描述/徽章字号与插画尺寸（`380px` → `300px`、最大高度 `160px` → `118px`），横幅实测高度由 224.7px 降至 176.6px（-21%），首屏可多露出一行功能卡片。
- **平板不再把 Hero 拆成上下两栏**：`992px` 断点原本改为 `flex-direction: column` 并将插画整幅下移，横幅被撑到 398.7px；现在保持横向双栏、仅把插画收窄至 220px，高度降至 175.1px（-56%）。
- **手机端 Hero 改为紧凑头部（本次重点）**：`768px` 以下隐藏 460×170 装饰 SVG（窄屏比例失真且纯占高度）、取消三个特性徽章的纵向堆叠改为横向紧凑徽章、内边距与字号整体收敛，并新增 `380px` 以下超小屏微调；描述文案完整保留、不做截断，仍遵循「无截断文本排版」。Chromium 无头实测：375×812 下横幅由 421px 降至 198.8px（-53%），320×700 下由 441.8px 降至 198.8px（-55%）；「固件状态」卡片顶部从 449px 上移到 216.8px，首屏无需滚动即可看到完整操作按钮与四项状态指标。
- **静态资源缓存版本同步**：`style.css?v=` / `app.js?v=` 随 `PKG_VERSION` 提升到 `2.3.2`（`Makefile` / `Cargo.toml` / `Cargo.lock` 同步），避免升级后浏览器继续使用旧样式；CI 一致性校验要求四处版本相同。

## [2.3.1] - 2026-11-09

### Fixed

- **自用 SNAPSHOT 固件因取不到版本号被挡住升级**：`DISTRIB_RELEASE` 字面是 `SNAPSHOT`，旧实现只认这一处，解析不出本机版本，版本方向恒为 `unknown`，界面只给「当前固件版本无法解析（SNAPSHOT），Release 版本 vX，请确认后升级」，把自用构建的设备挡在升级门外。现在本机版本按可信度逐级探测（构建写入的版本文件 → `DISTRIB_VERSION` / `DISTRIB_RELEASE` / os-release `VERSION_ID` → `DISTRIB_DESCRIPTION` / `DISTRIB_REVISION` / `BUILD_ID` / `/etc/openwrt_version` → `base-files` 包版本 → 固件构建时间），并把命中来源随结果返回（`local` / `local_source` / `local_source_text` / `local_confidence`）；判定区分 `decidable`、`suspect`、`incomparable` 三态，确实无法比较时不再阻断，而是按「将升级到该发布版本」放行并说明原因，机型匹配、架构匹配与 SHA-256 校验照旧生效，只有降级才要求二次确认。
- **同版本仍提示「有新版本」并把普通升级当主操作**：`relation == same` 且 Release 因重新构建而更新时间变化时，旧逻辑给出 `has_update=true`，界面显示「有新版本」并摆出普通升级按钮，容易被误当成升级。现在同版本一律 `has_update=false`，文案写明「已是最新（同版本）」；前端不显示「有新版本」角标、不展示普通升级按钮，只保留手动点击的「同版本重装（强制升级）」；后端在不带 `--force` 时同样拒绝同版本。降级需确认、无法比较放行的口径不变。
- **构建时间与普通发布号混比得出错误方向**：自用构建只能拿固件构建时间当版本时，旧实现把 `26.11.07` 直接和 `24.10.0` 这类发布号按数字比较（`26 > 24`），把本可升级的新 Release 判成「低于当前」而拒绝升级，首段数字更大的老发布则会被误判成可升级。现在先确认目标本身是日期型标签（`YY.MM.DD` / `YYYY.MM.DD`，且月日合法），只有日期型目标才用构建时间比较并按目标年份位数对齐；目标不是日期形态时不比较，落到 `incomparable` 按「将升级到该发布版本」放行并说明原因。
- **按钮的 `display:none` 被样式里的 `!important` 压过**：`.ou-container .ou-btn { display: inline-flex !important }` 使内联 `display:none` 失效，同版本场景下普通升级按钮仍然可见。现在改用 `is-hidden` 类配合 `display:none !important`，模板初始 class 与 `showBtn()` 同步切换，按钮显隐与判定状态保持一致。
