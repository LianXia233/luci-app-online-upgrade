// 状态模型 1/4：GitHub Actions 构建状态（**完全独立**）。
//
// 独立持有：状态枚举、数据结构、缓存文件 ACTIONS_FILE、TTL_ACTIONS、
// refresh()、错误处理。不读写其他任何模型的缓存。
//
// 只描述「远程 CI 构建任务」，绝不表示「是否已有可刷写固件」。
// status 与 conclusion 严格分开：只有 status=completed 才看 conclusion。

use crate::cache::{self, Envelope, ACTIONS_FILE, TTL_ACTIONS};
use crate::ghapi::{api_url, ApiError, Hub, RepoSpec};
use crate::json::Value;
use crate::model::{self, ApiMeta};

pub const MODEL: &str = "actions";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionsState {
    NotChecked, Querying, Queued, Requested, Waiting, Pending, Building,
    Success, Failure, Cancelled, TimedOut, Skipped, ActionRequired, Neutral,
    Unknown, ApiFailed, RateLimited,
}

impl ActionsState {
    pub fn code(self) -> &'static str {
        match self {
            ActionsState::NotChecked => "not_checked",
            ActionsState::Querying => "querying",
            ActionsState::Queued => "queued",
            ActionsState::Requested => "requested",
            ActionsState::Waiting => "waiting",
            ActionsState::Pending => "pending",
            ActionsState::Building => "building",
            ActionsState::Success => "success",
            ActionsState::Failure => "failure",
            ActionsState::Cancelled => "cancelled",
            ActionsState::TimedOut => "timed_out",
            ActionsState::Skipped => "skipped",
            ActionsState::ActionRequired => "action_required",
            ActionsState::Neutral => "neutral",
            ActionsState::Unknown => "unknown",
            ActionsState::ApiFailed => "api_failed",
            ActionsState::RateLimited => "rate_limited",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ActionsState::NotChecked => "未检测",
            ActionsState::Querying => "查询中",
            ActionsState::Queued => "排队等待",
            ActionsState::Requested => "已请求运行",
            ActionsState::Waiting => "等待执行",
            ActionsState::Pending => "等待处理",
            ActionsState::Building => "正在编译",
            ActionsState::Success => "编译成功",
            ActionsState::Failure => "编译失败",
            ActionsState::Cancelled => "已取消",
            ActionsState::TimedOut => "编译超时",
            ActionsState::Skipped => "已跳过",
            ActionsState::ActionRequired => "需要人工处理",
            ActionsState::Neutral => "中性结束状态",
            ActionsState::Unknown => "未知状态",
            ActionsState::ApiFailed => "API 查询失败",
            ActionsState::RateLimited => "API 限流",
        }
    }
    /// 该状态是否表示「构建任务尚未结束」
    pub fn is_pending(self) -> bool {
        matches!(
            self,
            ActionsState::Queued | ActionsState::Requested | ActionsState::Waiting
                | ActionsState::Pending | ActionsState::Building | ActionsState::Querying
        )
    }
    pub fn from_code(s: &str) -> ActionsState {
        match s {
            "not_checked" => ActionsState::NotChecked,
            "querying" => ActionsState::Querying,
            "queued" => ActionsState::Queued,
            "requested" => ActionsState::Requested,
            "waiting" => ActionsState::Waiting,
            "pending" => ActionsState::Pending,
            "building" => ActionsState::Building,
            "success" => ActionsState::Success,
            "failure" => ActionsState::Failure,
            "cancelled" => ActionsState::Cancelled,
            "timed_out" => ActionsState::TimedOut,
            "skipped" => ActionsState::Skipped,
            "action_required" => ActionsState::ActionRequired,
            "neutral" => ActionsState::Neutral,
            "api_failed" => ActionsState::ApiFailed,
            "rate_limited" => ActionsState::RateLimited,
            _ => ActionsState::Unknown,
        }
    }
}

