// Device architecture and board model compatibility evaluation.
// Automatically determines whether an online firmware is safe to flash on this router.

use crate::json::Value;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct DeviceProfile {
    pub model: String,
    pub board_name: String,
    pub target: String,
    pub arch: String,
    pub uname_arch: String,
    pub release: String,
    pub distro: String,
}

impl DeviceProfile {
    pub fn current() -> Self {
        let model = std::fs::read_to_string("/tmp/sysinfo/model")
            .unwrap_or_default()
            .trim()
            .to_string();

        let board_name = std::fs::read_to_string("/tmp/sysinfo/board_name")
            .unwrap_or_default()
            .trim()
            .to_string();

        let (distro, release, target, arch) = read_release_info();

        let uname_arch = Command::new("uname")
            .arg("-m")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| std::env::consts::ARCH.to_string());

        Self {
            model: if model.is_empty() { "Generic Router".into() } else { model },
            board_name,
            target,
            arch,
            uname_arch,
            release,
            distro: if distro.is_empty() { "OpenWrt".into() } else { distro },
        }
    }

    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("model".into(), Value::str(&self.model)),
            ("board_name".into(), Value::str(&self.board_name)),
            ("target".into(), Value::str(&self.target)),
            ("arch".into(), Value::str(&self.arch)),
            ("uname_arch".into(), Value::str(&self.uname_arch)),
            ("distro".into(), Value::str(&self.distro)),
            ("release".into(), Value::str(&self.release)),
        ])
    }
}

fn read_release_info() -> (String, String, String, String) {
    let text = std::fs::read_to_string("/etc/openwrt_release").unwrap_or_default();
    let grab = |key: &str| -> String {
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix(&format!("{key}='")) {
                return rest.trim_end_matches('\'').to_string();
            }
        }
        String::new()
    };
    let distro = grab("DISTRIB_ID");
    let release = grab("DISTRIB_RELEASE");
    let target = grab("DISTRIB_TARGET");
    let arch = grab("DISTRIB_ARCH");
    (distro, release, target, arch)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatLevel {
    Compatible,
    Warning,
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
}

impl CompatResult {
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            (
                "level".into(),
                Value::str(match self.level {
                    CompatLevel::Compatible => "compatible",
                    CompatLevel::Warning => "warning",
                    CompatLevel::Incompatible => "incompatible",
                }),
            ),
            ("can_upgrade".into(), Value::Bool(self.can_upgrade)),
            ("score".into(), Value::num(self.score.max(0) as u64)),
            ("badge".into(), Value::str(&self.badge)),
            ("reason".into(), Value::str(&self.reason)),
            ("detected_arch".into(), Value::str(&self.detected_arch)),
            ("detected_model".into(), Value::str(&self.detected_model)),
        ])
    }
}

const KNOWN_DEVICE_SIGNATURES: &[(&str, &[&str])] = &[
    ("h5000m", &["h5000m", "hiveton"]),
    ("nx30-pro", &["nx30-pro", "nx30pro", "nx30_pro"]),
    ("ax3000t", &["ax3000t"]),
    ("wr30u", &["wr30u"]),
    ("ax6000", &["ax6000", "redmi-ax6000", "redmi_ax6000"]),
    ("360-t7", &["360-t7", "360_t7", "qihoo-t7"]),
    ("gl-mt3000", &["gl-mt3000", "gl_mt3000", "mt3000"]),
    ("gl-mt2500", &["gl-mt2500", "gl_mt2500", "mt2500"]),
    ("gl-ax1800", &["gl-ax1800", "gl_ax1800", "ax1800"]),
    ("gl-axt1800", &["gl-axt1800", "gl_axt1800", "axt1800"]),
    ("e2633", &["e2633", "e2633np"]),
    ("ra82", &["ra82"]),
    ("komi-a31", &["komi-a31", "a31"]),
    ("cmiot-ax18", &["cmiot-ax18", "cmiot_ax18", "ax18"]),
    ("r30b1", &["r30b1"]),
    ("ct3003", &["ct3003"]),
    ("rax3000m", &["rax3000m"]),
    ("re-cp-03", &["re-cp-03", "re_cp_03"]),
    ("nanopi-r2s", &["nanopi-r2s", "nanopi_r2s", "r2s"]),
    ("nanopi-r4s", &["nanopi-r4s", "nanopi_r4s", "r4s"]),
    ("nanopi-r5s", &["nanopi-r5s", "nanopi_r5s", "r5s"]),
    ("nanopi-r6s", &["nanopi-r6s", "nanopi_r6s", "r6s"]),
    ("fastrhino-r68s", &["r68s", "fastrhino-r68s"]),
    ("t68m", &["t68m"]),
    ("h68k", &["h68k"]),
    ("k2p", &["k2p"]),
    ("newifi3", &["newifi3", "d2"]),
    ("mir3g", &["mir3g"]),
    ("mir4a", &["mir4a"]),
    (
        "x86-generic",
        &[
            "x86-64-generic",
            "x86_64-generic",
            "generic-squashfs-combined",
            "generic-ext4-combined",
        ],
    ),
];

