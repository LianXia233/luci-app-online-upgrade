// 本地固件版本检测。
//
// 自用构建的 SNAPSHOT 镜像没有可比较的发布号：`DISTRIB_RELEASE` 的值
// 就是 `SNAPSHOT`。若直接退化为“无法比较”，这类设备都会被当成
// 版本未知，从而堵死升级路径。因此这里按可信度从高到低依次
// 尝试多个来源，并记录*哪个*来源给出了该值，让界面展示
// 判定依据，而不是只给一个孤立结论。
//
// 优先级（高 → 低）：
//   1. 构建时写入的版本文件（CI 写入的标签）
//   2. `DISTRIB_VERSION`、`DISTRIB_RELEASE`、os-release 的 `VERSION_ID`
//   3. `DISTRIB_DESCRIPTION`、`DISTRIB_REVISION`、`BUILD_ID`、`/etc/openwrt_version`
//   4. `base-files` 软件包版本
//   5. 固件构建时间（弱依据：仅对 YY.MM.DD / YYYY.MM.DD 这类
//      日期型标签有意义；其他目标保持不可比）
//
// 这里不做任何猜测：所有来源都解析不出版本时，调用方会得到
// `Confidence::None`，并如实上报“无法比较”。

use crate::version::{civil_from_days, days_from_civil, pick, Version};
use std::cmp::Ordering;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Strong,
    Weak,
    None,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Strong => "strong",
            Confidence::Weak => "weak",
            Confidence::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LocalVersion {
    /// 解析出的版本号（当来源提供了点分数字时）。
    pub version: Option<Version>,
    /// 命中来源的原始文本（如 `SNAPSHOT`、`r0-23a1cd3` 等）。
    pub raw: String,
    /// 机器可读的来源标识。
    pub source: String,
    /// 人类可读的来源说明（显示在界面上）。
    pub source_text: String,
    pub confidence: Confidence,
    /// 构建日期（年、月、日），仅当来源为构建时间时设置。
    pub build_ymd: Option<(i64, u32, u32)>,
    /// 判定依据强度（0~100）：由来源等级、日期窗口与多源交叉验证共同决定。
    pub basis_score: u8,
    /// 依据明细：每个来源解析出了什么、是否一致。
    pub evidence: Vec<String>,
    /// 依据不足时的可操作建议（写版本文件 / 升级一次刷新锚点）。
    pub advice: String,
}

impl LocalVersion {
    pub fn missing(raw: &str) -> LocalVersion {
        LocalVersion {
            version: None,
            raw: raw.to_string(),
            source: "none".into(),
            source_text: String::new(),
            confidence: Confidence::None,
            build_ymd: None,
            basis_score: 0,
            evidence: vec!["所有版本来源均无法解析".into()],
            advice: "在固件中写入 /etc/online-upgrade-version 可获得强依据".into(),
        }
    }

    /// 可用于与 `target` 比较的版本号。
    ///
    /// 构建日期只对日期型标签有意义：自用构建标签使用 `YY.MM.DD`，
    /// 发布版可能使用 `YYYY.MM.DD`，
    /// 因此年份宽度需要与目标对齐（否则 `26.11.07` 会按 `26 < 2026`
    /// 比较，被误判为降级）。对于 `24.10.0` 这类普通发布号，
    /// 则完全不能比较日期——比较结果没有意义，
    /// 还可能掩盖真实更新——此时返回 `None`，
    /// 由调用方上报“无法比较”。
    pub fn comparable(&self, target: Option<&Version>) -> Option<Version> {
        if let Some((y, m, d)) = self.build_ymd {
            let target = target?;
            if !target.is_date_tag() {
                return None;
            }
            let four_digit = target.nums.first().copied().unwrap_or(0) >= 1900;
            return crate::version::from_ymd(
                if four_digit {
                    y as u64
                } else {
                    (y % 100) as u64
                },
                m,
                d,
            );
        }
        self.version.clone()
    }
}

/// 构建流水线可能写入 rootfs 的版本文件。
const VERSION_FILES: [(&str, &str); 4] = [
    (
        "/etc/online-upgrade-version",
        "构建写入的版本文件 /etc/online-upgrade-version",
    ),
    (
        "/etc/build-version",
        "构建写入的版本文件 /etc/build-version",
    ),
    (
        "/etc/firmware-version",
        "构建写入的版本文件 /etc/firmware-version",
    ),
    ("/etc/version", "构建写入的版本文件 /etc/version"),
];

