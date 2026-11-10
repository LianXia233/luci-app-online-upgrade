// Device identity / architecture compatibility evaluation.
//
// Design goals (see README "识别矩阵" for the full matrix):
// 1. Identity comes from what the router reports about itself
//    (/etc/board.json, /tmp/sysinfo/*, `ubus call system board`,
//    /etc/openwrt_release, apk/opkg arch), never from a curated device table.
// 2. Matching is structural: the asset file name must contain the board id /
//    model, or several independent identity tokens, or exactly one
//    device-specific token. Platform/SoC words (mediatek, mt7621, filogic,
//    aarch64, sysupgrade, ...) are never accepted as evidence.
// 3. An asset whose architecture is not declared is never treated as
//    "generic and therefore fine"; it stays "cannot confirm".
// 4. A user supplied rule (UCI `firmware_pattern`) is an explicit statement
//    of intent: a hit is reported as a rule match with its own basis, but an
//    architecture conflict still blocks.

use crate::json::Value;
use std::collections::BTreeSet;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct DeviceProfile {
    pub model: String,
    pub board_name: String,
    pub board_id: String,
    pub target: String,
    pub arch: String,
    pub pkg_arch: String,
    pub uname_arch: String,
    pub release: String,
    pub revision: String,
    pub distro: String,
}

impl DeviceProfile {
    pub fn current() -> Self {
        let sys_model = read_trim("/tmp/sysinfo/model");
        let sys_board = read_trim("/tmp/sysinfo/board_name");
        let (json_id, json_name) = read_board_json();
        let (distro, release, target, arch, revision) = read_release_info();

        // ubus is authoritative but costs a subprocess: only ask when the
        // file based sources are incomplete.
        let mut ubus_board = String::new();
        let mut ubus_model = String::new();
        let mut ubus_arch = String::new();
        let mut ubus_release = String::new();
        if sys_board.is_empty() || sys_model.is_empty() || arch.is_empty() {
            let (b, m, a, r) = read_ubus_board();
            ubus_board = b;
            ubus_model = m;
            ubus_arch = a;
            ubus_release = r;
        }

        let board_name = first_non_empty(&[&sys_board, &json_id, &ubus_board]);
        let board_id = first_non_empty(&[&json_id, &sys_board, &ubus_board]);
        let model = first_non_empty(&[&sys_model, &json_name, &ubus_model]);

        let uname_arch = Command::new("uname")
            .arg("-m")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| std::env::consts::ARCH.to_string());

        let pkg_arch = package_manager_arch();

        Self {
            model: if model.is_empty() {
                "Generic Router".into()
            } else {
                model
            },
            board_name,
            board_id,
            target,
            arch: first_non_empty(&[&arch, &ubus_arch]),
            pkg_arch,
            uname_arch,
            release: first_non_empty(&[&release, &ubus_release]),
            revision,
            distro: if distro.is_empty() {
                "OpenWrt".into()
            } else {
                distro
            },
        }
    }

    pub fn arch_family(&self) -> Arch {
        Arch::from_device(&format!(
            "{} {} {}",
            self.arch, self.pkg_arch, self.uname_arch
        ))
    }

    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("model".into(), Value::str(&self.model)),
            ("board_name".into(), Value::str(&self.board_name)),
            ("board_id".into(), Value::str(&self.board_id)),
            ("target".into(), Value::str(&self.target)),
            ("arch".into(), Value::str(&self.arch)),
            ("pkg_arch".into(), Value::str(&self.pkg_arch)),
            ("uname_arch".into(), Value::str(&self.uname_arch)),
            ("distro".into(), Value::str(&self.distro)),
            ("release".into(), Value::str(&self.release)),
            ("revision".into(), Value::str(&self.revision)),
            ("arch_family".into(), Value::str(self.arch_family().label())),
        ])
    }
}

