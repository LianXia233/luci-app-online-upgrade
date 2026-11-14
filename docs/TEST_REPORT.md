# 测试矩阵与测试报告（v2.6）

> 执行环境：Ubuntu 22.04 x86_64 云沙箱（Rust stable、Node 22），**另在 QEMU 中真实启动官方 OpenWrt 25.12.0 x86/64 镜像**。测试以**匿名 GitHub API** 为主，未使用任何明文 Token。报告遵循「矩阵 → 证据 → 结论 → 已知限制」。

## 1. 构建与测试执行结果

| 项目 | 命令 | 结果 |
| :--- | :--- | :--- |
| 基线单元测试 | `cargo test`（改造前） | 72 passed / 0 failed |
| 编译检查 | `cargo check` | **通过，0 warning** |
| 全量单元测试 | `cargo test` | **160 passed / 0 failed / 0 ignored**（1.45s，含 ABI 回归） |
| Release 构建 | `cargo build --release` | **成功**，产物 `rust/target/release/online-upgrade` |
| 静态 PIE 构建 | `RUSTFLAGS='-C target-feature=+crt-static' cargo build --release --target x86_64-unknown-linux-gnu` | **成功**，`static-pie linked`，注入 OpenWrt rootfs 可直接运行 |
| 前端语法 | `node --check app.js` | 通过 |
| Shell 语法 | `sh -n 99-online-upgrade` | 通过 |
| JSON 语法 | 2 个 JSON 文件解析 | 通过 |
| Ucode 静态 | 括号平衡（含正则字面量识别）、30 个方法与 ACL 对应 | 通过 |
| Ucode 真实编译 | VM 内 `/usr/bin/ucode -c` | **rc=0** |
| rpcd 真实加载 | VM 内 `ubus call luci.online-upgrade ...` | **5 个 RPC 均返回**（详见第 5 节） |
| 模板 | `div` 190/190、`section` 6/6、`details` 3/3 | 通过 |
| 版本一致性 | CI `verify` 四处版本戳 | 全部 `2.6.0` |
| 换行 | 47 个文本文件 | 全部 LF，无 CRLF |
| 脱敏 | 全仓扫描 `ghp_*` / `github_pat_*` | **0 命中** |
| 静态与接线综合校验 | 24 项 | **24 PASS / 0 FAIL** |
| 真实 OpenWrt 25.12.0 QEMU 虚拟机 | 5 轮启动 + 135 条 `OPTEST:` 证据 | **全部 rc=0**，详见第 5 节 |

## 2. 静态与接线综合校验明细（24 项）

| 检查项 | 结果 | 说明 |
| :--- | :--- | :--- |
| CI verify 版本一致性 | PASS | Makefile / Cargo.toml / Cargo.lock / app.js?v / style.css?v 均 2.6.0 |
| CHANGELOG 结构完整 | PASS | 含 2.6.0 段落；2.5.6 无重复；无占位文本 |
| `node --check app.js` | PASS | |
| `sh -n uci-defaults` | PASS | |
| JSON 语法（2 个） | PASS | |
| RPC 方法全部授权（30 个） | PASS | 无缺失 |
| ACL 无多余/重复方法 | PASS | |
| Ucode 括号平衡 | PASS | 含正则字面量识别 |
| `main.ut` 标签平衡 | PASS | 190/190、6/6、3/3 |
| UCI 默认配置 10 个新增字段 | PASS | |
| `uci-defaults` 补齐 10 个字段 | PASS | |
| `set_config` 接纳 10 个字段 | PASS | |
| 前端调用 5 个状态/诊断 RPC | PASS | `get_states` / `refresh_states` / `probe_repo` / `netcheck` / `clear_states_cache` |
| CLI 专用接口存在且授权 | PASS | `get_device` / `get_diagnostics` |
| 新配置控件回填 + 提交 | PASS | `cfg-repo-url` / `cfg-build-branch` / `cfg-workflow-filter` |
| 诊断工具按钮已接线 | PASS | 3 个按钮 + JS 绑定 |
| 移动端分区含远程状态卡片 | PASS | `MOBILE_SECTIONS` |
| 新卡片 DOM 齐全 | PASS | 四面板 + 诊断区 |
| CSS 工具行 + 760/380 适配 | PASS | |
| LF 换行（47 个文本文件） | PASS | 二进制图片按 UTF-8 解码失败自动排除 |
| 全仓无明文 GitHub Token | PASS | |
| release 二进制存在 | PASS | |
| 四套模型独立缓存/TTL | PASS | Rust 单测 |
| 诊断层只读 | PASS | Rust 单测 |

