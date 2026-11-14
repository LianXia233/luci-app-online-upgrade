# luci-app-online-upgrade v2.6 完整实施报告

> 目标：把现有插件改造为**不依赖特定设备、不依赖特定 CI 仓库、不依赖固定固件命名规则**的通用 OpenWrt / ImmortalWrt 在线固件检测与升级插件。
> 技术栈保持：Rust + Tokio 后端、rpcd Ucode 桥接、原生 JavaScript / CSS / HTML5 前端。
> 版本：`2.5.6` → `2.6.0` → `2.6.1` → `2.6.2`（第十四节为补丁记录）。
> 验证：构建 / 单测 / 静态校验 + **官方 OpenWrt 25.12.0 QEMU 真实虚拟机 5 轮**（见第十节）。

---

## 一、原项目问题（阶段 1 审查结论）

| # | 问题 | 影响 |
| :--- | :--- | :--- |
| 1 | 检测 / 下载 / 升级 / 兼容性全部由一份 `/tmp/online-upgrade-status.json`（`stage/pct/msg/error/result`）承载 | 一次 API 失败覆盖已获取的真实状态；四种信息无法独立存在 |
| 2 | 完全没有 GitHub Actions 与 Artifacts 概念 | 正在构建、构建失败、仅 Artifacts、产物过期等场景无法解释 |
| 3 | Release 检测与兼容性判定混在一个 `result` 里 | 构建状态与「有没有可刷写固件」被混为一谈 |
| 4 | 仅靠文件名判断设备适配 | SDK / rootfs / 校验文件易被当成固件；压缩包内镜像会漏掉 |
| 5 | HTTP 403 处理粗糙 | 匿名 API 限流与权限错误无法区分，容易被误报成「没有 Release」 |
| 6 | 无独立缓存、无请求去重、无并发限制、无失败退避 | 匿名 60 次/小时额度极易耗尽，且失败后无可用数据 |
| 7 | 设备信息维度不足 | Target / Subtarget / CPU 架构 / Package Arch / ABI 容易混淆 |
| 8 | 前端仅靠颜色/进度表达状态 | 状态无法文字化解释，桌面与移动端信息密度不可控 |

样本仓库 `LianXia233/OpenWRT-CI`、`LianXia233/H5000M-CI-Qmodem`、`LianXia233/OpenWRT-CI-H5000M` 仅用于兼容性测试，**未写入任何代码、配置或默认值**。

---

## 二、架构改造总览

```text
                        WebUI（原生 JS + CSS）
      ┌──────────────────────┼──────────────────────┐
      │ get_states / refresh_states / probe_repo /    │
      │ netcheck / clear_states_cache（新增 7 个 RPC）│
      ▼                                              ▼
 rpcd Ucode 桥接（保留全部 23 个旧方法 + ACL）  旧 check/download/upgrade 流程
      │                                              │（完全未改）
      ▼                                              ▼
 Rust CLI：states / device / refresh / probe-repo / netcheck / clear-cache
      │
      ├── actions.rs ──── actions.json ─┐
      ├── releases.rs ─── releases.json ─┤  四套独立状态模型
      ├── artifacts.rs ── artifacts.json┤  （各自缓存 / TTL / 刷新 / 错误处理）
      └── compatstate.rs ─ compat.json ──┘
                    │（只读快照）
                    ▼
            diagnostics.rs（统一诊断层，不可变引用）
                    │
            states.rs（对外聚合，不是第五套状态模型）

 辅助：ghapi.rs（API/限流/脱敏）  cache.rs（Envelope）
       device.rs（设备信息）      firmware.rs（固件发现/安全解析）
       network.rs（线路策略）    model.rs（元数据框架，禁止统一状态机）
```

**关键约束**：`model.rs` 只提供元数据展示框架，明确注释「没有、也绝不允许出现统一的 `upgrade_status` / 单一状态机」；诊断层签名全部为不可变引用，编译期阻止写回模型。

---

## 三、四套完全独立的状态模型

