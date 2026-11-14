# 错误诊断与状态说明（v2.6）

> 对应源码：`rust/src/ghapi.rs`（`ApiError` / `NetClass` / `ApiMeta`）、`cache.rs`、`model.rs`、`actions.rs`、`compatstate.rs`、`diagnostics.rs`、`network.rs`。

## 1. 错误信息必须包含的维度

每条错误都带：**模块 / 阶段 / HTTP 状态码 / 关键原因 / 是否可重试 / 是否存在缓存**。日志按模块加前缀区分，便于定位：

| 前缀 | 模块 |
| :--- | :--- |
| `[ghapi]` | GitHub API 请求、限流、脱敏、去重 |
| `[actions]` / `[releases]` / `[artifacts]` | 三套远程状态模型 |
| `[compat]` | 兼容性五层判定与下载后结果回写 |
| `[network]` | 线路策略与连通性探测 |
| `[download]` | 分段下载与线路回退 |
| `[verify]` | SHA-256、压缩包安全解析、`fwtool` / `sysupgrade -T` |

**绝不输出**：GitHub Token、敏感请求头、用户隐私。Token 仅在 `host == api.github.com` 时作为 `Authorization` 请求头发送；`ghapi::redact()` 对任何日志/错误文本做兜底脱敏。

## 2. `ApiError` 分类与中文提示

| code | 触发 | `user_message()` 文案（摘要） | 可重试 | 是否保留缓存 |
| :--- | :--- | :--- | :--- | :--- |
| `rate_limited` | 403 且 `remaining=0` / 有 `Retry-After` / 429 / body 含 `rate limit` / `abuse` | GitHub API 请求受到限流，暂时无法获取最新状态（约 N 秒后可重试）；已保留上次成功查询的数据 | 是（按退避） | 是 |
| `forbidden` | 403 但非限流（私有仓库、SSO、权限不足） | GitHub API 拒绝访问（权限或仓库可见性） | 否 | 是 |
| `not_found` | 404 | GitHub 资源不存在 | 否 | 是 |
| `unauthorized` | 401 | GitHub 鉴权失败（请检查 Token） | 否 | 是 |
| `server_error` | 5xx | GitHub 服务暂时不可用（HTTP N） | 是 | 是 |
| `bad_json` | 响应非合法 JSON | GitHub API 返回内容无法解析 | 视情况 | 是 |
| `bad_content` | 200 但内容不像 JSON / 400 / 422 | GitHub API 返回异常内容 | 否 | 是 |
| `network` | DNS / TCP / TLS / 超时 / 加速源 / 官方服务异常 | 网络请求失败（<NetClass 中文>） | 视分类 | 是 |
| `config` | 仓库地址为空 / 非法 | 仓库地址不合法 / 仓库地址为空 | 否 | 否 |

### 2.1 403 的严格区分

`Hub::classify_status()` 判定顺序：

1. `rate.is_exhausted()`（`X-RateLimit-Remaining: 0`）→ `RateLimited`
2. `rate.retry_after.is_some()`（`Retry-After` 头）→ `RateLimited`
3. `status == 429` → `RateLimited`
4. 响应 body 含 `rate limit` 或 `abuse` → `RateLimited`
5. 其余 403 → `Forbidden`

**实测证据**（真实匿名 API，共享出口 IP 额度耗尽时）：

```text
probe-repo https://github.com/LianXia233/H5000M-CI-Qmodem/tree/main
  ok = false
  error = GitHub API 请求受到限流，暂时无法获取最新状态（约 1700 秒后可重试）；已保留上次成功查询的数据
  error_code = rate_limited
  parsed.slug = LianXia233/H5000M-CI-Qmodem
```

即限流不会被误报成「没有 Release」或「资源不存在」。

## 3. `NetClass` 网络层分类

| code | 中文 | 典型原因 |
| :--- | :--- | :--- |
| `dns` | DNS 解析失败 | 域名无法解析 |
| `tcp` | TCP 连接失败 | 连接被拒 / 网络不可达 |
| `tls` | TLS 握手/证书失败 | 证书异常（**不关闭证书验证**） |
| `timeout` | HTTP 请求超时 | 超过 `api_timeout` |
| `http403` | HTTP 403 | 由 `ApiError` 进一步细分限流/权限 |
| `http404` | HTTP 404 | 资源不存在 |
| `http429` | HTTP 429 | 限流 |
| `http5xx` | HTTP 5xx | 服务端错误 |
| `bad_content` | 响应内容异常 | 非 JSON / HTML 错误页 |
| `rate_limited` | GitHub API 限流 | 匿名额度耗尽 |
| `mirror_down` | 加速源不可用 | 线路级失败，回退官方直连 |
| `official_down` | GitHub 官方服务不可用 | 所有线路均失败 |
| `ok` | 正常 | HTTP 200 |
| `unknown` | 未知网络错误 | 未归类 |

