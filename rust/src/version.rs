// 通用的固件版本号解析与比较。
//
// 设计原则：不假定仓库、厂商与资产命名规则，也不凭空编造版本号。
// 与早期版本相比，这里把「固定取最后一个点分数字」升级为
// **候选打分**：同时识别点分版本、带前缀/后缀的版本、
// 连字符日期、紧凑日期与单段版本，按证据强度选择最佳候选。
//
// 兼容性：`pick()` 仍是全局入口，所有既有调用点无需改动；
// 新增 `pick_best()` 返回证据与置信分，`versions_agree()`
// 用于标签与资产名的模糊一致性核对。

use regex::Regex;
use std::cmp::Ordering;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pre {
    Alpha(u64),
    Beta(u64),
    Rc(u64),
    Snapshot,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub nums: Vec<u64>,
    pub pre: Option<Pre>,
    /// 构建/发布序号（`-r1`、`+build5`），同版本之间仍可比较。
    pub build: Option<u64>,
    /// 匹配到的版本文本原文（保留 `23.05.3`、`26.11.07` 这类前导零）。
    pub raw: String,
}

/// 一次版本提取的完整结果，供调用方展示判定依据。
#[derive(Debug, Clone)]
pub struct VersionHit {
    pub version: Version,
    /// 证据分，越高越可信（不是版本号大小）。
    pub score: i32,
    pub evidence: Vec<String>,
    pub fragment: String,
}

fn dotted_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 组 1 = 点分数字主体（2~6 段），组 2/3 = 预发布词与编号，组 4 = -rN 构建号。
        Regex::new(
            r"(?i)(?:^|[^0-9a-z])(?:v|ver|version|release|fw|firmware|build)?([0-9]{1,6}(?:\.[0-9]{1,6}){1,5})(?:[-_.]?(rc|beta|alpha|pre)[-_.]?([0-9]{1,3})?)?(?:[-_.]?r([0-9]{1,4}))?",
        )
        .unwrap()
    })
}

/// 单段版本必须有明确前缀，否则 `x86-64`、`mt7621` 之类会被误判。
fn single_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(?:^|[^0-9a-z])(?:v|ver|version|release|fw|firmware|build)(?:[-_.: ]?)([0-9]{1,6})(?:[^0-9a-z]|$)")
            .unwrap()
    })
}

fn dash_date_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:^|[^0-9])([0-9]{4}|[0-9]{2})[-_]([0-9]{1,2})[-_]([0-9]{1,2})(?:[^0-9]|$)")
            .unwrap()
    })
}

fn compact_date_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:^|[^0-9])(20[0-9]{2})([0-9]{2})([0-9]{2})(?:[^0-9]|$)").unwrap()
    })
}

fn valid_month_day(m: u64, d: u64) -> bool {
    (1..=12).contains(&m) && (1..=31).contains(&d)
}

impl Version {
    fn from_caps(caps: &regex::Captures<'_>, text: &str) -> Option<Version> {
        let core = caps.get(1)?.as_str();
        let nums: Vec<u64> = core
            .split('.')
            .filter_map(|p| p.parse::<u64>().ok())
            .collect();
        if nums.len() < 2 {
            return None;
        }
        let end = caps.get(0)?.end();
        let tail = text.get(end..).unwrap_or("").to_ascii_lowercase();
        let snapshot = tail.starts_with("-snapshot") || tail.starts_with("_snapshot");

        let pre = if snapshot {
            Some(Pre::Snapshot)
        } else {
            caps.get(2).map(|m| {
                let n = caps
                    .get(3)
                    .and_then(|x| x.as_str().parse::<u64>().ok())
                    .unwrap_or(0);
                match m.as_str().to_ascii_lowercase().as_str() {
                    "rc" => Pre::Rc(n),
                    "beta" => Pre::Beta(n),
                    "alpha" => Pre::Alpha(n),
                    other => Pre::Other(format!("{other}{n}")),
                }
            })
        };
        let build = caps
            .get(4)
            .and_then(|x| x.as_str().parse::<u64>().ok());

        let mut raw = core.to_string();
        match &pre {
            Some(Pre::Alpha(n)) => raw.push_str(&format!("-alpha{n}")),
            Some(Pre::Beta(n)) => raw.push_str(&format!("-beta{n}")),
            Some(Pre::Rc(n)) => raw.push_str(&format!("-rc{n}")),
            Some(Pre::Snapshot) => raw.push_str("-SNAPSHOT"),
            Some(Pre::Other(s)) => raw.push_str(&format!("-{s}")),
            None => {}
        }
        if let Some(n) = build {
            raw.push_str(&format!("-r{n}"));
        }
        Some(Version { nums, pre, build, raw })
    }

