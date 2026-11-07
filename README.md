# luci-app-online-upgrade

ImmortalWrt / OpenWrt LuCI 插件 — 从 GitHub Releases 在线升级固件。

**v2 重构版**:Rust(Tokio)后端 + ucode rpcd 桥接 + 原生 JS/CSS 前端,零 Lua。

![Screenshot](screenshot.png)

## 架构

```
浏览器(原生 JS + CSS,无框架)
   │  fetch /ubus/  (uhttpd-mod-ubus JSON-RPC,会话鉴权)
   ▼
rpcd ucode 后端  /usr/share/rpcd/ucode/luci.online-upgrade
   │  popen / setsid
   ▼
Rust 静态二进制  /usr/bin/online-upgrade  (aarch64 / x86_64 musl)
   ├─ check    GitHub Release API(reqwest + rustls,代理回退 / 403 重试 / token)
   ├─ upgrade  分段多线程下载(参考 FluxDown 内核架构)→ 备份 → sysupgrade
   └─ 状态 JSON /tmp/online-upgrade-status.json(真实下载进度)
```

- **多线程下载器**:Range 探测 → 4MB 动态分块队列 → 最多 6 个 tokio worker
  work-stealing(快连接自动接管慢段);每块独立重试 + 30s 停滞看门狗;
  `.parts.json` 侧车断点续传;不支持 Range 自动退单流。
- **页面**:菜单 `系统 → 在线升级`(独立裸页面,自带暗色主题);
  备份上传/下载走 LuCI 标准 `cgi-upload`/`cgi-download`(带会话鉴权)。
- **升级后自举**:`keep.d` 保留配置与备份;uci-defaults 自动重装丢失的包;
  包列表存于 `/root/.pkg-list.txt`。

## 功能

- 自定义 GitHub 仓库 / Release 标签;`tag=latest` 自动追踪最新 Release
  (按时间倒序匹配第一个含目标固件的 Release,适配每日多产品线轮换源)
- 自动检测固件更新(版本号 + 编译时间戳双重判定)
- 一键在线升级,`sysupgrade -f` 自动保留配置
- 多线程分段下载 + GitHub 加速代理(默认 `https://ghfast.top/`)
- 升级前自动备份配置到 `/root/`,可下载 / 自动恢复 / 手动上传恢复
- 强制更新:已最新也可重新刷写
- 实时进度条(真实下载百分比,非模拟动画)

## 依赖

- `rpcd-mod-ucode`、`ucode-mod-fs`、`ucode-mod-uci`(ucode 桥接)
- LuCI 25.12+(ucode dispatcher + menu.d);包管理器 apk
- **不再依赖** curl / jsonfilter(HTTP 栈内置于 Rust 二进制)

## 编译

### Rust 后端(宿主机交叉编译,无需 OpenWrt SDK)

```bash
# aarch64(musl 静态,ring 需要 C 工具链 → 用 zig)
cargo install cargo-zigbuild
cargo zigbuild --release --target aarch64-unknown-linux-musl
cp target/aarch64-unknown-linux-musl/release/online-upgrade root/usr/bin/online-upgrade
```

CI(Ubuntu)使用 `gcc-aarch64-linux-gnu` / `musl-tools` +
`RUSTFLAGS="-C link-self-contained=yes -C linker=rust-lld"`,见
`.github/workflows/build.yml`(自动构建 mediatek/filogic 与 x86/64 双架构 apk 并发 Release)。

### 打包

```bash
# 先编好二进制放到 root/usr/bin/online-upgrade,再:
cd openwrt
make package/luci-app-online-upgrade/compile V=s
```

## 手动安装

```bash
apk add --allow-untrusted luci-app-online-upgrade-2.0.0-*.apk
```

## 配置

UCI 配置文件 `/etc/config/online-upgrade`:

```bash
# 以下均为示例 — 插件不内置任何更新源,repo/tag/pattern 全部由用户在页面配置
config online-upgrade 'settings'
    option repo 'LianXia233/OpenWRT-CI'   # 示例:owner/repo
    option tag 'latest'           # latest=自动解析最新 Release;也可固定如 MT5700-26.11.07
    option proxy 'https://gh.acg2.mom/'
    option firmware_pattern '^H5000M-.*sysupgrade\.bin$'  # 换 FM350 变体改成 H5000M-FM350
    option min_size_mb '15'       # 小于该大小的附件不视为固件;0 = 不限制
    option dl_workers '6'         # 下载并发;dl_chunk_mb/dl_stall_sec/dl_retries 同理可调
    option github_token ''        # 可选,提高 API 限速
    option keep_config '1'
```

命令行调试:

```bash
online-upgrade check    # 输出 JSON 到状态文件,日志在 /tmp/online-upgrade.log
online-upgrade status   # 查看当前状态 JSON
ubus call luci.online-upgrade get_status
```

## 许可证

GNU GENERAL PUBLIC LICENSE v2