| 模型 | 模块 | 独立缓存 | 默认 TTL | 状态数 | 只回答什么 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| GitHub Actions | `actions.rs` | `actions.json` | 120s | 17 | 远程 CI 构建任务状态 |
| GitHub Releases | `releases.rs` | `releases.json` | 300s | 9 | 正式发布与发布资产 |
| Actions Artifacts | `artifacts.rs` | `artifacts.json` | 180s | 11 | 临时构建产物（会过期） |
| 固件兼容性 | `compatstate.rs` | `compat.json` | 600s | 10 | 某个固件是否适合本机 |

- 六维隔离：数据结构、状态枚举、缓存文件、TTL、刷新函数、错误处理。
- `refresh --only <model>` 只刷新指定模型，其余缓存文件不动。
- API 失败时用 `model::replace_api()` **只替换 `api` 元数据**，`state` / `data` 保持上次成功值。
- `cache::Envelope` 显式记录 `stale` / `from_cache` / `last_success_at`，缓存绝不冒充实时数据。
- GitHub Actions **严格区分 `status` 与 `conclusion`**：仅 `status=completed` 才依据 conclusion 判定；`queued` / `requested` / `waiting` / `pending` / `in_progress` 一律显示进行中并附「尚未产生最终结论」。

详见 [docs/STATE_MODELS.md](docs/STATE_MODELS.md)。

---

## 四、统一诊断机制

`diagnose(&ActionsModel, &ReleasesModel, &ArtifactsModel, &CompatModel, Option<&ProbeResult>)` 只读汇总，输出 9 个中文章节：`network` / `actions` / `releases` / `artifacts` / `firmware` / `compat` / `availability` / `blocking` / `next`。

典型结论：

| 场景 | 结论 |
| :--- | :--- |
| 构建失败 + 有历史 Release | 最新构建失败，但历史正式发布仍可能可用 |
| 无 Release + 有 Artifacts | 没有正式 Release，不代表没有可用固件 |
| API 限流 | GitHub API 请求受到限流，暂时无法获取最新构建状态，已保留上次成功查询的数据 |
| 构建成功但无可识别固件 | 构建成功，但未识别到可刷写镜像；不等于设备不兼容 |
| 兼容性无法确认 | 当前设备的兼容性尚无法确认，因此暂不允许自动刷写 |

单测保证：构建失败不覆盖 Release、无 Release 不覆盖 Artifact、API 失败不显示编译失败、诊断层不修改模型。

**真实 VM 实测补充**：在 OpenWrt 25.12.0 QEMU 中真实触发匿名限流后，`get_diagnostics` 返回 `summary=已经找到固件文件，但与当前设备明确不兼容，已禁止自动刷写`、`can_upgrade=false`、`blocking_reasons=架构冲突：固件声明 [aarch64]，本机为 [x86-64-x86-64]`；同时 `actions` 章节输出「仍在 正在编译；不据此判定整个仓库构建失败」、`network` 章节输出「正常（HTTP 200，370 ms）」。四种状态同时成立且互不覆盖，见 [docs/TEST_REPORT.md](docs/TEST_REPORT.md) 第 5.5 节。

---

## 五、通用仓库识别与设备兼容性

**仓库解析**（`ghapi::parse_repo`）：支持 `owner/repo`、完整 GitHub URL、`/tree/<分支>`、`.git`、SSH 形式；拒绝注入字符与非 GitHub 主机。自动读取仓库公开信息、默认分支、`/actions/workflows` 全部工作流、最近 30 条运行记录，支持 `build_branch` 与 `workflow_filter` 筛选。公开仓库**优先匿名调用**，不强制 Token。

**设备信息**（`device.rs`）：型号、设备名、board_name、board_id、Device Tree compatible、Target / Subtarget、CPU 架构、Package Arch / ABI、固件版本、发行版、内核、系统标识、升级方式、平台标识；来源覆盖 `/etc/openwrt_release`、`/etc/os-release`、`/tmp/sysinfo/*`、`/etc/board.json`、`ubus call system board`、`/proc/device-tree/compatible`、`/proc/cpuinfo`、`uname`、`apk/opkg`。识别失败保留 `unknown`，**绝不推测**；五个易混维度互不替代。

