// 状态模型 2/4：GitHub Releases 发布状态（**完全独立**）。
//
// 独立持有：状态枚举、数据结构、缓存文件 RELEASES_FILE、TTL_RELEASES、
// refresh()、错误处理。不读写其他模型缓存。
//
// 只描述「正式发布信息」；Release 不存在绝不直接判定仓库没有固件。

use crate::cache::{self, Envelope, RELEASES_FILE, TTL_RELEASES};
use crate::firmware::{from_release_asset, FirmwareFile, FileKind};
use crate::ghapi::{api_url, ApiError, Hub, RepoSpec};
use crate::json::Value;
use crate::model::{self, ApiMeta};

pub const MODEL: &str = "releases";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleasesState {
    NotChecked, Querying, HasStable, OnlyPrerelease, HasDraft, None,
    ApiFailed, RateLimited, Unknown,
}

impl ReleasesState {
    pub fn code(self) -> &'static str {
        match self {
            ReleasesState::NotChecked => "not_checked",
            ReleasesState::Querying => "querying",
            ReleasesState::HasStable => "has_stable",
            ReleasesState::OnlyPrerelease => "only_prerelease",
            ReleasesState::HasDraft => "has_draft",
            ReleasesState::None => "none",
            ReleasesState::ApiFailed => "api_failed",
            ReleasesState::RateLimited => "rate_limited",
            ReleasesState::Unknown => "unknown",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ReleasesState::NotChecked => "未检测",
            ReleasesState::Querying => "查询中",
            ReleasesState::HasStable => "存在正式 Release",
            ReleasesState::OnlyPrerelease => "仅存在预发布 Release",
            ReleasesState::HasDraft => "存在 Draft",
            ReleasesState::None => "仓库没有 Release",
            ReleasesState::ApiFailed => "Release 查询失败",
            ReleasesState::RateLimited => "API 限流",
            ReleasesState::Unknown => "未知状态",
        }
    }
    pub fn from_code(s: &str) -> ReleasesState {
        match s {
            "not_checked" => ReleasesState::NotChecked,
            "querying" => ReleasesState::Querying,
            "has_stable" => ReleasesState::HasStable,
            "only_prerelease" => ReleasesState::OnlyPrerelease,
            "has_draft" => ReleasesState::HasDraft,
            "none" => ReleasesState::None,
            "api_failed" => ReleasesState::ApiFailed,
            "rate_limited" => ReleasesState::RateLimited,
            _ => ReleasesState::Unknown,
        }
    }
    /// 是否存在任何可用的发布信息（供诊断层只读判断）
    pub fn has_any(self) -> bool {
        matches!(self, ReleasesState::HasStable | ReleasesState::OnlyPrerelease | ReleasesState::HasDraft)
    }
}

#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub id: u64,
    pub tag_name: String,
    pub name: String,
    pub target_commitish: String,
    pub draft: bool,
    pub prerelease: bool,
    pub body: String,
    pub created_at: String,
    pub published_at: String,
    pub updated_at: String,
    pub html_url: String,
    pub assets: Vec<FirmwareFile>,
}

