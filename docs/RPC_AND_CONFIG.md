# RPC 接口与 UCI 配置说明（v2.6）

> 对应源码：`root/usr/share/rpcd/ucode/luci.online-upgrade`、`root/usr/share/rpcd/acl.d/luci-app-online-upgrade.json`、`root/etc/config/online-upgrade`、`root/etc/uci-defaults/99-online-upgrade`、`rust/src/main.rs`。

## 1. 兼容性承诺

- **保留全部 23 个旧 RPC 方法**、参数、返回字段与 ACL 分组，未删除、未改名。
- 新增 7 个方法**单独追加**到 ACL 的 `read` / `write` 分组，不改动旧条目。
- 旧 UCI 字段含义不变；新增字段采用「字段缺失才补默认值」策略，**绝不覆盖用户历史配置**。
- 新只读/刷新命令不写 `/tmp/online-upgrade-status.json`，不占用升级流程全局操作锁。

## 2. RPC 方法总表（30 个）

| 分组 | 方法 |
| :--- | :--- |
| 旧 · read（14） | `get_status` `get_log` `list_backups` `get_config` `get_sysinfo` `probe_assets` `list_tasks` `plugin_info` `get_plugin_status` `get_states` `get_device` `get_diagnostics` `probe_repo` `netcheck` |
| 旧 · write（16） | `check` `start_upgrade` `start_download` `pause_download` `resume_download` `cancel_download` `delete_task` `backup` `restore` `set_config` `delete_backup` `clear_log` `start_plugin_check` `start_plugin_update` `refresh_states` `clear_states_cache` |

其中加粗语义的 7 个为 v2.6 新增：`get_states`、`get_device`、`get_diagnostics`、`refresh_states`、`probe_repo`、`netcheck`、`clear_states_cache`。ACL 文件已全部授权，静态校验「30 个方法全部授权、无重复、无多余」通过。

## 3. 新增 7 个 RPC 详细定义

| 方法 | ACL | 参数 | 后端 CLI | 返回要点 |
| :--- | :--- | :--- | :--- | :--- |
| `get_states` | read | 无 | `states` | 一次返回 `device` / `actions` / `releases` / `artifacts` / `compat` / `network` / `diagnostics` / `cache_files` / `policy`；**只读缓存，不联网** |
| `get_device` | read | 无 | `device` | 标准化设备信息 JSON（CLI / 外部诊断用） |
| `get_diagnostics` | read | 无 | `states` 中的诊断段 | 仅诊断层结论（CLI / 外部诊断用） |
| `refresh_states` | write | `model`（`''`=全部 / `actions` / `releases` / `artifacts` / `compat` / `network`）、`force`（bool） | `refresh [--only X] [--force]` | detached 提交，立即返回 `{ok:true, model, force}`；后续由前端有限次读取缓存 |
| `probe_repo` | read | `repo`（**必填**，`owner/repo` 或完整 GitHub URL） | `probe-repo <repo>` | `{ok, parsed, repo, workflows, rate_limit, note}`；失败带 `error` / `error_code` / `parsed` |
| `netcheck` | read | `force`（bool） | `netcheck [--force]` | `ProbeResult`：`class` / `label` / `detail` / `http_status` / `latency_ms` / `retryable` / `rate_remaining` |
| `clear_states_cache` | write | 无 | `clear-cache` | `{ok:true, cleared:<数量>}` |

### 3.1 前端实际调用情况

| 方法 | 前端调用 | 说明 |
| :--- | :--- | :--- |
| `get_states` | 是 | 页面初始化、读取缓存、刷新后轮询、网络检测/清缓存后重载 |
| `refresh_states` | 是 | 「刷新状态」按钮 |
| `probe_repo` | 是 | 「校验仓库」按钮，仓库地址从表单 `repo_url` → 旧 `repo` → 粘贴 URL 依次取 |
| `netcheck` | 是 | 「网络检测」按钮（`force:true`） |
| `clear_states_cache` | 是 | 「清空状态缓存」按钮 |
| `get_device` / `get_diagnostics` | 否 | 这两个是 CLI / 外部诊断专用接口；WebUI 通过 `get_states` 一次拿到设备信息与诊断结论，避免重复请求 |

### 3.2 调用示例

```bash
# 只读聚合状态（不联网）
ubus call luci.online-upgrade get_states

# 只强制刷新 Actions 模型，其他三套缓存不动
ubus call luci.online-upgrade refresh_states '{"model":"actions","force":true}'

# 校验任意公开仓库（自动发现默认分支与全部公开 Workflow）
ubus call luci.online-upgrade probe_repo '{"repo":"https://github.com/owner/repo/tree/main"}'

# 真实 HTTP 探测（非 Ping）
ubus call luci.online-upgrade netcheck '{"force":true}'

# 清空四套模型与网络缓存
ubus call luci.online-upgrade clear_states_cache
```

`probe_repo` 的 `repo` 参数会经 `ghapi::parse_repo()` 校验：接受 `owner/repo`、完整 GitHub URL、`/tree/<branch>`、`.git`、SSH 形式；拒绝注入字符与非 GitHub 主机。空串会被 Ucode 层直接拒绝并提示「仓库地址不合法」。