## 3. 测试矩阵

### 3.1 设备信息矩阵

| 维度 | 用例 | 覆盖方式 | 结果 |
| :--- | :--- | :--- | :--- |
| Target / Subtarget | `mediatek` / `filogic`、`x86` / `64`、未知 | 单测 + **真实 25.12 VM `device`** | 分离正确；未知保留 `unknown` |
| CPU 架构 / Package Arch / ABI | `aarch64_cortex-a53` → `cortex-a53`；**`x86_64` 不得推导 `abi=64`** | 单测 + 真实 25.12 VM 回归 | 通过：`abi=unknown`、`unknown_fields` 含 `abi` |
| `apk` / `opkg` 双包管理器 | 25.12 无 `opkg`、`apk --print-arch=x86_64` | 真实 25.12 VM | 正确识别，不误报缺失 |
| 发行版 | OpenWrt / ImmortalWrt / 其它 / 缺失 | 单测 + 真实 VM | 通过：`OpenWrt 25.12.0 r32713` |
| 多来源兜底 | `/etc/openwrt_release`、`/etc/os-release`、`/tmp/sysinfo/*`、`/etc/board.json`、`ubus call system board`、`/proc/device-tree/compatible`、`/proc/cpuinfo`、`uname`、`apk/opkg` | 单测 + 沙箱 + 真实 VM | 沙箱无 OpenWrt 时 `unknown` 兜底不 panic；VM 中 `board.json` 生成 QEMU 机型 |
| 缺字段 | 全部字段缺失 | 单测 | 一律 `unknown`，不编造 |

### 3.2 GitHub 仓库结构矩阵

| 场景 | 覆盖方式 | 结果 |
| :--- | :--- | :--- |
| 单 / 多 Workflow | 真实匿名 API `probe-repo LianXia233/OpenWRT-CI` | **自动发现 12 个公开 Workflow** |
| 多分支 / `/tree/<branch>` URL | `probe-repo https://github.com/LianXia233/H5000M-CI-Qmodem/tree/main` | `parsed.slug` 正确解析；额度耗尽时正确报 `rate_limited` |
| 仅 Release / 仅 Artifacts / 二者兼有 / 无 Release | 诊断层 `rule_matrix()` 单测 | 通过：无 Release 不覆盖 Artifact 状态 |
| 无构建记录 / 构建失败 / 正在构建 / 成功但无固件 | `actions::map_run()` 单测 + **真实 VM 诊断层** | 通过：VM 中「检测到 30 个运行记录…仍在 正在编译；不据此判定整个仓库构建失败」 |
| 产物无法识别 | `firmware::classify_kind()` + `compatstate` 单测 | 归为 `unconfirmed`，**不等于明确不兼容** |
| 仓库地址形式 | `owner/repo`、完整 URL、`/tree/分支`、`.git`、SSH | `ghapi::parse_repo()` 单测 | 通过 |
| 注入字符 / 非 GitHub 主机 | `probe-repo 'owner/repo;rm -rf /'` | 拒绝：`仓库标识不合法 [owner/repo;rm -rf ]` |

### 3.3 状态模型矩阵