> 真实 OpenWrt 25.12.0 回归修复：`DISTRIB_ARCH='x86_64'` 曾被误拆成 `abi="64"`（把 CPU 位宽当 ABI）。现由 `derive_abi()` 统一判定——后缀必须含 ASCII 字母才视为 ABI 变体，`x86_64` / `aarch64` / `riscv64` 一律保留 `unknown`。VM 输出 `"abi":"unknown"`、`unknown_fields:["compatible","abi"]`。

---

## 六、固件发现、识别与五层兼容性

- 来源：Release Assets / Actions Artifacts / 自定义公开地址。
- 格式：`.bin` / `.img` / `.gz` / `.xz` / `.zst` / `.zip` / `.7z` / `.tar*` / `.ubi` / `.trx` / `.chk` / `.vmdk` 等。
- 用途分类：sysupgrade / factory / U-Boot / EFI / rootfs / kernel / packages / SDK / Toolchain / Debug / 校验文件 / 构建日志 / 归档 / 未知。短词使用 token 精确匹配，避免 `somefile.bin` 被 `efi` 误判。
- ZIP **只读取中央目录、绝不落盘解压**；拦截路径穿越、绝对路径、NUL、反斜杠、驱动器前缀、异常条目数与解压炸弹。
- 五层验证：L1 文件名/格式 → L2 内部元数据 → L3 设备维度比对 → L4 下载后完整性（HTTP 响应 / 大小 / SHA-256 / 实际格式）→ L5 `fwtool` + `sysupgrade -T`（只读测试）。
- 下载前绝不宣称 `compatible`；**「无法确认兼容」≠「明确不兼容」**；未通过安全检查不允许自动刷写；明确不兼容必须给出具体原因。
- 下载/升级流程通过 `compatstate::record_download_result()` 把 L4/L5 真实结果回写 compat 自己的缓存，供 WebUI 与诊断层只读消费；该函数不参与刷写决策。

---

## 七、网络、缓存与并发优化

| 能力 | 实现 |
| :--- | :--- |
| 匿名优先 | 公开仓库默认匿名；可选 Token 仅从 UCI 读取、仅在 `api.github.com` 发送 |
| 限流处理 | 读取 `X-RateLimit-*` / `Retry-After`；403 严格区分限流与权限；429/5xx 按退避重试 |
| 错误分类 | `NetClass` 14 类：DNS / TCP / TLS / 超时 / 403 / 404 / 429 / 5xx / 内容异常 / 限流 / 加速源 / 官方 / 正常 / 未知 |
| 请求去重 | `Hub` 进程内 `memo` + `Semaphore` 并发限制 + 共享 `Client` 连接池 |
| 失败退避 | `FAILURE_BACKOFF_SECS`，不无限重试、不高频轮询 |
| 缓存 | 四套独立 Envelope + 独立 TTL + 原子写入 + 体积上限 + stale 标记 |
| 线路策略 | **API 与下载分离**：API 默认直连 `api.github.com`；下载加速源优先、失败自动回退官方直连；下载 URL 只对下载主机加前缀，不改写 API 参数 |
| TLS | 明确不调用 `danger_accept_invalid_certs` / `danger_accept_invalid_hostnames`；VM 内真实 HTTPS 200 佐证 |
| 下载链路增强 | `fetch_firmware()` 线路候选统一为「加速源 → 官方直连 → API 资产直链」，并保留旧前缀线路兼容非 GitHub 主机 |

---

## 八、安全机制

