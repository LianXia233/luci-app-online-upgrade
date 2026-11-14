// 统一诊断层（阶段 6）。
//
// 职责：只读 Actions / Releases / Artifacts / Compat / Network 五份状态，
// 关联分析后生成面向用户的中文结论。**绝不写回或修改任何模型**。
//
// 状态隔离铁律（逐条落实）：
//   Actions 成功 ≠ 已有固件          Actions 失败 ≠ Release 不可用
//   Release 不存在 ≠ Actions 无产物  Artifact 存在 ≠ 内部有固件
//   文件存在 ≠ 本机兼容              API 失败 ≠ 远程构建失败
//   无法确认兼容 ≠ 明确不兼容        一个模型异常 ≠ 覆盖其他模型结果

use crate::actions::{ActionsModel, ActionsState};
use crate::artifacts::{ArtifactsModel, ArtifactsState};
use crate::compatstate::{CompatModel, CompatState};
use crate::ghapi::NetClass;
use crate::json::Value;
use crate::network::ProbeResult;
use crate::releases::{ReleasesModel, ReleasesState};

pub const MODEL: &str = "diagnostics";
pub const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info, Success, Warning, Error, Unknown,
}

impl Severity {
    pub fn code(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Success => "success",
            Severity::Warning => "warning",
            Severity::Error => "error",
            Severity::Unknown => "unknown",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "提示",
            Severity::Success => "正常",
            Severity::Warning => "需注意",
            Severity::Error => "异常",
            Severity::Unknown => "未知",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiagSection {
    pub key: &'static str,
    pub title: &'static str,
    pub text: String,
    pub severity: Severity,
    pub evidence: Vec<String>,
}

impl DiagSection {
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("key".into(), Value::str(self.key)),
            ("title".into(), Value::str(self.title)),
            ("text".into(), Value::str(&self.text)),
            ("severity".into(), Value::str(self.severity.code())),
            ("severity_label".into(), Value::str(self.severity.label())),
            ("evidence".into(), Value::Arr(self.evidence.iter().map(|s| Value::str(s)).collect())),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct Diagnosis {
    pub summary: String,
    pub severity: Severity,
    pub can_upgrade: bool,
    pub blocking_reasons: Vec<String>,
    pub next_actions: Vec<String>,
    pub sections: Vec<DiagSection>,
    /// 诊断只读快照的时间
    pub generated_at: u64,
    /// 参与汇总的各模型是否来自缓存/已过期
    pub model_freshness: Vec<(String, bool, bool)>,
}

impl Diagnosis {
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("schema_version".into(), Value::num(SCHEMA_VERSION)),
            ("model".into(), Value::str(MODEL)),
            ("summary".into(), Value::str(&self.summary)),
            ("severity".into(), Value::str(self.severity.code())),
            ("severity_label".into(), Value::str(self.severity.label())),
            ("can_upgrade".into(), Value::Bool(self.can_upgrade)),
            (
                "blocking_reasons".into(),
                Value::Arr(self.blocking_reasons.iter().map(|s| Value::str(s)).collect()),
            ),
            (
                "next_actions".into(),
                Value::Arr(self.next_actions.iter().map(|s| Value::str(s)).collect()),
            ),
            (
                "sections".into(),
                Value::Arr(self.sections.iter().map(|s| s.to_json()).collect()),
            ),
            ("generated_at".into(), Value::num(self.generated_at)),
            ("generated_at_local".into(), Value::str(&crate::cache::human_ts(self.generated_at))),
            (
                "model_freshness".into(),
                Value::Arr(
                    self.model_freshness
                        .iter()
                        .map(|(name, stale, cached)| Value::Obj(vec![
                            ("model".into(), Value::str(name)),
                            ("stale".into(), Value::Bool(*stale)),
                            ("from_cache".into(), Value::Bool(*cached)),
                        ]))
                        .collect(),
                ),
            ),
        ])
    }
}