| 用例 | 单测 / 实测 | 结果 |
| :--- | :--- | :--- |
| 四套缓存文件互不相同 | `states::tests::four_state_models_have_distinct_cache_files` + **真实 VM `CACHE`** | 通过：VM 中生成 5 个独立文件 |
| 四套默认 TTL 独立 | `states::tests::four_models_have_distinct_default_ttls` | 通过 |
| API 失败不覆盖业务状态 | `model::replace_api()` 单测 | 通过：只改 `api` 元数据 |
| 构建失败不覆盖 Release / 无 Release 不覆盖 Artifact | `diagnostics` 单测 | 通过 |
| 缓存标记 stale，不冒充实时 | `cache` 单测 | 通过 |
| `refresh --only X` 不动其他缓存 | `states::refresh_all` 编排 + 单测 | 通过 |
| 诊断层不修改模型 | 不可变引用 + 单测 | 通过（编译期约束） |
| 四套模型在聚合输出中各自独立 | **真实 VM `MODEL_TAGS`** | 通过：`actions` / `releases` / `artifacts` / `compat` / `diagnostics` 并存 |
| 缺少校验依据 ≠ 校验失败 | `compatstate::download_check_*` 单测 | 通过：降为 `unconfirmed` |
| L5 本机验证失败阻断刷写 | `compatstate` 单测 + **真实 VM `sysupgrade -T`** | 通过：`rc=1`，真实拒绝无效镜像 |

### 3.4 网络与错误分类矩阵

| 用例 | 覆盖方式 | 结果 |
| :--- | :--- | :--- |
| DNS / TCP / TLS / 超时 | `NetClass` 分类单测 + **真实 VM 网络失败轮次** | 通过：`class=tcp` 且不影响其他状态 |
| 403 限流 vs 403 权限 | `classify_status()` 单测 + **真实 VM 匿名 API 限流** | 通过：`error_code=rate_limited`，含「约 N 秒后可重试」 |
| 404 | 单测 + 真实 `probe-repo` 早期 bug 复现与修复 | 通过（见第 6 节 #12） |
| 429 / 5xx / `Retry-After` | `RateInfo` / `ApiError` 单测 | 通过 |
| `X-RateLimit-*` 解析 | **真实 VM `netcheck` / `ubus netcheck`** | 通过：`rate_remaining` 26 → 0 → 20 → 16 |
| 加速源失败 → 官方直连回退 | `network::download_routes()` 单测 | 通过：官方直连始终保留为最后一条 |
| API URL 不被下载加速源改写 | `download_routes()` 单测 | 通过 |
| 请求去重 / 并发限制 / 失败退避 | `Hub::get()` + `cache` 退避单测 | 通过 |
| 真实 HTTP 连通性 | 沙箱 + **真实 VM `netcheck`** | **HTTP 200**，沙箱 258–264 ms、VM 370–400 ms，`class=ok` |
| 真实 DNS 解析 | **真实 VM `nslookup api.github.com`** | 通过：`20.205.243.168` |
| 下载中断 / 损坏文件 | 旧 `downloader.rs` 既有单测 + 新增压缩包安全解析 | 通过 |

### 3.5 安全矩阵

| 用例 | 覆盖方式 | 结果 |
| :--- | :--- | :--- |
| 错误固件 / 明确不兼容固件 | `compatstate` 单测 + **真实 VM 诊断层** | 拒绝并给出具体原因（架构冲突 aarch64 vs x86-64） |
| 文件损坏 / SHA-256 不匹配 | `hash` + `compatstate::ChecksumFailed` 单测 | 拒绝 |
| 空文件 / HTML 错误页 | `ApiError::BadContent` + `firmware::detect_format` 单测 | 拒绝，不当固件 |
| 恶意 ZIP（路径穿越 / 绝对路径 / NUL / 反斜杠 / 驱动器前缀） | `firmware::safe_entry_name()` 单测 | 全部拒绝 |
| 解压炸弹（条目数 / 单条目体积 / 总体积 / 异常压缩比） | `inspect_zip()` 单测 | 拒绝 |
| 超大压缩包（>256MB） | `upgrade::inspect_downloaded_archive()` 上限检查 | 拒绝 |
| 归档包内无可刷写镜像 | `flashable_from_entries()` 接入 `verify_firmware_integrity` | 拒绝直接刷写 |
| 本机验证失败（`sysupgrade -T` / `fwtool`） | 单测 + **真实 VM dummy 镜像** | `rc=1`，输出 `Image check failed.` / `unable to extract trailer header` |
| Token 泄露 | 全仓扫描 + `ghapi::redact()` | 0 命中 |
| TLS 证书验证 | `shared_client()` 明确不调用 `danger_accept_invalid_certs`；**真实 VM HTTPS 200** | 保持开启 |
| 真实刷写 | **未执行**（安全要求） | 仅做 `sysupgrade -T` 只读测试 |

