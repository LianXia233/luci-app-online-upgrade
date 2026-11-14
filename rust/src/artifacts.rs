// 状态模型 3/4：GitHub Actions Artifacts 构建产物状态（**完全独立**）。
//
// 独立持有：状态枚举、数据结构、缓存文件 ARTIFACTS_FILE、TTL_ARTIFACTS、
// refresh()、错误处理。不读写其他模型缓存。
//
// 重要：Artifact ≠ Release Asset；Artifact 存在 ≠ 内部有可刷写固件；
// 匿名访问通常无法下载 Artifact（archive_download_url 需要鉴权），
// 这一点必须独立表达，不能当成"查询失败"或"没有产物"。

use crate::cache::{self, Envelope, ARTIFACTS_FILE, TTL_ARTIFACTS};
use crate::firmware::{from_artifact, FirmwareFile};
use crate::ghapi::{api_url, ApiError, Hub, RepoSpec};
use crate::json::Value;
use crate::model::{self, ApiMeta};

pub const MODEL: &str = "artifacts";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactsState {
    NotChecked, Querying, Available, Multiple, Expired, Deleted,
    NotDownloadable, NotGenerated, ApiFailed, RateLimited, Unknown,
}

impl ArtifactsState {
    pub fn code(self) -> &'static str {
        match self {
            ArtifactsState::NotChecked => "not_checked",
            ArtifactsState::Querying => "querying",
            ArtifactsState::Available => "available",
            ArtifactsState::Multiple => "multiple",
            ArtifactsState::Expired => "expired",
            ArtifactsState::Deleted => "deleted",
            ArtifactsState::NotDownloadable => "not_downloadable",
            ArtifactsState::NotGenerated => "not_generated",
            ArtifactsState::ApiFailed => "api_failed",
            ArtifactsState::RateLimited => "rate_limited",
            ArtifactsState::Unknown => "unknown",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ArtifactsState::NotChecked => "未检测",
            ArtifactsState::Querying => "查询中",
            ArtifactsState::Available => "存在可用 Artifact",
            ArtifactsState::Multiple => "存在多个 Artifact",
            ArtifactsState::Expired => "Artifact 已过期",
            ArtifactsState::Deleted => "Artifact 已删除",
            ArtifactsState::NotDownloadable => "Artifact 无法下载（需鉴权）",
            ArtifactsState::NotGenerated => "Artifact 尚未生成",
            ArtifactsState::ApiFailed => "Artifact 查询失败",
            ArtifactsState::RateLimited => "API 限流",
            ArtifactsState::Unknown => "未知状态",
        }
    }
    pub fn from_code(s: &str) -> ArtifactsState {
        match s {
            "not_checked" => ArtifactsState::NotChecked,
            "querying" => ArtifactsState::Querying,
            "available" => ArtifactsState::Available,
            "multiple" => ArtifactsState::Multiple,
            "expired" => ArtifactsState::Expired,
            "deleted" => ArtifactsState::Deleted,
            "not_downloadable" => ArtifactsState::NotDownloadable,
            "not_generated" => ArtifactsState::NotGenerated,
            "api_failed" => ArtifactsState::ApiFailed,
            "rate_limited" => ArtifactsState::RateLimited,
            _ => ArtifactsState::Unknown,
        }
    }
    /// 是否存在可用（未过期）产物
    pub fn has_usable(self) -> bool {
        matches!(self, ArtifactsState::Available | ArtifactsState::Multiple | ArtifactsState::NotDownloadable)
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactInfo {
    pub id: u64,
    pub name: String,
    pub size: u64,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: String,
    pub expired: bool,
    pub archived: bool,
    pub run_id: u64,
    pub workflow_id: u64,
    pub head_sha: String,
    pub download_url: String,
    /// 匿名访问通常拿不到 archive_download_url 的实际下载权限
    pub anonymous_downloadable: bool,
    pub file: FirmwareFile,
}

impl ArtifactInfo {
    pub fn from_json(v: &Value, has_token: bool) -> ArtifactInfo {
        let f = from_artifact(v);
        let run = v.get("workflow_run");
        let (run_id, workflow_id, head_sha) = match run {
            Some(r) => (gn(r, "id"), gn(r, "workflow_id"), gs(r, "head_sha")),
            None => (0, 0, String::new()),
        };
        let download_url = f.download_url.clone();
        ArtifactInfo {
            id: gn(v, "id"),
            name: f.name.clone(),
            size: f.size,
            created_at: f.created_at.clone(),
            updated_at: f.updated_at.clone(),
            expires_at: f.expires_at.clone(),
            expired: f.expired,
            archived: f.expired,
            run_id,
            workflow_id,
            head_sha,
            // 有 Token 且 URL 非空才认为可下载；否则仅能列出元数据
            anonymous_downloadable: has_token && !download_url.is_empty(),
            download_url,
            file: f,
        }
    }

    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("id".into(), Value::num(self.id)),
            ("name".into(), Value::str(&self.name)),
            ("size".into(), Value::num(self.size)),
            ("size_human".into(), Value::str(&crate::github::human_size(self.size))),
            ("created_at".into(), Value::str(&self.created_at)),
            ("created_at_local".into(), Value::str(&crate::github::utc_to_local(&self.created_at))),
            ("updated_at".into(), Value::str(&self.updated_at)),
            ("expires_at".into(), Value::str(&self.expires_at)),
            ("expires_at_local".into(), Value::str(&crate::github::utc_to_local(&self.expires_at))),
            ("expired".into(), Value::Bool(self.expired)),
            ("archived".into(), Value::Bool(self.archived)),
            ("run_id".into(), Value::num(self.run_id)),
            ("workflow_id".into(), Value::num(self.workflow_id)),
            ("head_sha".into(), Value::str(&self.head_sha)),
            ("download_url".into(), Value::str(&self.download_url)),
            ("downloadable".into(), Value::Bool(self.anonymous_downloadable)),
            ("is_firmware".into(), Value::Bool(false)),
            ("note".into(), Value::str("Artifact 需下载后安全解析内部文件，不能凭名称判定固件")),
            ("file".into(), self.file.to_json()),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactsModel {
    pub repo: String,
    pub state: ArtifactsState,
    pub label: String,
    pub detail: String,
    pub artifacts: Vec<ArtifactInfo>,
    pub total_count: u64,
    pub meta: ApiMeta,
    pub ttl: u64,
}

impl ArtifactsModel {
    pub fn not_checked(repo: &str) -> ArtifactsModel {
        ArtifactsModel {
            repo: repo.to_string(),
            state: ArtifactsState::NotChecked,
            label: ArtifactsState::NotChecked.label().into(),
            detail: "尚未查询 Actions Artifacts".into(),
            artifacts: Vec::new(),
            total_count: 0,
            meta: ApiMeta::not_checked(),
            ttl: cache::ttl_from_uci("ttl_artifacts", TTL_ARTIFACTS),
        }
    }

    pub fn load(repo: &str) -> ArtifactsModel {
        let ttl = cache::ttl_from_uci("ttl_artifacts", TTL_ARTIFACTS);
        let env = match cache::read(ARTIFACTS_FILE) {
            Some(e) => e,
            None => return ArtifactsModel::not_checked(repo),
        };
        let meta = ApiMeta::from_envelope(&env);
        let p = &env.payload;
        let artifacts = p
            .get("artifacts")
            .and_then(|x| x.as_arr())
            .unwrap_or(&[])
            .iter()
            .map(|v| ArtifactInfo {
                id: gn(v, "id"),
                name: gs(v, "name"),
                size: gn(v, "size"),
                created_at: gs(v, "created_at"),
                updated_at: gs(v, "updated_at"),
                expires_at: gs(v, "expires_at"),
                expired: gb(v, "expired"),
                archived: gb(v, "archived"),
                run_id: gn(v, "run_id"),
                workflow_id: gn(v, "workflow_id"),
                head_sha: gs(v, "head_sha"),
                download_url: gs(v, "download_url"),
                anonymous_downloadable: gb(v, "downloadable"),
                file: v
                    .get("file")
                    .cloned()
                    .map(|fv| FirmwareFile::new(&gs(&fv, "name"), crate::firmware::FileSource::Artifact))
                    .unwrap_or_else(|| FirmwareFile::new("", crate::firmware::FileSource::Artifact)),
            })
            .collect();
        ArtifactsModel {
            repo: gs(p, "repo").if_empty(repo),
            state: ArtifactsState::from_code(&gs(p, "state")),
            label: gs(p, "state_label"),
            detail: gs(p, "detail"),
            artifacts,
            total_count: gn(p, "total_count"),
            meta,
            ttl,
        }
    }

    pub fn to_json(&self) -> Value {
        let usable = self.artifacts.iter().filter(|a| !a.expired).count();
        let data = vec![
            ("repo".into(), Value::str(&self.repo)),
            ("detail".into(), Value::str(&self.detail)),
            (
                "artifacts".into(),
                Value::Arr(self.artifacts.iter().map(|a| a.to_json()).collect()),
            ),
            ("artifact_count".into(), Value::num(self.artifacts.len() as u64)),
            ("total_count".into(), Value::num(self.total_count)),
            ("usable_count".into(), Value::num(usable as u64)),
            ("expired_count".into(), Value::num(self.artifacts.iter().filter(|a| a.expired).count() as u64)),
            (
                "downloadable_count".into(),
                Value::num(self.artifacts.iter().filter(|a| a.anonymous_downloadable).count() as u64),
            ),
            ("note".into(), Value::str("Artifact 是临时构建产物，会过期；内部文件需下载后解析")),
        ];
        model::frame(MODEL, self.state.code(), &self.label, data, &self.meta, self.ttl)
    }

    pub async fn refresh(hub: &Hub, repo: &RepoSpec, force: bool) -> ArtifactsModel {
        let ttl = cache::ttl_from_uci("ttl_artifacts", TTL_ARTIFACTS);
        if !force {
            if let Some(env) = cache::usable(ARTIFACTS_FILE, ttl) {
                let mut m = ArtifactsModel::load(&repo.slug);
                let mut meta = ApiMeta::from_envelope(&env);
                meta.status = "cached".into();
                meta.from_cache = true;
                meta.stale = false;
                m.meta = meta;
                return m;
            }
            if cache::in_backoff(ARTIFACTS_FILE, cache::FAILURE_BACKOFF_SECS) {
                let mut m = ArtifactsModel::load(&repo.slug);
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
                let _ = cache::write(ARTIFACTS_FILE, &Envelope::fresh(payload, "ok"));
                m
            }
            Err(e) => {
                crate::logline!("[artifacts] 查询失败: {} ({})", e.code(), e.user_message());
                let status = if e.is_rate_limited() { "rate_limited" } else { "error" };
                let mut env = cache::preserve_on_failure(ARTIFACTS_FILE, status, &e.user_message());
                let meta = ApiMeta::from_error(&e, &env);
                env.payload = model::replace_api(env.payload.clone(), &meta);
                let _ = cache::write(ARTIFACTS_FILE, &env);
                let mut m = ArtifactsModel::load(&repo.slug);
                m.meta = meta;
                m
            }
        }
    }

    async fn query(hub: &Hub, repo: &RepoSpec) -> Result<ArtifactsModel, ApiError> {
        let url = api_url(repo, "actions/artifacts?per_page=50");
        let (v, rate) = hub.get_json(&url).await?;
        let total_count = v.get("total_count").and_then(|x| x.as_u64()).unwrap_or(0);
        let has_token = hub.token().is_some();
        let artifacts: Vec<ArtifactInfo> = v
            .get("artifacts")
            .and_then(|x| x.as_arr())
            .unwrap_or(&[])
            .iter()
            .map(|a| ArtifactInfo::from_json(a, has_token))
            .collect();

        let live: Vec<&ArtifactInfo> = artifacts.iter().filter(|a| !a.expired).collect();
        let state = if artifacts.is_empty() {
            ArtifactsState::NotGenerated
        } else if live.is_empty() {
            ArtifactsState::Expired
        } else if !has_token && live.iter().all(|a| !a.anonymous_downloadable) {
            ArtifactsState::NotDownloadable
        } else if live.len() > 1 {
            ArtifactsState::Multiple
        } else {
            ArtifactsState::Available
        };

        let detail = match state {
            ArtifactsState::NotGenerated =>
                "当前仓库没有可用的 Actions Artifact（可能已过期或被清理）；这不代表没有 Release".to_string(),
            ArtifactsState::Expired =>
                format!("发现 {} 个 Artifact，但全部已过期", artifacts.len()),
            ArtifactsState::NotDownloadable => format!(
                "发现 {} 个未过期 Artifact，但匿名访问无法下载（GitHub 要求鉴权）；可配置 Token 后重试",
                live.len()
            ),
            ArtifactsState::Multiple =>
                format!("发现 {} 个未过期 Artifact，需按名称/关联构建任务分别解析", live.len()),
            ArtifactsState::Available =>
                format!("发现 1 个未过期 Artifact（{}），需下载后解析内部文件", live[0].name),
            _ => "Artifact 状态未知".into(),
        };

        Ok(ArtifactsModel {
            repo: repo.slug.clone(),
            state,
            label: state.label().into(),
            detail,
            artifacts,
            total_count,
            meta: ApiMeta::ok(rate),
            ttl: cache::ttl_from_uci("ttl_artifacts", TTL_ARTIFACTS),
        })
    }

    /// 供兼容性模型读取：未过期的产物文件描述。
    pub fn live_files(&self) -> Vec<FirmwareFile> {
        self.artifacts
            .iter()
            .filter(|a| !a.expired)
            .map(|a| a.file.clone())
            .collect()
    }
}

fn gs(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn gn(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}
fn gb(v: &Value, k: &str) -> bool {
    matches!(v.get(k), Some(Value::Bool(true)))
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

    fn artifact(name: &str, expired: bool) -> Value {
        Value::Obj(vec![
            ("id".into(), Value::num(77)),
            ("name".into(), Value::str(name)),
            ("size_in_bytes".into(), Value::num(50_000_000)),
            ("expired".into(), Value::Bool(expired)),
            ("created_at".into(), Value::str("2026-11-13T01:00:00Z")),
            ("expires_at".into(), Value::str("2026-12-13T01:00:00Z")),
            ("archive_download_url".into(), Value::str("https://api.github.com/repos/o/r/actions/artifacts/77/zip")),
            ("workflow_run".into(), Value::Obj(vec![
                ("id".into(), Value::num(1234)),
                ("workflow_id".into(), Value::num(5678)),
                ("head_sha".into(), Value::str("abcdef")),
            ])),
        ])
    }

    #[test]
    fn artifact_is_not_automatically_a_firmware() {
        let a = ArtifactInfo::from_json(&artifact("OpenWrt-Firmware-Bundle", false), false);
        assert_eq!(a.name, "OpenWrt-Firmware-Bundle");
        assert!(!a.expired);
        let j = a.to_json();
        assert_eq!(j.get("is_firmware"), Some(&Value::Bool(false)));
        assert!(j.get("note").and_then(|x| x.as_str()).unwrap().contains("不能凭名称"));
    }

    #[test]
    fn anonymous_access_is_not_downloadable_but_not_a_failure() {
        let a = ArtifactInfo::from_json(&artifact("bundle", false), false);
        assert!(!a.anonymous_downloadable);
        let b = ArtifactInfo::from_json(&artifact("bundle", false), true);
        assert!(b.anonymous_downloadable, "配置 Token 后可下载");
        assert!(ArtifactsState::NotDownloadable.has_usable(), "无法下载≠无产物≠查询失败");
    }

    #[test]
    fn expired_artifact_is_tracked_with_its_own_state() {
        let a = ArtifactInfo::from_json(&artifact("old", true), false);
        assert!(a.expired);
        assert_eq!(ArtifactsState::Expired.code(), "expired");
        assert!(!ArtifactsState::Expired.has_usable());
    }

    #[test]
    fn expires_at_is_independently_displayed() {
        let a = ArtifactInfo::from_json(&artifact("bundle", false), false);
        let j = a.to_json();
        assert!(j.get("expires_at").is_some());
        assert!(j.get("expires_at_local").is_some());
        assert!(j.get("expired").is_some());
    }

    #[test]
    fn model_is_independent_and_does_not_claim_compatibility() {
        let j = ArtifactsModel::not_checked("o/r").to_json();
        assert_eq!(j.get("model").and_then(|x| x.as_str()), Some("artifacts"));
        assert!(j.get("compatibility").is_none());
        assert!(j.get("actions_state").is_none());
        assert!(j.get("releases").is_none());
    }

    #[test]
    fn live_files_excludes_expired() {
        let mut m = ArtifactsModel::not_checked("o/r");
        m.artifacts.push(ArtifactInfo::from_json(&artifact("live", false), false));
        m.artifacts.push(ArtifactInfo::from_json(&artifact("old", true), false));
        assert_eq!(m.live_files().len(), 1);
    }
}
