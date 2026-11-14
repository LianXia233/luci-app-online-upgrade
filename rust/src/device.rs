// 通用设备信息采集与标准化（阶段 2）。
//
// 原则：
// 1. 全部信息来自本机自报，不维护机型表、不针对任何设备写死逻辑。
// 2. 多来源逐级兜底：openwrt_release / os-release / sysinfo / board.json /
//    ubus / cpuinfo / opkg|apk / device-tree。任一来源缺失都不影响其他字段。
// 3. 识别失败保留「未知」空值，绝不自行推测（尤其不把 target、
//    subtarget、CPU 架构、ABI 混为一谈）。
// 4. 每个字段记录其来源，供诊断层解释"为什么是未知"。
// 5. 低开销：只在文件来源不足时才启动子进程。

use crate::json::Value;
use std::collections::BTreeMap;
use std::process::Command;

pub const UNKNOWN: &str = "unknown";

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// 设备型号（人类可读，如 "Hiveton H5000M"）
    pub model: String,
    /// 设备名称 / 主机名
    pub device_name: String,
    /// board_name（如 "hiveton,h5000m"）
    pub board_name: String,
    /// board.json 的 model.id
    pub board_id: String,
    /// Device Tree compatible 列表（NUL 分隔，标准化为数组）
    pub compatible: Vec<String>,
    /// OpenWrt Target（DISTRIB_TARGET 的第一段，如 "mediatek"）
    pub target: String,
    /// OpenWrt Subtarget（第二段，如 "filogic"）
    pub subtarget: String,
    /// DISTRIB_TARGET 原始值
    pub target_raw: String,
    /// CPU 架构（uname -m / cpuinfo，如 "aarch64"）
    pub cpu_arch: String,
    /// Package Architecture（如 "aarch64_cortex-a53"）
    pub pkg_arch: String,
    /// ABI / CPU 变体（从 pkg_arch 拆分，如 "cortex-a53"）
    pub abi: String,
    /// 当前固件版本（发行版本号）
    pub firmware_version: String,
    /// 当前发行版名称（OpenWrt / ImmortalWrt / ...）
    pub distro: String,
    /// 发行版版本标识（如 "24.10.0" / "SNAPSHOT"）
    pub distro_release: String,
    /// 内核版本（uname -r）
    pub kernel_version: String,
    /// 系统版本标识（组合，如 "ImmortalWrt 24.10.0 r12345"）
    pub system_id: String,
    /// 本机支持的升级方式（只列确实存在的工具）
    pub upgrade_methods: Vec<String>,
    /// 固件平台标识（target[/subtarget]，不含 CPU 架构）
    pub platform: String,
    /// 每个字段的来源标记（字段名 -> 来源名）
    pub sources: BTreeMap<String, String>,
}

impl DeviceInfo {
    pub fn empty() -> Self {
        DeviceInfo {
            model: String::new(),
            device_name: String::new(),
            board_name: String::new(),
            board_id: String::new(),
            compatible: Vec::new(),
            target: String::new(),
            subtarget: String::new(),
            target_raw: String::new(),
            cpu_arch: String::new(),
            pkg_arch: String::new(),
            abi: String::new(),
            firmware_version: String::new(),
            distro: String::new(),
            distro_release: String::new(),
            kernel_version: String::new(),
            system_id: String::new(),
            upgrade_methods: Vec::new(),
            platform: String::new(),
            sources: BTreeMap::new(),
        }
    }