## 4. 真实匿名 API 冒烟证据（release 二进制，宿主/沙箱）

| 命令 | 关键输出 | 判定 |
| :--- | :--- | :--- |
| `device` | `ok=true`，`model=unknown` `target=unknown` `cpu_arch=x86_64` | 沙箱无 OpenWrt 时正确兜底，不 panic、不推测 |
| `states` | 四套模型各自 `state` / `state_label` / `api.status`；`diagnostics.summary=未检测；当前设备的兼容性尚无法确认，因此暂不允许自动刷写` | 只读聚合、未联网、诊断层输出中文结论 |
| `probe-repo LianXia233/OpenWRT-CI` | `ok=true`，`default_branch=main`，`workflows=12` | 通用仓库自动发现成功（无硬编码） |
| `probe-repo https://github.com/LianXia233/H5000M-CI-Qmodem/tree/main` | `error_code=rate_limited`，中文限流提示，`parsed.slug` 正确 | 限流正确分类；完整 URL + 分支解析正确 |
| `probe-repo 'owner/repo;rm -rf /'` | `仓库标识不合法 [owner/repo;rm -rf ]` | 注入拒绝 |
| `netcheck` | `class=ok` `HTTP 200` `258 ms` `retryable=false` | 真实 HTTP 探测 |
| `refresh`（无 UCI 配置） | `error=仓库地址为空`，`hint=请先在「仓库配置」中填写有效的公开 GitHub 仓库地址` | 无配置不 panic，给出可操作提示 |
| `clear-cache` | `ok=true`，`cleared=2` | 缓存清理 |

## 5. 真实 OpenWrt 25.12.0 QEMU 虚拟机测试

> 用户明确要求「虚拟机安装 OP 镜像测试，25 版本或者快照版」。本节为**官方 OpenWrt 25.12.0 稳定版**真实镜像启动测试，非静态分析、非纯单元测试。所有证据来自串口日志 `serial*.log` 的 `OPTEST:` 行。

### 5.1 镜像与完整性校验

| 项 | 值 |
| :--- | :--- |
| 镜像 | `openwrt-25.12.0-x86-64-generic-ext4-combined.img.gz`（13,591,810 B） |
| rootfs | `openwrt-25.12.0-x86-64-rootfs.tar.gz`（4,841,553 B） |
| 官方 SHA256（rootfs） | `11c2f26b9d48c02cbdb3b63d499e1f6413ed62bcdbbea78937b12279b95acb1e` |
| 官方 SHA256（ext4 镜像） | `7f33f26f89a7096aa549ec45aa80e07577a3e69c725772a5fa4b8c18df38aeaa` |
| 校验结果 | 两次 `sha256sum -c` 均 **OK** |
| 解压后磁盘 | `combined-ext4.img` 126,353,408 B |
| MBR 分区 | 分区1 offset `262,144`（16MB 引导）；分区2 offset `17,301,504`（104MB rootfs，212,992 sectors）；GRUB `root=PARTUUID=83beaead-02` |
| 内核 / 发行版 | `Linux OpenWrt 6.12.71 #0 SMP`；`DISTRIB_RELEASE='25.12.0'`、`DISTRIB_REVISION='r32713-f919e7899d'`、`DISTRIB_TARGET='x86/64'`、`DISTRIB_ARCH='x86_64'` |

### 5.2 环境限制与绕过方法

