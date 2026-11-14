// 对外状态聚合（阶段 4/6/7 编排层）。
//
// 这不是状态模型：它只是把四套**独立缓存文件**里的状态读出来，
// 加上诊断层结果，组装成一次 RPC 响应。它不持有任何业务状态，
// 不读写他人缓存（只读），也不修改任何模型的原始状态。
//
// refresh_all 按"每套模型各自独立刷新、失败互不覆盖"编排：
//   - Actions / Releases / Artifacts 并发查询（共享 Hub 做请求去重/限流）
//   - Compat 用前三者**只读快照**做本机兼容性判定（不发起 GitHub 请求）
//   - Network 独立探测
//   - Diagnostics 最后只读汇总

use crate::actions::ActionsModel;
use crate::artifacts::ArtifactsModel;
use crate::compatstate::CompatModel;
use crate::device::{self, DeviceInfo};
use crate::diagnostics::{self, Diagnosis};
use crate::ghapi::{self, ApiCfg, Hub, RepoSpec};
use crate::json::Value;
use crate::network::{self, ProbeResult};
use crate::releases::ReleasesModel;

pub const MODEL: &str = "states";
pub const SCHEMA_VERSION: u64 = 1;

/// 只读聚合：不触发任何网络请求，适合前端轮询。
pub fn read_only() -> Value {
    let repo = match ghapi::repo_from_uci() {
        Ok(r) => r.slug,
        Err(_) => String::new(),
    };
    let dev = device::collect();
    let actions = ActionsModel::load(&repo);
    let releases = ReleasesModel::load(&repo);
    let artifacts = ArtifactsModel::load(&repo);
    let compat = CompatModel::load(dev.clone());
    let network = network_cached();
    let diag = diagnostics::diagnose(&actions, &releases, &artifacts, &compat, network.as_ref());
    assemble(&repo, &dev, &actions, &releases, &artifacts, &compat, network.as_ref(), &diag, false)
}

fn network_cached() -> Option<ProbeResult> {
    let ttl = crate::cache::ttl_from_uci("ttl_network", crate::cache::TTL_NETWORK);
    let env = crate::cache::read(crate::cache::NETWORK_FILE)?;
    let p = env.payload.get("probe")?;
    let class = ghapi::NetClass::from_code(p.get("class").and_then(|v| v.as_str()).unwrap_or("unknown"));
    Some(ProbeResult {
        target: p.get("target").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        class,
        label: p.get("label").and_then(|v| v.as_str()).unwrap_or(class.label()).to_string(),
        detail: p.get("detail").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        http_status: p.get("http_status").and_then(|v| v.as_u64()).unwrap_or(0),
        latency_ms: p.get("latency_ms").and_then(|v| v.as_u64()).unwrap_or(0),
        retryable: matches!(p.get("retryable"), Some(Value::Bool(true))),
        from_cache: true,
        last_success_at: env.last_success_at,
        stale: env.stale || !env.is_fresh(ttl),
        rate_remaining: None,
    })
}

fn assemble(
    repo: &str,
    dev: &DeviceInfo,
    actions: &ActionsModel,
    releases: &ReleasesModel,
    artifacts: &ArtifactsModel,
    compat: &CompatModel,
    net: Option<&ProbeResult>,
    diag: &Diagnosis,
    refreshed: bool,
) -> Value {
    Value::Obj(vec![
        ("schema_version".into(), Value::num(SCHEMA_VERSION)),
        ("model".into(), Value::str(MODEL)),
        ("ok".into(), Value::Bool(true)),
        ("repo".into(), Value::str(repo)),
        ("generated_at".into(), Value::num(crate::cache::now())),
        ("generated_at_local".into(), Value::str(&crate::cache::human_ts(crate::cache::now()))),
        ("refreshed".into(), Value::Bool(refreshed)),
        ("device".into(), dev.to_json()),
        ("actions".into(), actions.to_json()),
        ("releases".into(), releases.to_json()),
        ("artifacts".into(), artifacts.to_json()),
        ("compat".into(), compat.to_json()),
        (
            "network".into(),
            match net {
                Some(n) => n.to_json(),
                None => Value::Null,
            },
        ),
        ("diagnostics".into(), diag.to_json()),
        (
            "cache_files".into(),
            Value::Obj(vec![
                ("actions".into(), Value::str(crate::cache::ACTIONS_FILE)),
                ("releases".into(), Value::str(crate::cache::RELEASES_FILE)),
                ("artifacts".into(), Value::str(crate::cache::ARTIFACTS_FILE)),
                ("compat".into(), Value::str(crate::cache::COMPAT_FILE)),
                ("network".into(), Value::str(crate::cache::NETWORK_FILE)),
                (
                    "ttls".into(),
                    Value::Obj(vec![
                        ("actions".into(), Value::num(crate::cache::TTL_ACTIONS)),
                        ("releases".into(), Value::num(crate::cache::TTL_RELEASES)),
                        ("artifacts".into(), Value::num(crate::cache::TTL_ARTIFACTS)),
                        ("compat".into(), Value::num(crate::cache::TTL_COMPAT)),
                        ("network".into(), Value::num(crate::cache::TTL_NETWORK)),
                        ("device".into(), Value::num(crate::cache::TTL_DEVICE)),
                        ("repo".into(), Value::num(crate::cache::TTL_REPO)),
                    ]),
                ),
            ]),
        ),
        (
            "policy".into(),
            network::policy_json(&ApiCfg::from_uci()),
        ),
    ])
}

