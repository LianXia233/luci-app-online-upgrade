// 状态模型 4/4：固件兼容性状态（**完全独立**）。
//
// 独立持有：状态枚举、数据结构、缓存文件 COMPAT_FILE、TTL_COMPAT、
// refresh()、错误处理。不读写其他模型缓存。
//
// 只回答「某个固件文件是否适合本机」，绝不表示远程构建情况。
// 分层判定：
//   L1 文件名/目录初筛
//   L2 固件内部元数据识别（压缩包清单 / digest / 声明字段）
//   L3 设备型号 / compatible / target / subtarget / 架构 / ABI / 版本 / 类型比对
//   L4 下载完成后的完整性验证（大小 / SHA256）
//   L5 系统原生安全验证（fwtool + sysupgrade -T）
//
// 「无法确认」绝不降级为「明确不兼容」；明确不兼容必须给出具体原因。

use crate::cache::{self, Envelope, COMPAT_FILE, TTL_COMPAT};
use crate::device::DeviceInfo;
use crate::firmware::{match_device, FirmwareFile, FileSource, MatchLevel, MatchResult};
use crate::json::Value;
use crate::model::{self, ApiMeta};

pub const MODEL: &str = "compat";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatState {
    NotChecked, Checking, Compatible, Risk, Unconfirmed, Incompatible,
    Corrupted, UnsupportedFormat, ChecksumFailed, LocalVerifyFailed,
}

impl CompatState {
    pub fn code(self) -> &'static str {
        match self {
            CompatState::NotChecked => "not_checked",
            CompatState::Checking => "checking",
            CompatState::Compatible => "compatible",
            CompatState::Risk => "risk",
            CompatState::Unconfirmed => "unconfirmed",
            CompatState::Incompatible => "incompatible",
            CompatState::Corrupted => "corrupted",
            CompatState::UnsupportedFormat => "unsupported_format",
            CompatState::ChecksumFailed => "checksum_failed",
            CompatState::LocalVerifyFailed => "local_verify_failed",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            CompatState::NotChecked => "未检测",
            CompatState::Checking => "检测中",
            CompatState::Compatible => "确认兼容",
            CompatState::Risk => "存在兼容性风险",
            CompatState::Unconfirmed => "无法确认兼容",
            CompatState::Incompatible => "明确不兼容",
            CompatState::Corrupted => "文件损坏",
            CompatState::UnsupportedFormat => "文件格式不支持",
            CompatState::ChecksumFailed => "文件校验失败",
            CompatState::LocalVerifyFailed => "本机验证失败",
        }
    }
    pub fn from_code(s: &str) -> CompatState {
        match s {
            "not_checked" => CompatState::NotChecked,
            "checking" => CompatState::Checking,
            "compatible" => CompatState::Compatible,
            "risk" => CompatState::Risk,
            "unconfirmed" => CompatState::Unconfirmed,
            "incompatible" => CompatState::Incompatible,
            "corrupted" => CompatState::Corrupted,
            "unsupported_format" => CompatState::UnsupportedFormat,
            "checksum_failed" => CompatState::ChecksumFailed,
            "local_verify_failed" => CompatState::LocalVerifyFailed,
            _ => CompatState::Unconfirmed,
        }
    }
    /// 是否允许进入刷写流程（只有「确认兼容」才允许自动刷写）
    pub fn can_flash(self) -> bool {
        matches!(self, CompatState::Compatible)
    }
    pub fn is_blocking(self) -> bool {
        matches!(
            self,
            CompatState::Incompatible | CompatState::Corrupted
                | CompatState::UnsupportedFormat | CompatState::ChecksumFailed
                | CompatState::LocalVerifyFailed
        )
    }
}