| 限制 | 绕过方式 |
| :--- | :--- |
| 无 `/dev/kvm` | QEMU **TCG 软件模拟**，`-m 512 -smp 1` |
| 容器无 loop 设备，`mount -o loop` / `losetup -P` 均失败 | 离线重建根分区：`mkfs.ext4 -q -F -d <rootfs> -b 4096 part2-new.img 26624`，再 `dd if=part2-new.img of=combined-ext4.img bs=512 seek=33792 conv=notrunc`；`debugfs` 校验注入文件 |
| 缺 `fdisk` | Python `struct` 直接解析 MBR 分区表 |
| `rustup target add x86_64-unknown-linux-musl` 卡住 | 不依赖 musl target；改用 `RUSTFLAGS='-C target-feature=+crt-static' cargo build --release --target x86_64-unknown-linux-gnu`，产出 `static-pie linked` 二进制（5,241,272 B），注入 `/usr/bin/online-upgrade` |
| 镜像仅内建 2 个网卡驱动 | `modules.builtin` 仅含 `virtio_net` / `vmxnet3`；`-nic user,model=e1000` 设备被 PCI 枚举（日志 `[8086:100e]`）但**无驱动绑定**，故无 `eth0`。改用 `-netdev user,id=n0 -device virtio-net-pci,netdev=n0` |
| OpenWrt 25.12 默认 `br-lan` 静态 `192.168.1.1`、无默认路由 | `rc.local` 强制把 `br-lan` 改为 SLIRP 网段 `10.0.2.15`、加默认路由 `10.0.2.2`、写 `/tmp/resolv.conf` → `nameserver 10.0.2.3` |
| chroot 中 `/etc/resolv.conf` 悬空软链 | chroot 测试时临时替换宿主 resolv.conf；注入 VM 前恢复原软链，由 `rc.local` 写 `/tmp/resolv.conf` |

### 5.3 分步执行记录（5 轮）

| 轮次 | 日志 | 时长 / 行数 | 目的 | 结果 |
| :--- | :--- | :--- | :--- | :--- |
| 1 | `serial.log` | 58s / 22 行 | 基础启动、GRUB → procd → rpcd、`device` / `states` / `ubus` | 成功；但 busybox 无 `timeout` 命令，网络项未真正执行 |
| 2 | `serial2.log` | 53s / 24 行 | 去掉 `timeout`，跑 `netcheck` / `probe` / `refresh` / `sysupgrade -T` | `refresh` 生成 5 个独立缓存；网络 `class=tcp`（无 `eth0`）；`rc=$?` 因管道误报 `rc=0` |
| 3 | `serial3.log` | 28 行 | 注入 ABI 修复后的二进制 + 改 `virtio-net-pci` + 修退出码捕获 | `eth0` 出现；`"abi":"unknown"`；`sysupgrade -T rc=1` / `fwtool rc=1`；网络仍因 `br-lan` 静态地址不通 |
| 4 | `serial4.log` | 29 行 | `br-lan` 改 `10.0.2.15` + 默认路由 | **网络打通**：ping 0% 丢包、DNS 解析、`netcheck HTTP 200`；真实触发匿名限流 |
| 5 | `serial5.log` | 32 行 | 补 `get_diagnostics` RPC、模型标签与 stale 标记 | 拿到**统一诊断层完整中文结论**与四套模型并存证据 |

> 每轮 QEMU 均由 `rc.local` 末尾 `sync` + `poweroff` 正常收尾，`qemu-system-x86_64` 退出码全部 **0**。

### 5.4 关键证据表（第 4/5 轮，网络打通后）

