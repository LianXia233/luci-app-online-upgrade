// 固件文件自动发现与识别（阶段 5）。
//
// 不依赖任何固定文件名：从 Release Assets / Actions Artifacts / 自定义地址
// 发现文件，按格式与用途分类，压缩包安全解析后提取内部文件清单，
// 再按设备型号 / compatible / target / subtarget / CPU 架构 / ABI / 版本 /
// 镜像类型 / 内部元数据多维度匹配。
//
// 关键约束：
// - 不是所有 .bin 都是可刷写固件，也不是所有 ZIP 都是固件。
// - ZIP 解析纯 Rust 读中央目录，不落盘解压；tar/gz 系列只读清单不解压文件。
// - 路径穿越、解压炸弹、条目数/总大小上限都要拦截。
// - 不引入大型依赖（无 zip/flate2 crate）。

use crate::device::DeviceInfo;
use crate::json::Value;
use crate::version;
use std::collections::BTreeSet;
use std::process::Command;

// ---------------------------------------------------------------- 类型分类

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FwFormat {
    Bin, Img, Gz, Xz, Zst, Zip, SevenZip, Tar, TarGz, TarXz, TarZst, Vmdk, Vhd,
    Qcow2, Ubi, Trx, Chk, Sha256, Text, Log, Unknown,
}

impl FwFormat {
    pub fn label(self) -> &'static str {
        match self {
            FwFormat::Bin => "bin",
            FwFormat::Img => "img",
            FwFormat::Gz => "gz",
            FwFormat::Xz => "xz",
            FwFormat::Zst => "zst",
            FwFormat::Zip => "zip",
            FwFormat::SevenZip => "7z",
            FwFormat::Tar => "tar",
            FwFormat::TarGz => "tar.gz",
            FwFormat::TarXz => "tar.xz",
            FwFormat::TarZst => "tar.zst",
            FwFormat::Vmdk => "vmdk",
            FwFormat::Vhd => "vhd",
            FwFormat::Qcow2 => "qcow2",
            FwFormat::Ubi => "ubi",
            FwFormat::Trx => "trx",
            FwFormat::Chk => "chk",
            FwFormat::Sha256 => "sha256",
            FwFormat::Text => "text",
            FwFormat::Log => "log",
            FwFormat::Unknown => "unknown",
        }
    }
    pub fn is_archive(self) -> bool {
        matches!(
            self,
            FwFormat::Zip | FwFormat::SevenZip | FwFormat::Tar | FwFormat::TarGz
                | FwFormat::TarXz | FwFormat::TarZst | FwFormat::Gz | FwFormat::Xz
                | FwFormat::Zst
        )
    }
    /// 是否可能是可刷写的镜像容器（不等于"一定是固件"）
    pub fn may_be_image(self) -> bool {
        matches!(
            self,
            FwFormat::Bin | FwFormat::Img | FwFormat::Gz | FwFormat::Xz
                | FwFormat::Zst | FwFormat::Trx | FwFormat::Chk | FwFormat::Ubi
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Sysupgrade, Factory, FullImage, Uboot, Efi, Rootfs, Kernel, Packages,
    Sdk, Toolchain, Debug, Checksum, BuildLog, Config, Archive, Other, Unknown,
}

impl FileKind {
    pub fn label(self) -> &'static str {
        match self {
            FileKind::Sysupgrade => "sysupgrade 升级镜像",
            FileKind::Factory => "factory 出厂镜像",
            FileKind::FullImage => "完整固件镜像",
            FileKind::Uboot => "U-Boot 镜像",
            FileKind::Efi => "EFI 镜像",
            FileKind::Rootfs => "rootfs",
            FileKind::Kernel => "kernel",
            FileKind::Packages => "packages",
            FileKind::Sdk => "SDK",
            FileKind::Toolchain => "Toolchain",
            FileKind::Debug => "Debug 文件",
            FileKind::Checksum => "校验文件",
            FileKind::BuildLog => "构建日志",
            FileKind::Config => "配置/清单文件",
            FileKind::Archive => "压缩包",
            FileKind::Other => "其他文件",
            FileKind::Unknown => "无法识别",
        }
    }
    /// 只有这几类才可能进入刷写流程
    pub fn is_flashable_candidate(self) -> bool {
        matches!(self, FileKind::Sysupgrade | FileKind::FullImage | FileKind::Factory)
    }
    pub fn code(self) -> &'static str {
        match self {
            FileKind::Sysupgrade => "sysupgrade",
            FileKind::Factory => "factory",
            FileKind::FullImage => "full_image",
            FileKind::Uboot => "uboot",
            FileKind::Efi => "efi",
            FileKind::Rootfs => "rootfs",
            FileKind::Kernel => "kernel",
            FileKind::Packages => "packages",
            FileKind::Sdk => "sdk",
            FileKind::Toolchain => "toolchain",
            FileKind::Debug => "debug",
            FileKind::Checksum => "checksum",
            FileKind::BuildLog => "build_log",
            FileKind::Config => "config",
            FileKind::Archive => "archive",
            FileKind::Other => "other",
            FileKind::Unknown => "unknown",
        }
    }
}