fn first_non_empty(items: &[&String]) -> String {
    items
        .iter()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

fn read_trim(path: &str) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn read_board_json() -> (String, String) {
    let text = std::fs::read_to_string("/etc/board.json").unwrap_or_default();
    let val = match crate::json::parse(&text) {
        Ok(v) => v,
        Err(_) => return (String::new(), String::new()),
    };
    let model_obj = val.get("model");
    let id = model_obj
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = model_obj
        .and_then(|m| m.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (id, name)
}

fn read_ubus_board() -> (String, String, String, String) {
    let out = Command::new("ubus")
        .args(["call", "system", "board"])
        .output();
    let text = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return (String::new(), String::new(), String::new(), String::new()),
    };
    let val = match crate::json::parse(&text) {
        Ok(v) => v,
        Err(_) => return (String::new(), String::new(), String::new(), String::new()),
    };
    let grab = |path: &[&str]| -> String {
        let mut cur = &val;
        for key in path {
            match cur.get(key) {
                Some(next) => cur = next,
                None => return String::new(),
            }
        }
        cur.as_str().unwrap_or("").to_string()
    };
    (
        grab(&["board_name"]),
        grab(&["model"]),
        grab(&["release", "arch"]),
        grab(&["release", "version"]),
    )
}

fn read_release_info() -> (String, String, String, String, String) {
    let text = std::fs::read_to_string("/etc/openwrt_release").unwrap_or_default();
    let grab = |key: &str| -> String {
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix(&format!("{key}='")) {
                return rest.trim_end_matches('\'').to_string();
            }
            if let Some(rest) = line.strip_prefix(&format!("{key}=\"")) {
                return rest.trim_end_matches('"').to_string();
            }
        }
        String::new()
    };
    (
        grab("DISTRIB_ID"),
        grab("DISTRIB_RELEASE"),
        grab("DISTRIB_TARGET"),
        grab("DISTRIB_ARCH"),
        grab("DISTRIB_REVISION"),
    )
}

fn package_manager_arch() -> String {
    // apk-tools first, opkg second: both are generic package managers, the
    // only per-manager difference is the query command.
    for (cmd, args) in [
        ("apk", vec!["--print-arch"]),
        ("opkg", vec!["print-architecture"]),
    ] {
        let out = Command::new(cmd).args(&args).output();
        let text = match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => continue,
        };
        let mut found = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // `apk --print-arch` prints the arch alone; `opkg
            // print-architecture` prints `arch <name> <priority>`.
            let name = line.split_whitespace().nth(1).unwrap_or(line);
            if name == "all" || name == "noarch" {
                continue;
            }
            found = name.to_string();
        }
        if !found.is_empty() {
            return found;
        }
    }
    String::new()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Aarch64,
    X86,
    Mips,
    Arm32,
    Unknown,
}

impl Arch {
    pub fn label(self) -> &'static str {
        match self {
            Arch::Aarch64 => "aarch64",
            Arch::X86 => "x86_64",
            Arch::Mips => "mips",
            Arch::Arm32 => "arm32",
            Arch::Unknown => "unknown",
        }
    }

    pub fn from_device(s: &str) -> Arch {
        let s = s.to_ascii_lowercase();
        if s.contains("aarch64") || s.contains("arm64") {
            Arch::Aarch64
        } else if s.contains("x86_64")
            || s.contains("x86-64")
            || s.contains("amd64")
            || s.contains("i386")
            || s.contains("i686")
        {
            Arch::X86
        } else if s.contains("mips") {
            Arch::Mips
        } else if s.contains("armv7")
            || s.contains("armhf")
            || s.contains("cortex-a7")
            || s.contains("cortex-a9")
            || s.contains("mt7623")
            || s.contains("mt7629")
        {
            Arch::Arm32
        } else {
            Arch::Unknown
        }
    }
}