| 模块 | 证据 | 判定 |
| :--- | :--- | :--- |
| 设备信息 | `"pkg_arch":"x86_64"` / `"abi":"unknown"` / `unknown_fields:["compatible","abi"]` / `target=x86` `subtarget=64` `cpu_arch=x86_64` / `apk=x86_64` `opkg=` | **ABI 修复生效**：不再把 CPU 位宽 `64` 误当 ABI；识别失败保留未知 |
| 网卡 | `IFACE_DETECTED:br-lan TARGET:br-lan`；`eth0` UP；`br-lan inet addr:10.0.2.15` | virtio 驱动与桥接均正确 |
| 路由 | `0.0.0.0 10.0.2.2 UG br-lan`；`10.0.2.0/24 br-lan` | 默认路由生效 |
| 连通 | `PING 10.0.2.2` → `1 packets transmitted, 1 received, 0% packet loss`，17.765 ms | SLIRP 网关可达 |
| DNS | `nslookup api.github.com` → `Server: 10.0.2.3`、`Address: 20.205.243.168` | DNS 正常 |
| 网络探测 | `NETCHECK: class=ok / HTTP 200 / 380–400 ms / rate_remaining=20–26` | 真实 HTTPS + TLS 校验通过 |
| ubus 网络 | `UBUS_NETCHECK: HTTP 200 / 370 ms / rate_remaining=16` | rpcd Ucode 通路真实可用 |
| 限流 | `PROBE: error_code=rate_limited`，「GitHub API 请求受到限流…约 976–1210 秒后可重试；已保留上次成功查询的数据」 | 403 限流正确分类，未被误报为「无 Release / 构建失败」 |
| 缓存隔离 | `actions.json` / `releases.json` / `artifacts.json` / `compat.json` / `network.json` 五个独立文件 | 四套模型物理隔离 |
| rpcd | `ubus call ... get_states` / `get_device` / `probe_repo` / `netcheck` / `get_diagnostics` 均有返回 | `/usr/share/rpcd/ucode/luci.online-upgrade` 被真实扫描加载 |
| Ucode | `ucode -c` / `ucode -S -c` `rc=0` | 语法与严格编译通过 |
| 模型标签 | `MODEL_TAGS: states / actions / releases / artifacts / compat / diagnostics` | 四套模型 + 诊断层在聚合输出中各自独立 |
| 本机验证 | `sysupgrade -T`：`Image metadata not present` / `Invalid image type` / `Image check failed.` **`rc=1`**；`fwtool -i`：`unable to extract trailer header` **`rc=1`** | 真实工具拒绝无效镜像，退出码捕获已修正 |

### 5.5 真实限流场景下的统一诊断层结论（第 5 轮 `UBUS_DIAG`）

| 字段 | 输出 |
| :--- | :--- |
| `summary` | 「已经找到固件文件，但与当前设备明确不兼容，已禁止自动刷写」 |
| `severity` / `can_upgrade` | `error` / `false` |
| `blocking_reasons` | 「固件明确不兼容：`H5000M-MT5700-aarch64-26.11.14-sysupgrade.bin`: **架构冲突：固件声明 [aarch64]，本机为 [x86-64-x86-64]**」 |
| `next_actions` | 「等待构建完成或检查仓库配置（仓库地址、分支、Workflow 筛选、固件匹配规则）」 |
| `sections[network]` | 「正常（GitHub API 可达（HTTP 200，370 ms））」，`severity=success`，证据含探测目标 / 耗时 / 缓存来源 |
| `sections[actions]` | 「检测到 30 个运行记录，其中 [H5000M 自用定时编译（NetMonitor + Taygedo）] 仍在 **正在编译**；**不据此判定整个仓库构建失败**」 |

**这一轮直接验证了三条核心规格**：

1. Actions 未完成状态（`in_progress`）**不会被显示成构建失败** —— `status` / `conclusion` 严格分离。
2. 固件兼容性基于**设备维度比对**（架构冲突）给出具体原因，而非仅凭文件名；明确不兼容 → `can_upgrade=false`。
3. 网络正常、API 限流、Actions 进行中、Compat 明确不兼容**四种状态同时成立且互不覆盖** —— 诊断层只读汇总，不修改任何模型。

### 5.6 VM 测试结论

- 官方 25.12.0 镜像完整性校验通过，QEMU 真实启动链路（GRUB → Linux 6.12.71 → procd → `rc.local` → `poweroff`）5 轮全部成功。
- 静态二进制、rpcd Ucode、ACL、`ucode -c`、四套独立缓存、设备信息、网络栈、真实 GitHub API（含限流）、统一诊断层、`fwtool` / `sysupgrade -T` 均在**真实 OpenWrt 系统内**实测通过。
- 真实 VM 暴露并修复了 4 个问题（ABI 误推导、网卡驱动不匹配、退出码误报、SLIRP 网段不匹配），见第 6 节 #13–#16。
- **全程未执行真实刷写**，只做 `sysupgrade -T` 只读验证。

## 6. 缺陷发现与修复记录