/// GitHub 状态 → 插件状态。**status 未 completed 时绝不根据 conclusion 判失败**。
pub fn map_run(status: &str, conclusion: Option<&str>) -> (ActionsState, String) {
    let s = status.to_ascii_lowercase();
    if s != "completed" {
        let st = match s.as_str() {
            "queued" => ActionsState::Queued,
            "requested" => ActionsState::Requested,
            "waiting" => ActionsState::Waiting,
            "pending" => ActionsState::Pending,
            "in_progress" => ActionsState::Building,
            _ => ActionsState::Unknown,
        };
        return (st, format!("任务仍在进行（status={status}），尚未产生最终结论"));
    }
    let c = conclusion.unwrap_or("").to_ascii_lowercase();
    let st = match c.as_str() {
        "success" => ActionsState::Success,
        "failure" => ActionsState::Failure,
        "cancelled" => ActionsState::Cancelled,
        "timed_out" => ActionsState::TimedOut,
        "action_required" => ActionsState::ActionRequired,
        "neutral" => ActionsState::Neutral,
        "skipped" => ActionsState::Skipped,
        "" => ActionsState::Unknown,
        _ => ActionsState::Unknown,
    };
    let detail = if c.is_empty() {
        "构建已结束但 GitHub 未给出 conclusion".to_string()
    } else {
        format!("构建已完成，结论 conclusion={c}")
    };
    (st, detail)
}

#[derive(Debug, Clone)]
pub struct WorkflowBrief {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub state: String,
}

impl WorkflowBrief {
    fn from_json(v: &Value) -> WorkflowBrief {
        WorkflowBrief {
            id: v.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
            name: gs(v, "name"),
            path: gs(v, "path"),
            state: gs(v, "state"),
        }
    }
    fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("id".into(), Value::num(self.id)),
            ("name".into(), Value::str(&self.name)),
            ("path".into(), Value::str(&self.path)),
            ("state".into(), Value::str(&self.state)),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowRun {
    pub run_id: u64,
    pub run_number: u64,
    pub workflow_id: u64,
    pub workflow_name: String,
    pub workflow_path: String,
    pub branch: String,
    pub sha: String,
    pub event: String,
    pub created_at: String,
    pub started_at: String,
    pub completed_at: String,
    pub updated_at: String,
    pub status: String,
    pub conclusion: String,
    pub html_url: String,
    pub artifacts_url: String,
    pub run_state: ActionsState,
    pub run_detail: String,
}

impl WorkflowRun {
    pub fn from_json(v: &Value, workflows: &[WorkflowBrief]) -> WorkflowRun {
        let status = gs(v, "status");
        let conclusion = gs(v, "conclusion");
        let (run_state, run_detail) = map_run(&status, if conclusion.is_empty() { None } else { Some(&conclusion) });
        let wid = v.get("workflow_id").and_then(|x| x.as_u64()).unwrap_or(0);
        let wf = workflows.iter().find(|w| w.id == wid);
        WorkflowRun {
            run_id: v.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
            run_number: gn(v, "run_number"),
            workflow_id: wid,
            workflow_name: wf.map(|w| w.name.clone()).unwrap_or_else(|| gs(v, "name")),
            workflow_path: wf.map(|w| w.path.clone()).unwrap_or_default(),
            branch: gs(v, "head_branch"),
            sha: gs(v, "head_sha"),
            event: gs(v, "event"),
            created_at: gs(v, "created_at"),
            started_at: gs(v, "run_started_at"),
            completed_at: gs(v, "updated_at"),
            updated_at: gs(v, "updated_at"),
            status,
            conclusion,
            html_url: gs(v, "html_url"),
            artifacts_url: gs(v, "artifacts_url"),
            run_state,
            run_detail,
        }
    }

    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("run_id".into(), Value::num(self.run_id)),
            ("run_number".into(), Value::num(self.run_number)),
            ("workflow_id".into(), Value::num(self.workflow_id)),
            ("workflow_name".into(), Value::str(&self.workflow_name)),
            ("workflow_path".into(), Value::str(&self.workflow_path)),
            ("branch".into(), Value::str(&self.branch)),
            ("sha".into(), Value::str(&self.sha)),
            ("event".into(), Value::str(&self.event)),
            ("created_at".into(), Value::str(&self.created_at)),
            ("created_at_local".into(), Value::str(&crate::github::utc_to_local(&self.created_at))),
            ("started_at".into(), Value::str(&self.started_at)),
            ("completed_at".into(), Value::str(&self.completed_at)),
            ("completed_at_local".into(), Value::str(&crate::github::utc_to_local(&self.completed_at))),
            ("updated_at".into(), Value::str(&self.updated_at)),
            ("status".into(), Value::str(&self.status)),
            ("conclusion".into(), Value::str(&self.conclusion)),
            ("html_url".into(), Value::str(&self.html_url)),
            ("artifacts_url".into(), Value::str(&self.artifacts_url)),
            ("state".into(), Value::str(self.run_state.code())),
            ("state_label".into(), Value::str(self.run_state.label())),
            ("detail".into(), Value::str(&self.run_detail)),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct ActionsModel {
    pub repo: String,
    pub state: ActionsState,
    pub label: String,
    pub detail: String,
    pub default_branch: String,
    pub build_branch: String,
    pub workflow_filter: String,
    pub workflows: Vec<WorkflowBrief>,
    pub runs: Vec<WorkflowRun>,
    pub meta: ApiMeta,
    pub ttl: u64,
}

impl ActionsModel {
    pub fn not_checked(repo: &str) -> ActionsModel {
        ActionsModel {
            repo: repo.to_string(),
            state: ActionsState::NotChecked,
            label: ActionsState::NotChecked.label().into(),
            detail: "尚未查询 GitHub Actions 构建状态".into(),
            default_branch: String::new(),
            build_branch: String::new(),
            workflow_filter: String::new(),
            workflows: Vec::new(),
            runs: Vec::new(),
            meta: ApiMeta::not_checked(),
            ttl: cache::ttl_from_uci("ttl_actions", TTL_ACTIONS),
        }
    }