| 层面 | 措施 |
| :--- | :--- |
| 下载 | 线路失败才切换；用户取消/慢速中止为终止性，不被镜像静默重试掩盖；切换线路时清理半成品与续传 sidecar |
| 完整性 | SHA-256（配置 / 校验单 / Release `digest` 三来源）；无校验依据时明确「仅记录本地哈希，未宣称已校验」 |
| 归档安全 | 只读清单解析；恶意路径 / 解压炸弹 / 结构损坏一律拒绝（`force` 也不豁免）；>256MB 拒绝避免耗尽内存；归档内无可刷写镜像时拒绝直接刷写 |
| 本机验证 | `fwtool` 元数据 + `sysupgrade -T`；失败即阻断（非强制模式） |
| 临时文件 | 原子写缓存、独立临时目录、不覆盖既有文件 |
| 注入防护 | 仓库地址、文件名路径、ZIP 条目、配置字符串均校验；SQL/命令参数不拼接 shell |
| 脱敏 | Token 不进入代码 / 配置 / 日志 / 前端 / 错误消息 / 交付文件；`ghapi::redact()` 兜底；全仓扫描 0 命中 |
| 真实刷写 | **未执行**；VM 内仅做 `sysupgrade -T` / `fwtool` 只读验证 |

---

## 九、RPC / UCI / WebUI 与向后兼容

- **RPC**：保留 23 个旧方法、参数、返回字段与 ACL；新增 7 个方法单独追加到 read/write 分组（共 30 个，静态校验无缺失/重复）。
- **UCI**：新增 10 个可选字段，中性默认值；`uci-defaults` 用 `_set_default()` 只补缺失、不覆盖历史配置；`repo_url` 为空时回退旧 `repo`。
- **WebUI**：新增「远程状态与统一诊断」卡片（诊断结论、设备信息、四面板、网络策略），新增 3 个通用仓库/筛选配置控件与 3 个诊断工具按钮；状态文字 + 色标双表达；760px / 380px 单列适配。
- **旧流程**：`check` / `download` / `upgrade` 与刷写安全标准未改；新增只读/刷新命令不写全局 `status.json`、不占用升级操作锁。
- **版本戳**：`Makefile` / `Cargo.toml` / `Cargo.lock` / `app.js?v=` / `style.css?v=` 在 v2.6.0 阶段统一 `2.6.0`，v2.6.1 补丁同步升至 `2.6.1`。

详见 [docs/RPC_AND_CONFIG.md](docs/RPC_AND_CONFIG.md)。

---

## 十、测试结果

| 项目 | 结果 |
| :--- | :--- |
| `cargo check` | 通过，**0 warning** |
| `cargo test` | **160 passed / 0 failed**（基线 72；含 `derive_abi()` 10 组回归断言） |
| `cargo build --release` | 成功 |
| 静态 PIE 构建 | 成功（`static-pie linked`），注入 OpenWrt rootfs 可直接运行 |
| 静态与接线综合校验 | **24 / 24 PASS**（版本、语法、RPC/ACL、Ucode 括号、模板标签、UCI、前端接线、LF、脱敏、产物） |
| 真实匿名 API 冒烟 | `probe-repo` 成功发现 `default_branch=main` 与 **12 个 Workflow**；限流正确分类；`netcheck` HTTP 200 |
| **官方 OpenWrt 25.12.0 QEMU 真实虚拟机** | **5 轮启动全部 `rc=0`**，135 条 `OPTEST:` 证据；rpcd/ubus、四套缓存、设备信息、真实 API + 限流、诊断层、`sysupgrade -T` 全部实测 |
| 缺陷修复 | **17 项**（12 项开发期 + 4 项真实 VM 暴露 + 1 项 v2.6.1 WebUI 权限） |

### 10.1 真实 OpenWrt 25.12.0 QEMU 虚拟机验证摘要