| # | 现象 | 根因 | 修复 | 验证 |
| :--- | :--- | :--- | :--- | :--- |
| 1 | `cargo test --offline` 失败：`no matching package named futures-util found` | 环境离线缓存缺依赖 | 改用在线 `cargo test` | 依赖下载成功，72 → 160 测试通过 |
| 2 | 新模块 `expected function, found macro src` | `device.rs` 定义 `macro_rules! src` 却按函数调用 | 改为 `src!(...)` | `cargo check` 通过 |
| 3 | 5 个单测失败（KV 解析、initramfs 分类、`efi` 短词误判、ZIP 解压炸弹测试构造、断言过粗） | 真实解析 bug + 短词裸子串匹配 + 测试构造不合理 | `kv()` 逐行精确 `KEY=`；`name_tokens()` token 精确匹配；ZIP 测试改真实 store 比例；断言改判 `Unconfirmed` | 158 passed |
| 4 | Ucode 尾部 `Edit` 连续失败 | tab 缩进与预期空白不符 | `cat -A` 查看精确空白后按真实片段插入 | 通过 |
| 5 | `ctx.delete()` 兼容性风险 | 部分 ucode 版本无 `delete()` | 整数 `0` 用 `ctx.set(..., '')` 恢复默认 | 通过 |
| 6 | `CHANGELOG.md` 出现占位文本与重复 `2.5.6` 标题 | 插入 2.6.0 时把原 Unreleased 内容错位 | 用 git HEAD 原始头部 + 当前 2.6.0 + 原始历史尾部重建，带断言防误删 | 纯新增，零历史丢失 |
| 7 | `DeviceInfo::collect()` 编译错误 | 是模块级自由函数，非关联方法 | 改为 `crate::device::collect()` | 通过 |
| 8 | 删除 `actions.rs` 顶层 import 后 `cargo test` 失败 | 测试模块经 `use super::*` 依赖该导入 | 测试模块内显式 `use crate::model::SCHEMA_VERSION;` | 160 passed |
| 9 | 37 个 `cargo check` 未使用警告 | 新增 helper 未接入 / 冗余草稿 | 删除冗余 import 与 helper；真实接入 5 处诊断能力；其余设计预留字段加 `#[allow(dead_code)]` + 注释 | **0 warning** |
| 10 | 静态校验 3 项 FAIL（Ucode 括号、RPC 0 个方法、`screenshot.png` CRLF） | 检查器缺陷 | 修正检查器（正则字面量识别 / ACL 三层下钻 / 二进制排除） | 24/24 PASS |
| 11 | 前端未调用 5 个新增 RPC；新 UCI 字段无输入控件 | `saveConfig()` 未提交新字段、缺少按钮与控件 | 补 3 个配置控件 + 3 个诊断工具按钮 + 完整接线 | 24/24 PASS |
| 12 | **`probe-repo` 对公开仓库返回「资源不存在」** | `api_url(repo, "")` 拼出尾斜杠 `.../repos/{slug}/`，GitHub 返回 404 | `api_url()` 空 suffix 时不追加斜杠；加回归单测 | 修复后真实 API `ok=true` / `default_branch=main` / `workflows=12` |
| 13 | **真实 25.12 VM 中 `abi="64"`** | `DISTRIB_ARCH='x86_64'` 被 `split_once('_')` 把 CPU 位宽 `64` 当成 ABI；违反「识别失败保留未知、不得混淆维度」 | 新增纯函数 `derive_abi()`：仅当后缀含 ASCII 字母（CPU 变体/FPU/浮点 ABI）才拆分；纯数字位宽、空后缀、无分隔符返回 `None` → `unknown`；补 10 组回归单测 | VM 输出 `"abi":"unknown"`、`unknown_fields` 含 `abi`；`cargo test` 160 passed |
| 14 | **QEMU 内无 `eth0`，网络 `class=tcp`** | 镜像 `modules.builtin` 仅内建 `virtio_net` / `vmxnet3`；`-nic user,model=e1000` 的 `[8086:100e]` 设备无驱动绑定 | 改用 `-netdev user,id=n0 -device virtio-net-pci,netdev=n0` | VM 出现 `eth0` 与 `br-lan` |
| 15 | `SYSUPGRADE_T` / `FWTOOL_I` 日志误报 `rc=0` | `$(cmd | tr | head; echo rc=$?)` 的 `$?` 取到管道末端退出码 | 改为输出重定向到临时文件后单独保存 `$?` | VM 输出真实 `rc=1` |
| 16 | VM 有网卡仍 `Network is unreachable` | OpenWrt 25.12 默认 `br-lan` 静态 `192.168.1.1` 且无默认路由，与 SLIRP `10.0.2.0/24` 不匹配 | `rc.local` 强制改 `br-lan` 为 `10.0.2.15` + 默认路由 `10.0.2.2` + DNS | ping 0% 丢包、`netcheck` HTTP 200 |