fn sec(key: &'static str, title: &'static str, text: String, severity: Severity, evidence: Vec<String>) -> DiagSection {
    DiagSection { key, title, text, severity, evidence }
}

/// 规则矩阵主入口。参数全部是**不可变引用**，编译期保证诊断层无法修改模型。
pub fn diagnose(
    actions: &ActionsModel,
    releases: &ReleasesModel,
    artifacts: &ArtifactsModel,
    compat: &CompatModel,
    network: Option<&ProbeResult>,
) -> Diagnosis {
    let mut sections = Vec::new();
    let mut blocking = Vec::new();
    let mut next = Vec::new();

    // ---- 0. 数据新鲜度（缓存不得伪装成实时） ----
    let freshness = vec![
        ("actions".to_string(), actions.meta.stale, actions.meta.from_cache),
        ("releases".to_string(), releases.meta.stale, releases.meta.from_cache),
        ("artifacts".to_string(), artifacts.meta.stale, artifacts.meta.from_cache),
        ("compat".to_string(), compat.meta.stale, compat.meta.from_cache),
    ];
    let any_stale = freshness.iter().any(|(_, s, _)| *s);
    let any_api_error = [&actions.meta, &releases.meta, &artifacts.meta]
        .iter()
        .any(|m| m.status == "error" || m.status == "rate_limited");

    // ---- 1. 网络 / API 状况（独立章节，不改写任何远程状态） ----
    if let Some(n) = network {
        let net_severity = match n.class {
            NetClass::Ok => Severity::Success,
            NetClass::RateLimited | NetClass::Http429 => Severity::Warning,
            NetClass::Unknown => Severity::Unknown,
            _ => Severity::Error,
        };
        sections.push(sec(
            "network",
            "网络与 API 状况",
            format!("{}（{}）", n.label, n.detail),
            net_severity,
            vec![
                format!("探测目标：{}", n.target),
                format!("耗时 {} ms", n.latency_ms),
                if n.from_cache { "结果来自缓存探测".into() } else { "本次实时探测".into() },
            ],
        ));
        if n.class == NetClass::RateLimited || n.class == NetClass::Http429 {
            next.push("GitHub API 受到限流，请稍后重试，或在配置中填入 Token 提高额度".into());
        }
    } else {
        sections.push(sec(
            "network",
            "网络与 API 状况",
            "尚未执行网络连通性检测；无法判断故障发生在哪个环节".into(),
            Severity::Unknown,
            vec![],
        ));
    }

    // ---- 2. 当前远程构建情况 ----
    let a_text;
    let a_sev;
    if actions.meta.status == "error" || actions.meta.status == "rate_limited" {
        a_text = format!(
            "远程构建状态暂时无法确认：{}；下方展示的是上次成功查询保留的状态（{}）",
            actions.meta.error,
            actions.state.label()
        );
        a_sev = Severity::Unknown;
        next.push("稍后点击「刷新状态」重新查询 GitHub Actions；当前不会把 API 故障当成编译失败".into());
    } else {
        a_text = actions.detail.clone();
        a_sev = match actions.state {
            ActionsState::Success => Severity::Success,
            ActionsState::Building | ActionsState::Queued | ActionsState::Requested
            | ActionsState::Waiting | ActionsState::Pending => Severity::Info,
            ActionsState::Failure | ActionsState::TimedOut | ActionsState::ActionRequired => Severity::Warning,
            ActionsState::Cancelled | ActionsState::Skipped | ActionsState::Neutral => Severity::Info,
            _ => Severity::Unknown,
        };
    }
    sections.push(sec(
        "actions",
        "远程构建状态（Actions）",
        a_text,
        a_sev,
        vec![
            format!("插件状态：{}", actions.state.label()),
            format!("数据状态：{}{}", actions.meta.status, if actions.meta.stale { "（缓存/已过期）" } else { "" }),
            format!("关联运行记录 {} 条，Workflow {} 个", actions.runs.len(), actions.workflows.len()),
            actions.runs.first().map(|r| format!("最近一次：{} / {} / {}", r.workflow_name, r.status, r.conclusion)).unwrap_or_else(|| "无运行记录".into()),
        ],
    ));

    // ---- 3. 正式发布情况 ----
    let (r_text, r_sev) = if releases.meta.status == "error" || releases.meta.status == "rate_limited" {
        (
            format!("正式发布信息暂时无法确认：{}；保留上次状态（{}）", releases.meta.error, releases.state.label()),
            Severity::Unknown,
        )
    } else {
        (
            releases.detail.clone(),
            if releases.state.has_any() { Severity::Success } else if releases.state == ReleasesState::None { Severity::Info } else { Severity::Unknown },
        )
    };
    sections.push(sec(
        "releases",
        "正式发布状态（Releases）",
        r_text,
        r_sev,
        vec![
            format!("插件状态：{}", releases.state.label()),
            format!("Release {} 个", releases.releases.len()),
            format!("资产合计 {} 个", releases.releases.iter().map(|r| r.assets.len()).sum::<usize>()),
        ],
    ));

    // ---- 4. 临时构建产物情况 ----
    let (t_text, t_sev) = if artifacts.meta.status == "error" || artifacts.meta.status == "rate_limited" {
        (
            format!("临时构建产物暂时无法确认：{}；保留上次状态（{}）", artifacts.meta.error, artifacts.state.label()),
            Severity::Unknown,
        )
    } else {
        let sev = match artifacts.state {
            ArtifactsState::Available | ArtifactsState::Multiple => Severity::Success,
            ArtifactsState::NotDownloadable => Severity::Warning,
            ArtifactsState::Expired | ArtifactsState::Deleted | ArtifactsState::NotGenerated => Severity::Info,
            _ => Severity::Unknown,
        };
        (artifacts.detail.clone(), sev)
    };
    sections.push(sec(
        "artifacts",
        "临时构建产物（Artifacts）",
        t_text,
        t_sev,
        vec![
            format!("插件状态：{}", artifacts.state.label()),
            format!("未过期 {} 个 / 已过期 {} 个",
                artifacts.artifacts.iter().filter(|a| !a.expired).count(),
                artifacts.artifacts.iter().filter(|a| a.expired).count()),
            "Artifact 会过期，且内部文件需下载后解析，不等于可刷写固件".into(),
        ],
    ));

    // ---- 5. 固件文件识别 ----
    let fw_candidates = compat.checks.len();
    let fw_text = if fw_candidates == 0 {
        "暂未从 Release Assets / Actions Artifacts 中识别出可刷写的固件文件；这不代表设备不兼容".to_string()
    } else {
        format!(
            "识别到 {} 个固件候选，最佳匹配等级「{}」；{}",
            fw_candidates,
            compat.checks.first().map(|c| c.match_level.label()).unwrap_or("无法识别"),
            compat.detail
        )
    };
    let mut fw_details: Vec<String> = compat
        .checks
        .iter()
        .take(5)
        .map(|c| format!("{}：{}（{}）", c.file.name, c.state.label(), c.match_level.label()))
        .collect();
    fw_details.push(if releases.has_checksum_asset() {
        "Release 包含校验文件（sha256sums 等），可用于下载后的完整性验证".into()
    } else {
        "Release 未发现校验文件；下载后仅记录本地 SHA256，不会宣称已校验".into()
    });
    sections.push(sec(
        "firmware",
        "固件文件识别情况",
        fw_text,
        if fw_candidates > 0 { Severity::Info } else { Severity::Warning },
        fw_details,
    ));

    // ---- 6. 本机兼容性 ----
    let c_text = if compat.state == CompatState::Compatible {
        "兼容性已通过全部验证，允许进入刷写流程".to_string()
    } else {
        compat.blocking_reason().unwrap_or_else(|| compat.detail.clone())
    };
    sections.push(sec(
        "compat",
        "本机兼容性检查",
        c_text,
        match compat.state {
            CompatState::Compatible => Severity::Success,
            CompatState::Incompatible | CompatState::Corrupted | CompatState::ChecksumFailed
            | CompatState::LocalVerifyFailed | CompatState::UnsupportedFormat => Severity::Error,
            CompatState::Risk => Severity::Warning,
            _ => Severity::Unknown,
        },
        compat.checks.iter().take(3).flat_map(|c| c.basis.clone()).collect(),
    ));

    // ---- 7. 是否存在可用升级文件（规则矩阵核心） ----
    let (upgrade_text, upgrade_sev) = rule_matrix(actions, releases, artifacts, compat);
    sections.push(sec(
        "availability",
        "当前是否存在可用升级文件",
        upgrade_text.clone(),
        upgrade_sev,
        vec![
            format!("Actions={} / Releases={} / Artifacts={} / Compat={}",
                actions.state.label(), releases.state.label(), artifacts.state.label(), compat.state.label()),
        ],
    ));

    // ---- 8. 无法升级的具体原因 ----
    if !compat.state.can_flash() {
        blocking.push(compat.blocking_reason().unwrap_or_else(|| "兼容性未确认".into()));
    }
    if any_api_error {
        blocking.push("部分远程状态来自缓存（API 查询失败或限流），当前结论可能不是最新".into());
    }
    if any_stale {
        blocking.push("存在已过期的缓存数据，已明确标记，不按实时数据处理".into());
    }
    if actions.state == ActionsState::Failure && releases.state.has_any() {
        // 明确说明：构建失败不阻断历史 Release
        next.push("最新构建失败，但历史正式发布仍可能可用；可在固件列表中手动选择".into());
    }
    sections.push(sec(
        "blocking",
        "无法升级的原因",
        if blocking.is_empty() {
            "当前没有阻断升级的因素；最终仍需完成下载后的完整性与本机验证".into()
        } else {
            blocking.join("；")
        },
        if blocking.is_empty() { Severity::Success } else { Severity::Warning },
        blocking.clone(),
    ));

    // ---- 9. 建议下一步 ----
    if next.is_empty() {
        if compat.state == CompatState::Compatible {
            next.push("执行「下载并校验」，完成 SHA-256 与 sysupgrade -T 本机验证后再刷写".into());
        } else if fw_candidates > 0 {
            next.push("在「固件列表」中确认目标文件，或配置固件匹配规则后重新检测".into());
        } else {
            next.push("等待构建完成或检查仓库配置（仓库地址、分支、Workflow 筛选、固件匹配规则）".into());
        }
    }
    sections.push(sec(
        "next",
        "建议执行的下一步操作",
        next.join("；"),
        Severity::Info,
        next.clone(),
    ));

    // ---- 汇总：一句话结论 ----
    let summary = build_summary(actions, releases, artifacts, compat, network, &blocking);
    let severity = if blocking.iter().any(|s| s.contains("不兼容") || s.contains("损坏")) {
        Severity::Error
    } else if !blocking.is_empty() || any_api_error {
        Severity::Warning
    } else if compat.state == CompatState::Compatible {
        Severity::Success
    } else {
        Severity::Info
    };

    Diagnosis {
        summary,
        severity,
        can_upgrade: compat.state.can_flash() && !blocking.iter().any(|s| s.contains("不兼容")),
        blocking_reasons: blocking,
        next_actions: next,
        sections,
        generated_at: crate::cache::now(),
        model_freshness: freshness,
    }
}