/// Platform / SoC / image-type vocabulary. These are chip and packaging words,
/// not device identifiers, so they are never evidence for a model match.
const NON_IDENTITY_TOKENS: &[&str] = &[
    // SoC / vendor / target platforms
    "mediatek",
    "mtk",
    "qualcomm",
    "qualcommax",
    "rockchip",
    "allwinner",
    "sunxi",
    "broadcom",
    "bcm",
    "realtek",
    "marvell",
    "amlogic",
    "lantiq",
    "ath79",
    "ramips",
    "filogic",
    "ipq",
    "layerscape",
    "nxp",
    "intel",
    "amd",
    "generic",
    "router",
    "wireless",
    "wifi",
    "wlan",
    // architectures
    "aarch64",
    "arm64",
    "arm",
    "armv7",
    "armhf",
    "cortex",
    "cortex-a7",
    "cortex-a9",
    "cortex-a53",
    "cortex-a55",
    "cortex-a72",
    "mips",
    "mipsel",
    "mips64",
    "x86",
    "x86-64",
    "x86_64",
    "amd64",
    "i386",
    "i686",
    "mt7620",
    "mt7621",
    "mt7622",
    "mt7623",
    "mt7628",
    "mt7629",
    "mt7688",
    "mt7981",
    "mt7985",
    "mt7986",
    "mt7987",
    "mt7988",
    "rk3399",
    "rk3568",
    "rk3588",
    "ipq6010",
    "ipq6018",
    "ipq807x",
    "ipq9574",
    "bcm2711",
    "bcm2712",
    "cn913x",
    "24kc",
    "74kc",
    "1004kc",
    // distro / release words
    "openwrt",
    "immortalwrt",
    "lede",
    "snapshot",
    "stable",
    "release",
    "r",
    "rc",
    "beta",
    "alpha",
    "pre",
    "build",
    "builds",
    "test",
    "debug",
    "official",
    "self",
    "custom",
    "diyp",
    // image / packaging words
    "sysupgrade",
    "factory",
    "initramfs",
    "squashfs",
    "ext4",
    "combined",
    "efi",
    "nand",
    "nor",
    "emmc",
    "sdcard",
    "ubi",
    "tftp",
    "kernel",
    "rootfs",
    "firmware",
    "image",
    "img",
    "bin",
    "gz",
    "tar",
    "xz",
    "zip",
    "full",
    "mini",
    "lite",
    "pro",
    "plus",
    "max",
    "v1",
    "v2",
    "v3",
    "v4",
];