> 第 12 项为真实 API 冒烟发现的功能缺陷；第 13–16 项为**真实 OpenWrt 25.12.0 QEMU 虚拟机测试**发现/验证的问题，均已修复或给出确定绕过方案。

## 7. 结论

- 编译、单元测试（160）、release 构建、静态 PIE 构建、静态与接线校验（24 项）**全部通过，零警告**。
- 四套状态模型的隔离性、失败不覆盖、缓存新鲜度、诊断层只读性均有单测 + **真实 VM 聚合输出**双重保证。
- 通用仓库解析、默认分支/Workflow 自动发现、限流分类、网络探测经**真实匿名 GitHub API** 实测；VM 内真实触发 `rate_limited` 并正确保留状态。
- **统一诊断层在真实限流场景下输出了完整中文结论**：进行中不判失败、架构冲突给出具体原因、`can_upgrade=false`、网络正常独立成节。
- 安全边界（恶意 ZIP、解压炸弹、超大包、SHA-256、`sysupgrade -T`、`fwtool`、Token 脱敏、TLS 不关闭）有单测、代码级与**真实 VM 只读验证**三重保证；真实刷写未执行。
- 官方 OpenWrt **25.12.0** 镜像 SHA256 校验通过，QEMU 真实启动 5 轮全部 `rc=0`；`rpcd` Ucode、`fwtool`、`sysupgrade -T`、设备信息采集均已实机（VM）验证，不再是「未实测」项。
- 未使用任何明文 Token；全仓扫描 0 命中。

## 8. 已知限制

| 限制 | 原因 | 建议 |
| :--- | :--- | :--- |
| **物理嵌入式设备未实测** | VM 为 QEMU x86/64 通用镜像，非 ARM / 具体路由器硬件；Device Tree `compatible` 在 VM 中为空（已正确显示 `unknown`） | 在目标设备执行 `online-upgrade device` 与一次「下载并校验」 |
| aarch64 / x86_64 **musl 交叉编译与 `.apk` 打包**未在本地执行 | 由 GitHub Actions `rust` + SDK 作业负责 | 推送后查看 CI `rust` / `package` / `release` 作业 |
| VM 使用 glibc `static-pie` 二进制注入，非 SDK 产出的 musl 二进制 | `rustup target add musl` 在沙箱内卡住 | CI 产出 musl 二进制后在实机复测一次 |
| **真实刷写未执行** | 禁止未授权危险实机操作，符合安全要求 | 按既有 `upgrade` 流程在实机操作 |
| 匿名 API 60 次/小时（共享出口 IP），VM 内 `probe-repo` 被限流未能返回 `ok=true` | 连续多轮真实请求耗尽配额 | `ok=true` / `default_branch=main` / `workflows=12` 的成功证据来自此前宿主沙箱与 chroot 宿主网络实测；限流路径本身已在 VM 真实验证 |
| 下载中断 / 大文件真实下载未跑完整链路 | 无目标固件与实机 | 旧 `downloader.rs` 单测覆盖续传与慢速保护，建议实机补测 |
| SNAPSHOT 镜像未单独测试 | 用户要求「25 版本或者快照版」，本次选择官方 25.12.0 稳定版 | 如需快照版，另取 `snapshots/targets/x86/64/` 镜像按第 5 节矩阵重跑 |