| 维度 | 实测结果 |
| :--- | :--- |
| 镜像完整性 | 官方 `combined-ext4.img.gz` / `rootfs.tar.gz` SHA256 双次校验 OK；内核 `6.12.71`；`DISTRIB_RELEASE=25.12.0` |
| 启动链路 | GRUB → Linux → procd → `rc.local` → `poweroff`，5 轮 QEMU 退出码全部 0 |
| 设备信息 | `target=x86` / `subtarget=64` / `cpu_arch=x86_64` / `pkg_arch=x86_64` / **`abi=unknown`**（修复后）；`apk=x86_64`、`opkg` 不存在 |
| rpcd Ucode | `ubus call luci.online-upgrade get_states / get_device / probe_repo / netcheck / get_diagnostics` 均返回；`ucode -c` rc=0 |
| 四套缓存 | `actions.json` / `releases.json` / `artifacts.json` / `compat.json` / `network.json` 独立生成 |
| 网络 | `virtio-net-pci` + SLIRP：`br-lan 10.0.2.15`、默认路由 `10.0.2.2`、ping 0% 丢包、DNS `api.github.com → 20.205.243.168` |
| 真实 API | `netcheck` HTTP 200 / 370–400 ms / `rate_remaining` 26→0→20→16；真实触发 `rate_limited` 并正确保留状态 |
| 统一诊断 | `can_upgrade=false`，架构冲突给出具体原因；Actions 进行中不判失败；网络正常独立成节 |
| 本机验证 | `sysupgrade -T` `rc=1`（`Image check failed.`）、`fwtool -i` `rc=1`（`unable to extract trailer header`） |
| 安全 | 全程**未执行真实刷写**，仅只读验证 |

> 完整证据表、分步执行记录、镜像注入方法（无 KVM / 无 loop 设备下的 `mkfs.ext4 -d` + `dd` 方案）、4 项 VM 缺陷修复与已知限制见 [docs/TEST_REPORT.md](docs/TEST_REPORT.md) 第 5、6、8 节。

完整矩阵与证据见 [docs/TEST_REPORT.md](docs/TEST_REPORT.md)；错误码与排查步骤见 [docs/ERROR_DIAGNOSTICS.md](docs/ERROR_DIAGNOSTICS.md)。

---

## 十一、完整变更清单

### 11.1 修改的文件（14 个，+1262 / -14 行）

| 文件 | 变更 |
| :--- | :--- |
| `Makefile` | `PKG_VERSION` 2.5.6 → 2.6.0 |
| `rust/Cargo.toml` / `rust/Cargo.lock` | crate 版本 → 2.6.0（未新增大型依赖） |
| `rust/src/main.rs` | 声明 12 个新模块；新增 `states` / `device` / `refresh` / `probe-repo` / `netcheck` / `clear-cache` 子命令与 `arg_value()`；扩展 `usage()` |
| `rust/src/upgrade.rs` | 下载线路接入 `network::download_routes()`；新增 `inspect_downloaded_archive()` 压缩包安全前置；L4/L5 结果回写 compat 缓存；**刷写决策逻辑未改** |
| `CHANGELOG.md` | 新增 `## [2.6.0] - 2026-11-14`（Added / Changed / Fixed / Security），补记尾斜杠 404 与 ABI 修复；原 `Unreleased` 与 2.5.6 历史完整保留 |
| `README.md` | 新增 v2.6 通用化架构章节；UCI 表 +10 行、示例 +10 行；CLI +8 行、Ubus +8 行；新增「错误诊断与状态说明」章节 |
| `root/etc/config/online-upgrade` | +10 个 v2.6 可选字段（中性默认值） |
| `root/etc/uci-defaults/99-online-upgrade` | 新增 `_set_default()` 与 10 个字段默认值，不覆盖历史配置 |
| `root/usr/share/rpcd/acl.d/luci-app-online-upgrade.json` | read 组 +5、write 组 +2 个新方法授权 |
| `root/usr/share/rpcd/ucode/luci.online-upgrade` | 保留全部旧方法；`set_config` 接纳 10 个新字段并校验；新增 7 个 RPC 方法 |
| `root/usr/share/ucode/luci/template/online-upgrade/main.ut` | 新增「远程状态与统一诊断」卡片、3 个配置控件、3 个诊断工具按钮；版本戳 → 2.6.0 |
| `root/www/luci-static/resources/online-upgrade/app.js` | 新增四套模型/诊断/设备/网络渲染、请求去重、有限次刷新读取、3 个诊断工具函数、移动端分区；`fillConfig` / `saveConfig` 支持 3 个新配置字段 |
| `root/www/luci-static/resources/online-upgrade/style.css` | 新增 v2.6 状态卡片、诊断色标 + 文字、工具按钮行、760px / 380px 单列适配与长文本换行 |