pub fn detect_format(name: &str) -> FwFormat {
    let n = name.to_ascii_lowercase();
    let has = |s: &str| n.ends_with(s) || n.contains(&format!("{s}."));
    if has(".tar.gz") || has(".tgz") { return FwFormat::TarGz; }
    if has(".tar.xz") || has(".txz") { return FwFormat::TarXz; }
    if has(".tar.zst") { return FwFormat::TarZst; }
    if has(".tar") { return FwFormat::Tar; }
    if has(".sha256") || has(".sha256sum") || n == "sha256sums" || n == "sha256sums.txt"
        || n == "checksums.txt" || has(".md5") || has(".sums")
    { return FwFormat::Sha256; }
    if has(".7z") { return FwFormat::SevenZip; }
    if has(".zip") { return FwFormat::Zip; }
    if has(".img.gz") || has(".bin.gz") || has(".gz") { return FwFormat::Gz; }
    if has(".xz") { return FwFormat::Xz; }
    if has(".zst") { return FwFormat::Zst; }
    if has(".vmdk") { return FwFormat::Vmdk; }
    if has(".vhd") || has(".vhdx") { return FwFormat::Vhd; }
    if has(".qcow2") { return FwFormat::Qcow2; }
    if has(".ubi") { return FwFormat::Ubi; }
    if has(".trx") { return FwFormat::Trx; }
    if has(".chk") { return FwFormat::Chk; }
    if has(".log") || has(".txt") { return if n.contains("log") { FwFormat::Log } else { FwFormat::Text }; }
    if has(".json") || has(".yaml") || has(".yml") || has(".config") || has(".conf") {
        return FwFormat::Text;
    }
    if has(".bin") { return FwFormat::Bin; }
    if has(".img") { return FwFormat::Img; }
    FwFormat::Unknown
}