/// 场景规则矩阵。逐条对应任务给出的典型场景。
fn rule_matrix(
    a: &ActionsModel,
    r: &ReleasesModel,
    t: &ArtifactsModel,
    c: &CompatModel,
) -> (String, Severity) {
    let a_pending = a.state.is_pending();
    let a_failed = matches!(a.state, ActionsState::Failure | ActionsState::TimedOut);
    let a_success = a.state == ActionsState::Success;
    let no_runs = a.state == ActionsState::Unknown && a.runs.is_empty();
    let rel_any = r.state.has_any();
    let art_any = t.state.has_usable();
    let fw_ok = c.state == CompatState::Compatible;
    let fw_unrecognized = c.state == CompatState::Unconfirmed;

    if !a.meta.status.starts_with("ok") && !a.meta.status.starts_with("cached")
        && r.meta.status != "ok" && t.meta.status != "ok"
    {
        return ("远程状态暂时无法确认（API 请求失败或限流），已保留各模型上次成功的数据，未把 API 故障当成构建失败".into(), Severity::Unknown);
    }

    let text = if a_pending && !rel_any && !art_any {
        "当前正在编译，暂未发现可用产物；建议等待构建完成后刷新状态".to_string()
    } else if a_success && !rel_any && art_any {
        "编译成功，发现临时构建产物（Artifact）；需下载并解析内部文件才能确认是否包含可用固件".to_string()
    } else if a_success && rel_any && !art_any {
        "编译成功，存在正式发布（Release）；请在资产中确认适配本机的固件".to_string()
    } else if a_success && rel_any && art_any {
        "存在正式发布及临时构建产物；两者相互独立，请分别核对，优先使用正式发布".to_string()
    } else if a_failed && rel_any && !art_any {
        "最新构建失败，但历史正式发布仍可能可用；构建失败不影响已有 Release".to_string()
    } else if no_runs && rel_any && !art_any {
        "未发现相关构建记录，但存在正式发布；可直接检查 Release 资产".to_string()
    } else if no_runs && !rel_any && art_any {
        "未发现相关构建记录，但发现临时构建产物；请解析 Artifact 内部文件".to_string()
    } else if a_success && !rel_any && art_any && fw_unrecognized {
        "构建成功，但暂未识别出可用固件；产物可能需要下载后解析，或需配置固件匹配规则".to_string()
    } else if !rel_any && !art_any && !a_pending {
        "暂未发现正式发布或临时构建产物；若仓库仍在编译请稍后刷新，否则请检查仓库配置".to_string()
    } else {
        format!(
            "Actions={}、Releases={}、Artifacts={}；是否可升级以本机兼容性结果（{}）为准",
            a.state.label(), r.state.label(), t.state.label(), c.state.label()
        )
    };

    let sev = if fw_ok {
        Severity::Success
    } else if a_pending || (!rel_any && !art_any) || c.state == CompatState::Unconfirmed {
        Severity::Warning
    } else if c.state.is_blocking() {
        Severity::Error
    } else {
        Severity::Info
    };
    (text, sev)
}