    fn from_dash_date(caps: &regex::Captures<'_>) -> Option<Version> {
        let y = caps.get(1)?.as_str();
        let m = caps.get(2)?.as_str().parse::<u64>().ok()?;
        let d = caps.get(3)?.as_str().parse::<u64>().ok()?;
        if !valid_month_day(m, d) {
            return None;
        }
        Some(Version {
            nums: vec![y.parse::<u64>().ok()?, m, d],
            pre: None,
            build: None,
            raw: format!("{y}.{m:02}.{d:02}"),
        })
    }

    fn from_compact_date(caps: &regex::Captures<'_>) -> Option<Version> {
        let y: u64 = caps.get(1)?.as_str().parse().ok()?;
        let m: u64 = caps.get(2)?.as_str().parse().ok()?;
        let d: u64 = caps.get(3)?.as_str().parse().ok()?;
        if !valid_month_day(m, d) {
            return None;
        }
        Some(Version {
            nums: vec![y, m, d],
            pre: None,
            build: None,
            raw: format!("{y}.{m:02}.{d:02}"),
        })
    }

    fn from_single(caps: &regex::Captures<'_>) -> Option<Version> {
        let n: u64 = caps.get(1)?.as_str().parse().ok()?;
        Some(Version {
            nums: vec![n],
            pre: None,
            build: None,
            raw: n.to_string(),
        })
    }

    fn pre_key(&self) -> (u8, u64, u8) {
        match &self.pre {
            None => (3, 0, 0),
            Some(Pre::Rc(n)) => (2, *n, 0),
            Some(Pre::Beta(n)) => (1, *n, 0),
            Some(Pre::Alpha(n)) => (0, *n, 0),
            Some(Pre::Snapshot) => (0, 0, 1),
            Some(Pre::Other(_)) => (0, 0, 2),
        }
    }

    pub fn compare(&self, other: &Version) -> Ordering {
        let n = self.nums.len().max(other.nums.len());
        for i in 0..n {
            let a = self.nums.get(i).copied().unwrap_or(0);
            let b = other.nums.get(i).copied().unwrap_or(0);
            if a != b {
                return a.cmp(&b);
            }
        }
        match self.pre_key().cmp(&other.pre_key()) {
            Ordering::Equal => self.build.unwrap_or(0).cmp(&other.build.unwrap_or(0)),
            other => other,
        }
    }

    pub fn as_display(&self) -> String {
        self.raw.clone()
    }

    /// 判断是否为 `YY.MM.DD` / `YYYY.MM.DD` 这类日期型标签。
    /// 归一化后的连字符日期同样适用。
    pub fn is_date_tag(&self) -> bool {
        if self.pre.is_some() || self.build.is_some() || self.nums.len() != 3 {
            return false;
        }
        let core = self
            .raw
            .split(['-', '+'])
            .next()
            .unwrap_or(&self.raw)
            .trim_start_matches('v');
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 {
            return false;
        }
        let (ys, ms, ds) = (parts[0], parts[1], parts[2]);
        if !(ys.len() == 2 || ys.len() == 4) || !ys.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if ms.is_empty() || ds.is_empty() || ms.len() > 2 || ds.len() > 2 {
            return false;
        }
        if !ms.chars().chain(ds.chars()).all(|c| c.is_ascii_digit()) {
            return false;
        }
        let year: i32 = match ys.parse() {
            Ok(v) => v,
            Err(_) => return false,
        };
        let month: u32 = ms.parse().unwrap_or(0);
        let day: u32 = ds.parse().unwrap_or(0);
        let year_ok = if ys.len() == 4 {
            (1990..=2100).contains(&year)
        } else {
            true
        };
        year_ok && (1..=12).contains(&month) && (1..=31).contains(&day)
    }
}

struct Candidate {
    hit: VersionHit,
    index: usize,
}