fn norm(s: &str) -> String {
    let mut out = String::new();
    let mut sep = false;
    for ch in s.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            sep = false;
        } else if !sep && !out.is_empty() {
            out.push('-');
            sep = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn is_identity_token(t: &str) -> bool {
    if t.len() < 3 || NON_IDENTITY_TOKENS.contains(&t) {
        return false;
    }
    // pure numbers, version-ish fragments and two-letter glue are not identities
    if t.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if t.len() <= 2 {
        return false;
    }
    true
}

fn is_specific_token(t: &str) -> bool {
    // A token carrying both a digit and a letter (h5000m, ax3000t, r4s) is
    // specific enough to identify a device on its own.
    t.chars().any(|c| c.is_ascii_digit()) && t.chars().any(|c| c.is_ascii_alphabetic())
}

impl DeviceProfile {
    pub fn identity_strings(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in [&self.board_id, &self.board_name, &self.model] {
            let n = norm(s);
            if !n.is_empty() && !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }

    pub fn identity_tokens(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for ident in self.identity_strings() {
            for tok in ident.split('-') {
                if is_identity_token(tok) {
                    set.insert(tok.to_string());
                }
            }
        }
        set.into_iter().collect()
    }
}

fn asset_arch(fname_norm: &str) -> (Arch, Vec<String>) {
    const AARCH64: &[&str] = &[
        "aarch64",
        "arm64",
        "cortex-a53",
        "cortex-a55",
        "cortex-a72",
        "filogic",
        "mt7622",
        "mt7981",
        "mt7985",
        "mt7986",
        "mt7987",
        "mt7988",
        "rk3568",
        "rk3588",
        "rk3399",
        "ipq6010",
        "ipq6018",
        "ipq807x",
        "ipq9574",
        "bcm2711",
        "bcm2712",
        "cn913x",
    ];
    const X86: &[&str] = &["x86", "x86-64", "x86_64", "amd64", "i386", "i686"];
    const MIPS: &[&str] = &[
        "mipsel", "mips", "mips64", "mt7620", "mt7621", "mt7628", "mt7688", "24kc", "74kc",
        "1004kc",
    ];
    const ARM32: &[&str] = &[
        "armv7",
        "armhf",
        "cortex-a7",
        "cortex-a9",
        "mt7623",
        "mt7629",
    ];

    let mut evidence = Vec::new();
    let mut found: Option<Arch> = None;
    for (arch, list) in [
        (Arch::Aarch64, AARCH64),
        (Arch::X86, X86),
        (Arch::Mips, MIPS),
        (Arch::Arm32, ARM32),
    ] {
        for marker in list {
            let hit = if marker.contains('-') || marker.contains('_') {
                fname_norm.contains(marker) || fname_norm.replace('-', "_").contains(marker)
            } else {
                fname_norm.split('-').any(|t| t == *marker)
                    || fname_norm.contains(&format!("-{marker}-"))
                    || fname_norm.starts_with(&format!("{marker}-"))
            };
            if hit {
                if found.is_none() {
                    found = Some(arch);
                }
                if !evidence.contains(&marker.to_string()) {
                    evidence.push(marker.to_string());
                }
            }
        }
    }
    (found.unwrap_or(Arch::Unknown), evidence)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatLevel {
    Compatible,
    Warning,
    Unknown,
    Incompatible,
}

#[derive(Debug, Clone)]
pub struct CompatResult {
    pub level: CompatLevel,
    pub can_upgrade: bool,
    pub score: i32,
    pub badge: String,
    pub reason: String,
    pub detected_arch: String,
    pub detected_model: String,
    /// ok | conflict | undeclared | unknown
    pub arch_check: &'static str,
    /// exact | partial | rule | unmatched
    pub model_check: &'static str,
    pub requires_confirm: bool,
    pub basis: Vec<String>,
    pub matched_rule: Option<String>,
    pub device_arch: String,
    pub asset_arch: String,
}

impl CompatResult {
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            (
                "level".into(),
                Value::str(match self.level {
                    CompatLevel::Compatible => "compatible",
                    CompatLevel::Warning => "warning",
                    CompatLevel::Unknown => "unknown",
                    CompatLevel::Incompatible => "incompatible",
                }),
            ),
            ("can_upgrade".into(), Value::Bool(self.can_upgrade)),
            (
                "requires_confirm".into(),
                Value::Bool(self.requires_confirm),
            ),
            ("score".into(), Value::num(self.score.max(0) as u64)),
            ("badge".into(), Value::str(&self.badge)),
            ("reason".into(), Value::str(&self.reason)),
            ("detected_arch".into(), Value::str(&self.detected_arch)),
            ("detected_model".into(), Value::str(&self.detected_model)),
            (
                "checks".into(),
                Value::Obj(vec![
                    ("arch".into(), Value::str(self.arch_check)),
                    ("model".into(), Value::str(self.model_check)),
                ]),
            ),
            (
                "basis".into(),
                Value::Arr(self.basis.iter().map(|s| Value::str(s)).collect()),
            ),
            (
                "matched_rule".into(),
                match &self.matched_rule {
                    Some(r) => Value::str(r),
                    None => Value::Null,
                },
            ),
            ("device_arch".into(), Value::str(&self.device_arch)),
            ("asset_arch".into(), Value::str(&self.asset_arch)),
        ])
    }
}

pub fn evaluate_asset(device: &DeviceProfile, filename: &str) -> CompatResult {
    evaluate(device, filename, None)
}