/// 把文件名切成 token，用于**精确**词匹配。
/// 短词（efi / sdk / debug / full / kernel）绝不能用裸 contains，
/// 否则 "somefile.bin" 会因为包含 "efi" 被误判成 EFI 镜像。
fn name_tokens(name: &str) -> Vec<String> {
    name.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn has_token(tokens: &[String], word: &str) -> bool {
    tokens.iter().any(|t| t == word)
}

/// 文件用途分类：按"排除优先"顺序，避免把校验文件/SDK/日志当固件。
pub fn classify_kind(name: &str) -> FileKind {
    let n = name.to_ascii_lowercase();
    let fmt = detect_format(name);
    let tok = name_tokens(name);

    if matches!(fmt, FwFormat::Sha256) || n.contains("sha256sum") || n.contains("checksum") {
        return FileKind::Checksum;
    }
    if n.contains("build.log") || n.contains("buildlog") || n.ends_with(".log")
        || n.contains("compile-log")
    { return FileKind::BuildLog; }
    if has_token(&tok, "sdk") || n.contains("openwrt-sdk") { return FileKind::Sdk; }
    if n.contains("toolchain") { return FileKind::Toolchain; }
    if n.contains("packages") { return FileKind::Packages; }
    if has_token(&tok, "debug") || has_token(&tok, "dbg")
        || n.contains("-dbg") || n.contains("symbols")
    { return FileKind::Debug; }
    if n.contains("uboot") || n.contains("u-boot") || n.contains("bootloader") {
        return FileKind::Uboot;
    }
    if has_token(&tok, "efi") || n.contains("bootx64") { return FileKind::Efi; }
    if n.contains("rootfs") || n.contains("root-fs") { return FileKind::Rootfs; }
    // initramfs 优先于 kernel：带 initramfs 的镜像属于 factory 类，
    // 不是纯 kernel 文件。
    if n.contains("initramfs") && !n.contains("sysupgrade") { return FileKind::Factory; }
    if (has_token(&tok, "kernel") || n.contains("vmlinuz") || n.contains("zimage"))
        && !n.contains("sysupgrade")
    { return FileKind::Kernel; }
    if n.contains("factory") { return FileKind::Factory; }
    if n.contains("sysupgrade") || n.contains("upgrade") { return FileKind::Sysupgrade; }
    if n.contains("combined") || has_token(&tok, "full") || n.contains("sdcard")
        || n.contains("squashfs") || matches!(fmt, FwFormat::Vmdk | FwFormat::Vhd | FwFormat::Qcow2)
    { return FileKind::FullImage; }
    if n.contains("manifest") || n.contains(".config") || n.ends_with(".json")
        || n.contains("profiles")
    { return FileKind::Config; }
    if fmt.is_archive() { return FileKind::Archive; }
    if fmt.may_be_image() { return FileKind::FullImage; }
    if fmt == FwFormat::Unknown { return FileKind::Unknown; }
    FileKind::Other
}

// ---------------------------------------------------------------- 文件结构

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Custom（自定义下载地址）与 ArchiveInner（压缩包内部文件）为设计预留来源，
/// 当前自动发现流程只产出 ReleaseAsset / Artifact。
#[allow(dead_code)]
pub enum FileSource {
    ReleaseAsset,
    Artifact,
    Custom,
    ArchiveInner,
}

impl FileSource {
    pub fn label(self) -> &'static str {
        match self {
            FileSource::ReleaseAsset => "Release Asset",
            FileSource::Artifact => "Actions Artifact",
            FileSource::Custom => "自定义地址",
            FileSource::ArchiveInner => "压缩包内部文件",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FirmwareFile {
    pub name: String,
    pub source: FileSource,
    pub format: FwFormat,
    pub kind: FileKind,
    pub size: u64,
    pub sha256: String,
    pub digest: String,
    pub download_url: String,
    pub api_url: String,
    pub html_url: String,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: String,
    pub version: String,
    pub target: String,
    pub subtarget: String,
    /// 预留：L2 解析固件内部元数据后填充的机型 / compatible 提示
    #[allow(dead_code)]
    pub model_hint: String,
    #[allow(dead_code)]
    pub compatible_hint: String,
    pub cpu_arch: String,
    pub abi: String,
    pub run_id: u64,
    pub workflow_id: u64,
    pub expired: bool,
    pub archived: bool,
    pub parse_result: String,
    /// 压缩包内部清单（仅解析元数据，不解压内容）
    pub inner_files: Vec<ArchiveEntry>,
}

#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    pub kind: FileKind,
}

impl FirmwareFile {
    pub fn new(name: &str, source: FileSource) -> FirmwareFile {
        let format = detect_format(name);
        let kind = classify_kind(name);
        let version = version::pick(name).map(|v| v.as_display()).unwrap_or_default();
        FirmwareFile {
            name: name.to_string(),
            source,
            format,
            kind,
            size: 0,
            sha256: String::new(),
            digest: String::new(),
            download_url: String::new(),
            api_url: String::new(),
            html_url: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            expires_at: String::new(),
            version,
            target: String::new(),
            subtarget: String::new(),
            model_hint: String::new(),
            compatible_hint: String::new(),
            cpu_arch: String::new(),
            abi: String::new(),
            run_id: 0,
            workflow_id: 0,
            expired: false,
            archived: false,
            parse_result: "已按文件名与格式完成初筛".into(),
            inner_files: Vec::new(),
            // tokens 稍后用于提取架构词
        }
    }
}

/// 从文件名 token 中提取可识别的架构/ABI 线索（只做初筛，不做兼容判定）。
pub fn infer_arch_hints(name: &str) -> (String, String) {
    let n = name.to_ascii_lowercase().replace('_', "-");
    let arch_markers = [
        "aarch64", "arm64", "x86-64", "x86_64", "amd64", "mipsel", "mips64",
        "armv7", "armhf", "cortex-a53", "cortex-a7", "cortex-a9", "24kc", "74kc",
    ];
    let mut arch = String::new();
    let mut abi = String::new();
    for m in arch_markers {
        if n.contains(m) {
            if m.starts_with("cortex") || m.ends_with("kc") {
                if abi.is_empty() { abi = m.to_string(); }
            } else if arch.is_empty() {
                arch = m.to_string();
            }
        }
    }
    (arch, abi)
}

fn val_str(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn val_num(v: &Value, k: &str) -> u64 {
    match v.get(k) {
        Some(Value::Num(n)) if *n >= 0.0 => *n as u64,
        Some(Value::Str(s)) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

fn val_bool(v: &Value, k: &str) -> bool {
    matches!(v.get(k), Some(Value::Bool(true)))
}

/// GitHub API 的 asset.digest 形如 "sha256:<hex>"，是可信校验源。
fn parse_digest(v: &Value) -> String {
    let d = val_str(v, "digest");
    d.strip_prefix("sha256:").unwrap_or("").to_ascii_lowercase()
}

/// 从 Release Asset 对象构造固件文件（不要求对象是固件，全部保留供筛选）。
pub fn from_release_asset(a: &Value, tag: &str, release_url: &str) -> FirmwareFile {
    let name = val_str(a, "name");
    let mut f = FirmwareFile::new(&name, FileSource::ReleaseAsset);
    f.size = val_num(a, "size");
    f.download_url = val_str(a, "browser_download_url");
    f.api_url = val_str(a, "url");
    f.digest = parse_digest(a);
    f.html_url = release_url.to_string();
    f.created_at = val_str(a, "created_at");
    f.updated_at = val_str(a, "updated_at");
    f.sha256 = if f.digest.len() == 64 { f.digest.clone() } else { String::new() };
    if f.version.is_empty() {
        f.version = version::pick(tag).map(|v| v.as_display()).unwrap_or_default();
    }
    let (arch, abi) = infer_arch_hints(&name);
    f.cpu_arch = arch;
    f.abi = abi;
    f.parse_result = if f.kind.is_flashable_candidate() {
        "识别为可刷写镜像候选（文件名 + GitHub 资产元数据）".into()
    } else {
        format!("识别为 {}，不进入刷写候选", f.kind.label())
    };
    f
}

/// 从 Actions Artifact 对象构造（Artifact 不等于固件，需下载后解析）。
pub fn from_artifact(a: &Value) -> FirmwareFile {
    let name = val_str(a, "name");
    let mut f = FirmwareFile::new(&name, FileSource::Artifact);
    f.size = val_num(a, "size_in_bytes");
    f.download_url = val_str(a, "archive_download_url");
    f.api_url = format!(
        "https://api.github.com/repos/{{repo}}/actions/artifacts/{}",
        val_num(a, "id")
    );
    f.created_at = val_str(a, "created_at");
    f.updated_at = val_str(a, "updated_at");
    f.expires_at = val_str(a, "expires_at");
    f.expired = val_bool(a, "expired");
    f.archived = f.expired;
    if let Some(run) = a.get("workflow_run") {
        f.run_id = val_num(run, "id");
        f.workflow_id = val_num(run, "workflow_id");
    }
    let (arch, abi) = infer_arch_hints(&name);
    f.cpu_arch = arch;
    f.abi = abi;
    f.parse_result = "Artifact 名称不足以判定内部文件，需下载后安全解析".into();
    f
}

impl FirmwareFile {
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("name".into(), Value::str(&self.name)),
            ("source".into(), Value::str(self.source.label())),
            ("format".into(), Value::str(self.format.label())),
            ("kind".into(), Value::str(self.kind.code())),
            ("kind_label".into(), Value::str(self.kind.label())),
            ("flashable_candidate".into(), Value::Bool(self.kind.is_flashable_candidate())),
            ("size".into(), Value::num(self.size)),
            ("size_human".into(), Value::str(&crate::github::human_size(self.size))),
            ("sha256".into(), Value::str(&self.sha256)),
            ("digest".into(), Value::str(&self.digest)),
            ("download_url".into(), Value::str(&self.download_url)),
            ("html_url".into(), Value::str(&self.html_url)),
            ("created_at".into(), Value::str(&self.created_at)),
            ("updated_at".into(), Value::str(&self.updated_at)),
            ("expires_at".into(), Value::str(&self.expires_at)),
            ("version".into(), Value::str(&self.version)),
            ("target".into(), Value::str(&self.target)),
            ("subtarget".into(), Value::str(&self.subtarget)),
            ("cpu_arch".into(), Value::str(&self.cpu_arch)),
            ("abi".into(), Value::str(&self.abi)),
            ("run_id".into(), Value::num(self.run_id)),
            ("workflow_id".into(), Value::num(self.workflow_id)),
            ("expired".into(), Value::Bool(self.expired)),
            ("parse_result".into(), Value::str(&self.parse_result)),
            (
                "inner_files".into(),
                Value::Arr(
                    self.inner_files
                        .iter()
                        .map(|e| Value::Obj(vec![
                            ("name".into(), Value::str(&e.name)),
                            ("size".into(), Value::num(e.size)),
                            ("kind".into(), Value::str(e.kind.code())),
                        ]))
                        .collect(),
                ),
            ),
        ])
    }
}

// ---------------------------------------------------------------- 压缩包安全解析

pub const MAX_ARCHIVE_ENTRIES: usize = 512;
pub const MAX_ARCHIVE_TOTAL_UNCOMPRESSED: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_SINGLE_UNCOMPRESSED: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_COMPRESSION_RATIO: u64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    TooManyEntries,
    DecompressionBomb,
    UnsafePath(String),
    Truncated,
    Unsupported,
    Io(String),
}

impl ArchiveError {
    pub fn message(&self) -> String {
        match self {
            ArchiveError::TooManyEntries => format!("压缩包条目数超过上限 {MAX_ARCHIVE_ENTRIES}"),
            ArchiveError::DecompressionBomb => "疑似解压炸弹（未压缩体积或压缩比异常）".into(),
            ArchiveError::UnsafePath(p) => format!("压缩包包含不安全路径 [{p}]，已拒绝"),
            ArchiveError::Truncated => "压缩包结构不完整或已损坏".into(),
            ArchiveError::Unsupported => "不支持的压缩包格式".into(),
            ArchiveError::Io(e) => format!("读取压缩包失败: {e}"),
        }
    }
}

/// 检查条目路径：拒绝绝对路径、".."、反斜杠、NUL、驱动器前缀。
pub fn safe_entry_name(name: &str) -> Result<(), ArchiveError> {
    if name.is_empty()
        || name.contains('\0')
        || name.starts_with('/')
        || name.starts_with('\\')
        || name.contains("..")
        || name.contains('\\')
        || (name.len() >= 2 && name.as_bytes()[1] == b':')
    {
        return Err(ArchiveError::UnsafePath(name.chars().take(120).collect()));
    }
    Ok(())
}

fn le16(b: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(p..p + 2)?.try_into().ok()?))
}
fn le32(b: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(p..p + 4)?.try_into().ok()?))
}