## 4. 状态必须文字化，颜色只是辅助

| 场景 | 正确显示 | 错误显示（禁止） |
| :--- | :--- | :--- |
| `status=queued` / `in_progress` | 「排队中」/「构建中，尚未产生最终结论」 | 「构建失败」 |
| `status=completed, conclusion=failure` | 「构建失败」 | 「没有固件」 |
| API 限流 | 「API 请求受到限流，已保留上次成功查询的数据（stale）」 | 「无 Release」「编译失败」 |
| 固件无法识别 | 「无法确认兼容，暂不允许自动刷写」 | 「明确不兼容」 |
| 归档包内无可刷写镜像 | 「归档包内未发现可刷写的 sysupgrade/factory 镜像，请勿直接刷写压缩包」 | 直接交给 `sysupgrade` |
| 缺少 SHA-256 校验依据 | 「仅记录本地 SHA256，未宣称已校验」 | 「校验通过」 |

## 5. 缓存新鲜度语义

| 字段 | 含义 |
| :--- | :--- |
| `stale` | 数据已过期 / 来自失败保留，**不能当实时数据使用** |
| `from_cache` | 本次结果来自缓存而非实时请求 |
| `queried_at` / `last_success_at` | 本次查询时间 / 上次成功查询时间（前端显示本地时间） |
| `api.status` | `ok` / `error` / `rate_limited` / `not_checked` / `querying` / `cached` |

**API 失败不污染远程真实状态**：`model::replace_api()` 只替换模型 JSON 里的 `api` 元数据，`state` / `data` 保持上次成功值。诊断层会在 `blocking` 章节明确追加「部分远程状态来自缓存（API 查询失败或限流），当前结论可能不是最新」。

## 6. 排查步骤（分步执行记录）

1. **先读只读缓存，不联网**：`online-upgrade states` 或 `ubus call luci.online-upgrade get_states`，查看四套模型各自的 `api.status` / `stale` 与诊断层 `next` 建议。
2. **判断是网络层还是 API 层**：`online-upgrade netcheck`（真实 HTTP 探测 `/rate_limit`，不消耗 core 额度），看 `class` / `http_status` / `latency_ms` / `rate_remaining`。
3. **校验仓库地址**：`online-upgrade probe-repo owner/repo`，成功时返回 `repo.slug`、`default_branch`、`workflows` 数量；失败时看 `error_code` 与 `parsed.slug`（能看出 URL 是否被正确解析）。
4. **按模型单独强制刷新**：`online-upgrade refresh --only actions --force`，避免全量刷新浪费匿名额度；观察该模型缓存是否变化、其他三套是否保持不变。
5. **必要时清空缓存重来**：`online-upgrade clear-cache`（不动固件文件、任务历史、运行日志），再执行第 3、4 步。
6. **固件下载后若被阻断**：看 `[verify]` 日志——依次检查 SHA-256、归档包安全解析、`fwtool` 元数据、`sysupgrade -T` 结论；明确不兼容会给出具体原因，无法确认兼容会提示暂不自动刷写。

## 7. 常见问题对照

| 现象 | 根因 | 处理 |
| :--- | :--- | :--- |
| `probe-repo` 返回「资源不存在」但仓库确实公开 | 早期版本 `api_url(repo, "")` 拼出尾斜杠 `/repos/{slug}/`，GitHub 对该路径返回 404 | 已在 v2.6 修复：空 suffix 不加尾斜杠，并有回归单测 `api_url_empty_suffix_has_no_trailing_slash` |
| 「约 N 秒后可重试」 | 匿名 API 60 次/小时额度耗尽（局域网共享出口 IP 常见） | 等待 `X-RateLimit-Reset`，或配置只读权限 Token（不强制） |
| 加速源返回 HTML 而非 JSON | 加速源不支持 API | API 保持直连 `api.github.com`；仅下载链路使用加速源并自动回退 |
| 构建显示「等待中」很久 | `status` 仍是 `waiting` / `queued` | 属正常进行中状态，不是失败；刷新后仍不变再查仓库 Workflow |
| 「无法确认兼容」但不提示不兼容 | 信息不足（无校验依据 / 文件无法识别） | 属设计行为：不自动刷写，也不会误判成明确不兼容 |