impl ReleaseInfo {
    pub fn from_json(v: &Value) -> ReleaseInfo {
        let tag = gs(v, "tag_name");
        let html = gs(v, "html_url");
        let assets = v
            .get("assets")
            .and_then(|x| x.as_arr())
            .unwrap_or(&[])
            .iter()
            .map(|a| from_release_asset(a, &tag, &html))
            .collect();
        ReleaseInfo {
            id: gn(v, "id"),
            tag_name: tag,
            name: gs(v, "name"),
            target_commitish: gs(v, "target_commitish"),
            draft: gb(v, "draft"),
            prerelease: gb(v, "prerelease"),
            body: gs(v, "body"),
            created_at: gs(v, "created_at"),
            published_at: gs(v, "published_at"),
            updated_at: gs(v, "updated_at"),
            html_url: html,
            assets,
        }
    }

    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("id".into(), Value::num(self.id)),
            ("tag_name".into(), Value::str(&self.tag_name)),
            ("name".into(), Value::str(&self.name)),
            ("target_commitish".into(), Value::str(&self.target_commitish)),
            ("draft".into(), Value::Bool(self.draft)),
            ("prerelease".into(), Value::Bool(self.prerelease)),
            ("body".into(), Value::str(&self.body.chars().take(4000).collect::<String>())),
            ("created_at".into(), Value::str(&self.created_at)),
            ("created_at_local".into(), Value::str(&crate::github::utc_to_local(&self.created_at))),
            ("published_at".into(), Value::str(&self.published_at)),
            ("published_at_local".into(), Value::str(&crate::github::utc_to_local(&self.published_at))),
            ("updated_at".into(), Value::str(&self.updated_at)),
            ("html_url".into(), Value::str(&self.html_url)),
            ("assets".into(), Value::Arr(self.assets.iter().map(|a| a.to_json()).collect())),
            ("asset_count".into(), Value::num(self.assets.len() as u64)),
            (
                "firmware_candidates".into(),
                Value::num(self.assets.iter().filter(|a| a.kind.is_flashable_candidate()).count() as u64),
            ),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct ReleasesModel {
    pub repo: String,
    pub state: ReleasesState,
    pub label: String,
    pub detail: String,
    pub releases: Vec<ReleaseInfo>,
    pub meta: ApiMeta,
    pub ttl: u64,
}

impl ReleasesModel {
    pub fn not_checked(repo: &str) -> ReleasesModel {
        ReleasesModel {
            repo: repo.to_string(),
            state: ReleasesState::NotChecked,
            label: ReleasesState::NotChecked.label().into(),
            detail: "尚未查询 GitHub Releases".into(),
            releases: Vec::new(),
            meta: ApiMeta::not_checked(),
            ttl: cache::ttl_from_uci("ttl_releases", TTL_RELEASES),
        }
    }

    pub fn load(repo: &str) -> ReleasesModel {
        let ttl = cache::ttl_from_uci("ttl_releases", TTL_RELEASES);
        let env = match cache::read(RELEASES_FILE) {
            Some(e) => e,
            None => return ReleasesModel::not_checked(repo),
        };
        let meta = ApiMeta::from_envelope(&env);
        let p = &env.payload;
        let state = ReleasesState::from_code(&gs(p, "state"));
        let releases = p
            .get("releases")
            .and_then(|x| x.as_arr())
            .unwrap_or(&[])
            .iter()
            .map(|v| {
                // 从缓存还原时资产已是结构化对象，重建简化版即可
                let tag = gs(v, "tag_name");
                let html = gs(v, "html_url");
                ReleaseInfo {
                    id: gn(v, "id"),
                    tag_name: tag.clone(),
                    name: gs(v, "name"),
                    target_commitish: gs(v, "target_commitish"),
                    draft: gb(v, "draft"),
                    prerelease: gb(v, "prerelease"),
                    body: gs(v, "body"),
                    created_at: gs(v, "created_at"),
                    published_at: gs(v, "published_at"),
                    updated_at: gs(v, "updated_at"),
                    html_url: html.clone(),
                    assets: v
                        .get("assets")
                        .and_then(|x| x.as_arr())
                        .unwrap_or(&[])
                        .iter()
                        .map(|a| from_release_asset(a, &tag, &html))
                        .collect(),
                }
            })
            .collect();
        ReleasesModel {
            repo: gs(p, "repo").if_empty(repo),
            state,
            label: gs(p, "state_label"),
            detail: gs(p, "detail"),
            releases,
            meta,
            ttl,
        }
    }