    /// 从**自己的**缓存文件还原。不读其他模型文件。
    pub fn load(repo: &str) -> ActionsModel {
        let ttl = cache::ttl_from_uci("ttl_actions", TTL_ACTIONS);
        let env = match cache::read(ACTIONS_FILE) {
            Some(e) => e,
            None => return ActionsModel::not_checked(repo),
        };
        let meta = ApiMeta::from_envelope(&env);
        let p = &env.payload;
        let state = ActionsState::from_code(&gs(p, "state"));
        let mut m = ActionsModel {
            repo: gs(p, "repo").if_empty(repo),
            state,
            label: gs(p, "state_label"),
            detail: gs(p, "detail"),
            default_branch: gs(p, "default_branch"),
            build_branch: gs(p, "build_branch"),
            workflow_filter: gs(p, "workflow_filter"),
            workflows: Vec::new(),
            runs: Vec::new(),
            meta,
            ttl,
        };
        if let Some(arr) = p.get("runs").and_then(|x| x.as_arr()) {
            m.runs = arr
                .iter()
                .map(|v| WorkflowRun {
                    run_id: gn(v, "run_id"),
                    run_number: gn(v, "run_number"),
                    workflow_id: gn(v, "workflow_id"),
                    workflow_name: gs(v, "workflow_name"),
                    workflow_path: gs(v, "workflow_path"),
                    branch: gs(v, "branch"),
                    sha: gs(v, "sha"),
                    event: gs(v, "event"),
                    created_at: gs(v, "created_at"),
                    started_at: gs(v, "started_at"),
                    completed_at: gs(v, "completed_at"),
                    updated_at: gs(v, "updated_at"),
                    status: gs(v, "status"),
                    conclusion: gs(v, "conclusion"),
                    html_url: gs(v, "html_url"),
                    artifacts_url: String::new(),
                    run_state: ActionsState::from_code(&gs(v, "state")),
                    run_detail: gs(v, "detail"),
                })
                .collect();
        }
        if let Some(arr) = p.get("workflows").and_then(|x| x.as_arr()) {
            m.workflows = arr
                .iter()
                .map(|v| WorkflowBrief {
                    id: gn(v, "id"),
                    name: gs(v, "name"),
                    path: gs(v, "path"),
                    state: gs(v, "state"),
                })
                .collect();
        }
        m
    }

    pub fn to_json(&self) -> Value {
        let data = vec![
            ("repo".into(), Value::str(&self.repo)),
            ("detail".into(), Value::str(&self.detail)),
            ("default_branch".into(), Value::str(&self.default_branch)),
            ("build_branch".into(), Value::str(&self.build_branch)),
            ("workflow_filter".into(), Value::str(&self.workflow_filter)),
            (
                "workflows".into(),
                Value::Arr(self.workflows.iter().map(|w| w.to_json()).collect()),
            ),
            (
                "runs".into(),
                Value::Arr(self.runs.iter().map(|r| r.to_json()).collect()),
            ),
            ("run_count".into(), Value::num(self.runs.len() as u64)),
            ("workflow_count".into(), Value::num(self.workflows.len() as u64)),
            (
                "latest_build".into(),
                match self.runs.first() {
                    Some(r) => r.to_json(),
                    None => Value::Null,
                },
            ),
        ];
        model::frame(MODEL, self.state.code(), &self.label, data, &self.meta, self.ttl)
    }