fn build_summary(
    a: &ActionsModel,
    r: &ReleasesModel,
    t: &ArtifactsModel,
    c: &CompatModel,
    n: Option<&ProbeResult>,
    blocking: &[String],
) -> String {
    if let Some(net) = n {
        if matches!(net.class, NetClass::Dns | NetClass::Tcp | NetClass::Tls | NetClass::Timeout | NetClass::OfficialDown)
        {
            return format!("网络异常（{}），远程状态无法确认；已保留各模型上次成功的数据", net.class.label());
        }
        if net.class == NetClass::RateLimited || net.class == NetClass::Http429 {
            return "GitHub API 请求受到限流，暂时无法获取最新构建状态，已保留上次成功查询的数据".into();
        }
    }
    if c.state == CompatState::Incompatible {
        return "已经找到固件文件，但与当前设备明确不兼容，已禁止自动刷写".into();
    }
    if c.state == CompatState::Unconfirmed && !c.checks.is_empty() {
        return "已经找到固件文件，但当前设备的兼容性尚无法确认，因此暂不允许自动刷写".into();
    }
    if c.state == CompatState::Compatible {
        return "已找到通过兼容性检查的固件，完成下载后的完整性与本机验证后即可升级".into();
    }
    if a.state.is_pending() && !r.state.has_any() && !t.state.has_usable() {
        return "GitHub Actions 正在编译，当前尚未生成可用固件".into();
    }
    if a.state == ActionsState::Success && c.checks.is_empty() {
        return "最近一次构建已经成功，但暂时没有识别到可用的固件文件".into();
    }
    if t.state.has_usable() && c.checks.is_empty() {
        return "发现构建产物，但压缩包内部尚未找到适合当前设备的升级镜像".into();
    }
    if !blocking.is_empty() {
        return format!("{}；{}", a.state.label(), blocking[0]);
    }
    format!(
        "远程构建 {}、正式发布 {}、临时产物 {}；当前{}",
        a.state.label(), r.state.label(), t.state.label(),
        if c.state.can_flash() { "存在可用升级文件" } else { "暂无可自动刷写的固件" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceInfo;
    use crate::model::ApiMeta;

    fn base() -> (ActionsModel, ReleasesModel, ArtifactsModel, CompatModel) {
        let mut a = ActionsModel::not_checked("o/r");
        a.meta = ApiMeta::ok(Default::default());
        let mut r = ReleasesModel::not_checked("o/r");
        r.meta = ApiMeta::ok(Default::default());
        let mut t = ArtifactsModel::not_checked("o/r");
        t.meta = ApiMeta::ok(Default::default());
        let c = CompatModel::not_checked(DeviceInfo::empty());
        (a, r, t, c)
    }

    #[test]
    fn building_with_no_outputs() {
        let (mut a, r, t, c) = base();
        a.state = ActionsState::Building;
        a.label = a.state.label().into();
        a.detail = "正在编译".into();
        let d = diagnose(&a, &r, &t, &c, None);
        assert!(d.summary.contains("正在编译"));
        assert!(!d.can_upgrade);
        let av = d.sections.iter().find(|s| s.key == "availability").unwrap();
        assert!(av.text.contains("暂未发现可用产物"));
    }

    #[test]
    fn build_success_with_release() {
        let (mut a, mut r, t, c) = base();
        a.state = ActionsState::Success;
        a.label = a.state.label().into();
        r.state = ReleasesState::HasStable;
        r.label = r.state.label().into();
        let d = diagnose(&a, &r, &t, &c, None);
        let av = d.sections.iter().find(|s| s.key == "availability").unwrap();
        assert!(av.text.contains("正式发布"));
    }

    #[test]
    fn build_failure_does_not_hide_existing_release() {
        let (mut a, mut r, t, c) = base();
        a.state = ActionsState::Failure;
        a.label = a.state.label().into();
        r.state = ReleasesState::HasStable;
        r.label = r.state.label().into();
        let d = diagnose(&a, &r, &t, &c, None);
        let av = d.sections.iter().find(|s| s.key == "availability").unwrap();
        assert!(av.text.contains("历史正式发布仍可能可用"), "{}", av.text);
        assert!(d.next_actions.iter().any(|s| s.contains("历史正式发布")));
    }

    #[test]
    fn no_release_does_not_hide_artifacts() {
        let (mut a, r, mut t, c) = base();
        a.state = ActionsState::Unknown;
        a.label = a.state.label().into();
        t.state = ArtifactsState::Available;
        t.label = t.state.label().into();
        let d = diagnose(&a, &r, &t, &c, None);
        let av = d.sections.iter().find(|s| s.key == "availability").unwrap();
        assert!(av.text.contains("临时构建产物"), "{}", av.text);
    }

    #[test]
    fn api_failure_is_not_reported_as_build_failure() {
        let (mut a, mut r, mut t, c) = base();
        a.meta.status = "error".into();
        a.meta.error = "HTTP 500".into();
        r.meta.status = "rate_limited".into();
        r.meta.error = "限流".into();
        t.meta.status = "error".into();
        let d = diagnose(&a, &r, &t, &c, None);
        assert!(!d.summary.contains("编译失败"), "{}", d.summary);
        let ac = d.sections.iter().find(|s| s.key == "actions").unwrap();
        assert!(ac.text.contains("暂时无法确认"));
        assert_eq!(a.state.label(), a.state.label(), "诊断不得修改模型状态");
    }

    #[test]
    fn rate_limited_keeps_last_success_data_explicitly_marked() {
        let (mut a, r, t, c) = base();
        a.state = ActionsState::Success;
        a.label = a.state.label().into();
        a.meta.status = "rate_limited".into();
        a.meta.stale = true;
        a.meta.from_cache = true;
        a.meta.error = "GitHub API 请求受到限流".into();
        let d = diagnose(&a, &r, &t, &c, None);
        assert_eq!(a.state, ActionsState::Success, "限流不得把成功状态改成失败");
        assert!(d.blocking_reasons.iter().any(|s| s.contains("缓存")));
        assert_eq!(d.model_freshness[0].1, true);
    }

    #[test]
    fn unconfirmed_compat_is_not_incompatible() {
        let (a, r, t, mut c) = base();
        c.state = CompatState::Unconfirmed;
        c.label = c.state.label().into();
        c.detail = "无法确认".into();
        let d = diagnose(&a, &r, &t, &c, None);
        assert!(!d.summary.contains("明确不兼容"), "{}", d.summary);
        assert!(d.blocking_reasons.iter().any(|s| s.contains("尚无法确认")));
    }

    #[test]
    fn explicit_incompatible_blocks_upgrade_with_reason() {
        let (a, r, t, mut c) = base();
        c.state = CompatState::Incompatible;
        c.label = c.state.label().into();
        c.detail = "架构冲突".into();
        let d = diagnose(&a, &r, &t, &c, None);
        assert!(!d.can_upgrade);
        assert!(d.summary.contains("明确不兼容"));
        assert!(!d.blocking_reasons.is_empty());
    }

    #[test]
    fn diagnosis_never_mutates_models() {
        let (mut a, mut r, mut t, mut c) = base();
        a.state = ActionsState::Failure;
        r.state = ReleasesState::HasStable;
        t.state = ArtifactsState::Available;
        c.state = CompatState::Unconfirmed;
        let before = (a.state, r.state, t.state, c.state);
        let _ = diagnose(&a, &r, &t, &c, None);
        assert_eq!((a.state, r.state, t.state, c.state), before, "诊断层只读，不得修改任何模型");
    }

    #[test]
    fn all_nine_sections_are_present() {
        let (a, r, t, c) = base();
        let d = diagnose(&a, &r, &t, &c, None);
        assert_eq!(d.sections.len(), 9);
        let keys: Vec<&str> = d.sections.iter().map(|s| s.key).collect();
        for k in ["network", "actions", "releases", "artifacts", "firmware", "compat", "availability", "blocking", "next"] {
            assert!(keys.contains(&k), "缺少章节 {k}");
        }
        assert_eq!(d.to_json().get("model").and_then(|x| x.as_str()), Some("diagnostics"));
    }

    #[test]
    fn network_failure_produces_clear_summary() {
        use crate::network::ProbeResult;
        let (a, r, t, c) = base();
        let n = ProbeResult {
            target: "https://api.github.com/rate_limit".into(),
            class: NetClass::Dns,
            label: NetClass::Dns.label().into(),
            detail: "DNS 解析失败".into(),
            http_status: 0,
            latency_ms: 0,
            retryable: false,
            from_cache: false,
            last_success_at: 0,
            stale: false,
            rate_remaining: None,
        };
        let d = diagnose(&a, &r, &t, &c, Some(&n));
        assert!(d.summary.contains("网络异常"));
        assert!(d.summary.contains("已保留"));
    }
}