    pub fn to_json(&self) -> Value {
        let data = vec![
            ("repo".into(), Value::str(&self.repo)),
            ("detail".into(), Value::str(&self.detail)),
            (
                "releases".into(),
                Value::Arr(self.releases.iter().map(|r| r.to_json()).collect()),
            ),
            ("release_count".into(), Value::num(self.releases.len() as u64)),
            (
                "latest".into(),
                match self.releases.first() {
                    Some(r) => r.to_json(),
                    None => Value::Null,
                },
            ),
            (
                "total_assets".into(),
                Value::num(self.releases.iter().map(|r| r.assets.len()).sum::<usize>() as u64),
            ),
            (
                "total_firmware_candidates".into(),
                Value::num(
                    self.releases
                        .iter()
                        .flat_map(|r| r.assets.iter())
                        .filter(|a| a.kind.is_flashable_candidate())
                        .count() as u64,
                ),
            ),
        ];
        model::frame(MODEL, self.state.code(), &self.label, data, &self.meta, self.ttl)
    }

    pub async fn refresh(hub: &Hub, repo: &RepoSpec, force: bool) -> ReleasesModel {
        let ttl = cache::ttl_from_uci("ttl_releases", TTL_RELEASES);
        if !force {
            if let Some(env) = cache::usable(RELEASES_FILE, ttl) {
                let mut m = ReleasesModel::load(&repo.slug);
                let mut meta = ApiMeta::from_envelope(&env);
                meta.status = "cached".into();
                meta.from_cache = true;
                meta.stale = false;
                m.meta = meta;
                return m;
            }
            if cache::in_backoff(RELEASES_FILE, cache::FAILURE_BACKOFF_SECS) {
                let mut m = ReleasesModel::load(&repo.slug);
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
                let _ = cache::write(RELEASES_FILE, &Envelope::fresh(payload, "ok"));
                m
            }
            Err(e) => {
                crate::logline!("[releases] 查询失败: {} ({})", e.code(), e.user_message());
                let status = if e.is_rate_limited() { "rate_limited" } else { "error" };
                let mut env = cache::preserve_on_failure(RELEASES_FILE, status, &e.user_message());
                let meta = ApiMeta::from_error(&e, &env);
                env.payload = model::replace_api(env.payload.clone(), &meta);
                let _ = cache::write(RELEASES_FILE, &env);
                let mut m = ReleasesModel::load(&repo.slug);
                m.meta = meta;
                m
            }
        }
    }

    async fn query(hub: &Hub, repo: &RepoSpec) -> Result<ReleasesModel, ApiError> {
        let url = api_url(repo, "releases?per_page=30");
        let (v, rate) = hub.get_json(&url).await?;
        let arr = v.as_arr().ok_or_else(|| {
            ApiError::BadContent("Releases 接口返回的不是数组".into())
        })?;
        let releases: Vec<ReleaseInfo> = arr.iter().map(ReleaseInfo::from_json).collect();

        let state = if releases.is_empty() {
            ReleasesState::None
        } else if releases.iter().any(|r| r.draft) && !releases.iter().any(|r| !r.draft) {
            ReleasesState::HasDraft
        } else if releases.iter().any(|r| !r.draft && !r.prerelease) {
            ReleasesState::HasStable
        } else {
            ReleasesState::OnlyPrerelease
        };

        let total_assets: usize = releases.iter().map(|r| r.assets.len()).sum();
        let fw: usize = releases
            .iter()
            .flat_map(|r| r.assets.iter())
            .filter(|a| a.kind.is_flashable_candidate())
            .count();
        let detail = match state {
            ReleasesState::None =>
                "该仓库目前没有公开 Release；这不代表 Actions 没有构建产物".to_string(),
            ReleasesState::HasDraft =>
                format!("发现 {} 个 Draft（匿名查询通常看不到 Draft，需 Token 权限）", releases.len()),
            ReleasesState::OnlyPrerelease =>
                format!("发现 {} 个预发布 Release，暂无正式发布", releases.len()),
            ReleasesState::HasStable => format!(
                "发现 {} 个 Release（共 {} 个资产，其中 {} 个识别为固件候选）",
                releases.len(), total_assets, fw
            ),
            _ => "Release 状态未知".into(),
        };

        Ok(ReleasesModel {
            repo: repo.slug.clone(),
            state,
            label: state.label().into(),
            detail,
            releases,
            meta: ApiMeta::ok(rate),
            ttl: cache::ttl_from_uci("ttl_releases", TTL_RELEASES),
        })
    }