/// 纯 Rust 读取 ZIP 中央目录，只提取元数据，绝不落盘解压。
pub fn inspect_zip(data: &[u8]) -> Result<Vec<ArchiveEntry>, ArchiveError> {
    if data.len() < 22 {
        return Err(ArchiveError::Truncated);
    }
    // EOCD 签名 0x06054b50，从末尾向前搜索
    let start = data.len().saturating_sub(65557);
    let mut eocd = None;
    for p in (start..=data.len() - 22).rev() {
        if le32(data, p) == Some(0x06054b50) {
            eocd = Some(p);
            break;
        }
    }
    let e = eocd.ok_or(ArchiveError::Truncated)?;
    let count = le16(data, e + 10).ok_or(ArchiveError::Truncated)? as usize;
    let cd_size = le32(data, e + 12).ok_or(ArchiveError::Truncated)? as usize;
    let cd_off = le32(data, e + 16).ok_or(ArchiveError::Truncated)? as usize;
    if count > MAX_ARCHIVE_ENTRIES {
        return Err(ArchiveError::TooManyEntries);
    }
    if cd_off.checked_add(cd_size).map(|x| x > data.len()).unwrap_or(true) {
        return Err(ArchiveError::Truncated);
    }

    let mut out = Vec::new();
    let mut total: u64 = 0;
    let mut p = cd_off;
    for _ in 0..count {
        if le32(data, p) != Some(0x02014b50) {
            return Err(ArchiveError::Truncated);
        }
        let comp_size = le32(data, p + 20).ok_or(ArchiveError::Truncated)? as u64;
        let uncomp_size = le32(data, p + 24).ok_or(ArchiveError::Truncated)? as u64;
        let name_len = le16(data, p + 28).ok_or(ArchiveError::Truncated)? as usize;
        let extra_len = le16(data, p + 30).ok_or(ArchiveError::Truncated)? as usize;
        let comment_len = le16(data, p + 32).ok_or(ArchiveError::Truncated)? as usize;
        let name_start = p + 46;
        let name_end = name_start.checked_add(name_len).ok_or(ArchiveError::Truncated)?;
        let name_bytes = data.get(name_start..name_end).ok_or(ArchiveError::Truncated)?;
        let name = String::from_utf8_lossy(name_bytes).to_string();
        safe_entry_name(&name)?;
        if uncomp_size > MAX_SINGLE_UNCOMPRESSED {
            return Err(ArchiveError::DecompressionBomb);
        }
        if comp_size > 0 && uncomp_size / comp_size.max(1) > MAX_COMPRESSION_RATIO {
            return Err(ArchiveError::DecompressionBomb);
        }
        total = total.saturating_add(uncomp_size);
        if total > MAX_ARCHIVE_TOTAL_UNCOMPRESSED {
            return Err(ArchiveError::DecompressionBomb);
        }
        out.push(ArchiveEntry {
            kind: classify_kind(&name),
            name,
            size: uncomp_size,
            compressed_size: comp_size,
        });
        p = p
            .checked_add(46 + name_len + extra_len + comment_len)
            .ok_or(ArchiveError::Truncated)?;
        if p > data.len() {
            return Err(ArchiveError::Truncated);
        }
    }
    Ok(out)
}