/// 五层中每一层的独立结果（互不覆盖）。
#[derive(Debug, Clone)]
pub struct LayerResult {
    pub layer: u8,
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

impl LayerResult {
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("layer".into(), Value::num(self.layer as u64)),
            ("name".into(), Value::str(self.name)),
            ("passed".into(), Value::Bool(self.passed)),
            ("detail".into(), Value::str(&self.detail)),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct CompatCheck {
    pub file: FirmwareFile,
    pub state: CompatState,
    pub label: String,
    pub reason: String,
    pub match_level: MatchLevel,
    pub match_score: i32,
    pub basis: Vec<String>,
    pub conflicts: Vec<String>,
    pub layers: Vec<LayerResult>,
    pub can_upgrade: bool,
    pub requires_confirm: bool,
    pub checked_at: u64,
}

impl CompatCheck {
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("file".into(), self.file.to_json()),
            ("name".into(), Value::str(&self.file.name)),
            ("state".into(), Value::str(self.state.code())),
            ("state_label".into(), Value::str(&self.label)),
            ("reason".into(), Value::str(&self.reason)),
            ("match_level".into(), Value::str(self.match_level.code())),
            ("match_level_label".into(), Value::str(self.match_level.label())),
            ("match_score".into(), Value::num(self.match_score.max(0) as u64)),
            ("basis".into(), Value::Arr(self.basis.iter().map(|s| Value::str(s)).collect())),
            ("conflicts".into(), Value::Arr(self.conflicts.iter().map(|s| Value::str(s)).collect())),
            ("layers".into(), Value::Arr(self.layers.iter().map(|l| l.to_json()).collect())),
            ("can_upgrade".into(), Value::Bool(self.can_upgrade)),
            ("requires_confirm".into(), Value::Bool(self.requires_confirm)),
            ("checked_at".into(), Value::num(self.checked_at)),
            ("checked_at_local".into(), Value::str(&crate::cache::human_ts(self.checked_at))),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct CompatModel {
    pub device: DeviceInfo,
    pub state: CompatState,
    pub label: String,
    pub detail: String,
    pub checks: Vec<CompatCheck>,
    pub selected: Option<usize>,
    /// 候选总数。从缓存恢复时 checks 为空，用该计数保留「识别到几个候选」的事实。
    pub check_count: usize,
    /// 最佳候选的匹配等级（无候选时为空）。
    pub best_level: Option<MatchLevel>,
    /// 最佳候选是否命中了用户显式配置的 firmware_pattern。
    pub rule_hit: bool,
    /// 最佳候选文件名（用于向用户点名具体固件）。
    pub best_name: Option<String>,
    pub meta: ApiMeta,
    pub ttl: u64,
}

impl CompatModel {
    pub fn not_checked(device: DeviceInfo) -> CompatModel {
        CompatModel {
            device,
            state: CompatState::NotChecked,
            label: CompatState::NotChecked.label().into(),
            detail: "尚未对候选固件执行兼容性检查".into(),
            checks: Vec::new(),
            selected: None,
            check_count: 0,
            best_level: None,
            rule_hit: false,
            best_name: None,
            meta: ApiMeta::not_checked(),
            ttl: cache::ttl_from_uci("ttl_compat", TTL_COMPAT),
        }
    }

