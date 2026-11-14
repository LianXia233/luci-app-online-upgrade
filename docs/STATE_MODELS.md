# 四套独立状态模型与统一诊断层（v2.6）

> 本文档描述 `luci-app-online-upgrade` v2.6 的底层状态设计。对应源码：`rust/src/actions.rs`、`releases.rs`、`artifacts.rs`、`compatstate.rs`、`diagnostics.rs`、`states.rs`、`cache.rs`、`model.rs`。

## 1. 设计原则

旧版用一份 `/tmp/online-upgrade-status.json`（`stage` / `pct` / `msg` / `error` / `result`）代表检测、下载、升级、兼容性等全部信息，导致一次 API 失败就能覆盖已获取的真实状态，也无法解释「构建失败但历史 Release 可用」这类场景。

v2.6 的硬性约束：

| 约束 | 落地方式 |
| :--- | :--- |
| 四套模型底层真正独立 | 各自独立 Rust 模块、状态枚举、缓存文件、TTL、刷新函数、错误处理 |
| 不允许单一 `upgrade_status` | `model.rs` 只提供元数据框架（`ApiMeta` / `frame`），明确注释禁止统一状态机 |
| 诊断层只读 | `diagnose()` 签名全部使用不可变引用 `&ActionsModel` / `&ReleasesModel` / `&ArtifactsModel` / `&CompatModel`，编译期阻止写回 |
| 失败不覆盖真实状态 | API 失败时只替换模型 JSON 中的 `api` 元数据（`model::replace_api`），业务 `state` / `data` 一字不动 |
| 缓存不冒充实时数据 | `Envelope` 显式记录 `stale` / `from_cache` / `last_success_at` |
| 向后兼容 | 旧 `status.json` 与旧 `check` / `download` / `upgrade` 流程完全保留，新命令不写它、不占用升级操作锁 |

## 2. 四套模型的物理隔离

| 模型 | 模块 | 独立缓存文件 | 默认 TTL | UCI 覆盖键 | 只回答什么 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| GitHub Actions | `actions.rs` | `/tmp/online-upgrade-cache/actions.json` | 120s | `ttl_actions` | 远程 CI 构建任务状态 |
| GitHub Releases | `releases.rs` | `/tmp/online-upgrade-cache/releases.json` | 300s | `ttl_releases` | 正式发布与发布资产 |
| Actions Artifacts | `artifacts.rs` | `/tmp/online-upgrade-cache/artifacts.json` | 180s | `ttl_artifacts` | 临时构建产物（会过期） |
| 固件兼容性 | `compatstate.rs` | `/tmp/online-upgrade-cache/compat.json` | 600s | `ttl_compat` | 某个固件文件是否适合本机 |
| 网络探测（辅助，非第五套业务模型） | `network.rs` | `/tmp/online-upgrade-cache/network.json` | 60s | `ttl_network` | API 连通性与线路状态 |

单测 `states::tests::four_state_models_have_distinct_cache_files` 与 `four_models_have_distinct_default_ttls` 验证四个缓存路径互不相同、TTL 独立。`refresh --only <model>` 只刷新指定模型，其余缓存文件不动。

## 3. 各模型状态枚举（真实 code）

### 3.1 Actions（`ActionsState`）

| code | 含义 | 触发条件 |
| :--- | :--- | :--- |
| `not_checked` / `querying` | 未检测 / 查询中 | 初始态 |
| `queued` / `requested` / `waiting` / `pending` / `building` | 排队中 / 已请求 / 等待中 / 待处理 / 构建中 | `status` 非 `completed` |
| `success` / `failure` / `cancelled` / `timed_out` / `skipped` / `action_required` / `neutral` | 已完成的各种结论 | 仅当 `status=completed`，按 `conclusion` 映射 |
| `unknown` | 未知状态 | 出现未能识别的 `status` / `conclusion`，不推测 |
| `api_failed` / `rate_limited` | API 失败 / 限流 | 查询异常 |

**`status` 与 `conclusion` 严格分离**：`map_run()` 中只要 `status != completed`，就一律返回进行中状态并附说明「任务仍在进行（status=...），尚未产生最终结论」；即使响应里残留旧的 `conclusion` 也不会显示成构建失败。查询 `/actions/workflows?per_page=100` 与 `/actions/runs?per_page=30`，支持 `build_branch` 与 `workflow_filter` 筛选，不只看最近一条运行记录。

### 3.2 Releases（`ReleasesState`）

`not_checked` / `querying` / `has_stable` / `only_prerelease` / `has_draft` / `none` / `api_failed` / `rate_limited` / `unknown`。

`ReleaseInfo` 保留 id、tag、name、target_commitish、draft、prerelease、body、created/published/updated、html_url、assets。资产经 `firmware::from_release_asset()` 分类，**不把所有 assets 当固件**；`all_assets()` 供 Compat 模型只读消费。

### 3.3 Artifacts（`ArtifactsState`）

`not_checked` / `querying` / `available` / `multiple` / `expired` / `deleted` / `not_downloadable` / `not_generated` / `api_failed` / `rate_limited` / `unknown`。

`ArtifactInfo` 保留 id、name、size、created/updated/expires、expired、archived、run_id、workflow_id、head_sha、download_url、downloadable。明确：**匿名访问通常无法下载 Artifact，这是 `not_downloadable`，不等于查询失败**；Artifact 名称也不足以判定内部是否含固件，需下载后解析。

### 3.4 固件兼容性（`CompatState`）

`not_checked` / `checking` / `compatible` / `risk` / `unconfirmed` / `incompatible` / `corrupted` / `unsupported_format` / `checksum_failed` / `local_verify_failed`。