/// 对已下载的压缩包做安全清单解析。ZIP 走纯 Rust；tar 系列调用系统 `tar -t`
/// 只列清单（文件不经过 shell，参数直接传递），同样执行安全校验。
pub fn inspect_archive_file(path: &str) -> Result<Vec<ArchiveEntry>, ArchiveError> {
    let data = std::fs::read(path).map_err(|e| ArchiveError::Io(e.to_string()))?;
    if data.len() >= 4 && &data[..2] == b"PK" {
        return inspect_zip(&data);
    }
    let lower = path.to_ascii_lowercase();
    let flag = if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        "-tzf"
    } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
        "-tJf"
    } else if lower.ends_with(".tar.zst") {
        "-tIf" // 部分 tar 不支持，失败会返回 Unsupported
    } else if lower.ends_with(".tar") {
        "-tf"
    } else {
        return Err(ArchiveError::Unsupported);
    };
    let out = Command::new("tar")
        .args([flag, path])
        .output()
        .map_err(|e| ArchiveError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(ArchiveError::Unsupported);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut entries = Vec::new();
    for line in text.lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        safe_entry_name(name)?;
        if entries.len() >= MAX_ARCHIVE_ENTRIES {
            return Err(ArchiveError::TooManyEntries);
        }
        entries.push(ArchiveEntry {
            name: name.to_string(),
            size: 0,
            compressed_size: 0,
            kind: classify_kind(name),
        });
    }
    Ok(entries)
}

/// 从压缩包清单里找出真正的可刷写镜像候选。
pub fn flashable_from_entries(entries: &[ArchiveEntry]) -> Vec<&ArchiveEntry> {
    entries
        .iter()
        .filter(|e| {
            e.kind.is_flashable_candidate()
                && matches!(
                    detect_format(&e.name),
                    FwFormat::Bin | FwFormat::Img | FwFormat::Gz | FwFormat::Xz
                        | FwFormat::Zst | FwFormat::Trx | FwFormat::Chk | FwFormat::Ubi
                )
        })
        .collect()
}

// ---------------------------------------------------------------- 多维度匹配

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchLevel {
    High,
    Partial,
    NeedVerify,
    Mismatch,
    Unrecognized,
}

impl MatchLevel {
    pub fn code(self) -> &'static str {
        match self {
            MatchLevel::High => "high",
            MatchLevel::Partial => "partial",
            MatchLevel::NeedVerify => "need_verify",
            MatchLevel::Mismatch => "mismatch",
            MatchLevel::Unrecognized => "unrecognized",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            MatchLevel::High => "高置信度匹配",
            MatchLevel::Partial => "部分信息匹配",
            MatchLevel::NeedVerify => "需要进一步验证",
            MatchLevel::Mismatch => "明确不匹配",
            MatchLevel::Unrecognized => "无法识别",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub level: MatchLevel,
    pub score: i32,
    pub basis: Vec<String>,
    pub conflicts: Vec<String>,
    /// 是否允许进入后续下载/校验流程（不等于允许刷写）
    pub allow_next: bool,
}

fn norm(s: &str) -> String {
    let mut out = String::new();
    let mut sep = false;
    for c in s.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            sep = false;
        } else if !sep && !out.is_empty() {
            out.push('-');
            sep = true;
        }
    }
    while out.ends_with('-') { out.pop(); }
    out
}