pub fn evaluate_asset(device: &DeviceProfile, filename: &str) -> CompatResult {
    let fname_lower = filename.to_ascii_lowercase();
    let dev_arch_lower = device.arch.to_ascii_lowercase();
    let dev_uname_lower = device.uname_arch.to_ascii_lowercase();
    let dev_target_lower = device.target.to_ascii_lowercase();
    let dev_board_lower = device.board_name.to_ascii_lowercase();
    let dev_model_lower = device.model.to_ascii_lowercase();

    let is_dev_arm64 = dev_arch_lower.contains("aarch64")
        || dev_arch_lower.contains("arm64")
        || dev_uname_lower == "aarch64";

    let is_dev_x86_64 = dev_arch_lower.contains("x86_64")
        || dev_uname_lower == "x86_64"
        || dev_target_lower.contains("x86");

    let is_dev_mips = dev_arch_lower.contains("mips")
        || dev_uname_lower.starts_with("mips")
        || dev_target_lower.contains("ramips");

    // 1. Architecture analysis
    let has_x86 = fname_lower.contains("x86_64")
        || fname_lower.contains("x86-64")
        || fname_lower.contains("amd64")
        || fname_lower.contains("i386")
        || fname_lower.contains("i686")
        || fname_lower.contains("-x86-")
        || fname_lower.contains("_x86_");

    let has_arm64 = fname_lower.contains("aarch64")
        || fname_lower.contains("arm64")
        || fname_lower.contains("filogic")
        || fname_lower.contains("mt7981")
        || fname_lower.contains("mt7986")
        || fname_lower.contains("mt7988")
        || fname_lower.contains("rk3568")
        || fname_lower.contains("rk3588")
        || fname_lower.contains("cortex-a53")
        || fname_lower.contains("cortex-a72");

    let has_mips = fname_lower.contains("mipsel")
        || fname_lower.contains("mips_")
        || fname_lower.contains("mt7621")
        || fname_lower.contains("24kc");

    if is_dev_arm64 && has_x86 {
        return CompatResult {
            level: CompatLevel::Incompatible,
            can_upgrade: false,
            score: -100,
            badge: "架构冲突·严禁刷入".into(),
            reason: format!(
                "严重错误：本机为 aarch64(ARM64)，该固件为 x86_64 镜像，强行刷入将导致设备完全变砖！"
            ),
            detected_arch: "x86_64".into(),
            detected_model: String::new(),
        };
    }

    if is_dev_arm64 && has_mips {
        return CompatResult {
            level: CompatLevel::Incompatible,
            can_upgrade: false,
            score: -100,
            badge: "架构冲突·严禁刷入".into(),
            reason: "严重错误：本机为 aarch64(ARM64)，该固件为 MIPS 架构，强行刷入将导致设备变砖！".into(),
            detected_arch: "mips".into(),
            detected_model: String::new(),
        };
    }

    if is_dev_x86_64 && (has_arm64 || has_mips) {
        return CompatResult {
            level: CompatLevel::Incompatible,
            can_upgrade: false,
            score: -100,
            badge: "架构冲突·严禁刷入".into(),
            reason: "严重错误：本机为 x86_64 软路由，该固件为嵌入式 ARM/MIPS 固件，无法在 x86 平台运行！".into(),
            detected_arch: if has_arm64 { "aarch64".into() } else { "mips".into() },
            detected_model: String::new(),
        };
    }

    if is_dev_mips && (has_x86 || has_arm64) {
        return CompatResult {
            level: CompatLevel::Incompatible,
            can_upgrade: false,
            score: -100,
            badge: "架构冲突·严禁刷入".into(),
            reason: "严重错误：本机为 MIPS 芯片，该固件为其他架构，无法适配！".into(),
            detected_arch: if has_x86 { "x86".into() } else { "arm".into() },
            detected_model: String::new(),
        };
    }

    // 2. Chip Target Platform analysis
    if dev_target_lower.contains("filogic") {
        if fname_lower.contains("rockchip")
            || fname_lower.contains("qualcommax")
            || fname_lower.contains("ipq")
            || fname_lower.contains("bcm27")
            || fname_lower.contains("sunxi")
        {
            return CompatResult {
                level: CompatLevel::Incompatible,
                can_upgrade: false,
                score: -80,
                badge: "芯片平台冲突".into(),
                reason: "目标平台不符：当前设备芯片为 MediaTek Filogic，该固件针对其他芯片组平台！".into(),
                detected_arch: "aarch64".into(),
                detected_model: String::new(),
            };
        }
    }

    // 3. Board Model Analysis
    let current_matched_sig = KNOWN_DEVICE_SIGNATURES.iter().find(|(_, keys)| {
        keys.iter().any(|k| {
            dev_board_lower.contains(k)
                || dev_model_lower.contains(k)
                || (k.contains("x86") && is_dev_x86_64)
        })
    });

    if let Some((curr_tag, curr_keys)) = current_matched_sig {
        // A known device signature was detected on the current router!
        // Check if filename has a conflicting device signature:
        for (other_tag, other_keys) in KNOWN_DEVICE_SIGNATURES {
            if other_tag == curr_tag {
                continue;
            }
            let is_conflict = other_keys.iter().any(|k| fname_lower.contains(k));
            if is_conflict {
                return CompatResult {
                    level: CompatLevel::Incompatible,
                    can_upgrade: false,
                    score: -90,
                    badge: "机型不符·禁止刷入".into(),
                    reason: format!(
                        "机型不匹配：固件针对的是 [{other_tag}]，而当前路由器为 [{}]，硬件驱动与分区不兼容，严禁刷入！",
                        device.model
                    ),
                    detected_arch: device.uname_arch.clone(),
                    detected_model: other_tag.to_string(),
                };
            }
        }

        // Check if filename matches our own keys:
        let exact_match = curr_keys.iter().any(|k| fname_lower.contains(k));
        if exact_match {
            let mut score = 100;
            if fname_lower.contains("sysupgrade") || fname_lower.contains("combined") {
                score += 20;
            }
            return CompatResult {
                level: CompatLevel::Compatible,
                can_upgrade: true,
                score,
                badge: "完美适配".into(),
                reason: format!(
                    "设备机型与硬件架构完全匹配 ({} / {})，可安全刷写",
                    device.model, device.uname_arch
                ),
                detected_arch: device.uname_arch.clone(),
                detected_model: curr_tag.to_string(),
            };
        }
    } else {
        // The router's board_name isn't in our curated signature list.
        // Fallback: check direct token matches from board_name and model.
        let tokens: Vec<&str> = dev_board_lower
            .split(&[',', '-', '_', ' '][..])
            .chain(dev_model_lower.split(&[',', '-', '_', ' '][..]))
            .filter(|t| t.len() >= 3 && *t != "generic" && *t != "router" && *t != "wireless")
            .collect();

        if let Some(matched_token) = tokens.iter().find(|t| fname_lower.contains(**t)) {
            let mut score = 80;
            if fname_lower.contains("sysupgrade") || fname_lower.contains("combined") {
                score += 20;
            }
            return CompatResult {
                level: CompatLevel::Compatible,
                can_upgrade: true,
                score,
                badge: "机型匹配".into(),
                reason: format!(
                    "固件名称匹配本机硬件标识 [{} ({})]，可正常刷写",
                    matched_token, device.uname_arch
                ),
                detected_arch: device.uname_arch.clone(),
                detected_model: matched_token.to_string(),
            };
        }
    }

    // 4. x86 Generic Case
    if is_dev_x86_64 && (fname_lower.contains("generic") || fname_lower.contains("combined")) {
        return CompatResult {
            level: CompatLevel::Compatible,
            can_upgrade: true,
            score: 95,
            badge: "完美适配".into(),
            reason: "检测为 x86_64 通用固件镜像，与本机完全兼容".into(),
            detected_arch: "x86_64".into(),
            detected_model: "x86-generic".into(),
        };
    }

    // 5. Image type warnings
    if fname_lower.contains("factory") {
        return CompatResult {
            level: CompatLevel::Warning,
            can_upgrade: true,
            score: 30,
            badge: "Factory固件·需注意".into(),
            reason: "该固件包含 factory 标识（通常用于原厂Web直刷或救砖），在线升级推荐使用 sysupgrade 固件".into(),
            detected_arch: device.uname_arch.clone(),
            detected_model: String::new(),
        };
    }

    if fname_lower.contains("initramfs") {
        return CompatResult {
            level: CompatLevel::Warning,
            can_upgrade: true,
            score: 20,
            badge: "内存镜像·需注意".into(),
            reason: "该文件为 initramfs 临时引导镜像，重启后不保存配置，通常请选择 sysupgrade 固件".into(),
            detected_arch: device.uname_arch.clone(),
            detected_model: String::new(),
        };
    }

    // 6. Generic Warning (Architecture/Target matches, but model is ambiguous)
    let score = if fname_lower.contains("sysupgrade") || fname_lower.contains("combined") {
        55
    } else {
        40
    };

    CompatResult {
        level: CompatLevel::Warning,
        can_upgrade: true,
        score,
        badge: "需人工核对".into(),
        reason: format!(
            "基础架构符合({})，但文件名未包含明确的设备专属标识，请确认该固件确系为 [{}] 构建后刷入",
            device.uname_arch, device.model
        ),
        detected_arch: device.uname_arch.clone(),
        detected_model: String::new(),
    }
}