fn has_version_prefix(whole: &str) -> bool {
    let low = whole.to_ascii_lowercase();
    let head = low.trim_start_matches(|c: char| !c.is_ascii_alphabetic());
    head.starts_with("version")
        || head.starts_with("ver")
        || head.starts_with("release")
        || head.starts_with("firmware")
        || head.starts_with("fw")
        || head.starts_with("build")
        || head.starts_with('v')
}

fn context(text: &str, start: usize, end: usize) -> String {
    let from = start.saturating_sub(28);
    let to = (end + 28).min(text.len());
    text.get(from..to).unwrap_or("").to_ascii_lowercase()
}

fn before_text(text: &str, start: usize) -> String {
    let from = start.saturating_sub(14);
    text.get(from..start).unwrap_or("").to_ascii_lowercase()
}

/// 从任意文本中提取最可信的版本号，并给出证据链。
pub fn pick_best(text: &str) -> Option<VersionHit> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut idx = 0usize;

    for caps in dotted_re().captures_iter(text) {
        let whole = caps.get(0)?.as_str();
        let start = caps.get(0)?.start();
        let end = caps.get(0)?.end();
        if let Some(v) = Version::from_caps(&caps, text) {
            let ctx = context(text, start, end);
            let before = before_text(text, start);
            let mut score = (idx * 6) as i32 + 4;
            let mut ev = vec!["点分数字版本".to_string()];
            if has_version_prefix(whole)
                || ["version", "release", "firmware", "build", "ver_", "fw_"]
                    .iter()
                    .any(|k| before.contains(k))
            {
                score += 6;
                ev.push("带版本前缀".into());
            }
            if v.nums.len() >= 5 {
                score -= 1;
                ev.push("多段版本".into());
            }
            if v.is_date_tag() {
                score += 5;
                ev.push("日期型版本".into());
            }
            if v.build.is_some() {
                ev.push("含构建序号".into());
            }
            if ctx.contains("sysupgrade") || ctx.contains(".bin") || ctx.contains("image") {
                score += 2;
                ev.push("位于固件镜像名附近".into());
            }
            // 只看紧邻前置窗口：既避免固件名里偶然出现 "image"/"kernel" 之类
            // 被远处上下文误伤，也能让 "linux-6.6.73" 这类内核版本降权。
            // 关键词用词干，因为前置窗口会在数字前的分隔符处截断。
            if ["kernel", "linux", "gcc", "toolchain", "u-boot", "uclibc", "musl"]
                .iter()
                .any(|k| before.contains(k))
            {
                score -= 12;
                ev.push("紧邻内核/工具链上下文，已降权".into());
            }
            out.push(Candidate {
                hit: VersionHit {
                    version: v,
                    score,
                    evidence: ev,
                    fragment: whole.trim().to_string(),
                },
                index: idx,
            });
        }
        idx += 1;
    }

    for caps in dash_date_re().captures_iter(text) {
        if let Some(v) = Version::from_dash_date(&caps) {
            let whole = caps.get(0).unwrap().as_str();
            if out.iter().any(|c| c.hit.version.raw == v.raw) {
                continue;
            }
            out.push(Candidate {
                hit: VersionHit {
                    version: v,
                    score: 7,
                    evidence: vec!["连字符/下划线日期，已归一化为点分版本".into()],
                    fragment: whole.trim().to_string(),
                },
                index: idx,
            });
            idx += 1;
        }
    }

    for caps in compact_date_re().captures_iter(text) {
        if let Some(v) = Version::from_compact_date(&caps) {
            let whole = caps.get(0).unwrap().as_str();
            if out.iter().any(|c| c.hit.version.raw == v.raw) {
                continue;
            }
            out.push(Candidate {
                hit: VersionHit {
                    version: v,
                    score: 6,
                    evidence: vec!["8 位紧凑日期，已归一化".into()],
                    fragment: whole.trim().to_string(),
                },
                index: idx,
            });
            idx += 1;
        }
    }

    for caps in single_re().captures_iter(text) {
        if let Some(v) = Version::from_single(&caps) {
            let whole = caps.get(0).unwrap().as_str();
            out.push(Candidate {
                hit: VersionHit {
                    version: v,
                    score: 1,
                    evidence: vec!["带前缀的单段版本".into()],
                    fragment: whole.trim().to_string(),
                },
                index: idx,
            });
            idx += 1;
        }
    }

    out.into_iter()
        .max_by(|a, b| {
            a.hit
                .score
                .cmp(&b.hit.score)
                .then(a.index.cmp(&b.index))
        })
        .map(|c| c.hit)
}