/// 多维度匹配：设备型号 / compatible / target / subtarget / CPU 架构 /
/// ABI / 版本 / 镜像类型 / 内部元数据。规则可配置，不堆积机型特判。
pub fn match_device(dev: &DeviceInfo, f: &FirmwareFile, rule: Option<&str>) -> MatchResult {
    let text = norm(&format!("{} {}", f.name, f.inner_files.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(" ")));
    let mut basis = Vec::new();
    let mut conflicts = Vec::new();
    let mut score: i32 = 0;

    // 第 1 层：文件类型门槛 —— 校验文件/SDK/日志/UBoot 绝不进入刷写候选
    if !f.kind.is_flashable_candidate() && f.inner_files.is_empty() {
        return MatchResult {
            level: MatchLevel::Mismatch,
            score: -100,
            basis: vec![format!("文件类型为 {}，不是可刷写的 sysupgrade/完整镜像", f.kind.label())],
            conflicts: vec!["文件类型不匹配".into()],
            allow_next: false,
        };
    }

    // 第 2 层：设备型号 / compatible 身份命中
    let identities = dev.identity_strings();
    let mut identity_hits = 0;
    for id in &identities {
        if id.len() >= 3 && text.contains(id) {
            identity_hits += 1;
            basis.push(format!("命中本机身份标识 [{id}]"));
        }
    }
    // 型号 token 级命中（兼容 "H5000M" 这类独立词）
    let tokens: BTreeSet<String> = text.split('-').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    for id in &identities {
        for t in id.split('-') {
            if t.len() >= 4 && tokens.contains(t) {
                basis.push(format!("命中身份词 [{t}]"));
                identity_hits += 1;
            }
        }
    }
    if identity_hits == 0 && rule.is_none() {
        basis.push("文件名与压缩包清单中均未出现本机型号/compatible 标识".into());
    }

    // 第 3 层：Target / Subtarget（必须区分，不混用）
    if !dev.target.is_empty() && text.contains(&norm(&dev.target)) {
        score += 15;
        basis.push(format!("命中 Target [{}]", dev.target));
    }
    if !dev.subtarget.is_empty() && text.contains(&norm(&dev.subtarget)) {
        score += 15;
        basis.push(format!("命中 Subtarget [{}]", dev.subtarget));
    }

    // 第 4 层：CPU 架构与 ABI —— 冲突是硬性否决
    let dev_arch = norm(&format!("{} {} {}", dev.cpu_arch, dev.pkg_arch, dev.abi));
    let asset_arch = norm(&format!("{} {}", f.cpu_arch, f.abi));
    let arch_conflict = detect_arch_conflict(&dev_arch, &asset_arch);
    if arch_conflict {
        conflicts.push(format!(
            "架构冲突：固件声明 [{}]，本机为 [{}]",
            if asset_arch.is_empty() { "未知" } else { &asset_arch },
            dev_arch
        ));
        score -= 120;
    } else if !asset_arch.is_empty() && !dev_arch.is_empty() {
        score += 25;
        basis.push(format!("架构信息一致/兼容（固件 [{}]，本机 [{}]）", asset_arch, dev_arch));
    } else {
        basis.push("文件名未声明架构，无法据此确认架构兼容".into());
        score -= 10;
    }

    // 第 5 层：镜像类型与版本
    if f.kind == FileKind::Sysupgrade {
        score += 20;
        basis.push("镜像类型为 sysupgrade，适合在线升级".into());
    } else if f.kind == FileKind::Factory {
        score -= 40;
        conflicts.push("factory/initramfs 镜像不是在线升级包".into());
    } else if f.kind == FileKind::FullImage {
        score += 5;
        basis.push("完整固件镜像（需按设备升级方式确认）".into());
    }
    if !f.version.is_empty() && rule.is_some() {
        score += 5;
    }

    // 用户显式规则：意图声明，命中即视为可用（架构冲突仍否决）
    if let Some(r) = rule {
        if let Ok(re) = regex::Regex::new(r) {
            if re.is_match(&f.name) {
                score += 60;
                basis.push(format!("配置规则 [{r}] 命中该文件"));
            }
        }
    }

    let level = if arch_conflict || score < -50 {
        MatchLevel::Mismatch
    } else if identity_hits > 0 && score >= 45 {
        MatchLevel::High
    } else if identity_hits > 0 || score >= 25 {
        MatchLevel::Partial
    } else if rule.is_some() || !f.inner_files.is_empty() {
        MatchLevel::NeedVerify
    } else {
        MatchLevel::Unrecognized
    };

    MatchResult {
        level,
        score,
        basis,
        conflicts,
        allow_next: !matches!(level, MatchLevel::Mismatch),
    }
}