fn read_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// 从 shell 风格文件中读取 `KEY='value'`、`KEY="value"` 或 `KEY=value`。
fn kv(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let rest = match line.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
            Some(r) => r,
            None => continue,
        };
        let value = rest
            .trim_start_matches('\'')
            .trim_end_matches('\'')
            .trim_start_matches('"')
            .trim_end_matches('"')
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// 镜像构建时间，取自发行标记文件的 mtime。
///
/// rootfs 文件的 mtime 来自构建过程而非刷机时刻，
/// 因此可用于此处的判断。时间按 UTC+8 显示，与本插件
/// 其他部分保持一致。
fn build_ymd() -> Option<(i64, u32, u32)> {
    let mut secs: Option<i64> = None;
    for path in ["/etc/openwrt_release", "/etc/openwrt_version"] {
        if let Ok(meta) = std::fs::metadata(path) {
            if let Ok(modified) = meta.modified() {
                if let Ok(d) = modified.duration_since(UNIX_EPOCH) {
                    secs = Some(d.as_secs() as i64);
                    break;
                }
            }
        }
    }
    let secs = secs?;
    if secs <= 0 {
        return None;
    }
    let (y, m, d) = civil_from_days((secs + 8 * 3600).div_euclid(86400));
    if y < 2015 {
        return None;
    }
    Some((y, m as u32, d as u32))
}