关键语义：**`unconfirmed`（无法确认兼容）绝不显示为 `incompatible`（明确不兼容）**。`blocking_reason()` 对 `unconfirmed` 给出「当前设备的兼容性尚无法确认，因此暂不允许自动刷写」；只有 `compatible` 才 `can_flash()`。

五层判定每层独立记录 `LayerResult { layer, name, passed, detail }`：

| 层 | 名称 | 远程阶段 | 下载后 |
| :--- | :--- | :--- | :--- |
| L1 | 文件名 / 格式初筛 | 是 | 复用 |
| L2 | 固件内部元数据（压缩包清单 / `digest` / 声明字段） | 是 | 复用 |
| L3 | 设备维度比对（型号 / compatible / Target / Subtarget / CPU 架构 / Package Arch / ABI / 版本 / 类型） | 是 | 复用 |
| L4 | 下载后完整性（HTTP 响应 / 大小 / SHA-256 / 实际格式） | 否 | `finalize_after_download` 合并 |
| L5 | 系统原生验证（`fwtool` + `sysupgrade -T`，只读不刷写） | 否 | `finalize_after_download` 合并 |

远程阶段 `evaluate()` 最多到 L3，**下载前绝不宣称 `compatible`**。实际下载/升级流程通过 `compatstate::record_download_result()` 把 L4/L5 真实结果写回 compat 自己的缓存；该函数不发起 GitHub 请求、不写其他三套模型缓存、不参与刷写决策。缺少 SHA-256 校验依据时降级为 `unconfirmed` 而非 `checksum_failed`。

## 4. 缓存 Envelope

```text
Envelope {
  payload,            // 模型自己的完整 JSON（失败时保留上次成功 payload）
  fetched_at,         // 本次写入时间
  last_success_at,    // 上次成功查询时间
  stale,              // 缓存已过期 / 来自失败保留，不能当实时数据
  api_status,         // ok | error | rate_limited | cached
  api_error,
  source,             // live | cache | failure_preserved
}
```

- `cache::write()` 原子写入（临时文件 + `rename`），并限制缓存文件最大体积，避免撑爆 tmpfs。
- `cache::preserve_on_failure()` 在 API 失败时保留上次成功的 `payload`，只把 `stale` / `api_status` / `api_error` 标记为失败。
- `cache::invalidate_all()` 只清四套模型 + 网络 + 辅助槽位，**不动** `status.json`、任务历史、固件文件、运行日志。
- `ApiMeta` 统一携带 `queried_at` / `last_success_at` / `stale` / `from_cache` / `rate_limit`，供前端显示「数据有多新」。

## 5. 统一诊断层

`diagnostics::diagnose()` 签名：

```rust
pub fn diagnose(
    actions: &ActionsModel,
    releases: &ReleasesModel,
    artifacts: &ArtifactsModel,
    compat: &CompatModel,
    network: Option<&ProbeResult>,
) -> Diagnosis
```

全部为不可变引用，编译期阻止诊断层修改任何模型。输出 9 个章节：

| 章节 key | 内容 |
| :--- | :--- |
| `network` | API 连通性、线路策略、限流额度 |
| `actions` | 构建状态与运行记录摘要 |
| `releases` | 正式/预发布/Draft/无 Release |
| `artifacts` | 可用/多个/过期/匿名不可下载 |
| `firmware` | 可识别固件候选、Release 是否含校验文件 |
| `compat` | 本机兼容性结论与阻断原因 |
| `availability` | 「当前是否存在可用升级文件」规则矩阵结论 |
| `blocking` | 无法升级的具体原因汇总 |
| `next` | 建议下一步 |

`rule_matrix()` 覆盖的典型场景：

| Actions | Releases | Artifacts | Compat | 结论方向 |
| :--- | :--- | :--- | :--- | :--- |
| 构建失败 | 有历史正式 Release | — | — | 构建失败不阻断历史 Release，可手动选择 |
| 构建成功 | 无 Release | 有 Artifacts | — | 没有正式 Release，不代表没有可用固件 |
| 构建成功 | 无 Release | 无 Artifacts | — | 构建成功但未发现可刷写产物 |
| API 限流 | 有缓存 | — | — | 已保留上次成功查询的数据，结论可能不是最新 |
| 任意 | 任意 | 任意 | `unconfirmed` | 兼容性尚无法确认，暂不允许自动刷写 |

单测覆盖：构建失败不覆盖 Release、无 Release 不覆盖 Artifact、API 失败不显示编译失败、诊断层不修改模型。

## 6. 对外聚合（不是第五套状态模型）

`states::read_only()` 只读四套独立缓存 + 网络缓存 + 诊断结果，**不发起任何 GitHub 请求**，输出 `device` / `actions` / `releases` / `artifacts` / `compat` / `network` / `diagnostics` / `cache_files` / `policy`。

`states::refresh_all(only, force)` 用 `tokio::join!` 并发刷新 Actions / Releases / Artifacts / Network，共享一个 `Hub` 做请求去重与并发限制；Compat 从 Releases / Artifacts 的**只读快照**取得候选文件后自行评估；最后才调用诊断层只读汇总。

## 7. 结论

四套模型在数据结构、状态枚举、缓存文件、TTL、刷新与错误处理六个维度全部隔离；诊断层是纯只读关联分析；缓存数据带明确新鲜度语义。因此「构建失败」「没有 Release」「Artifact 过期」「限流」「固件不兼容」可以并存且互不覆盖，前端四张卡片只是这四套真实底层模型的视图，而非同一状态变量的四种配色。