    /// 未被识别的字段列表（供诊断层提示"设备信息不完整"）。
    pub fn unknown_fields(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        let pairs: &[(&'static str, &str)] = &[
            ("model", &self.model),
            ("board_name", &self.board_name),
            ("compatible", ""),
            ("target", &self.target),
            ("subtarget", &self.subtarget),
            ("cpu_arch", &self.cpu_arch),
            ("pkg_arch", &self.pkg_arch),
            ("abi", &self.abi),
            ("firmware_version", &self.firmware_version),
            ("distro", &self.distro),
            ("kernel_version", &self.kernel_version),
            ("platform", &self.platform),
        ];
        for (k, v) in pairs {
            if *k == "compatible" {
                if self.compatible.is_empty() {
                    out.push(*k);
                }
            } else if v.is_empty() {
                out.push(*k);
            }
        }
        if self.upgrade_methods.is_empty() {
            out.push("upgrade_methods");
        }
        out
    }

    /// 归一化后的设备身份候选（供固件匹配使用，不含平台/架构通用词）。
    pub fn identity_strings(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in [&self.board_name, &self.board_id, &self.model] {
            let n = normalize(s);
            if !n.is_empty() && !out.contains(&n) {
                out.push(n);
            }
        }
        for c in &self.compatible {
            // compatible 通常形如 "hiveton,h5000m"，逗号后的部分是具体板级标识
            for part in c.split(',') {
                let n = normalize(part);
                if !n.is_empty() && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out
    }

    pub fn to_json(&self) -> Value {
        let mut pairs: Vec<(String, Value)> = vec![
            ("model".into(), Value::str(display(&self.model))),
            ("device_name".into(), Value::str(display(&self.device_name))),
            ("board_name".into(), Value::str(display(&self.board_name))),
            ("board_id".into(), Value::str(display(&self.board_id))),
            (
                "compatible".into(),
                Value::Arr(self.compatible.iter().map(|s| Value::str(s)).collect()),
            ),
            ("target".into(), Value::str(display(&self.target))),
            ("subtarget".into(), Value::str(display(&self.subtarget))),
            ("target_raw".into(), Value::str(display(&self.target_raw))),
            ("cpu_arch".into(), Value::str(display(&self.cpu_arch))),
            ("pkg_arch".into(), Value::str(display(&self.pkg_arch))),
            ("abi".into(), Value::str(display(&self.abi))),
            (
                "firmware_version".into(),
                Value::str(display(&self.firmware_version)),
            ),
            ("distro".into(), Value::str(display(&self.distro))),
            (
                "distro_release".into(),
                Value::str(display(&self.distro_release)),
            ),
            (
                "kernel_version".into(),
                Value::str(display(&self.kernel_version)),
            ),
            ("system_id".into(), Value::str(display(&self.system_id))),
            (
                "upgrade_methods".into(),
                Value::Arr(self.upgrade_methods.iter().map(|s| Value::str(s)).collect()),
            ),
            ("platform".into(), Value::str(display(&self.platform))),
            ("complete".into(), Value::Bool(self.unknown_fields().is_empty())),
            (
                "unknown_fields".into(),
                Value::Arr(
                    self.unknown_fields()
                        .iter()
                        .map(|s| Value::str(s))
                        .collect(),
                ),
            ),
        ];
        let mut src: Vec<(String, Value)> = self
            .sources
            .iter()
            .map(|(k, v)| (k.clone(), Value::str(v)))
            .collect();
        pairs.push(("sources".into(), Value::Obj(std::mem::take(&mut src))));
        Value::Obj(pairs)
    }
}

fn display(s: &str) -> &str {
    if s.is_empty() {
        UNKNOWN
    } else {
        s
    }
}

fn normalize(s: &str) -> String {
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

fn read_trim(path: &str) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// 解析 KEY='value' / KEY="value" / KEY=value 三式，兼容 openwrt_release、
/// os-release 及各类衍生发行版。
fn kv(text: &str, key: &str) -> Option<String> {
    // 必须精确匹配 "KEY=" 前缀且逐行继续：早期版本用 `?` 会在第一行
    // 不匹配时直接返回 None，导致 DISTRIB_RELEASE 等后续字段全部丢失。
    let prefix = format!("{key}=");
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(&prefix) else {
            continue;
        };
        let rest = rest.trim();
        let val = if rest.len() >= 2
            && ((rest.starts_with('\'') && rest.ends_with('\''))
                || (rest.starts_with('"') && rest.ends_with('"')))
        {
            &rest[1..rest.len() - 1]
        } else {
            rest.split('#').next().unwrap_or(rest).trim()
        };
        if !val.is_empty() {
            return Some(val.to_string());
        }
    }
    None
}

fn run(args: &[&str]) -> Option<String> {
    let (cmd, rest) = args.split_first()?;
    let out = Command::new(cmd).args(rest).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn which(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1 && echo y")])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "y")
        .unwrap_or(false)
}

/// 采集完整设备信息。每个字段独立兜底，失败即保持未知。
pub fn collect() -> DeviceInfo {
    let mut d = DeviceInfo::empty();
    // 用宏而非闭包：闭包会长期可变借用 d，导致后续字段访问被借用检查拒绝。
    macro_rules! src {
        ($field:expr, $origin:expr) => {{
            d.sources.insert($field.to_string(), $origin.to_string());
        }};
    }

    // ---- 1. 发行版与固件版本：openwrt_release 优先，os-release 兜底 ----
    let owrt = std::fs::read_to_string("/etc/openwrt_release").unwrap_or_default();
    let osrel = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let pick = |k: &str| kv(&owrt, k).or_else(|| kv(&osrel, k));

    d.distro = pick("DISTRIB_ID")
        .or_else(|| kv(&osrel, "NAME"))
        .or_else(|| kv(&osrel, "ID"))
        .unwrap_or_default();
    let distro_origin = if kv(&owrt, "DISTRIB_ID").is_some() {
        "openwrt_release"
    } else {
        "os-release"
    };
    if !d.distro.is_empty() {
        src!("distro", distro_origin);
    }

    d.distro_release = pick("DISTRIB_RELEASE")
        .or_else(|| kv(&osrel, "VERSION_ID"))
        .unwrap_or_default();
    if !d.distro_release.is_empty() {
        src!("distro_release", "openwrt_release/os-release");
    }
    d.firmware_version = d.distro_release.clone();
    if !d.firmware_version.is_empty() {
        src!("firmware_version", "DISTRIB_RELEASE/VERSION_ID");
    }

    let revision = pick("DISTRIB_REVISION").unwrap_or_default();
    // 系统版本标识：仅由已确认字段组合，缺失部分不臆造
    let mut id_parts = Vec::new();
    if !d.distro.is_empty() {
        id_parts.push(d.distro.clone());
    }
    if !d.distro_release.is_empty() {
        id_parts.push(d.distro_release.clone());
    }
    if !revision.is_empty() {
        id_parts.push(revision.clone());
    }
    d.system_id = id_parts.join(" ");
    if !d.system_id.is_empty() {
        src!("system_id", "composed");
    }

    let descr = pick("DISTRIB_DESCRIPTION").or_else(|| kv(&osrel, "PRETTY_NAME"));
    if let Some(x) = descr {
        if d.system_id.is_empty() {
            d.system_id = x.clone();
            src!("system_id", "DISTRIB_DESCRIPTION/PRETTY_NAME");
        }
    }

    // ---- 2. Target / Subtarget：严格拆分，缺一段就保留未知 ----
    d.target_raw = pick("DISTRIB_TARGET")
        .or_else(|| kv(&osrel, "OPENWRT_TARGET"))
        .unwrap_or_default();
    if !d.target_raw.is_empty() {
        src!("target_raw", "DISTRIB_TARGET");
        let parts: Vec<&str> = d.target_raw.split('/').filter(|s| !s.is_empty()).collect();
        if let Some(t) = parts.first() {
            d.target = t.to_string();
            src!("target", "DISTRIB_TARGET[0]");
        }
        if let Some(st) = parts.get(1) {
            d.subtarget = st.to_string();
            src!("subtarget", "DISTRIB_TARGET[1]");
        }
        // platform 只用 target/subtarget，绝不与 CPU 架构混用
        d.platform = parts.join("/");
        src!("platform", "DISTRIB_TARGET");
    }

    // ---- 3. 型号 / board_name / board_id ----
    d.model = read_trim("/tmp/sysinfo/model").unwrap_or_default();
    if d.model.is_empty() {
        d.model = kv(&osrel, "OPENWRT_BOARD").or_else(|| kv(&osrel, "OPENWRT_DEVICE")).unwrap_or_default();
    }
    if !d.model.is_empty() {
        src!("model", "sysinfo/os-release");
    }
    d.board_name = read_trim("/tmp/sysinfo/board_name").unwrap_or_default();
    if !d.board_name.is_empty() {
        src!("board_name", "sysinfo");
    }

    if let Some(text) = std::fs::read_to_string("/etc/board.json").ok() {
        if let Ok(v) = crate::json::parse(&text) {
            if let Some(m) = v.get("model") {
                if d.board_id.is_empty() {
                    d.board_id = m
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !d.board_id.is_empty() {
                        src!("board_id", "board.json");
                    }
                }
                if d.model.is_empty() {
                    d.model = m
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !d.model.is_empty() {
                        src!("model", "board.json");
                    }
                }
            }
        }
    }

    // ---- 4. ubus：仅在关键字段缺失时才启动子进程 ----
    if d.board_name.is_empty() || d.model.is_empty() || d.target_raw.is_empty() {
        if let Some(text) = run(&["ubus", "call", "system", "board"]) {
            if let Ok(v) = crate::json::parse(&text) {
                let grab = |path: &[&str]| -> String {
                    let mut cur = &v;
                    for k in path {
                        match cur.get(k) {
                            Some(n) => cur = n,
                            None => return String::new(),
                        }
                    }
                    cur.as_str().unwrap_or("").to_string()
                };
                if d.board_name.is_empty() {
                    d.board_name = grab(&["board_name"]);
                    if !d.board_name.is_empty() {
                        src!("board_name", "ubus");
                    }
                }
                if d.model.is_empty() {
                    d.model = grab(&["model"]);
                    if !d.model.is_empty() {
                        src!("model", "ubus");
                    }
                }
                if d.target_raw.is_empty() {
                    let t = grab(&["release", "target"]);
                    if !t.is_empty() {
                        d.target_raw = t.clone();
                        src!("target_raw", "ubus");
                        let parts: Vec<&str> = t.split('/').filter(|s| !s.is_empty()).collect();
                        if let Some(x) = parts.first() {
                            d.target = x.to_string();
                            src!("target", "ubus");
                        }
                        if let Some(x) = parts.get(1) {
                            d.subtarget = x.to_string();
                            src!("subtarget", "ubus");
                        }
                        d.platform = parts.join("/");
                        src!("platform", "ubus");
                    }
                }
                if d.firmware_version.is_empty() {
                    d.firmware_version = grab(&["release", "version"]);
                    if !d.firmware_version.is_empty() {
                        src!("firmware_version", "ubus");
                    }
                }
            }
        }
    }

    d.device_name = read_trim("/proc/sys/kernel/hostname")
        .or_else(|| run(&["uname", "-n"]))
        .unwrap_or_default();
    if !d.device_name.is_empty() {
        src!("device_name", "hostname");
    }

    // ---- 5. Device Tree compatible（NUL 分隔）----
    for p in [
        "/proc/device-tree/compatible",
        "/sys/firmware/devicetree/base/compatible",
    ] {
        if let Ok(bytes) = std::fs::read(p) {
            let list: Vec<String> = bytes
                .split(|b| *b == 0)
                .filter_map(|part| {
                    let s = String::from_utf8_lossy(part).trim().to_string();
                    (!s.is_empty()).then_some(s)
                })
                .collect();
            if !list.is_empty() {
                d.compatible = list;
                src!("compatible", p);
                break;
            }
        }
    }

    // ---- 6. CPU 架构（uname -m 优先，cpuinfo 兜底）----
    d.cpu_arch = run(&["uname", "-m"])
        .or_else(|| std::env::consts::ARCH.to_string().into())
        .unwrap_or_default();
    src!("cpu_arch", "uname");
    if d.cpu_arch.is_empty() {
        if let Some(text) = std::fs::read_to_string("/proc/cpuinfo").ok() {
            for line in text.lines() {
                if let Some(v) = line.split_once(':').map(|(_, v)| v.trim()) {
                    if line.starts_with("CPU architecture") || line.starts_with("Hardware") {
                        d.cpu_arch = v.to_string();
                        src!("cpu_arch", "cpuinfo");
                        break;
                    }
                }
            }
        }
    }

    // ---- 7. Package Architecture / ABI ----
    // DISTRIB_ARCH 最精确（含 ABI 变体），包管理器输出作兜底。
    d.pkg_arch = pick("DISTRIB_ARCH").unwrap_or_default();
    if !d.pkg_arch.is_empty() {
        src!("pkg_arch", "DISTRIB_ARCH");
    } else {
        for (cmd, args) in [
            ("apk", &["--print-arch"][..]),
            ("opkg", &["print-architecture"][..]),
        ] {
            let mut argv: Vec<&str> = vec![cmd];
            argv.extend_from_slice(args);
            if let Some(text) = run(&argv) {
                let mut found = String::new();
                for line in text.lines() {
                    let name = line.split_whitespace().nth(1).unwrap_or(line.trim());
                    if name == "all" || name == "noarch" || name.is_empty() {
                        continue;
                    }
                    found = name.to_string();
                }
                if !found.is_empty() {
                    d.pkg_arch = found;
                    src!("pkg_arch", cmd);
                    break;
                }
            }
        }
    }
    // ABI 只在 pkg_arch 明确为「arch_变体」形式且后缀确为 CPU/浮点变体时拆分。
    // x86_64 / x86-64 这类纯位宽后缀不是 ABI，识别失败保留 unknown，绝不推测。
    if let Some(abi) = derive_abi(&d.pkg_arch) {
        d.abi = abi;
        src!("abi", "pkg_arch suffix");
    }

    // ---- 8. 内核版本 ----
    d.kernel_version = run(&["uname", "-r"])
        .or_else(|| {
            std::fs::read_to_string("/proc/version")
                .ok()
                .and_then(|s| s.split_whitespace().nth(2).map(|x| x.to_string()))
        })
        .unwrap_or_default();
    if !d.kernel_version.is_empty() {
        src!("kernel_version", "uname");
    }

    // ---- 9. 升级方式：只列确实存在的工具 ----
    if which("sysupgrade") {
        d.upgrade_methods.push("sysupgrade".into());
        // -T 是否存在由实际调用验证，这里仅标注可用工具
        if which("fwtool") {
            d.upgrade_methods.push("fwtool".into());
        }
    }
    if which("mtd") {
        d.upgrade_methods.push("mtd".into());
    }
    if !d.upgrade_methods.is_empty() {
        src!("upgrade_methods", "command probe");
    }

    d
}

/// 供外部快速判断本机是否具备 sysupgrade -T 兼容性预检能力。
pub fn supports_sysupgrade_test() -> bool {
    which("sysupgrade")
}

/// 从包架构字符串推导 ABI 变体（CPU 变体 / FPU / 浮点 ABI）。
///
/// OpenWrt `DISTRIB_ARCH` 标准形如 `arch_abi`：
/// `aarch64_cortex-a53`、`mipsel_24kc`、`arm_cortex-a7_neon-vfpv4`。
/// 但 `x86_64`（真实 25.12.0 实测）下划线后是纯位宽数字，不是 ABI；
/// `aarch64`、`riscv64` 等纯 CPU 架构名则根本没有 ABI 后缀。
///
/// 判定规则（通用，不绑定任何设备）：后缀必须含 ASCII 字母。
/// 纯数字位宽（64/32）、空后缀、无分隔符一律返回 None，
/// 调用方据此保留 `unknown`，绝不把 CPU 架构的一部分误当成 ABI。
fn derive_abi(pkg_arch: &str) -> Option<String> {
    let suffix = pkg_arch
        .split_once('_')
        .or_else(|| pkg_arch.split_once('-'))
        .map(|(_, s)| s.trim())?;
    if suffix.is_empty() || !suffix.bytes().any(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some(suffix.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kv_parses_all_three_quote_styles() {
        let text = "DISTRIB_ID='ImmortalWrt'\nDISTRIB_RELEASE=\"24.10.0\"\nDISTRIB_TARGET=mediatek/filogic\nEMPTY=''\n";
        assert_eq!(kv(text, "DISTRIB_ID").as_deref(), Some("ImmortalWrt"));
        assert_eq!(kv(text, "DISTRIB_RELEASE").as_deref(), Some("24.10.0"));
        assert_eq!(kv(text, "DISTRIB_TARGET").as_deref(), Some("mediatek/filogic"));
        assert_eq!(kv(text, "EMPTY"), None);
        assert_eq!(kv(text, "MISSING"), None);
    }

    #[test]
    fn target_and_subtarget_are_separated_not_conflated() {
        // 纯函数级验证：模拟 DISTRIB_TARGET 拆分逻辑
        let raw = "mediatek/filogic";
        let parts: Vec<&str> = raw.split('/').filter(|s| !s.is_empty()).collect();
        assert_eq!(parts, vec!["mediatek", "filogic"]);
        let raw2 = "x86";
        let parts2: Vec<&str> = raw2.split('/').filter(|s| !s.is_empty()).collect();
        assert_eq!(parts2, vec!["x86"]);
        assert!(parts2.get(1).is_none(), "无 subtarget 时必须保留未知");
    }

    #[test]
    fn abi_only_derived_from_explicit_suffix() {
        // 标准 arch_abi 形式：后缀确为 CPU/浮点变体
        assert_eq!(derive_abi("aarch64_cortex-a53").as_deref(), Some("cortex-a53"));
        assert_eq!(derive_abi("mipsel_24kc").as_deref(), Some("24kc"));
        assert_eq!(
            derive_abi("arm_cortex-a7_neon-vfpv4").as_deref(),
            Some("cortex-a7_neon-vfpv4")
        );
        assert_eq!(derive_abi("aarch64-cortex-a53").as_deref(), Some("cortex-a53"));

        // 真实 OpenWrt 25.12.0 回归：x86_64 的 "64" 是 CPU 位宽，不是 ABI
        assert_eq!(derive_abi("x86_64"), None, "x86_64 不得推导 abi=64");
        assert_eq!(derive_abi("x86-64"), None, "纯位宽后缀不得推导 ABI");
        assert_eq!(derive_abi("i386_32"), None);
        // 纯 CPU 架构名无分隔符，保留 unknown，不推测
        assert_eq!(derive_abi("aarch64"), None, "无分隔符不得推测 ABI");
        assert_eq!(derive_abi("riscv64"), None);
        assert_eq!(derive_abi("x86_"), None, "空后缀不得推测 ABI");
    }

    #[test]
    fn empty_device_keeps_unknown_not_guessed() {
        let d = DeviceInfo::empty();
        assert!(!d.unknown_fields().is_empty());
        assert!(d.identity_strings().is_empty());
        let j = d.to_json();
        assert_eq!(
            j.get("target").and_then(|v| v.as_str()),
            Some("unknown"),
            "未知字段必须显式显示 unknown"
        );
        assert!(matches!(j.get("complete"), Some(Value::Bool(false))));
    }

    #[test]
    fn normalize_handles_vendor_comma_form() {
        assert_eq!(normalize("Hiveton,H5000M"), "hiveton-h5000m");
        assert_eq!(normalize("  Xiaomi Router AX3000T "), "xiaomi-router-ax3000t");
    }

    #[test]
    fn identity_strings_include_dt_compatible_parts() {
        let mut d = DeviceInfo::empty();
        d.board_name = "hiveton,h5000m".into();
        d.model = "Hiveton H5000M".into();
        d.compatible = vec!["hiveton,h5000m".into(), "mediatek,mt7981".into()];
        let ids = d.identity_strings();
        assert!(ids.contains(&"hiveton-h5000m".to_string()));
        assert!(ids.contains(&"mt7981".to_string()));
    }

    #[test]
    fn collect_never_panics_on_missing_files() {
        // 沙箱里没有 OpenWrt 文件系统，采集必须安全返回（全部保留未知）
        let d = collect();
        let j = d.to_json();
        assert!(j.get("model").is_some());
        assert!(j.get("target").is_some());
        assert!(j.get("cpu_arch").is_some());
        assert!(j.get("kernel_version").is_some());
    }
}