### 11.2 新增的 Rust 模块（12 个，约 6.6k 行）

| 模块 | 职责 |
| :--- | :--- |
| `cache.rs` | 7 个独立缓存槽位、TTL、失败退避、`Envelope`、原子写、stale 标记 |
| `model.rs` | `ApiMeta` / `frame` / `replace_api` 元数据框架（明确禁止统一状态机） |
| `device.rs` | 通用设备信息采集与标准化，多来源兜底、unknown 保留；**新增 `derive_abi()` 修复 `x86_64` 误推导 ABI（真实 25.12 VM 回归）** |
| `ghapi.rs` | 仓库解析、限流、错误分类、Token 脱敏、请求去重、并发限制、公开信息发现；修复空 suffix 尾斜杠 404 |
| `firmware.rs` | 格式/用途分类、ZIP 安全解析、多维度匹配与置信度 |
| `actions.rs` | 状态模型 1/4：Actions（status/conclusion 严格分离） |
| `releases.rs` | 状态模型 2/4：Releases 与资产分类 |
| `artifacts.rs` | 状态模型 3/4：临时构建产物（含过期/匿名不可下载） |
| `compatstate.rs` | 状态模型 4/4：五层兼容性、下载后结果回写 |
| `network.rs` | 真实 HTTP 探测、线路策略、错误分类、缓存 |
| `diagnostics.rs` | 统一诊断层（只读、9 章节、规则矩阵、中文结论） |
| `states.rs` | 对外只读聚合与并发刷新编排（非状态模型） |

### 11.3 新增文档（5 个）

| 文件 | 内容 |
| :--- | :--- |
| `docs/STATE_MODELS.md` | 四套状态模型与统一诊断层设计、状态码表、Envelope、诊断规则矩阵 |
| `docs/RPC_AND_CONFIG.md` | 30 个 RPC 总表、新增 7 个方法定义、CLI 子命令、UCI 10 字段、兼容策略 |
| `docs/ERROR_DIAGNOSTICS.md` | `ApiError` / `NetClass` 码表、中文提示、缓存新鲜度、分步排查、Token 脱敏 |
| `docs/TEST_REPORT.md` | 5 类测试矩阵、24 项静态校验、真实 API 冒烟证据、**OpenWrt 25.12.0 QEMU 5 轮虚拟机证据**、17 项缺陷修复记录、已知限制 |
| `IMPLEMENTATION_REPORT.md` | 本文件（完整实施报告 + 变更清单 + VM 验证摘要） |

---

## 十二、后续问题与建议

| # | 事项 | 建议 |
| :--- | :--- | :--- |
| 1 | **rpcd Ucode / `board.json` / `fwtool` / `sysupgrade -T` 已在 OpenWrt 25.12.0 QEMU 虚拟机实测通过**，但尚未在物理嵌入式设备验证 | 在目标设备执行 `online-upgrade device` 与一次「下载并校验」；Device Tree `compatible` 在 VM 中为空已正确显示 `unknown` |
| 2 | VM 使用 glibc `static-pie` 二进制注入，非 SDK 产出的 musl 二进制（`rustup target add musl` 在沙箱卡住） | CI 产出 musl 二进制后在实机复测一次 |
| 3 | aarch64 / x86_64 musl 交叉编译与 `.apk` 打包未在本地执行 | 推送后查看 GitHub Actions `verify` → `rust` → `package` → `release` 作业 |
| 4 | 真实刷写未执行（本次未做任何危险实机操作） | 按既有 `upgrade` 流程在实机操作 |
| 5 | 匿名 API 60 次/小时（共享出口 IP）限制；VM 内真实触发 `rate_limited`，`probe-repo` 未能返回 `ok=true` | 成功证据来自宿主沙箱 / chroot 宿主网络实测；可选配置只读权限 Token，不强制、不硬编码 |
| 6 | `get_device` / `get_diagnostics` 为 CLI / 外部诊断接口，WebUI 通过 `get_states` 聚合获取 | 保持现状，避免重复请求；`get_diagnostics` 已在 VM 中经 `ubus call` 实测 |
| 7 | 下载中断 / 大文件真实下载链路未在沙箱跑完整流程 | 旧 `downloader.rs` 单测已覆盖续传与慢速保护，建议实机补测 |
| 8 | SNAPSHOT 镜像未单独测试（按「25 版本或者快照版」选择了 25.12.0 稳定版） | 如需快照版，另取 `snapshots/targets/x86/64/` 镜像按同一矩阵重跑 |

