// 通用的固件版本号解析与比较。
//
// 刻意不假定仓库、厂商与资产命名规则：
// 任何点分数字版本（可带 `v` 前缀，以及可选的
// `-rc`/`-beta`/`-alpha` 后缀）都能识别。
// 资产名通常把厂商/发行版/构建号放在固件版本之前，
// 把板型/架构标识放在其后，因此取*最后一个*候选；
// 这样可避免把厂商版本号和 SoC/架构编号误当成固件版本。
//
// 不含任何点分数字的版本（例如标签 `自用构建`）返回 None；
// 此时调用方必须走显式的“无法比较”路径，
// 而不是凭空编造一个版本号。

use regex::Regex;
use std::cmp::Ordering;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pre {
    Alpha(u64),
    Beta(u64),
    Rc(u64),
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub nums: Vec<u64>,
    pub pre: Option<Pre>,
    /// 匹配到的版本文本原文（保留 23.05.3 这类前导零）。
    pub raw: String,
}

fn candidate_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 分组 1 = 点分数字主体，分组 2 = 预发布类型词，分组 3 = 其编号
        Regex::new(
            r"(?i)(?:^|[^0-9a-z])(?:v)?([0-9]{1,6}(?:\.[0-9]{1,6}){1,3})(?:[-_.]?(rc|beta|alpha|pre)[-_.]?([0-9]{1,3})?)?",
        )
        .unwrap()
    })
}

impl Version {
    fn from_caps(caps: &regex::Captures<'_>) -> Option<Version> {
        let core = caps.get(1).unwrap().as_str();
        let nums: Vec<u64> = core
            .split('.')
            .filter_map(|p| p.parse::<u64>().ok())
            .collect();
        if nums.len() < 2 {
            return None;
        }
        let pre = caps.get(2).map(|m| {
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
        });
        let raw = match &pre {
            Some(Pre::Alpha(n)) => format!("{core}-alpha{n}"),
            Some(Pre::Beta(n)) => format!("{core}-beta{n}"),
            Some(Pre::Rc(n)) => format!("{core}-rc{n}"),
            Some(Pre::Other(s)) => format!("{core}-{s}"),
            None => core.to_string(),
        };
        Some(Version { nums, pre, raw })
    }

    fn pre_key(&self) -> (u8, u64, u8) {
        match &self.pre {
            None => (3, 0, 0),
            Some(Pre::Rc(n)) => (2, *n, 0),
            Some(Pre::Beta(n)) => (1, *n, 0),
            Some(Pre::Alpha(n)) => (0, *n, 0),
            Some(Pre::Other(_)) => (0, 0, 1),
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
        self.pre_key().cmp(&other.pre_key())
    }

    pub fn as_display(&self) -> String {
        self.raw.clone()
    }

    /// 判断是否为 `YY.MM.DD` / `YYYY.MM.DD` 这类日期型标签。
    ///
    /// 自用构建只能拿「固件构建时间」当版本，而构建时间只有和日期型标签比较才有意义：
    /// 把 `26.11.08` 去和 `24.10.0` 这类发布号比大小会得出毫无意义的关系
    /// （甚至把新发布判成降级、把老发布判成可升级），所以先确认目标本身是日期形态。
    pub fn is_date_tag(&self) -> bool {
        if self.pre.is_some() || self.nums.len() != 3 {
            return false;
        }
        let raw = self.raw.trim_start_matches('v');
        let parts: Vec<&str> = raw.split('.').collect();
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
        // 四位年份要落在可信区间，两位年份无法再校验
        let year_ok = if ys.len() == 4 {
            (1990..=2100).contains(&year)
        } else {
            true
        };
        year_ok && (1..=12).contains(&month) && (1..=31).contains(&day)
    }
}

/// `text` 中最后一个点分版本（为何取“最后一个”见模块说明）。
pub fn pick(text: &str) -> Option<Version> {
    let mut last = None;
    for caps in candidate_re().captures_iter(text) {
        if let Some(v) = Version::from_caps(&caps) {
            last = Some(v);
        }
    }
    last
}

/// 由日历日期（`年.月.日`）构造版本号。
///
/// 用于由构建时间推导的本地版本。年份宽度由调用方决定
/// （自用构建标签用 2 位 `YY.MM.DD`，发布号用 4 位 `YYYY.MM.DD`）。
pub fn from_ymd(year: u64, month: u32, day: u32) -> Option<Version> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(Version {
        nums: vec![year, month as u64, day as u64],
        pre: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(s: &str) -> Option<String> {
        pick(s).map(|v| v.as_display())
    }

    #[test]
    fn vendor_prefix_is_not_the_version() {
        // 厂商自己的 2.1.0 不能盖过发行版版本号。
        assert_eq!(
            picked("vendor-2.1.0-immortalwrt-24.10.0-x86-64-generic.img.gz").as_deref(),
            Some("24.10.0")
        );
    }

    #[test]
    fn arch_and_soc_numbers_are_not_versions() {
        assert_eq!(picked("x86-64-24kc-mt7621-sysupgrade.bin"), None);
        assert_eq!(picked("mt7981-aarch64-cortex-a53-sysupgrade.bin"), None);
    }

    #[test]
    fn v_prefix_and_multisegment() {
        assert_eq!(picked("v25.12.0.1-image.bin").as_deref(), Some("25.12.0.1"));
        assert_eq!(picked("fw-v1.2.3.bin").as_deref(), Some("1.2.3"));
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
    fn self_build_dated_tag() {
        assert_eq!(picked("自用-26.11.07").as_deref(), Some("26.11.07"));
        assert_eq!(
            picked("MyBuild-2026.11.07-1200").as_deref(),
            Some("2026.11.07")
        );
        assert_eq!(picked("自用构建"), None);
    }

    #[test]
    fn date_tag_only_for_yy_mm_dd_shape() {
        assert!(pick("26.11.08").unwrap().is_date_tag());
        assert!(pick("2026.11.08").unwrap().is_date_tag());
        // 普通发布号/四段版本/预发布都不是日期形态
        assert!(!pick("24.10.0").unwrap().is_date_tag());
        assert!(!pick("25.12.0.1").unwrap().is_date_tag());
        assert!(!pick("26.11").unwrap().is_date_tag());
        assert!(!pick("26.11.08-rc1").unwrap().is_date_tag());
        // 月/日越界不算日期
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
    fn isolated_values_and_unparsable_text() {
        assert_eq!(picked("25.12.0").as_deref(), Some("25.12.0"));
        assert_eq!(picked("SNAPSHOT"), None);
    }
}