/// `apk info -v <包名>` / `opkg info <包名>` → 版本号（若包管理器能提供）。
fn package_version(pkg: &str) -> Option<String> {
    for (cmd, args) in [
        ("apk", vec!["info", "-v", pkg]),
        ("opkg", vec!["info", pkg]),
    ] {
        let out = match std::process::Command::new(cmd).args(&args).output() {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix(&format!("{pkg}-")) {
                return Some(v.trim().to_string());
            }
            if let Some(v) = line.strip_prefix("Version:") {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// 取第一个文本中包含可解析版本号的候选来源。
fn select(candidates: &[(String, String, String, Confidence)]) -> Option<usize> {
    candidates
        .iter()
        .position(|(_, _, value, _)| pick(value).is_some())
}

/// 本插件在刷写成功后写入的版本锚点（UCI `last_upgrade_version`）。
/// 默认值 `0` 表示尚未记录过，不得当成版本。
fn upgrade_anchor() -> Option<String> {
    let v = crate::uci::get("last_upgrade_version")?;
    let v = v.trim().to_string();
    if v.is_empty() || v == "0" || v.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(v)
}

/// 锚点可信度：不再要求与构建日期逐日相等。
///
/// overlay copy-up、配置恢复都会改动 rootfs mtime，用这个并不可靠的
/// 基准去否定插件自己写入的「上次成功升级版本」是逻辑倒置：设备因此
/// 退到 build_time 弱依据，版本被抬高且所有比较都被标成 suspect。
/// 现在改为「锚点日期不晚于构建日期且差值在 14 天窗口内」即采信，
/// 超出窗口仍采信但降低依据强度并给出核对建议。
///
/// 注意：`last_upgrade_version` 记录的是「上次经本插件成功升级的版本」，
/// 不是当前运行固件版本。用户在插件之外手动刷机后，锚点会滞留旧值；
/// 单看日期窗口无法区分「mtime 漂移」与「固件真的换了」，因此
/// `detect_with_target()` 在比较前先用 `anchor_is_stale()` 做交叉核对。
/// 交叉核对：锚点是否已过期（当前固件很可能就是目标版本）。
///
/// 日期型标签（自用构建的 `YY.MM.DD`）与固件构建日期同名时，
/// 「构建日期 == 目标版本」是当前固件即为目标版本的正面证据。
/// 此时若锚点版本更旧，说明用户在插件之外刷入过新固件而
/// `last_upgrade_version` 未更新；再拿锚点当本机版本就会把
/// 同版本误判成 `newer` / `has_update=true`。
/// 返回按目标年份位数对齐后的构建日期版本，供调用方作为本机版本。
pub fn anchor_is_stale(
    anchor: &Version,
    build: Option<(i64, u32, u32)>,
    target: Option<&Version>,
) -> Option<Version> {
    let target = target?;
    if !target.is_date_tag() || anchor.compare(target) != Ordering::Less {
        return None;
    }
    let (y, m, d) = build?;
    let four_digit = target.nums.first().copied().unwrap_or(0) >= 1900;
    let build_version = crate::version::from_ymd(
        if four_digit { y as u64 } else { (y % 100) as u64 },
        m,
        d,
    )?;
    (build_version.compare(target) == Ordering::Equal).then_some(build_version)
}

fn anchor_trust(anchor: &Version, build: Option<(i64, u32, u32)>) -> (u8, String, bool) {
    let Some((y, m, d)) = build else {
        return (60, "锚点可用，但未能与固件构建日期核对".into(), true);
    };
    if anchor.nums.len() < 3 {
        return (45, "锚点不是日期型版本，仅作参考".into(), true);
    }
    let ay = anchor.nums[0] as i64;
    let year = if ay >= 1900 { ay } else { 2000 + ay };
    let (am, ad) = (anchor.nums[1] as u32, anchor.nums[2] as u32);
    if !(1..=12).contains(&am) || !(1..=31).contains(&ad) {
        return (40, "锚点日期字段越界，仅作参考".into(), true);
    }
    let delta = days_from_civil(y, m, d) - days_from_civil(year, am, ad);
    let stamp = format!("{y}-{m:02}-{d:02}");
    if delta == 0 {
        (90, format!("锚点日期与固件构建日期 {stamp} 一致"), true)
    } else if (0..=14).contains(&delta) {
        (
            75,
            format!("锚点日期早于构建日期 {stamp} 共 {delta} 天，在合理窗口内"),
            true,
        )
    } else if delta < 0 {
        let late = -delta;
        if late > 30 {
            return (
                30,
                format!("锚点日期晚于构建日期 {stamp} 共 {late} 天，疑似残留锚点，不予采信"),
                false,
            );
        }
        (
            45,
            format!("锚点日期晚于构建日期 {stamp} 共 {late} 天，可能不是本机当前固件，建议核对"),
            true,
        )
    } else {
        (
            40,
            format!("锚点日期早于构建日期 {stamp} 共 {delta} 天，超出 14 天窗口，可能已过期"),
            true,
        )
    }
}

/// 本机版本检测：在采用升级锚点前交叉核对锚点是否已过期。
///
/// `target` 是本次比较的云端目标版本；不需要比较时可传 `None`，
/// 此时不做锚点过期交叉核对，仅按来源优先级与日期窗口判定。
pub fn detect_with_target(target: Option<&Version>) -> LocalVersion {
    let rel = read_file("/etc/openwrt_release").unwrap_or_default();
    let osrel = read_file("/etc/os-release").unwrap_or_default();

    // (来源标识, 来源说明, 原始值, 可信度)
    let mut cands: Vec<(String, String, String, Confidence)> = Vec::new();

    for (path, label) in VERSION_FILES {
        if let Some(v) = read_file(path) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                cands.push((
                    format!("version_file:{path}"),
                    label.to_string(),
                    v,
                    Confidence::Strong,
                ));
            }
        }
    }

    let distro = [
        ("DISTRIB_VERSION", "发行信息 DISTRIB_VERSION"),
        ("DISTRIB_RELEASE", "发行信息 DISTRIB_RELEASE"),
        ("DISTRIB_DESCRIPTION", "发行描述 DISTRIB_DESCRIPTION"),
        ("DISTRIB_REVISION", "发行修订号 DISTRIB_REVISION"),
    ];
    for (key, label) in distro {
        let conf = if key == "DISTRIB_REVISION" {
            // r1234-abcdef 形式的修订号不是发布版本号
            Confidence::Weak
        } else {
            Confidence::Strong
        };
        if let Some(v) = kv(&rel, key) {
            cands.push((key.to_string(), label.to_string(), v, conf));
        }
    }
    for (key, label, conf) in [
        ("VERSION_ID", "os-release VERSION_ID", Confidence::Strong),
        ("BUILD_ID", "os-release BUILD_ID", Confidence::Weak),
    ] {
        if let Some(v) = kv(&osrel, key) {
            cands.push((key.to_string(), label.to_string(), v, conf));
        }
    }
    if let Some(v) = read_file("/etc/openwrt_version") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            cands.push((
                "/etc/openwrt_version".into(),
                "构建版本标记 /etc/openwrt_version".into(),
                v,
                Confidence::Weak,
            ));
        }
    }
    if let Some(v) = package_version("base-files") {
        cands.push((
            "package:base-files".into(),
            "base-files 包版本".into(),
            v,
            Confidence::Weak,
        ));
    }

    // 保留第一个非空原始文本（通常是 `SNAPSHOT`），用于“无法
    // 比较”提示，即使所有来源都解析失败。
    let mut raw_marker = String::new();
    if let Some(v) = kv(&rel, "DISTRIB_RELEASE") {
        raw_marker = v;
    }
    if raw_marker.is_empty() {
        for (_, _, value, _) in &cands {
            if !value.is_empty() {
                raw_marker = value.clone();
                break;
            }
        }
    }

    // 多源交叉验证：记录每个来源解析出的版本，一致则提升依据强度。
    let mut evidence: Vec<String> = Vec::new();
    let mut parsed: Vec<(String, String)> = Vec::new();
    for (key, _, value, _) in &cands {
        if let Some(v) = pick(value) {
            evidence.push(format!("{key} → {}", v.as_display()));
            parsed.push((key.clone(), v.as_display()));
        }
    }
    let distinct: std::collections::BTreeSet<&str> =
        parsed.iter().map(|(_, v)| v.as_str()).collect();
    let corroborated = parsed.len() >= 2 && distinct.len() == 1;
    if corroborated {
        evidence.push("多个独立来源版本一致，已交叉验证".into());
    } else if distinct.len() > 1 {
        evidence.push("不同来源版本不一致，依据强度已下调".into());
    }

    if let Some(i) = select(&cands) {
        let (key, label, value, conf) = cands[i].clone();
        let version = pick(&value);
        let mut score: u8 = if conf == Confidence::Strong { 85 } else { 55 };
        if corroborated {
            score = score.saturating_add(10);
        }
        if distinct.len() > 1 {
            score = score.saturating_sub(20);
        }
        return LocalVersion {
            version,
            raw: value,
            source: key,
            source_text: label,
            confidence: conf,
            build_ymd: None,
            basis_score: score.min(100),
            evidence,
            advice: if distinct.len() > 1 {
                "各版本来源不一致，建议写入 /etc/online-upgrade-version 统一固件版本标记".into()
            } else {
                String::new()
            },
        };
    }

    let build = build_ymd();

    // 交叉核对优先于锚点采信：固件构建日期恰为目标版本、而锚点更旧时，
    // 判定锚点是插件之外刷机留下的过期记录，改以构建日期作为本机版本。
    // 若这里不拦截，anchor_trust() 会把「早于构建日期 N 天」当成 mtime
    // 漂移而给 75 分强依据，把同版本误报成「发现新版本」。
    if let (Some(t), Some((y, m, d)), Some(anchor_raw)) = (target, build, upgrade_anchor()) {
        if let Some(anchor) = pick(&anchor_raw) {
            if let Some(build_version) =
                anchor_is_stale(&anchor, Some((y, m, d)), Some(t))
            {
                let stamp = format!("{y}-{m:02}-{d:02}");
                let mut ev = evidence.clone();
                ev.push(format!("固件构建日期 {stamp} 与目标版本 {} 一致", t.as_display()));
                ev.push(format!(
                    "升级锚点 {} 早于固件构建日期，判定为插件之外刷机留下的过期记录",
                    anchor.as_display()
                ));
                return LocalVersion {
                    version: Some(build_version.clone()),
                    raw: build_version.as_display(),
                    source: "build_date_matches_target".into(),
                    source_text: format!(
                        "固件构建日期 {stamp} 与目标版本 {} 一致（升级锚点已过期）",
                        t.as_display()
                    ),
                    confidence: Confidence::Weak,
                    build_ymd: None,
                    basis_score: 55,
                    evidence: ev,
                    advice: format!(
                        "锚点 {} 疑似过期：当前固件很可能已是 {}。通过插件升级一次可刷新锚点，或在固件中写入 /etc/online-upgrade-version",
                        anchor.as_display(),
                        build_version.as_display()
                    ),
                };
            }
        }
    }

    // SNAPSHOT 镜像没有可解析的发布号时，优先采用「上次成功升级记录的版本」：
    // 它由本插件在刷写成功后写入，比 rootfs 文件 mtime 稳定（overlay copy-up、
    // 恢复配置都会改动 mtime）。锚点不再要求与构建日期逐日相等，而是按
    // 14 天合理窗口与日期方向给出依据强度；明显超前于构建日期的锚点拒绝采信。
    if let Some(anchor_raw) = upgrade_anchor() {
        if let Some(anchor) = pick(&anchor_raw) {
            let (score, note, accept) = anchor_trust(&anchor, build);
            if accept {
                let conf = if score >= 70 {
                    Confidence::Strong
                } else {
                    Confidence::Weak
                };
                let mut ev = evidence.clone();
                ev.push(format!("上次成功升级记录 {}：{}", anchor.as_display(), note));
                return LocalVersion {
                    version: Some(anchor),
                    raw: anchor_raw,
                    source: "last_upgrade_version".into(),
                    source_text: format!("上次成功升级记录的版本（{note}）"),
                    confidence: conf,
                    build_ymd: None,
                    basis_score: score,
                    evidence: ev,
                    advice: if score < 70 {
                        "锚点可能过期：成功升级一次可刷新记录，或写入 /etc/online-upgrade-version".into()
                    } else {
                        String::new()
                    },
                };
            }
        }
    }

    match build {
        Some((y, m, d)) => LocalVersion {
            version: None,
            raw: if raw_marker.is_empty() {
                "未知".to_string()
            } else {
                raw_marker
            },
            source: "build_time".into(),
            source_text: format!("固件构建时间 {y}-{m:02}-{d:02}（弱依据，仅能与日期型标签比较）"),
            confidence: Confidence::Weak,
            build_ymd: Some((y, m, d)),
            basis_score: 30,
            evidence: {
                let mut e = evidence.clone();
                e.push(format!("回退到固件构建时间 {y}-{m:02}-{d:02}（弱依据）"));
                e
            },
            advice: "本机版本仅来自构建时间：成功升级一次可写入强依据，或在固件中写入 /etc/online-upgrade-version".into(),
        },
        None => LocalVersion::missing(if raw_marker.is_empty() {
            "未知"
        } else {
            &raw_marker
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(value: &str, key: &str, conf: Confidence) -> (String, String, String, Confidence) {
        (key.to_string(), key.to_string(), value.to_string(), conf)
    }

    #[test]
    fn parses_first_usable_source() {
        let cands = vec![
            c("SNAPSHOT", "DISTRIB_RELEASE", Confidence::Strong),
            c("r0-23a1cd3", "DISTRIB_REVISION", Confidence::Weak),
            c("26.11.07", "/etc/openwrt_version", Confidence::Weak),
        ];
        let i = select(&cands).unwrap();
        assert_eq!(cands[i].2, "26.11.07");
    }

    #[test]
    fn snapshot_only_means_no_version() {
        let cands = vec![
            c("SNAPSHOT", "DISTRIB_RELEASE", Confidence::Strong),
            c(
                "ImmortalWRT SNAPSHOT r0-23a1cd3",
                "DISTRIB_DESCRIPTION",
                Confidence::Strong,
            ),
        ];
        assert!(select(&cands).is_none());
    }

    #[test]
    fn build_time_aligns_year_width_with_target() {
        let local = LocalVersion {
            version: None,
            raw: "SNAPSHOT".into(),
            source: "build_time".into(),
            source_text: "固件构建时间".into(),
            confidence: Confidence::Weak,
            build_ymd: Some((2026, 11, 7)),
            basis_score: 30,
            evidence: vec![],
            advice: String::new(),
        };
        let short = local.comparable(Some(&pick("26.11.08").unwrap())).unwrap();
        assert_eq!(short.as_display(), "26.11.07");
        let long = local
            .comparable(Some(&pick("2026.11.08").unwrap()))
            .unwrap();
        assert_eq!(long.as_display(), "2026.11.07");
        // 2 位与 4 位不能混比：对齐后 26.11.07 < 26.11.08
        assert!(short.compare(&pick("26.11.08").unwrap()).is_lt());
        assert!(long.compare(&pick("2026.11.08").unwrap()).is_lt());
    }

    #[test]
    fn build_time_skips_non_date_target() {
        let local = LocalVersion {
            version: None,
            raw: "SNAPSHOT".into(),
            source: "build_time".into(),
            source_text: "固件构建时间".into(),
            confidence: Confidence::Weak,
            build_ymd: Some((2026, 11, 7)),
            basis_score: 30,
            evidence: vec![],
            advice: String::new(),
        };
        // 普通发布号与构建时间不可比：直接放弃比较，交给「无法比较」放行，
        // 否则 26.11.07 > 24.10.0 会被当成当前版本更高而拒绝升级。
        assert!(local.comparable(Some(&pick("24.10.0").unwrap())).is_none());
        assert!(local
            .comparable(Some(&pick("25.12.0.1").unwrap()))
            .is_none());
        assert!(local.comparable(None).is_none());
    }

    #[test]
    fn anchor_trust_uses_window_not_exact_date() {
        let a = pick("26.11.10").unwrap();
        let (exact, _, ok) = anchor_trust(&a, Some((2026, 11, 10)));
        assert!(ok && exact >= 85);
        let (within, note, ok) = anchor_trust(&a, Some((2026, 11, 13)));
        assert!(ok && within >= 70, "{note}");
        assert!(note.contains("3 天"));
        let (stale, _, _) = anchor_trust(&a, Some((2026, 12, 20)));
        assert!(stale < 70);
        let (future, _, _) = anchor_trust(&a, Some((2026, 11, 1)));
        assert!(future < 70);
        let (nodate, _, _) = anchor_trust(&a, None);
        assert!(nodate >= 60);
        let short = pick("1.2").unwrap();
        assert!(anchor_trust(&short, Some((2026, 1, 2))).0 < 70);
    }

    #[test]
    fn anchor_is_stale_detects_out_of_band_flash() {
        // 设备锚点 26.11.10、固件构建于 2026-11-15、目标版本 26.11.15
        let stale = anchor_is_stale(
            &pick("26.11.10").unwrap(),
            Some((2026, 11, 15)),
            Some(&pick("26.11.15").unwrap()),
        );
        assert_eq!(stale.unwrap().as_display(), "26.11.15");
        // 四位年份标签同样对齐
        assert_eq!(
            anchor_is_stale(
                &pick("26.11.10").unwrap(),
                Some((2026, 11, 15)),
                Some(&pick("2026.11.15").unwrap()),
            )
            .unwrap()
            .as_display(),
            "2026.11.15"
        );
        // 锚点不比目标旧时无需判定过期
        assert!(anchor_is_stale(
            &pick("26.11.15").unwrap(),
            Some((2026, 11, 15)),
            Some(&pick("26.11.15").unwrap()),
        )
        .is_none());
        // 目标不是日期型标签、构建日期与目标不同、无构建日期：均不触发
        assert!(anchor_is_stale(
            &pick("26.11.10").unwrap(),
            Some((2026, 11, 15)),
            Some(&pick("24.10.0").unwrap()),
        )
        .is_none());
        assert!(anchor_is_stale(
            &pick("26.11.10").unwrap(),
            Some((2026, 11, 14)),
            Some(&pick("26.11.15").unwrap()),
        )
        .is_none());
        assert!(anchor_is_stale(
            &pick("26.11.10").unwrap(),
            None,
            Some(&pick("26.11.15").unwrap()),
        )
        .is_none());
        // 无目标版本（detect() 兼容入口）时不做判定
        assert!(anchor_is_stale(&pick("26.11.10").unwrap(), Some((2026, 11, 15)), None).is_none());
    }

    #[test]
    fn anchor_trust_rejects_clearly_future_anchor() {
        let stale = pick("26.12.30").unwrap();
        let (score, _, accept) = anchor_trust(&stale, Some((2026, 11, 15)));
        assert!(!accept);
        assert!(score < 60);
        let recent = pick("26.11.13").unwrap();
        let (score, _, accept) = anchor_trust(&recent, Some((2026, 11, 15)));
        assert!(accept && score >= 70);
    }

    #[test]
    fn basis_score_is_exposed_for_weak_sources() {
        let local = LocalVersion::missing("SNAPSHOT");
        assert_eq!(local.basis_score, 0);
        assert!(!local.advice.is_empty());
        assert!(!local.evidence.is_empty());
    }

    #[test]
    fn build_time_rejects_absurd_dates() {
        // 2000 年以前的时间戳不可能来自固件构建
        let (y, _, _) = civil_from_days((1_000_000i64 + 8 * 3600).div_euclid(86400));
        assert!(y < 2015);
    }
}