/// Evaluate with an explicit user rule that already matched this file name.
pub fn evaluate_with_rule(device: &DeviceProfile, filename: &str, rule: &str) -> CompatResult {
    evaluate(device, filename, Some(rule))
}

fn evaluate(device: &DeviceProfile, filename: &str, rule: Option<&str>) -> CompatResult {
    let fname = norm(filename);
    let fname_tokens: BTreeSet<String> = fname
        .split('-')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();

    let dev_arch = device.arch_family();
    let (asset_arch, arch_evidence) = asset_arch(&fname);
    let device_arch_text = if device.arch.is_empty() {
        format!("{}({})", device.uname_arch, device.pkg_arch)
    } else {
        device.arch.clone()
    };

    let mut basis: Vec<String> = Vec::new();
    basis.push(format!(
        "本机身份：board_name=[{}]，model=[{}]，board_id=[{}]",
        device.board_name, device.model, device.board_id
    ));

    let (arch_check, arch_reason) = match (dev_arch, asset_arch) {
        (Arch::Unknown, _) => (
            "unknown",
            format!(
                "本机架构无法确认（DISTRIB_ARCH=[{}]，pkg arch=[{}]，uname=[{}]）",
                device.arch, device.pkg_arch, device.uname_arch
            ),
        ),
        (_, Arch::Unknown) => ("undeclared", "文件名未声明架构，无法比对".to_string()),
        (d, a) if d == a => (
            "ok",
            format!(
                "架构一致：文件名声明 [{}]（命中 {}），本机为 [{}]",
                a.label(),
                arch_evidence.join(", "),
                device_arch_text
            ),
        ),
        (d, a) => (
            "conflict",
            format!(
                "架构冲突：文件名声明 [{}]（命中 {}），本机为 [{} / {}]",
                a.label(),
                arch_evidence.join(", "),
                d.label(),
                device_arch_text
            ),
        ),
    };
    basis.push(arch_reason.clone());

    let arch_score = match arch_check {
        "ok" => 0,
        "undeclared" => -30,
        "unknown" => -40,
        _ => -100,
    };

    if arch_check == "conflict" {
        return CompatResult {
            level: CompatLevel::Incompatible,
            can_upgrade: false,
            score: -100,
            badge: "架构冲突·严禁刷入".into(),
            reason: format!(
                "架构冲突：该固件为 [{}]，本机为 [{}]。刷入不兼容架构会直接变砖，已禁止升级。",
                asset_arch.label(),
                dev_arch.label()
            ),
            detected_arch: asset_arch.label().into(),
            detected_model: String::new(),
            arch_check,
            model_check: "unmatched",
            requires_confirm: false,
            basis,
            matched_rule: None,
            device_arch: dev_arch.label().into(),
            asset_arch: asset_arch.label().into(),
        };
    }

    // Explicit user rule: the operator stated this file is for this device, so a
    // rule hit counts as a match ("自用构建" fallback). Architecture conflict has
    // already returned above — that is positive evidence of a mismatch and must
    // not be overridable by a regex. A file name without any architecture word is
    // only "undeclared", never "generic": it is allowed solely because the
    // operator asserted it, and the basis records that fact.
    if let Some(rule) = rule {
        let mut base = 90 + arch_score;
        if fname.contains("sysupgrade") || fname.contains("combined") {
            base += 10;
        }
        let reason = match arch_check {
            "ok" => format!(
                "配置规则 [{}] 命中该文件，且架构一致（{}），视为本机可用",
                rule,
                dev_arch.label()
            ),
            "unknown" => format!(
                "配置规则 [{}] 命中该文件；本机架构不可读（{}），按配置规则判为可用，请核对依据",
                rule, device_arch_text
            ),
            _ => format!(
                "配置规则 [{}] 命中该文件；资产名未声明架构（{}），按配置规则判为可用，请核对依据",
                rule, device_arch_text
            ),
        };
        let badge = match arch_check {
            "ok" => "配置规则匹配",
            "unknown" => "配置规则匹配·架构未知",
            _ => "配置规则匹配·架构未声明",
        };
        basis.push(format!("配置规则 firmware_pattern 命中：{}", rule));
        basis.push(format!("判定依据：{}", reason));
        return CompatResult {
            level: CompatLevel::Compatible,
            can_upgrade: true,
            score: base,
            badge: badge.into(),
            reason,
            detected_arch: if asset_arch == Arch::Unknown {
                "未声明".into()
            } else {
                asset_arch.label().into()
            },
            detected_model: rule.to_string(),
            arch_check,
            model_check: "rule",
            requires_confirm: false,
            basis,
            matched_rule: Some(rule.to_string()),
            device_arch: dev_arch.label().into(),
            asset_arch: asset_arch.label().into(),
        };
    }

    // Structural model comparison.
    let identities = device.identity_strings();
    let tokens = device.identity_tokens();
    let mut model_check = "unmatched";
    let mut detected_model = String::new();
    let model_reason: String;

    if let Some(full) = identities.iter().find(|id| fname.contains(id.as_str())) {
        model_check = "exact";
        detected_model = full.clone();
        model_reason = format!("机型一致：文件名包含完整本机标识 [{}]", full);
    } else {
        let matched: Vec<&String> = tokens
            .iter()
            .filter(|t| fname_tokens.contains(t.as_str()))
            .collect();
        if matched.len() >= 2 {
            model_check = "exact";
            detected_model = matched
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(",");
            model_reason = format!(
                "机型一致：文件名同时命中本机标识 [{}]（{} 个独立标识）",
                detected_model,
                matched.len()
            );
        } else if matched.len() == 1 {
            let t = matched[0];
            if is_specific_token(t) {
                model_check = "exact";
                detected_model = t.clone();
                model_reason = format!("机型一致：文件名命中本机专属标识 [{}]", t);
            } else {
                model_check = "partial";
                detected_model = t.clone();
                model_reason = format!("机型部分匹配：文件名仅命中通用词 [{}]，不足以确认机型", t);
            }
        } else {
            model_reason = format!(
                "机型未匹配：本机标识候选 [{}] 均未出现在文件名中",
                tokens.join(", ")
            );
        }
    }
    basis.push(model_reason.clone());

    let image_hint_bad = fname.contains("factory") || fname.contains("initramfs");
    let (level, can_upgrade, requires_confirm, badge, reason, score) = match (arch_check, model_check) {
        ("ok", "exact") if image_hint_bad => (
            CompatLevel::Warning,
            false,
            true,
            "镜像类型不适用",
            "该文件为 factory/initramfs 镜像，不是 sysupgrade 在线升级包，请选择 sysupgrade 镜像".to_string(),
            45,
        ),
        ("ok", "exact") => {
            let extra = if fname.contains("sysupgrade") || fname.contains("combined") {
                20
            } else {
                0
            };
            (
                CompatLevel::Compatible,
                true,
                false,
                "机型与架构匹配",
                format!(
                    "机型与架构均匹配（{} / {}），可正常刷写",
                    device.model,
                    dev_arch.label()
                ),
                100 + extra,
            )
        }
        ("ok", "partial") => (
            CompatLevel::Warning,
            false,
            true,
            "需人工确认",
            format!(
                "仅部分机型标识命中，无法确认该固件是否为 [{}] 构建；确认无误后再强制升级",
                device.model
            ),
            55 + arch_score,
        ),
        ("ok", _) => (
            CompatLevel::Unknown,
            false,
            true,
            "无法确认是否适配",
            format!(
                "架构一致，但文件名未包含本机机型标识 [{}]，无法确认是否适配；请配置 firmware_pattern 或手动确认",
                device.model
            ),
            25,
        ),
        (_, "exact") => (
            CompatLevel::Warning,
            false,
            true,
            "机型匹配·架构未声明",
            format!(
                "文件名命中本机机型，但未声明架构（本机 {}），无法自动确认架构适配",
                dev_arch.label()
            ),
            60 + arch_score,
        ),
        _ => (
            CompatLevel::Unknown,
            false,
            true,
            "无法确认是否适配",
            "机型与架构均无法确认，禁止自动升级；如需升级请先配置 firmware_pattern 或手动指定文件"
                .to_string(),
            15 + arch_score,
        ),
    };

    CompatResult {
        level,
        can_upgrade,
        score,
        badge: badge.into(),
        reason,
        detected_arch: if asset_arch == Arch::Unknown {
            "未声明".into()
        } else {
            asset_arch.label().into()
        },
        detected_model,
        arch_check,
        model_check,
        requires_confirm,
        basis,
        matched_rule: None,
        device_arch: dev_arch.label().into(),
        asset_arch: asset_arch.label().into(),
    }
}