    /// 独立刷新：只写 ACTIONS_FILE。失败时保留上次成功的业务状态，
    /// 仅把 api 元数据标为 error/rate_limited + stale。
    pub async fn refresh(hub: &Hub, repo: &RepoSpec, force: bool) -> ActionsModel {
        let ttl = cache::ttl_from_uci("ttl_actions", TTL_ACTIONS);
        if !force {
            if let Some(env) = cache::usable(ACTIONS_FILE, ttl) {
                let mut m = ActionsModel::load(&repo.slug);
                let mut meta = ApiMeta::from_envelope(&env);
                meta.status = "cached".into();
                meta.from_cache = true;
                meta.stale = false;
                m.meta = meta;
                return m;
            }
            if cache::in_backoff(ACTIONS_FILE, cache::FAILURE_BACKOFF_SECS) {
                let mut m = ActionsModel::load(&repo.slug);
                m.meta.from_cache = true;
                m.meta.stale = true;
                return m;
            }
        }

        match Self::query(hub, repo).await {
            Ok(mut m) => {
                m.meta = ApiMeta::ok(m.meta.rate.clone());
                m.ttl = ttl;
                let payload = m.to_json();
                let _ = cache::write(ACTIONS_FILE, &Envelope::fresh(payload, "ok"));
                m
            }
            Err(e) => {
                crate::logline!("[actions] 查询失败: {} ({})", e.code(), e.user_message());
                let status = if e.is_rate_limited() { "rate_limited" } else { "error" };
                let mut env = cache::preserve_on_failure(ACTIONS_FILE, status, &e.user_message());
                let meta = ApiMeta::from_error(&e, &env);
                env.payload = model::replace_api(env.payload.clone(), &meta);
                let _ = cache::write(ACTIONS_FILE, &env);
                let mut m = ActionsModel::load(&repo.slug);
                m.meta = meta;
                m
            }
        }
    }

    async fn query(hub: &Hub, repo: &RepoSpec) -> Result<ActionsModel, ApiError> {
        // 1) 公开 Workflow 列表（自动发现，不假设只有一个）
        let wf_url = api_url(repo, "actions/workflows?per_page=100");
        let (wf_val, rate) = hub.get_json(&wf_url).await?;
        let mut workflows: Vec<WorkflowBrief> = wf_val
            .get("workflows")
            .and_then(|x| x.as_arr())
            .unwrap_or(&[])
            .iter()
            .map(WorkflowBrief::from_json)
            .collect();

        // 2) 最近运行记录：取多条，不能只看一条就下结论
        let runs_url = api_url(repo, "actions/runs?per_page=30");
        let (runs_val, _) = hub.get_json(&runs_url).await?;
        let mut runs: Vec<WorkflowRun> = runs_val
            .get("workflow_runs")
            .and_then(|x| x.as_arr())
            .unwrap_or(&[])
            .iter()
            .map(|v| WorkflowRun::from_json(v, &workflows))
            .collect();

        // 3) 分支筛选：用户配置 build_branch 优先，否则跟随默认分支
        let build_branch = if repo.build_branch.is_empty() {
            String::new()
        } else {
            repo.build_branch.clone()
        };
        if !build_branch.is_empty() {
            runs.retain(|r| r.branch.eq_ignore_ascii_case(&build_branch));
        }
        // 4) Workflow 筛选（名称或路径子串，不区分大小写）
        let filter = repo.workflow_filter.to_ascii_lowercase();
        if !filter.is_empty() {
            runs.retain(|r| {
                r.workflow_name.to_ascii_lowercase().contains(&filter)
                    || r.workflow_path.to_ascii_lowercase().contains(&filter)
            });
            workflows.retain(|w| {
                w.name.to_ascii_lowercase().contains(&filter)
                    || w.path.to_ascii_lowercase().contains(&filter)
            });
        }

        // 5) 汇总状态：任一未完成 → 以最"活跃"的为准；否则取最新一条结论
        let (state, detail) = if let Some(p) = runs.iter().find(|r| r.run_state.is_pending()) {
            (
                p.run_state,
                format!(
                    "检测到 {} 个运行记录，其中 [{}] 仍在 {}；不据此判定整个仓库构建失败",
                    runs.len(),
                    p.workflow_name,
                    p.run_state.label()
                ),
            )
        } else if let Some(first) = runs.first() {
            (
                first.run_state,
                format!(
                    "最近一次构建 [{}]（{} / {}）：{}",
                    first.workflow_name,
                    if first.branch.is_empty() { "未知分支" } else { &first.branch },
                    if first.sha.is_empty() { "未知提交" } else { &first.sha[..first.sha.len().min(7)] },
                    first.run_state.label()
                ),
            )
        } else {
            (
                ActionsState::Unknown,
                if filter.is_empty() && build_branch.is_empty() {
                    "未发现任何 Actions 运行记录".into()
                } else {
                    "当前分支/Workflow 筛选条件下未发现运行记录".into()
                },
            )
        };

        Ok(ActionsModel {
            repo: repo.slug.clone(),
            state,
            label: state.label().into(),
            detail,
            default_branch: String::new(),
            build_branch,
            workflow_filter: repo.workflow_filter.clone(),
            workflows,
            runs,
            meta: ApiMeta::ok(rate),
            ttl: cache::ttl_from_uci("ttl_actions", TTL_ACTIONS),
        })
    }
}

// ---- 小型 JSON 取值助手（模块内私有，避免污染其他模型） ----
fn gs(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn gn(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}
trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}
impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() { fallback.to_string() } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SCHEMA_VERSION;

