// Generic firmware-version parsing and comparison.
//
// Deliberately free of repo-, vendor- and asset-naming assumptions:
// any dotted numeric version (with an optional `v` prefix and an optional
// `-rc`/`-beta`/`-alpha` suffix) is recognised. Asset names usually put
// vendor/distro/build numbers before the firmware version and board/arch
// tokens after it, so the *last* candidate wins; that keeps vendor version
// numbers and SoC/arch numbers from being mistaken for the firmware version.
//
// Versions without any dotted number (e.g. a tag like `自用构建`) yield None;
// callers must then fall back to an explicit "cannot compare" path instead
// of inventing a version.

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
    /// Verbatim matched version text (keeps leading zeroes such as 23.05.3).
    pub raw: String,
}

fn candidate_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // group 1 = dotted numeric core, group 2 = pre-release word, group 3 = its number
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
}

/// Last dotted version found in `text` (see module docs for why "last").
pub fn pick(text: &str) -> Option<Version> {
    let mut last = None;
    for caps in candidate_re().captures_iter(text) {
        if let Some(v) = Version::from_caps(&caps) {
            last = Some(v);
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(s: &str) -> Option<String> {
        pick(s).map(|v| v.as_display())
    }

    #[test]
    fn vendor_prefix_is_not_the_version() {
        // The vendor's own 2.1.0 must not win over the distribution version.
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
        assert_eq!(picked("MyBuild-2026.11.07-1200").as_deref(), Some("2026.11.07"));
        assert_eq!(picked("自用构建"), None);
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