/// 兼容入口：返回最佳候选的版本号。
pub fn pick(text: &str) -> Option<Version> {
    pick_best(text).map(|h| h.version)
}

/// 模糊一致性核对：段数可补零、两位/四位年份可对齐。
/// 用于判断 Release 标签与资产名是否描述同一个版本。
pub fn versions_agree(a: &Version, b: &Version) -> bool {
    let n = a.nums.len().max(b.nums.len());
    for i in 0..n {
        let mut x = a.nums.get(i).copied().unwrap_or(0);
        let mut y = b.nums.get(i).copied().unwrap_or(0);
        if i == 0 && (x >= 1900 || y >= 1900) {
            x %= 100;
            y %= 100;
        }
        if x != y {
            return false;
        }
    }
    true
}

/// 由日历日期（`年.月.日`）构造版本号。
pub fn from_ymd(year: u64, month: u32, day: u32) -> Option<Version> {
    if !valid_month_day(month as u64, day as u64) {
        return None;
    }
    Some(Version {
        nums: vec![year, month as u64, day as u64],
        pre: None,
        build: None,
        raw: format!("{year}.{month:02}.{day:02}"),
    })
}

/// 自 1970-01-01 起的天数 → (年, 月, 日)。
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (年, 月, 日) → 自 1970-01-01 起的天数；用于计算日期差。
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m as i64 + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(s: &str) -> Option<String> {
        pick(s).map(|v| v.as_display())
    }

    #[test]
    fn vendor_prefix_is_not_the_version() {
        assert_eq!(
            picked("vendor-2.1.0-immortalwrt-24.10.0-x86-64-generic.img.gz").as_deref(),
            Some("24.10.0")
        );
    }

    #[test]
    fn arch_and_soc_numbers_are_not_versions() {
        assert_eq!(picked("x86-64-24kc-mt7621-sysupgrade.bin"), None);
        assert_eq!(picked("mt7981-aarch64-cortex-a53-sysupgrade.bin"), None);
        assert_eq!(picked("r0-146dc3d"), None);
    }

    #[test]
    fn v_prefix_and_multisegment() {
        assert_eq!(picked("v25.12.0.1-image.bin").as_deref(), Some("25.12.0.1"));
        assert_eq!(picked("fw-v1.2.3.bin").as_deref(), Some("1.2.3"));
        assert_eq!(picked("1.2.3.4.5.6-sysupgrade.bin").as_deref(), Some("1.2.3.4.5.6"));
    }

    #[test]
    fn explicit_prefixes_and_single_segment() {
        assert_eq!(picked("version=2.6.9").as_deref(), Some("2.6.9"));
        assert_eq!(picked("release_25.12.0").as_deref(), Some("25.12.0"));
        assert_eq!(picked("build-v3.bin").as_deref(), Some("3"));
        assert_eq!(picked("ver 7").as_deref(), Some("7"));
    }

    #[test]
    fn suffixes_are_preserved_and_compared() {
        assert_eq!(picked("2.6.9-r1.apk").as_deref(), Some("2.6.9-r1"));
        assert!(pick("2.6.9-r1").unwrap().compare(&pick("2.6.9").unwrap()).is_gt());
        assert!(pick("2.6.9-r2").unwrap().compare(&pick("2.6.9-r1").unwrap()).is_gt());
        assert_eq!(picked("25.12.0-rc1.bin").as_deref(), Some("25.12.0-rc1"));
        assert_eq!(picked("25.12.0-SNAPSHOT.bin").as_deref(), Some("25.12.0-SNAPSHOT"));
        assert!(pick("25.12.0-SNAPSHOT").unwrap().compare(&pick("25.12.0").unwrap()).is_lt());
    }

    #[test]
    fn prerelease_is_lower_than_final() {
        let rc = pick("25.12.0-rc1").unwrap();
        let fin = pick("25.12.0").unwrap();
        assert_eq!(rc.compare(&fin), Ordering::Less);
        let rc2 = pick("25.12.0-rc2").unwrap();
        assert_eq!(rc.compare(&rc2), Ordering::Less);
        let beta = pick("25.12.0-beta1").unwrap();
        assert_eq!(beta.compare(&rc), Ordering::Less);
    }

    #[test]
    fn date_formats_are_normalized() {
        assert_eq!(picked("自用-26.11.07").as_deref(), Some("26.11.07"));
        assert_eq!(picked("release-2026-11-15.zip").as_deref(), Some("2026.11.15"));
        assert_eq!(picked("build_26-11-15.bin").as_deref(), Some("26.11.15"));
        assert_eq!(picked("firmware-20261115.bin").as_deref(), Some("2026.11.15"));
        assert!(pick("2026-11-15").unwrap().is_date_tag());
        assert!(pick("26-11-15").unwrap().is_date_tag());
        assert_eq!(picked("自用构建"), None);
    }

    #[test]
    fn date_tag_only_for_yy_mm_dd_shape() {
        assert!(pick("26.11.08").unwrap().is_date_tag());
        assert!(pick("2026.11.08").unwrap().is_date_tag());
        assert!(!pick("24.10.0").unwrap().is_date_tag());
        assert!(!pick("25.12.0.1").unwrap().is_date_tag());
        assert!(!pick("26.11").unwrap().is_date_tag());
        assert!(!pick("26.11.08-rc1").unwrap().is_date_tag());
        assert!(!pick("26.13.01").unwrap().is_date_tag());
        assert!(!pick("26.11.00").unwrap().is_date_tag());
    }

    #[test]
    fn four_segments_compare() {
        let a = pick("25.12.0").unwrap();
        let b = pick("25.12.0.1").unwrap();
        assert_eq!(a.compare(&b), Ordering::Less);
        assert_eq!(b.compare(&a), Ordering::Greater);
        assert_eq!(a.compare(&pick("25.12.0").unwrap()), Ordering::Equal);
    }

    #[test]
    fn kernel_version_does_not_override_firmware_version() {
        assert_eq!(
            picked("distro-24.10.0-linux-6.6.73-sysupgrade.bin").as_deref(),
            Some("24.10.0")
        );
    }

    #[test]
    fn fuzzy_agreement_handles_year_width_and_padding() {
        assert!(versions_agree(&pick("26.11.15").unwrap(), &pick("2026.11.15").unwrap()));
        assert!(versions_agree(&pick("2.6.9").unwrap(), &pick("v2.6.9.0").unwrap()));
        assert!(!versions_agree(&pick("26.11.15").unwrap(), &pick("26.11.14").unwrap()));
    }

    #[test]
    fn pick_best_reports_evidence() {
        let hit = pick_best("H5000M-SelfUse-aarch64-26.11.15-sysupgrade.bin").unwrap();
        assert_eq!(hit.version.as_display(), "26.11.15");
        assert!(hit.evidence.iter().any(|e| e.contains("日期型版本")));
        assert!(!hit.fragment.is_empty());
    }

    #[test]
    fn days_from_civil_roundtrip() {
        let d = days_from_civil(2026, 11, 15);
        assert_eq!(civil_from_days(d), (2026, 11, 15));
        assert_eq!(days_from_civil(2026, 11, 13) - days_from_civil(2026, 11, 10), 3);
    }

    #[test]
    fn isolated_values_and_unparsable_text() {
        assert_eq!(picked("25.12.0").as_deref(), Some("25.12.0"));
        assert_eq!(picked("SNAPSHOT"), None);
    }

    #[test]
    fn dotted_date_is_not_duplicated_by_dash_rule() {
        let hit = pick_best("H5000M-SelfUse-26.11.15-sysupgrade.bin").unwrap();
        assert_eq!(hit.version.as_display(), "26.11.15");
        assert!(hit.evidence.iter().any(|e| e.contains("日期型版本")));
        assert!(!hit.evidence.iter().any(|e| e.contains("归一化")));
    }

    #[test]
    fn single_segment_never_overrides_dotted_version() {
        assert_eq!(
            picked("fw-v3-24.10.0-sysupgrade.bin").as_deref(),
            Some("24.10.0")
        );
        assert_eq!(picked("ver 7").as_deref(), Some("7"));
    }

    #[test]
    fn prefix_suffix_and_fuzzy_agreement_edges() {
        assert_eq!(picked("firmware-v1.2.3.4.5-rc2-r1.bin").as_deref(), Some("1.2.3.4.5-rc2-r1"));
        assert!(versions_agree(&pick("自用-26.11.15").unwrap(), &pick("2026.11.15").unwrap()));
        assert!(!versions_agree(&pick("26.11.15").unwrap(), &pick("26.11.15.1").unwrap()));
    }
}