    /// 从自己的缓存文件还原（不做网络请求）。
    pub fn load(device: DeviceInfo) -> CompatModel {
        let ttl = cache::ttl_from_uci("ttl_compat", TTL_COMPAT);
        let env = match cache::read(COMPAT_FILE) {
            Some(e) => e,
            None => return CompatModel::not_checked(device),
        };
        let meta = ApiMeta::from_envelope(&env);
        let p = &env.payload;
        let state = CompatState::from_code(&gs(p, "state"));
        // 缓存里保留了候选摘要（含匹配等级与依据）。据此恢复候选计数与最佳等级，
        // 否则 states/诊断层会把「已识别到候选」误判成「没有识别到任何固件」。
        let cached_checks: &[Value] = match p.get("checks").and_then(|c| c.as_arr()) {
            Some(a) => a,
            None => &[],
        };
        let check_count = p
            .get("check_count")
            .and_then(|c| c.as_u64())
            .map(|n| n as usize)
            .unwrap_or(cached_checks.len())
            .max(cached_checks.len());
        let best_level = cached_checks
            .first()
            .and_then(|c| c.get("match_level"))
            .and_then(|v| v.as_str())
            .and_then(level_from_code);
        let best_name = cached_checks
            .first()
            .and_then(|c| c.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let rule_hit = cached_checks.iter().any(|c| {
            c.get("basis")
                .and_then(|b| b.as_arr())
                .map(|bs| {
                    bs.iter().any(|s| {
                        s.as_str().map(|t| t.contains("配置规则")).unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        });
        CompatModel {
            device,
            state,
            label: gs(p, "state_label"),
            detail: gs(p, "detail"),
            checks: Vec::new(), // 详细列表在 refresh 时重建，缓存只保留汇总
            selected: p.get("selected").and_then(|x| x.as_u64()).map(|x| x as usize),
            check_count,
            best_level,
            best_name,
            rule_hit,
            meta,
            ttl,
        }
    }

    pub fn to_json(&self) -> Value {
        let data = vec![
            ("detail".into(), Value::str(&self.detail)),
            ("device".into(), self.device.to_json()),
            (
                "checks".into(),
                Value::Arr(self.checks.iter().map(|c| c.to_json()).collect()),
            ),
            (
                "check_count".into(),
                Value::num(self.check_count.max(self.checks.len()) as u64),
            ),
            (
                "best_match_level".into(),
                match self.best_level {
                    Some(l) => Value::str(l.code()),
                    None => Value::Null,
                },
            ),
            ("rule_hit".into(), Value::Bool(self.rule_hit)),
            (
                "best_match_name".into(),
                match self.best_name {
                    Some(ref n) => Value::str(n),
                    None => Value::Null,
                },
            ),
            (
                "selected".into(),
                match self.selected {
                    Some(i) => Value::num(i as u64),
                    None => Value::Null,
                },
            ),
            (
                "selected_file".into(),
                match self.selected.and_then(|i| self.checks.get(i)) {
                    Some(c) => c.to_json(),
                    None => Value::Null,
                },
            ),
            ("can_upgrade".into(), Value::Bool(self.state.can_flash())),
            ("supports_sysupgrade_test".into(), Value::Bool(crate::device::supports_sysupgrade_test())),
        ];
        model::frame(MODEL, self.state.code(), &self.label, data, &self.meta, self.ttl)
    }

    /// 独立刷新：输入是「已发现的候选文件」，由调用方从 Releases/Artifacts
    /// 模型**只读**取得；本模型不发起任何 GitHub 请求，也不写其他模型缓存。
    pub fn refresh(device: DeviceInfo, files: Vec<FirmwareFile>, rule: Option<&str>, force: bool) -> CompatModel {
        let ttl = cache::ttl_from_uci("ttl_compat", TTL_COMPAT);
        if !force {
            if let Some(env) = cache::usable(COMPAT_FILE, ttl) {
                let mut m = CompatModel::load(device);
                let mut meta = ApiMeta::from_envelope(&env);
                meta.status = "cached".into();
                meta.from_cache = true;
                meta.stale = false;
                m.meta = meta;
                return m;
            }
        }

        let mut checks: Vec<CompatCheck> = files
            .iter()
            .filter(|f| f.kind.is_flashable_candidate() || !f.inner_files.is_empty())
            .map(|f| evaluate(device.clone(), f, rule))
            .collect();

        // 按置信度排序：高置信度优先；同为无法确认的不排到最前误导用户
        checks.sort_by(|a, b| rank(&b.match_level).cmp(&rank(&a.match_level))
            .then(b.match_score.cmp(&a.match_score)));

        let selected = checks
            .iter()
            .position(|c| c.state == CompatState::Compatible)
            .or_else(|| checks.iter().position(|c| c.state == CompatState::Risk));

        let (state, detail) = if checks.is_empty() {
            (
                CompatState::Unconfirmed,
                "未发现可识别的固件候选文件；不等于设备不兼容，可能是产物尚未生成或文件无法识别".into(),
            )
        } else if let Some(i) = selected {
            if checks[i].state == CompatState::Compatible {
                (CompatState::Compatible, format!("已找到确认兼容的固件 [{}]，仍需下载后完成完整性与本机验证", checks[i].file.name))
            } else {
                (CompatState::Risk, format!("找到 [{}] 但存在兼容性风险，需人工确认", checks[i].file.name))
            }
        } else if checks.iter().any(|c| c.state.is_blocking()) {
            let reasons: Vec<String> = checks
                .iter()
                .filter(|c| c.state.is_blocking())
                .map(|c| format!("{}: {}", c.file.name, c.reason))
                .collect();
            (CompatState::Incompatible, format!("存在明确不兼容/损坏的候选：{}", reasons.join("; ")))
        } else {
            (
                CompatState::Unconfirmed,
                "已识别到固件候选，但兼容性尚无法确认，暂不允许自动刷写".into(),
            )
        };

        let m = CompatModel {
            device,
            state,
            label: state.label().into(),
            detail,
            check_count: checks.len(),
            best_level: checks.first().map(|c| c.match_level),
            best_name: checks.first().map(|c| c.file.name.clone()),
            rule_hit: checks
                .first()
                .map(|c| c.basis.iter().any(|s| s.contains("配置规则")))
                .unwrap_or(false),
            checks,
            selected,
            meta: ApiMeta::ok(Default::default()),
            ttl,
        };
        let payload = m.to_json();
        let _ = cache::write(COMPAT_FILE, &Envelope::fresh(payload, "ok"));
        m
    }

    /// 是否需要阻止自动刷写（供 upgrade 流程只读调用）。
    pub fn blocking_reason(&self) -> Option<String> {
        if self.state.can_flash() {
            None
        } else {
            Some(match self.state {
                CompatState::Unconfirmed => "当前设备的兼容性尚无法确认，因此暂不允许自动刷写".into(),
                CompatState::Incompatible => format!("固件明确不兼容：{}", self.detail),
                _ => format!("兼容性检查未通过（{}）：{}", self.state.label(), self.detail),
            })
        }
    }
}

fn level_from_code(s: &str) -> Option<MatchLevel> {
    match s {
        "high" => Some(MatchLevel::High),
        "partial" => Some(MatchLevel::Partial),
        "need_verify" => Some(MatchLevel::NeedVerify),
        "mismatch" => Some(MatchLevel::Mismatch),
        "unrecognized" => Some(MatchLevel::Unrecognized),
        _ => None,
    }
}

fn rank(m: &MatchLevel) -> u8 {
    match m {
        MatchLevel::High => 5,
        MatchLevel::Partial => 4,
        MatchLevel::NeedVerify => 3,
        MatchLevel::Unrecognized => 2,
        MatchLevel::Mismatch => 1,
    }
}

/// 单个文件的五层判定。每层独立记录结果，不互相覆盖。
pub fn evaluate(device: DeviceInfo, f: &FirmwareFile, rule: Option<&str>) -> CompatCheck {
    let mut layers = Vec::new();
    let mut conflicts = Vec::new();
    let mut basis = Vec::new();
    let now = cache::now();

    // ---- L1 文件名/目录初筛 ----
    let l1_ok = f.kind.is_flashable_candidate() || !f.inner_files.is_empty();
    layers.push(LayerResult {
        layer: 1,
        name: "文件名与格式初筛",
        passed: l1_ok,
        detail: format!(
            "文件 [{}]，格式 {}，类型 {}；{}",
            f.name,
            f.format.label(),
            f.kind.label(),
            if l1_ok { "进入候选" } else { "非候选文件" }
        ),
    });
    basis.push(format!("来源：{}；格式：{}；识别类型：{}", f.source.label(), f.format.label(), f.kind.label()));

    if !l1_ok {
        return CompatCheck {
            file: f.clone(),
            state: CompatState::Incompatible,
            label: CompatState::Incompatible.label().into(),
            reason: format!("该文件是 {}，不是可刷写的镜像", f.kind.label()),
            match_level: MatchLevel::Mismatch,
            match_score: 0,
            basis,
            conflicts: vec![format!("文件类型 {} 不可刷写", f.kind.label())],
            layers,
            can_upgrade: false,
            requires_confirm: false,
            checked_at: now,
        };
    }

    // ---- L2 内部元数据识别 ----
    let inner_flashable = f.inner_files.iter().any(|e| e.kind.is_flashable_candidate());
    let l2_ok = !f.inner_files.is_empty() || !f.sha256.is_empty() || f.format.may_be_image();
    layers.push(LayerResult {
        layer: 2,
        name: "固件内部元数据识别",
        passed: l2_ok,
        detail: if !f.inner_files.is_empty() {
            format!(
                "压缩包内解析到 {} 个条目，其中 {} 个是可刷写镜像候选",
                f.inner_files.len(),
                f.inner_files.iter().filter(|e| e.kind.is_flashable_candidate()).count()
            )
        } else if !f.sha256.is_empty() {
            "GitHub 资产提供可信 SHA-256 digest，可作为完整性依据".into()
        } else {
            "按文件名/格式识别，暂无内部元数据（下载后可补充验证）".into()
        },
    });
    if inner_flashable {
        basis.push("压缩包内部发现可刷写镜像条目".into());
    }

    // ---- L3 设备多维度比对 ----
    let m: MatchResult = match_device(&device, f, rule);
    basis.extend(m.basis.clone());
    conflicts.extend(m.conflicts.clone());
    if !m.allow_next {
        basis.push("多维度比对未通过初筛，不进入下载/校验流程".into());
    }
    let l3_ok = m.level != MatchLevel::Mismatch;
    layers.push(LayerResult {
        layer: 3,
        name: "设备型号/架构/平台比对",
        passed: l3_ok,
        detail: format!("{}（匹配分 {}）", m.level.label(), m.score),
    });

    // L3 明确不匹配 → 明确不兼容（有具体原因）
    if !l3_ok {
        return CompatCheck {
            file: f.clone(),
            state: CompatState::Incompatible,
            label: CompatState::Incompatible.label().into(),
            reason: if conflicts.is_empty() { "多维度比对未通过".into() } else { conflicts.join("；") },
            match_level: m.level,
            match_score: m.score,
            basis,
            conflicts,
            layers,
            can_upgrade: false,
            requires_confirm: false,
            checked_at: now,
        };
    }

    // ---- L4 下载后完整性（此处仅登记待办，实际由 downloader/upgrade 执行）----
    layers.push(LayerResult {
        layer: 4,
        name: "下载后完整性验证",
        passed: false,
        detail: if !f.sha256.is_empty() {
            "已具备可信 SHA-256，待下载完成后比对文件大小与哈希".into()
        } else {
            "未提供可信哈希，下载后仅能记录实际大小与 SHA-256，不能宣称已校验".into()
        },
    });

    // ---- L5 系统原生安全验证（sysupgrade -T / fwtool）----
    layers.push(LayerResult {
        layer: 5,
        name: "本机升级兼容性验证",
        passed: false,
        detail: if crate::device::supports_sysupgrade_test() {
            "本机支持 sysupgrade -T，下载完成后将执行只读镜像测试".into()
        } else {
            "本机未检测到 sysupgrade，无法执行系统级预检".into()
        },
    });

    // 远程阶段只能到 L3；L4/L5 必须通过下载后验证才能置为通过。
    // 因此：高置信度也只能标为「存在兼容性风险·需下载后验证」，
    // 绝不能在未下载时就宣称「确认兼容」。
    let (state, reason, can_upgrade, requires_confirm) = match m.level {
        MatchLevel::High => (
            CompatState::Risk,
            format!(
                "文件名与设备信息高置信度匹配（{}），但尚未下载，需完成完整性与 sysupgrade -T 本机验证后才会允许刷写",
                m.level.label()
            ),
            false,
            true,
        ),
        MatchLevel::Partial => (
            CompatState::Risk,
            "仅部分信息匹配，无法确认该固件是否为本机构建；确认无误后再强制升级".into(),
            false,
            true,
        ),
        MatchLevel::NeedVerify => (
            CompatState::Unconfirmed,
            "已发现候选文件，但缺少足够的设备匹配依据，需要下载后进一步解析内部元数据".into(),
            false,
            true,
        ),
        MatchLevel::Unrecognized => (
            CompatState::Unconfirmed,
            "无法从文件名或压缩包清单识别设备适配信息，不能判定为不兼容".into(),
            false,
            true,
        ),
        MatchLevel::Mismatch => unreachable!("上面已提前返回"),
    };

    CompatCheck {
        file: f.clone(),
        state,
        label: state.label().into(),
        reason,
        match_level: m.level,
        match_score: m.score,
        basis,
        conflicts,
        layers,
        can_upgrade,
        requires_confirm,
        checked_at: now,
    }
}

/// 供下载/升级流程调用：把 L4/L5 的实际结果合并进缓存中的兼容性状态。
/// 只有 L1-L3 全部通过且 L4+L5 通过，才允许置为 Compatible。
pub fn finalize_after_download(
    device: DeviceInfo,
    f: &FirmwareFile,
    rule: Option<&str>,
    l4_passed: bool,
    l4_detail: &str,
    l5_passed: bool,
    l5_detail: &str,
) -> CompatCheck {
    let mut c = evaluate(device, f, rule);
    if let Some(l) = c.layers.iter_mut().find(|l| l.layer == 4) {
        l.passed = l4_passed;
        l.detail = l4_detail.to_string();
    }
    if let Some(l) = c.layers.iter_mut().find(|l| l.layer == 5) {
        l.passed = l5_passed;
        l.detail = l5_detail.to_string();
    }

    let arch_ok = !c.conflicts.iter().any(|s| s.contains("架构"));
    c.state = if !l4_passed {
        CompatState::ChecksumFailed
    } else if !l5_passed {
        CompatState::LocalVerifyFailed
    } else if !arch_ok {
        CompatState::Incompatible
    } else {
        CompatState::Compatible
    };
    c.label = c.state.label().into();
    c.can_upgrade = c.state.can_flash();
    c.requires_confirm = false;
    c.reason = if c.state == CompatState::Compatible {
        format!("已通过 L1-L5 全部验证（{}），允许进入刷写流程", l5_detail)
    } else {
        format!(
            "本机验证未通过：{}；{}",
            l4_detail,
            l5_detail
        )
    };
    c
}

/// 由下载/升级流程提供的 L4/L5 真实结果构建一份兼容性结论（纯函数，便于测试）。
/// `l4_basis` 表示是否存在期望哈希；缺失校验依据属于「无法确认」，
/// 绝不能降级显示成「校验失败」或「明确不兼容」。
fn download_check(
    device: DeviceInfo,
    name: &str,
    size: u64,
    sha256: &str,
    rule: Option<&str>,
    l4_basis: bool,
    l4_passed: bool,
    l4_detail: &str,
    l5_passed: bool,
    l5_detail: &str,
) -> CompatCheck {
    let mut f = FirmwareFile::new(name, FileSource::ReleaseAsset);
    f.size = size;
    f.sha256 = sha256.to_string();
    let (cpu_arch, abi) = crate::firmware::infer_arch_hints(name);
    f.cpu_arch = cpu_arch;
    f.abi = abi;

    let mut c = finalize_after_download(
        device,
        &f,
        rule,
        l4_basis && l4_passed,
        l4_detail,
        l5_passed,
        l5_detail,
    );

    if !l4_basis && l5_passed && c.state == CompatState::ChecksumFailed {
        c.state = CompatState::Unconfirmed;
        c.label = CompatState::Unconfirmed.label().into();
        c.reason = format!(
            "{l4_detail}；{l5_detail}。缺少校验依据，兼容性尚无法确认，不会自动宣称可刷写"
        );
        c.can_upgrade = false;
        c.requires_confirm = true;
    }
    c
}

/// 供实际下载/升级流程调用：把 L4（下载后完整性）与 L5（fwtool /
/// `sysupgrade -T`）的真实结果写进 compat 模型**自己的缓存**，供 WebUI 与
/// 诊断层只读消费。
///
/// 边界：不发起 GitHub 请求、不写其他三套模型缓存、不参与刷写决策
/// （决策仍完全由既有 upgrade 链路与 `force` 语义负责）。
pub fn record_download_result(
    name: &str,
    size: u64,
    sha256: &str,
    rule: Option<&str>,
    l4_basis: bool,
    l4_passed: bool,
    l4_detail: &str,
    l5_passed: bool,
    l5_detail: &str,
) {
    let device = crate::device::collect();
    let check = download_check(
        device.clone(),
        name,
        size,
        sha256,
        rule,
        l4_basis,
        l4_passed,
        l4_detail,
        l5_passed,
        l5_detail,
    );

    let mut m = CompatModel::not_checked(device);
    m.state = check.state;
    m.label = check.label.clone();
    m.detail = check.reason.clone();
    m.checks = vec![check];
    m.selected = Some(0);
    m.meta = ApiMeta::ok(Default::default());
    let payload = m.to_json();
    if let Err(e) = cache::write(COMPAT_FILE, &Envelope::fresh(payload, "ok")) {
        crate::logline!("[compat] 下载后兼容性状态写入缓存失败（不影响刷写）: {e}");
    }
}

fn gs(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev() -> DeviceInfo {
        let mut d = DeviceInfo::empty();
        d.model = "Hiveton H5000M".into();
        d.board_name = "hiveton,h5000m".into();
        d.compatible = vec!["hiveton,h5000m".into()];
        d.target = "mediatek".into();
        d.subtarget = "filogic".into();
        d.cpu_arch = "aarch64".into();
        d.pkg_arch = "aarch64_cortex-a53".into();
        d.abi = "cortex-a53".into();
        d
    }

    fn fw(name: &str) -> FirmwareFile {
        let mut f = FirmwareFile::new(name, FileSource::ReleaseAsset);
        let (a, abi) = crate::firmware::infer_arch_hints(name);
        f.cpu_arch = a;
        f.abi = abi;
        f.size = 76_000_000;
        f.sha256 = "a".repeat(64);
        f
    }

    #[test]
    fn unrecognized_is_never_reported_as_incompatible() {
        let d = dev();
        let c = evaluate(d.clone(), &fw("mystery-bundle.bin"), None);
        assert_eq!(c.state, CompatState::Unconfirmed, "无法识别≠不兼容");
        assert!(!c.can_upgrade);
        let m = CompatModel::refresh(d, vec![fw("mystery-bundle.bin")], None, true);
        assert_eq!(m.state, CompatState::Unconfirmed);
        assert!(m.blocking_reason().unwrap().contains("尚无法确认"));
    }

    #[test]
    fn high_confidence_still_requires_download_verification() {
        let c = evaluate(dev(), &fw("H5000M-aarch64-26.11.13-sysupgrade.bin"), None);
        assert_eq!(c.match_level, MatchLevel::High);
        assert_eq!(c.state, CompatState::Risk, "未下载不得宣称确认兼容");
        assert!(!c.can_upgrade);
        assert_eq!(c.layers.len(), 5);
        assert!(!c.layers[3].passed, "L4 未下载必须为未通过");
        assert!(!c.layers[4].passed, "L5 未下载必须为未通过");
    }

    #[test]
    fn explicit_mismatch_has_concrete_reason() {
        let c = evaluate(dev(), &fw("H5000M-mipsel-24kc-sysupgrade.bin"), None);
        assert_eq!(c.state, CompatState::Incompatible);
        assert!(c.reason.contains("架构冲突"), "明确不兼容必须给具体原因: {}", c.reason);
        assert!(!c.conflicts.is_empty());
    }

    #[test]
    fn checksum_file_is_filtered_out_not_device_mismatch() {
        let m = CompatModel::refresh(dev(), vec![fw("sha256sums")], None, true);
        assert!(m.checks.is_empty());
        assert_eq!(m.state, CompatState::Unconfirmed, "识别失败不得判设备不兼容");
        assert!(!m.state.is_blocking(), "识别失败不得阻断为明确不兼容");
        assert!(m.detail.contains("不等于设备不兼容"));
    }

    #[test]
    fn finalize_after_download_can_promote_to_compatible() {
        let f = fw("H5000M-aarch64-26.11.13-sysupgrade.bin");
        let c = finalize_after_download(dev(), &f, None, true, "SHA256 一致", true, "sysupgrade -T 通过");
        assert_eq!(c.state, CompatState::Compatible);
        assert!(c.can_upgrade);
        assert!(c.layers.iter().all(|l| l.passed));
    }

    #[test]
    fn failed_local_verification_blocks_flashing() {
        let f = fw("H5000M-aarch64-26.11.13-sysupgrade.bin");
        let c = finalize_after_download(dev(), &f, None, true, "SHA256 一致", false, "sysupgrade -T 拒绝");
        assert_eq!(c.state, CompatState::LocalVerifyFailed);
        assert!(!c.can_upgrade);
    }

    #[test]
    fn failed_checksum_blocks_flashing() {
        let f = fw("H5000M-aarch64-26.11.13-sysupgrade.bin");
        let c = finalize_after_download(dev(), &f, None, false, "SHA256 不匹配", true, "sysupgrade -T 通过");
        assert_eq!(c.state, CompatState::ChecksumFailed);
        assert!(!c.can_upgrade);
    }

    #[test]
    fn compat_model_does_not_claim_remote_build_state() {
        let j = CompatModel::not_checked(dev()).to_json();
        assert_eq!(j.get("model").and_then(|x| x.as_str()), Some("compat"));
        assert!(j.get("actions").is_none());
        assert!(j.get("releases").is_none());
        assert!(j.get("artifacts").is_none());
    }

    #[test]
    fn unsupported_format_is_own_state() {
        assert_eq!(CompatState::UnsupportedFormat.code(), "unsupported_format");
        assert!(CompatState::UnsupportedFormat.is_blocking());
    }

    #[test]
    fn download_check_missing_basis_is_unconfirmed_not_failure() {
        let hash = "a".repeat(64);
        let ok = download_check(
            dev(), "sysupgrade.bin", 1024, &hash, None,
            true, true, "SHA256 一致", true, "sysupgrade -T 通过",
        );
        assert_eq!(ok.state, CompatState::Compatible);
        assert!(ok.can_upgrade);

        let no_basis = download_check(
            dev(), "sysupgrade.bin", 1024, &hash, None,
            false, false, "未提供校验依据，仅记录本地 SHA256",
            true, "sysupgrade -T 通过",
        );
        assert_eq!(no_basis.state, CompatState::Unconfirmed, "缺少校验依据≠校验失败");
        assert!(!no_basis.can_upgrade);
        assert!(!no_basis.reason.contains("校验失败"));

        let l5_fail = download_check(
            dev(), "sysupgrade.bin", 1024, &hash, None,
            true, true, "SHA256 一致", false, "sysupgrade -T 拒绝",
        );
        assert_eq!(l5_fail.state, CompatState::LocalVerifyFailed);
        assert!(!l5_fail.can_upgrade);
    }
}