---

## 十三、版本与运行方式

```bash
# 构建（本机验证）
cd rust && cargo check && cargo test && cargo build --release

# 只读状态（不联网）
online-upgrade states

# 通用仓库校验（匿名优先）
online-upgrade probe-repo owner/repo

# 并发刷新四套模型 + 网络，输出统一诊断
online-upgrade refresh
online-upgrade refresh --only actions --force

# 网络探测 / 清缓存
online-upgrade netcheck
online-upgrade clear-cache

# 旧流程完全保留
online-upgrade check
online-upgrade download
online-upgrade upgrade
```

真实虚拟机验证使用的方式（供复现）：

```bash
# 静态 PIE 二进制（沙箱无 musl target / 无 KVM / 无 loop 设备）
RUSTFLAGS='-C target-feature=+crt-static' \
  cargo build --release --target x86_64-unknown-linux-gnu

# 离线重建 OpenWrt 根分区并注入插件
mkfs.ext4 -q -F -d <rootfs> -b 4096 part2-new.img 26624
dd if=part2-new.img of=combined-ext4.img bs=512 seek=33792 conv=notrunc

# QEMU TCG 启动（镜像仅内建 virtio_net / vmxnet3 驱动，勿用 e1000）
qemu-system-x86_64 -m 512 -smp 1 -no-reboot -display none -monitor none \
  -drive file=combined-ext4.img,format=raw \
  -netdev user,id=n0 -device virtio-net-pci,netdev=n0 \
  -serial file:serial.log
```

> 所有提交源文件强制 **LF 换行**；版本 `2.6.2`；全程未写入任何明文 GitHub Token。

---

## 十四、v2.6.1 补丁（RPC -32002 修复）

| 项 | 内容 |
| :--- | :--- |
| 现象 | WebUI「远程状态与统一诊断」卡片未加载，提示 `请求失败: RPC error -32002` |
| 根因 | `-32002` = rpcd `/ubus` 的 `Access denied`。升级安装后 `postinst` 只清了 LuCI 缓存，**未重载 rpcd**；rpcd 启动时扫描 ucode 插件并缓存 ACL 白名单，新增的 `get_states` 等方法不在旧白名单里。已登录会话的 ACL 快照同样是登录时生成的 |
| 修复 1 | `Makefile` `postinst` 增加 `/etc/init.d/rpcd restart`（失败回落 `reload`，再失败不影响安装结果） |
| 修复 2 | `app.js` 把 `-32002` 转成可操作中文指引（重新登录 / 重启 rpcd / 确认已装最新版），并在四套面板内渲染明确的「无法读取」文字状态，不再留空白卡片 |
| 修复 3 | 顺手修 `states-hint` 用 `textContent` 写入却先 `esc()` 导致的 HTML 实体明文显示 |
| 版本 | `2.5.6 → 2.6.0 → 2.6.1`；五处版本戳同步，**`app.js?v=` 必须变化**，否则浏览器仍执行缓存旧脚本 |
| 排障 | 见 [docs/ERROR_DIAGNOSTICS.md](docs/ERROR_DIAGNOSTICS.md) 第 7.1 节 |
| **v2.6.2 追加修复** | v2.6.1 的 `rpcd restart` 在部分设备 `stop` 成功但 `start` 失败，rpcd 停在停止状态，LuCI 登录报「密码错误」。改为 `reload` + `running` 兜底 + `start`，错误写 `/tmp/online-upgrade-postinst.log`；对应缺陷 #18 |

> 注意：CI `release` 作业「只保留最新 Release」，发布 `v2.6.1` 后 `v2.6.0` 的 Release 会被清理，Git tag 保留。