pub fn verify_downloaded_image(device: &DeviceProfile, path: &str) -> Result<(), String> {
    // 1. fwtool metadata check
    if which("fwtool") {
        let meta_tmp = "/tmp/.fwtool-meta.json";
        let _ = std::fs::remove_file(meta_tmp);
        let out = Command::new("fwtool")
            .args(["-i", meta_tmp, path])
            .output();
        if let Ok(o) = out {
            if o.status.success() {
                if let Ok(content) = std::fs::read_to_string(meta_tmp) {
                    if let Ok(val) = crate::json::parse(&content) {
                        if let Some(devs) = val.get("supported_devices").and_then(|d| d.as_arr()) {
                            let match_board = devs.iter().any(|d| {
                                d.as_str()
                                    .map(|s| {
                                        s == device.board_name
                                            || device.board_name.contains(s)
                                            || s.contains(&device.board_name)
                                    })
                                    .unwrap_or(false)
                            });
                            if !match_board && !device.board_name.is_empty() && !devs.is_empty() {
                                let supported_list: Vec<&str> =
                                    devs.iter().filter_map(|d| d.as_str()).collect();
                                crate::logline!(
                                    "[compat] ℹ️ 固件内部元数据支持机型: {:?}, 当前设备机型: {}",
                                    supported_list,
                                    device.board_name
                                );
                            }
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_file(meta_tmp);
    }

    // 2. sysupgrade -T
    if which("sysupgrade") {
        crate::logline!("[compat] 执行系统底层固件兼容性预检 (sysupgrade -T)...");
        let output = Command::new("sysupgrade")
            .args(["-T", path])
            .output();
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
                    crate::logline!("[compat] ❌ {}", msg);
                    return Err(msg);
                } else {
                    let first_line = stdout.lines().next().unwrap_or("兼容性测试通过");
                    crate::logline!("[compat] ✅ sysupgrade -T 测试通过: {}", first_line);
                }
            }
            Err(e) => {
                crate::logline!("[compat] ⚠️ 无法执行 sysupgrade -T: {e}");
            }
        }
    }

    Ok(())
}

fn which(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1 && echo y")])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "y")
        .unwrap_or(false)
}