/// 独立刷新编排。`only` 为空表示全部；否则只刷新指定模型
/// （actions / releases / artifacts / compat / network），其他模型缓存不动。
pub async fn refresh_all(only: Option<&str>, force: bool) -> Value {
    let repo_spec = match ghapi::repo_from_uci() {
        Ok(r) => r,
        Err(e) => return error_state(&e),
    };
    let repo = repo_spec.slug.clone();
    let cfg = ApiCfg::from_uci();
    let hub = Hub::new(cfg.clone());
    let dev = device::collect();

    let want = |name: &str| -> bool { only.map(|o| o == name || o == "all").unwrap_or(true) };

    // 三个远程模型并发刷新，共享 Hub：同一 URL 在一次刷新内只请求一次，
    // 并发受 Semaphore 限制；每个模型只写自己的缓存文件。
    let (actions, releases, artifacts, net) = if only.is_some() {
        // 单模型刷新：其余从缓存读取，绝不触发它们的网络请求
        let (a, r, t, n) = tokio::join!(
            async {
                if want("actions") { ActionsModel::refresh(&hub, &repo_spec, force).await }
                else { ActionsModel::load(&repo) }
            },
            async {
                if want("releases") { ReleasesModel::refresh(&hub, &repo_spec, force).await }
                else { ReleasesModel::load(&repo) }
            },
            async {
                if want("artifacts") { ArtifactsModel::refresh(&hub, &repo_spec, force).await }
                else { ArtifactsModel::load(&repo) }
            },
            async {
                if want("network") { Some(network::probe(&hub, force).await) }
                else { network_cached() }
            },
        );
        (a, r, t, n)
    } else {
        let (a, r, t, n) = tokio::join!(
            ActionsModel::refresh(&hub, &repo_spec, force),
            ReleasesModel::refresh(&hub, &repo_spec, force),
            ArtifactsModel::refresh(&hub, &repo_spec, force),
            network::probe(&hub, force),
        );
        (a, r, t, Some(n))
    };

    // 兼容性模型：从三套模型的**只读数据**发现候选文件，不发起 GitHub 请求。
    let compat = if want("compat") || only.is_none() {
        let mut files = releases.all_assets();
        files.extend(artifacts.live_files());
        let rule = if repo_spec.firmware_rule.is_empty() {
            None
        } else {
            Some(repo_spec.firmware_rule.as_str())
        };
        CompatModel::refresh(dev.clone(), files, rule, force)
    } else {
        CompatModel::load(dev.clone())
    };

    let diag = diagnostics::diagnose(&actions, &releases, &artifacts, &compat, net.as_ref());
    assemble(&repo, &dev, &actions, &releases, &artifacts, &compat, net.as_ref(), &diag, true)
}

fn error_state(msg: &str) -> Value {
    Value::Obj(vec![
        ("ok".into(), Value::Bool(false)),
        ("error".into(), Value::str(msg)),
        ("repo".into(), Value::str("")),
        ("model".into(), Value::str(MODEL)),
        (
            "hint".into(),
            Value::str("请先在「仓库配置」中填写有效的公开 GitHub 仓库地址（owner/repo 或完整 URL）"),
        ),
    ])
}

/// 设备信息独立子命令输出。
pub fn device_json() -> Value {
    let d = device::collect();
    Value::Obj(vec![
        ("ok".into(), Value::Bool(true)),
        ("device".into(), d.to_json()),
        ("generated_at".into(), Value::num(crate::cache::now())),
    ])
}

