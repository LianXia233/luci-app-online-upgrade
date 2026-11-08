// Local firmware version detection.
//
// A self-built SNAPSHOT image has no comparable release number: `DISTRIB_RELEASE`
// is literally `SNAPSHOT`, so falling back to "cannot compare" makes every such
// device look like an unknown-version box and blocks the upgrade path. Instead
// try a chain of sources, highest confidence first, and remember *which* source
// produced the value so the UI can show the basis instead of a bare verdict.
//
// Order (high → low):
//   1. version files stamped by the build (CI-written tag)
//   2. `DISTRIB_VERSION`, `DISTRIB_RELEASE`, os-release `VERSION_ID`
//   3. `DISTRIB_DESCRIPTION`, `DISTRIB_REVISION`, `BUILD_ID`, `/etc/openwrt_version`
//   4. the `base-files` package version
//   5. the firmware build time (weak: only meaningful against YY.MM.DD style tags)
//
// Nothing here guesses: when no source yields a parseable version, the caller
// gets `Confidence::None` and reports "cannot compare" honestly.

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
    /// Parsed version, when a source produced a dotted number.
    pub version: Option<Version>,
    /// Verbatim text of the winning source (`SNAPSHOT`, `r0-23a1cd3`, ...).
    pub raw: String,
    /// Machine-readable source key.
    pub source: String,
    /// Human-readable source description (shown in the UI).
    pub source_text: String,
    pub confidence: Confidence,
    /// Build date (year, month, day), only set when the source is build time.
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

    /// Version usable for comparison against `target`.
    ///
    /// A build date is emitted with the same year width as the target version:
    /// self-build tags use `YY.MM.DD`, while a release may use `YYYY.MM.DD`.
    /// Without this alignment `26.11.07` would compare as `26 < 2026` and be
    /// mistaken for a downgrade.
    pub fn comparable(&self, target: Option<&Version>) -> Option<Version> {
        if let Some((y, m, d)) = self.build_ymd {
            let four_digit = target
                .map(|t| t.nums.first().copied().unwrap_or(0) >= 1900)
                .unwrap_or(false);
            return crate::version::from_ymd(
                if four_digit { y as u64 } else { (y % 100) as u64 },
                m,
                d,
            );
        }
        self.version.clone()
    }
}

/// Version files a build pipeline may stamp into the rootfs.
const VERSION_FILES: [(&str, &str); 4] = [
    (
        "/etc/online-upgrade-version",
        "构建写入的版本文件 /etc/online-upgrade-version",
    ),
    ("/etc/build-version", "构建写入的版本文件 /etc/build-version"),
    (
        "/etc/firmware-version",
        "构建写入的版本文件 /etc/firmware-version",
    ),
    ("/etc/version", "构建写入的版本文件 /etc/version"),
];

fn read_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Read `KEY='value'`, `KEY="value"` or `KEY=value` from a shell-style file.
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

/// Build time of the image, taken from the mtime of the release marker file.
///
/// Rootfs file mtimes come from the build, not from the flash moment, which is
/// what makes them usable here. Times are shown in UTC+8, matching the rest of
/// this plugin.
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

/// `apk info -v <pkg>` / `opkg info <pkg>` → version, if the manager knows it.
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

/// First candidate whose text contains a parseable version wins.
fn select(candidates: &[(String, String, String, Confidence)]) -> Option<usize> {
    candidates
        .iter()
        .position(|(_, _, value, _)| pick(value).is_some())
}

pub fn detect() -> LocalVersion {
    let rel = read_file("/etc/openwrt_release").unwrap_or_default();
    let osrel = read_file("/etc/os-release").unwrap_or_default();

    // (source key, source text, raw value, confidence)
    let mut cands: Vec<(String, String, String, Confidence)> = Vec::new();

    for (path, label) in VERSION_FILES {
        if let Some(v) = read_file(path) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                cands.push((format!("version_file:{path}"), label.to_string(), v, Confidence::Strong));
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
            // r1234-abcdef style revisions are not release numbers
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

    // Keep the first non-empty raw text (usually `SNAPSHOT`) for the "cannot
    // compare" message even when nothing parses.
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

    match build_ymd() {
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
        None => LocalVersion::missing(if raw_marker.is_empty() { "未知" } else { &raw_marker }),
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
            c("ImmortalWRT SNAPSHOT r0-23a1cd3", "DISTRIB_DESCRIPTION", Confidence::Strong),
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
        let long = local.comparable(Some(&pick("2026.11.08").unwrap())).unwrap();
        assert_eq!(long.as_display(), "2026.11.07");
        // 2 位与 4 位不能混比：对齐后 26.11.07 < 26.11.08
        assert!(short.compare(&pick("26.11.08").unwrap()).is_lt());
        assert!(long.compare(&pick("2026.11.08").unwrap()).is_lt());
    }

    #[test]
    fn build_time_rejects_absurd_dates() {
        // 2000 年以前的时间戳不可能来自固件构建
        let (y, _, _) = civil_from_days((1_000_000i64 + 8 * 3600).div_euclid(86400));
        assert!(y < 2015);
    }
}