## 4. CLI 子命令

| 子命令 | 作用 | 是否联网 | 是否写 `status.json` |
| :--- | :--- | :--- | :--- |
| `check` / `download` / `upgrade` / `probe` / `backup` / `restore` / `status` / `clear-log` / `plugin-*` | 旧流程，全部保留 | 视命令而定 | 旧命令按原逻辑 |
| `device` | 标准化设备信息 | 否 | 否 |
| `states` | 只读四套模型 + 网络 + 诊断 | 否 | 否 |
| `probe-repo <url>` | 仓库校验 + 公开信息/默认分支/Workflow 自动发现 | 是（匿名优先） | 否 |
| `netcheck [--force]` | 真实 HTTP 探测 `api.github.com/rate_limit` | 是 | 否 |
| `refresh [--only X] [--force]` | 并发刷新四套模型 + 网络，最后只读汇总诊断 | 是 | 否 |
| `clear-cache` | 清空状态缓存 | 否 | 否 |

## 5. UCI 配置：v2.6 新增字段

配置文件：`/etc/config/online-upgrade`，section `settings`。

| 字段 | 类型 | 默认值 | 作用 |
| :--- | :--- | :--- | :--- |
| `repo_url` | string | 空 | 通用仓库地址；支持 `owner/repo`、完整 URL、`/tree/<分支>`、`.git`、SSH 形式。**优先级高于旧 `repo`；留空则回退旧 `repo`，保证历史配置兼容** |
| `build_branch` | string | 空 | Actions 运行记录分支筛选；留空跟随仓库默认分支。不影响 Release 资产检测 |
| `workflow_filter` | string | 空 | Actions Workflow 名称子串筛选；留空显示全部公开工作流 |
| `api_via_proxy` | bool | `0` | 是否让 API 也走加速源。默认关闭（多数加速源不支持 API）；下载链路仍加速源优先 |
| `api_timeout` | integer | `25` | GitHub API 请求超时（秒），`0` 恢复默认 |
| `ttl_actions` | integer | `120` | Actions 模型独立缓存 TTL（秒），`0` 恢复默认 |
| `ttl_releases` | integer | `300` | Releases 模型独立缓存 TTL（秒），`0` 恢复默认 |
| `ttl_artifacts` | integer | `180` | Artifacts 模型独立缓存 TTL（秒），`0` 恢复默认 |
| `ttl_compat` | integer | `600` | 兼容性模型独立缓存 TTL（秒），`0` 恢复默认 |
| `ttl_network` | integer | `60` | 网络探测缓存 TTL（秒），`0` 恢复默认 |

### 5.1 WebUI 可配置项

「高级配置」中新增 3 个输入控件：`cfg-repo-url`、`cfg-build-branch`、`cfg-workflow-filter`，`fillConfig()` 回填、`saveConfig()` 提交。`get_config` 使用 `uci cursor().get_all(...)` 返回**全部字段**，因此回填不会因字段缺失而误清空用户通过 UCI 设置的值。

其余字段（`api_via_proxy` / `api_timeout` / 五套 `ttl_*`）通过 UCI 命令设置：

```bash
uci set online-upgrade.settings.ttl_actions='180'
uci set online-upgrade.settings.api_timeout='30'
uci commit online-upgrade
```

### 5.2 `uci-defaults` 兼容策略

`99-online-upgrade` 使用 `_set_default()`：仅当 `uci -q get` 返回空时才写入默认值。

```sh
_set_default() {
    [ -z "$(uci -q get online-upgrade.settings.$1)" ] || return 0
    uci -q set "online-upgrade.settings.$1=$2"
}
```

因此从 2.5.x 升级到 2.6.0 时，用户已有的 `repo` / `tag` / `proxy` / `github_token` 等配置全部保留，只补齐缺失的新字段。

### 5.3 `set_config` 校验

- 字符串类：`repo_url`（≤512）、`build_branch`（≤128）、`workflow_filter`（≤256）；空串表示清空该可选值；含引号/空白会被拒绝。
- 整数类：`api_timeout` 与五套 `ttl_*`；传 `0` 表示恢复内置默认值。实现用 `ctx.set(..., '')` 而非依赖某些 ucode 版本不一定提供的 `ctx.delete()`，后端 `ttl_from_uci()` 解析失败即回落默认值。

## 6. 版本一致性

CI `verify` 作业要求以下四处版本严格一致，当前均为 `2.6.2`（v2.6.1 修 `-32002`，v2.6.2 修 `rpcd` 停服回归）：

| 位置 | 值 |
| :--- | :--- |
| `Makefile` `PKG_VERSION` | `2.6.2` |
| `rust/Cargo.toml` `version` | `2.6.2` |
| `rust/Cargo.lock` `online-upgrade` | `2.6.2` |
| `main.ut` 的 `app.js?v=` / `style.css?v=` | `2.6.2` |

浏览器缓存戳随版本升级，避免升级后仍执行旧脚本。**修复前端问题时必须 bump 版本**：若 `app.js?v=` 不变，浏览器会继续使用缓存的旧脚本，代码修复不会生效；插件自我更新也依赖 `PKG_VERSION` 判断是否有新版本。