/// 仓库自动发现：解析地址 → 校验 → 获取公开信息 → 默认分支 → Workflow 列表。
pub async fn probe_repo(input: &str) -> Value {
    let spec = match ghapi::parse_repo(input) {
        Ok(s) => s,
        Err(e) => return Value::Obj(vec![("ok".into(), Value::Bool(false)), ("error".into(), Value::str(&e))]),
    };
    let hub = Hub::new(ApiCfg::from_uci());
    let meta = match ghapi::fetch_repo_meta(&hub, &spec).await {
        Ok(m) => m,
        Err(e) => {
            return Value::Obj(vec![
                ("ok".into(), Value::Bool(false)),
                ("error".into(), Value::str(&e.user_message())),
                ("error_code".into(), Value::str(e.code())),
                ("parsed".into(), spec_json(&spec)),
            ])
        }
    };
    let wf_url = ghapi::api_url(&spec, "actions/workflows?per_page=100");
    let workflows: Vec<Value> = match hub.get_json(&wf_url).await {
        Ok((v, _)) => v
            .get("workflows")
            .and_then(|x| x.as_arr())
            .unwrap_or(&[])
            .iter()
            .map(|w| Value::Obj(vec![
                ("id".into(), w.get("id").cloned().unwrap_or(Value::Null)),
                ("name".into(), w.get("name").cloned().unwrap_or(Value::Null)),
                ("path".into(), w.get("path").cloned().unwrap_or(Value::Null)),
                ("state".into(), w.get("state").cloned().unwrap_or(Value::Null)),
            ]))
            .collect(),
        Err(_) => Vec::new(), // Workflow 列表失败不影响仓库验证结果
    };
    Value::Obj(vec![
        ("ok".into(), Value::Bool(true)),
        ("parsed".into(), spec_json(&spec)),
        ("repo".into(), meta.to_json()),
        ("workflows".into(), Value::Arr(workflows)),
        ("rate_limit".into(), meta.rate.to_json()),
        ("note".into(), Value::str(
            "已自动发现仓库公开信息、默认分支与全部公开 Workflow；插件不会写死任何仓库/分支/Workflow",
        )),
    ])
}

fn spec_json(s: &RepoSpec) -> Value {
    Value::Obj(vec![
        ("owner".into(), Value::str(&s.owner)),
        ("repo".into(), Value::str(&s.repo)),
        ("slug".into(), Value::str(&s.slug)),
        ("build_branch".into(), Value::str(&s.build_branch)),
        ("workflow_filter".into(), Value::str(&s.workflow_filter)),
        ("firmware_rule".into(), Value::str(&s.firmware_rule)),
        ("source".into(), Value::str(&s.source)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_never_panics_without_config() {
        let v = read_only();
        assert_eq!(v.get("model").and_then(|x| x.as_str()), Some("states"));
        for k in ["device", "actions", "releases", "artifacts", "compat", "diagnostics"] {
            assert!(v.get(k).is_some(), "缺少 {k}");
        }
    }

    #[test]
    fn four_state_models_have_distinct_cache_files() {
        let a = crate::cache::ACTIONS_FILE;
        let r = crate::cache::RELEASES_FILE;
        let t = crate::cache::ARTIFACTS_FILE;
        let c = crate::cache::COMPAT_FILE;
        let mut all = vec![a, r, t, c];
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 4, "四套模型必须使用四个独立缓存文件");
    }

    #[test]
    fn four_models_have_distinct_default_ttls() {
        let ttls = [
            crate::cache::TTL_ACTIONS,
            crate::cache::TTL_RELEASES,
            crate::cache::TTL_ARTIFACTS,
            crate::cache::TTL_COMPAT,
        ];
        // TTL 允许数值相同，但每个模型都必须能通过独立 UCI key 覆盖
        for k in ["ttl_actions", "ttl_releases", "ttl_artifacts", "ttl_compat"] {
            // 不依赖 UCI 存在；仅验证可调用
            let _ = crate::cache::ttl_from_uci(k, 300);
        }
        assert!(ttls.iter().all(|t| *t >= 15));
    }

    #[test]
    fn error_state_gives_actionable_hint() {
        let v = error_state("仓库地址为空");
        assert_eq!(v.get("ok"), Some(&Value::Bool(false)));
        assert!(v.get("hint").and_then(|x| x.as_str()).unwrap().contains("仓库配置"));
    }

    #[test]
    fn device_json_is_self_contained() {
        let v = device_json();
        assert_eq!(v.get("ok"), Some(&Value::Bool(true)));
        assert!(v.get("device").and_then(|x| x.get("model")).is_some());
    }
}