    /// 供兼容性模型读取：所有 Release 资产（不做筛选）。
    pub fn all_assets(&self) -> Vec<FirmwareFile> {
        self.releases.iter().flat_map(|r| r.assets.iter().cloned()).collect()
    }

    /// 供诊断层只读判断：是否有校验文件（sha256sums 等）。
    pub fn has_checksum_asset(&self) -> bool {
        self.releases
            .iter()
            .flat_map(|r| r.assets.iter())
            .any(|a| a.kind == FileKind::Checksum)
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

    fn release(tag: &str, draft: bool, pre: bool, assets: &[&str]) -> Value {
        Value::Obj(vec![
            ("id".into(), Value::num(1)),
            ("tag_name".into(), Value::str(tag)),
            ("name".into(), Value::str(tag)),
            ("draft".into(), Value::Bool(draft)),
            ("prerelease".into(), Value::Bool(pre)),
            ("html_url".into(), Value::str("https://github.com/o/r/releases/tag/t")),
            (
                "assets".into(),
                Value::Arr(
                    assets
                        .iter()
                        .map(|n| Value::Obj(vec![
                            ("name".into(), Value::str(n)),
                            ("size".into(), Value::num(76_000_000)),
                            ("browser_download_url".into(), Value::str("https://example/x.bin")),
                        ]))
                        .collect(),
                ),
            ),
        ])
    }

    #[test]
    fn state_machine_distinguishes_stable_prerelease_draft_and_none() {
        let stable = ReleaseInfo::from_json(&release("v1", false, false, &[]));
        let pre = ReleaseInfo::from_json(&release("v2-beta", false, true, &[]));
        let draft = ReleaseInfo::from_json(&release("v3", true, false, &[]));
        assert!(!stable.draft && !stable.prerelease);
        assert!(pre.prerelease);
        assert!(draft.draft);
        assert!(ReleasesState::HasStable.has_any());
        assert!(ReleasesState::OnlyPrerelease.has_any());
        assert!(!ReleasesState::None.has_any());
    }

    #[test]
    fn no_release_does_not_claim_no_firmware() {
        let m = ReleasesModel::not_checked("o/r");
        let j = m.to_json();
        assert!(j.get("no_firmware").is_none(), "Release 模型不得断言固件缺失");
        assert!(j.get("compatibility").is_none());
        assert!(j.get("artifacts").is_none());
    }

    #[test]
    fn assets_are_classified_not_all_treated_as_firmware() {
        let r = ReleaseInfo::from_json(&release(
            "v1",
            false,
            false,
            &["fw-sysupgrade.bin", "sha256sums", "openwrt-sdk.tar.xz", "build.log"],
        ));
        assert_eq!(r.assets.len(), 4);
        assert_eq!(r.assets.iter().filter(|a| a.kind.is_flashable_candidate()).count(), 1);
        assert!(r.assets.iter().any(|a| a.kind == FileKind::Checksum));
        assert!(r.assets.iter().any(|a| a.kind == FileKind::Sdk));
    }

    #[test]
    fn all_assets_exposed_for_compat_model() {
        let mut m = ReleasesModel::not_checked("o/r");
        m.releases.push(ReleaseInfo::from_json(&release("v1", false, false, &["a.bin", "b.img.gz"])));
        assert_eq!(m.all_assets().len(), 2);
        assert!(!m.has_checksum_asset());
        m.releases.push(ReleaseInfo::from_json(&release("v2", false, false, &["sha256sums"])));
        assert!(m.has_checksum_asset());
    }

    #[test]
    fn to_json_has_independent_model_tag() {
        let j = ReleasesModel::not_checked("o/r").to_json();
        assert_eq!(j.get("model").and_then(|x| x.as_str()), Some("releases"));
        assert_eq!(j.get("state").and_then(|x| x.as_str()), Some("not_checked"));
    }
}