pub fn verify_downloaded_image(device: &DeviceProfile, path: &str) -> Result<(), String> {
    // 1. fwtool metadata: authoritative supported device list. Only enforced
    // when the image really carries metadata; compressed images yield no
    // metadata here and are covered by sysupgrade -T below.
    if which("fwtool") {
        let meta_tmp = "/tmp/.fwtool-meta.json";
        let _ = std::fs::remove_file(meta_tmp);
        let out = Command::new("fwtool").args(["-i", meta_tmp, path]).output();
        if let Ok(o) = out {
            if o.status.success() {
                if let Ok(content) = std::fs::read_to_string(meta_tmp) {
                    if let Ok(val) = crate::json::parse(&content) {
                        if let Some(devs) = val.get("supported_devices").and_then(|d| d.as_arr()) {
                            let supported: Vec<String> = devs
                                .iter()
                                .filter_map(|d| d.as_str().map(|s| s.to_string()))
                                .collect();
                            if !supported.is_empty() && !device.board_name.is_empty() {
                                let hit =
                                    supported.iter().any(|s| board_match(s, &device.board_name));
                                crate::logline!(
                                    "[compat] 固件元数据支持机型: {}, 本机机型: {}",
                                    supported.join(", "),
                                    device.board_name
                                );
                                if !hit {
                                    let _ = std::fs::remove_file(meta_tmp);
                                    return Err(format!(
                                        "固件内部机型元数据不支持本机 [{}]（支持：{}），已终止升级",
                                        device.board_name,
                                        supported.join(", ")
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_file(meta_tmp);
    }

    // 2. sysupgrade -T (read-only image test; never flashes).
    if which("sysupgrade") {
        crate::logline!("[compat] 执行系统底层固件兼容性预检 (sysupgrade -T)...");
        let output = Command::new("sysupgrade").args(["-T", path]).output();
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if !out.status.success() {
                    let err_detail = if !stderr.is_empty() { stderr } else { stdout };
                    let msg = format!(
                        "系统内核级固件兼容性预检未通过 (sysupgrade -T 拒绝): {}",
                        err_detail.replace('\n', "; ")
                    );
                    crate::logline!("[compat] {}", msg);
                    return Err(msg);
                } else {
                    let first_line = stdout.lines().next().unwrap_or("兼容性测试通过");
                    crate::logline!("[compat] sysupgrade -T 测试通过: {}", first_line);
                }
            }
            Err(e) => {
                crate::logline!("[compat] 无法执行 sysupgrade -T: {e}");
            }
        }
    }

    Ok(())
}

fn board_match(a: &str, b: &str) -> bool {
    let na = norm(a);
    let nb = norm(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    na == nb || na.contains(&nb) || nb.contains(&na)
}

fn which(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1 && echo y")])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "y")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev() -> DeviceProfile {
        DeviceProfile {
            model: "Hiveton H5000M".into(),
            board_name: "hiveton,h5000m".into(),
            board_id: "hiveton,h5000m".into(),
            target: "mediatek/filogic".into(),
            arch: "aarch64_cortex-a53".into(),
            pkg_arch: "aarch64".into(),
            uname_arch: "aarch64".into(),
            release: "SNAPSHOT".into(),
            revision: "r0".into(),
            distro: "ImmortalWRT".into(),
        }
    }

    #[test]
    fn platform_word_does_not_match() {
        // mt7621 is a platform word: it must never turn an unrelated asset into a match.
        let r = evaluate_asset(
            &dev(),
            "openwrt-ramips-mt7621-xiaomi_mi-router-4a-gigabit-squashfs-sysupgrade.bin",
        );
        assert_ne!(r.level, CompatLevel::Compatible);
        assert_eq!(r.model_check, "unmatched");
        assert!(!r.can_upgrade);
    }

    #[test]
    fn exact_model_and_arch_match() {
        let r = evaluate_asset(
            &dev(),
            "ImmortalWRT-25.12.0-mediatek-filogic-hiveton_h5000m-squashfs-sysupgrade.bin",
        );
        assert_eq!(r.level, CompatLevel::Compatible);
        assert_eq!(r.model_check, "exact");
        assert_eq!(r.arch_check, "ok");
    }

    #[test]
    fn token_only_name_matches_model_but_arch_is_undeclared() {
        let r = evaluate_asset(&dev(), "H5000M-sysupgrade-25.12.0.bin");
        assert_eq!(r.model_check, "exact");
        // no architecture word in the name: never auto-approved
        assert_eq!(r.arch_check, "undeclared");
        assert_eq!(r.level, CompatLevel::Warning);
        assert!(!r.can_upgrade);
        assert!(r.requires_confirm);
    }

    #[test]
    fn missing_arch_is_not_generic() {
        let r = evaluate_asset(&dev(), "H5000M-sysupgrade.bin");
        assert_eq!(r.arch_check, "undeclared");
        assert_eq!(r.level, CompatLevel::Warning);
        assert!(!r.can_upgrade);
    }

    #[test]
    fn wrong_arch_blocks_even_with_rule() {
        let r = evaluate_with_rule(&dev(), "x86-64-generic-squashfs-combined.img.gz", "^.*$");
        assert_eq!(r.level, CompatLevel::Incompatible);
        assert!(!r.can_upgrade);
    }

    #[test]
    fn rule_gives_explicit_basis() {
        let r = evaluate_with_rule(
            &dev(),
            "my-private-build-26.11.07.bin",
            "^my-private-build-",
        );
        assert_eq!(r.model_check, "rule");
        assert_eq!(r.matched_rule.as_deref(), Some("^my-private-build-"));
        assert!(r.basis.iter().any(|b| b.contains("firmware_pattern")));
    }

    #[test]
    fn self_build_rule_hit_is_usable_even_without_arch_word() {
        // 「自用构建」兜底：文件名不含机型也不含架构，只有用户配置的规则能认出来。
        // 命中规则即视为本机可用，但依据必须写清楚「资产未声明架构」。
        let r = evaluate_with_rule(
            &dev(),
            "my-private-build-26.11.07.bin",
            "^my-private-build-",
        );
        assert_eq!(r.arch_check, "undeclared");
        assert_eq!(r.level, CompatLevel::Compatible);
        assert!(r.can_upgrade);
        assert!(!r.requires_confirm);
        assert!(r.basis.iter().any(|b| b.contains("未声明架构")));
        assert!(r.reason.contains("配置规则"));
    }

    #[test]
    fn vendor_only_name_is_unknown() {
        let r = evaluate_asset(&dev(), "hiveton-custom-image.bin");
        // hiveton is an identity token but not specific (no digit) -> partial
        assert_eq!(r.model_check, "partial");
        assert!(!r.can_upgrade);
    }
}