/// 架构家族冲突检测（只比较家族，不做机型特判）。
fn detect_arch_conflict(device: &str, asset: &str) -> bool {
    if device.is_empty() || asset.is_empty() {
        return false;
    }
    let fam = |s: &str| -> u8 {
        if s.contains("aarch64") || s.contains("arm64") { 1 }
        else if s.contains("x86") || s.contains("amd64") || s.contains("i386") { 2 }
        else if s.contains("mips") { 3 }
        else if s.contains("armv7") || s.contains("armhf") || s.contains("cortex-a7") { 4 }
        else { 0 }
    };
    let (d, a) = (fam(device), fam(asset));
    d != 0 && a != 0 && d != a
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

    #[test]
    fn format_detection_covers_required_extensions() {
        let cases = [
            ("fw.bin", FwFormat::Bin), ("fw.img", FwFormat::Img),
            ("fw.img.gz", FwFormat::Gz), ("fw.tar.gz", FwFormat::TarGz),
            ("fw.tar.xz", FwFormat::TarXz), ("fw.tar.zst", FwFormat::TarZst),
            ("fw.zip", FwFormat::Zip), ("fw.tar", FwFormat::Tar),
            ("fw.xz", FwFormat::Xz), ("fw.zst", FwFormat::Zst),
        ];
        for (n, want) in cases {
            assert_eq!(detect_format(n), want, "{n}");
        }
    }

    #[test]
    fn checksum_and_log_are_not_flashable() {
        assert_eq!(classify_kind("sha256sums"), FileKind::Checksum);
        assert_eq!(classify_kind("fw.bin.sha256"), FileKind::Checksum);
        assert_eq!(classify_kind("build.log"), FileKind::BuildLog);
        assert_eq!(classify_kind("openwrt-sdk.tar.xz"), FileKind::Sdk);
        assert_eq!(classify_kind("toolchain.tar.gz"), FileKind::Toolchain);
        assert_eq!(classify_kind("packages.tar.gz"), FileKind::Packages);
        assert_eq!(classify_kind("u-boot.bin"), FileKind::Uboot);
        assert!(!classify_kind("sha256sums").is_flashable_candidate());
        assert!(!classify_kind("openwrt-sdk.tar.xz").is_flashable_candidate());
    }

    #[test]
    fn not_every_bin_is_a_firmware() {
        // 无镜像类型词的裸 .bin 归为 full image 候选但仍需设备匹配，
        // 校验文件/SDK/debug 则明确排除
        assert_eq!(classify_kind("random.bin"), FileKind::FullImage);
        assert_eq!(classify_kind("debug-symbols.bin"), FileKind::Debug);
        assert!(!classify_kind("debug-symbols.bin").is_flashable_candidate());
        assert!(!classify_kind("fw.zip").is_flashable_candidate(), "ZIP 不能直接视为固件");
    }

    #[test]
    fn sysupgrade_factory_initramfs_are_distinguished() {
        assert_eq!(classify_kind("H5000M-sysupgrade.bin"), FileKind::Sysupgrade);
        assert_eq!(classify_kind("H5000M-factory.bin"), FileKind::Factory);
        assert_eq!(classify_kind("H5000M-combined.img.gz"), FileKind::FullImage);
        // initramfs 也可能带 sysupgrade 词，sysupgrade 优先
        assert_eq!(classify_kind("x-initramfs-kernel.bin"), FileKind::Factory);
    }

    #[test]
    fn digest_from_release_asset_is_a_trusted_sha256() {
        let a = Value::Obj(vec![
            ("name".into(), Value::str("fw-sysupgrade.bin")),
            ("size".into(), Value::num(12345)),
            ("digest".into(), Value::str("sha256:8C17EEB1009C9C31420E038243EFF74588ABC833160476DB9C8DD74DAA301F0E")),
            ("browser_download_url".into(), Value::str("https://example/fw.bin")),
        ]);
        let f = from_release_asset(&a, "v1.2.3", "https://example/release");
        assert_eq!(f.sha256.len(), 64);
        assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(f.kind, FileKind::Sysupgrade);
    }

    #[test]
    fn safe_entry_name_blocks_traversal_and_absolute_paths() {
        for bad in ["../etc/passwd", "/etc/passwd", "a/../../b", "C:\\boot", "a\\b", "x\0y"] {
            assert!(safe_entry_name(bad).is_err(), "{bad} 必须拒绝");
        }
        assert!(safe_entry_name("firmware/H5000M-sysupgrade.bin").is_ok());
    }

    fn make_zip(entries: &[(&str, u32, u32)]) -> Vec<u8> {
        // 构造最小 ZIP：本地头 + 数据 + 中央目录 + EOCD（内容为占位）
        let mut b = Vec::new();
        let mut central = Vec::new();
        let mut offset: u32 = 0;
        for (name, comp, uncomp) in entries {
            let nb = name.as_bytes();
            let local: Vec<u8> = [
                &0x04034b50u32.to_le_bytes()[..], &20u16.to_le_bytes()[..],
                &0u16.to_le_bytes()[..], &0u16.to_le_bytes()[..],
                &0u16.to_le_bytes()[..], &0u16.to_le_bytes()[..], &0u32.to_le_bytes()[..],
                &comp.to_le_bytes()[..], &uncomp.to_le_bytes()[..],
                &(nb.len() as u16).to_le_bytes()[..], &0u16.to_le_bytes()[..],
            ].concat();
            let mut l = local;
            l.extend_from_slice(nb);
            let start = offset;
            b.extend_from_slice(&l);
            offset += l.len() as u32;
            let mut c: Vec<u8> = [
                &0x02014b50u32.to_le_bytes()[..], &20u16.to_le_bytes()[..],
                &20u16.to_le_bytes()[..], &0u16.to_le_bytes()[..],
                &0u16.to_le_bytes()[..], &0u16.to_le_bytes()[..], &0u16.to_le_bytes()[..],
                &0u32.to_le_bytes()[..], &comp.to_le_bytes()[..], &uncomp.to_le_bytes()[..],
                &(nb.len() as u16).to_le_bytes()[..], &0u16.to_le_bytes()[..],
                &0u16.to_le_bytes()[..], &0u16.to_le_bytes()[..], &0u16.to_le_bytes()[..],
                &0u32.to_le_bytes()[..], &start.to_le_bytes()[..],
            ].concat();
            c.extend_from_slice(nb);
            central.extend_from_slice(&c);
        }
        let cd_off = offset;
        let cd_size = central.len() as u32;
        b.extend_from_slice(&central);
        let eocd: Vec<u8> = [
            &0x06054b50u32.to_le_bytes()[..], &0u16.to_le_bytes()[..],
            &0u16.to_le_bytes()[..], &(entries.len() as u16).to_le_bytes()[..],
            &(entries.len() as u16).to_le_bytes()[..], &cd_size.to_le_bytes()[..],
            &cd_off.to_le_bytes()[..], &0u16.to_le_bytes()[..],
        ].concat();
        b.extend_from_slice(&eocd);
        b
    }

    #[test]
    fn zip_central_directory_is_parsed_without_extraction() {
        // 固件通常以 store 模式打包（压缩比≈1），这里用真实比例构造，
        // 否则会命中解压炸弹检测（那属于另一条测试用例）。
        let z = make_zip(&[
            ("H5000M-sysupgrade.bin", 76_000_000, 76_000_000),
            ("sha256sums", 113, 113),
        ]);
        let entries = inspect_zip(&z).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, FileKind::Sysupgrade);
        assert_eq!(entries[1].kind, FileKind::Checksum);
        assert_eq!(flashable_from_entries(&entries).len(), 1);
    }

    #[test]
    fn zip_path_traversal_entry_is_rejected() {
        let z = make_zip(&[("../../etc/passwd", 10, 10)]);
        assert!(matches!(inspect_zip(&z), Err(ArchiveError::UnsafePath(_))));
    }

    #[test]
    fn zip_decompression_bomb_is_rejected() {
        let z = make_zip(&[("huge.bin", 1, 100_000)]);
        assert_eq!(inspect_zip(&z).unwrap_err(), ArchiveError::DecompressionBomb);
        // 单条目未压缩体积超上限同样拦截
        let z2 = make_zip(&[("giant.bin", 100, MAX_SINGLE_UNCOMPRESSED as u32 + 1)]);
        assert_eq!(inspect_zip(&z2).unwrap_err(), ArchiveError::DecompressionBomb);
    }

    #[test]
    fn high_confidence_match_for_own_device() {
        let mut f = FirmwareFile::new("H5000M-aarch64-26.11.13-sysupgrade.bin", FileSource::ReleaseAsset);
        let (a, abi) = infer_arch_hints(&f.name);
        f.cpu_arch = a; f.abi = abi;
        let r = match_device(&dev(), &f, None);
        assert_eq!(r.level, MatchLevel::High, "{:?} {:?}", r.level, r.basis);
        assert!(r.allow_next);
        assert!(!r.basis.is_empty());
    }

    #[test]
    fn wrong_arch_is_mismatch_not_unconfirmed() {
        let mut f = FirmwareFile::new("H5000M-mipsel-24kc-sysupgrade.bin", FileSource::ReleaseAsset);
        let (a, abi) = infer_arch_hints(&f.name);
        f.cpu_arch = a; f.abi = abi;
        let r = match_device(&dev(), &f, None);
        assert_eq!(r.level, MatchLevel::Mismatch);
        assert!(!r.allow_next);
        assert!(r.conflicts.iter().any(|s| s.contains("架构冲突")));
    }

    #[test]
    fn unrecognized_file_is_not_mismatch() {
        let f = FirmwareFile::new("somefile.bin", FileSource::Artifact);
        let r = match_device(&dev(), &f, None);
        // 无身份命中、无规则 → 无法识别，绝不是"设备不兼容"
        assert_eq!(r.level, MatchLevel::Unrecognized);
        assert!(r.allow_next, "无法识别应允许进一步验证，而非直接拒绝设备");
    }

    #[test]
    fn checksum_asset_is_not_a_flashable_candidate() {
        let f = FirmwareFile::new("sha256sums", FileSource::ReleaseAsset);
        let r = match_device(&dev(), &f, None);
        assert_eq!(r.level, MatchLevel::Mismatch);
        assert!(!r.allow_next);
    }
}