    #[test]
    fn status_and_conclusion_are_strictly_separated() {
        // 未完成的运行绝不能显示为失败，即使 conclusion 残留旧值
        let (s, _) = map_run("in_progress", Some("failure"));
        assert_eq!(s, ActionsState::Building);
        let (s2, _) = map_run("queued", None);
        assert_eq!(s2, ActionsState::Queued);
        let (s3, _) = map_run("completed", Some("failure"));
        assert_eq!(s3, ActionsState::Failure);
    }

    #[test]
    fn all_completed_conclusions_are_mapped() {
        let cases = [
            ("success", ActionsState::Success),
            ("failure", ActionsState::Failure),
            ("cancelled", ActionsState::Cancelled),
            ("timed_out", ActionsState::TimedOut),
            ("action_required", ActionsState::ActionRequired),
            ("neutral", ActionsState::Neutral),
            ("skipped", ActionsState::Skipped),
            ("", ActionsState::Unknown),
            ("weird_new_value", ActionsState::Unknown),
        ];
        for (c, want) in cases {
            assert_eq!(map_run("completed", Some(c)).0, want, "conclusion={c}");
        }
        assert_eq!(map_run("completed", None).0, ActionsState::Unknown);
    }

    #[test]
    fn unknown_status_is_unknown_not_failure() {
        assert_eq!(map_run("some_new_status", None).0, ActionsState::Unknown);
    }

    #[test]
    fn pending_states_do_not_mean_failure() {
        for s in [ActionsState::Queued, ActionsState::Requested, ActionsState::Waiting,
                  ActionsState::Pending, ActionsState::Building] {
            assert!(s.is_pending());
            assert_ne!(s, ActionsState::Failure);
        }
        assert!(!ActionsState::Success.is_pending());
        assert!(!ActionsState::Failure.is_pending());
    }

    #[test]
    fn to_json_roundtrip_state_and_runs() {
        let mut m = ActionsModel::not_checked("owner/repo");
        m.state = ActionsState::Building;
        m.label = ActionsState::Building.label().into();
        m.runs.push(WorkflowRun {
            run_id: 42, run_number: 7, workflow_id: 9,
            workflow_name: "build".into(), workflow_path: ".github/workflows/b.yml".into(),
            branch: "main".into(), sha: "abcdef1234567".into(), event: "push".into(),
            created_at: "2026-11-13T01:00:00Z".into(),
            started_at: "2026-11-13T01:00:00Z".into(),
            completed_at: String::new(), updated_at: "2026-11-13T01:05:00Z".into(),
            status: "in_progress".into(), conclusion: String::new(),
            html_url: "https://github.com/o/r/actions/runs/42".into(),
            artifacts_url: String::new(),
            run_state: ActionsState::Building,
            run_detail: "任务仍在进行".into(),
        });
        let j = m.to_json();
        assert_eq!(j.get("model").and_then(|x| x.as_str()), Some("actions"));
        assert_eq!(j.get("state").and_then(|x| x.as_str()), Some("building"));
        let runs = j.get("runs").and_then(|x| x.as_arr()).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].get("state").and_then(|x| x.as_str()), Some("building"));
        assert_eq!(j.get("schema_version").and_then(|x| x.as_u64()), Some(SCHEMA_VERSION));
    }

    #[test]
    fn actions_success_does_not_claim_firmware_exists() {
        let j = ActionsModel::not_checked("o/r").to_json();
        assert!(j.get("has_firmware").is_none(), "Actions 模型不得声明固件可用性");
        assert!(j.get("compatibility").is_none(), "Actions 模型不得含兼容性结论");
    }
}
