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

use crate::version::{civil_from_days, pick, Version};
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

/// 锚点的前三段是否与 rootfs 构建日期一致（兼容 2 位 / 4 位年份）。
/// 不一致说明设备可能被手动刷成了别的固件，此时不得采信锚点。
fn anchor_matches_build(anchor: &Version, y: i64, m: u32, d: u32) -> bool {
    let nums = &anchor.nums;
    if nums.len() < 3 {
        return false;
    }
    let ay = nums[0] as i64;
    let year_ok = if ay >= 1900 { ay == y } else { ay == y % 100 };
    year_ok && nums[1] as u32 == m && nums[2] as u32 == d
}

pub fn detect() -> LocalVersion {
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

    if let Some(i) = select(&cands) {
        let (key, label, value, conf) = cands[i].clone();
        let version = pick(&value);
        return LocalVersion {
            version,
            raw: value,
            source: key,
            source_text: label,
            confidence: conf,
            build_ymd: None,
        };
    }

    let build = build_ymd();

    // SNAPSHOT 镜像没有可解析的发布号时，优先采用「上次成功升级记录的版本」：
    // 它由本插件在刷写成功后写入，比 rootfs 文件 mtime 稳定（overlay copy-up、
    // 恢复配置都会改动 mtime）。为避免手动刷机后锚点过期，要求锚点日期与
    // rootfs 构建日期一致才按强依据采信；不一致则退回构建时间。
    if let Some(anchor_raw) = upgrade_anchor() {
        if let Some(anchor) = pick(&anchor_raw) {
            let trusted = match build {
                Some((y, m, d)) => anchor_matches_build(&anchor, y, m, d),
                None => true,
            };
            if trusted {
                return LocalVersion {
                    version: Some(anchor),
                    raw: anchor_raw,
                    source: "last_upgrade_version".into(),
                    source_text: match build {
                        Some((y, m, d)) => format!(
                            "上次成功升级记录的版本（与固件构建日期 {y}-{m:02}-{d:02} 一致）"
                        ),
                        None => "上次成功升级记录的版本（未能与构建日期核对）".to_string(),
                    },
                    confidence: Confidence::Strong,
                    build_ymd: None,
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
    fn anchor_matches_build_date() {
        assert!(anchor_matches_build(&pick("26.11.10").unwrap(), 2026, 11, 10));
        assert!(anchor_matches_build(&pick("2026.11.10").unwrap(), 2026, 11, 10));
        assert!(!anchor_matches_build(&pick("26.11.09").unwrap(), 2026, 11, 10));
        // 段数不足时不可当作日期使用
        assert!(!anchor_matches_build(&pick("1.2").unwrap(), 2026, 1, 2));
    }

    #[test]
    fn build_time_rejects_absurd_dates() {
        // 2000 年以前的时间戳不可能来自固件构建
        let (y, _, _) = civil_from_days((1_000_000i64 + 8 * 3600).div_euclid(86400));
        assert!(y < 2015);
    }
}
