// 插件自身版本检测与原地更新。
//
// 与固件升级完全解耦：
//   - 固件升级读取 UCI 里的固件仓库，写 /tmp/online-upgrade-status.json；
//   - 插件更新固定读取本插件仓库（UCI `plugin_repo`），写
//     /tmp/online-upgrade-plugin-status.json，不会覆盖固件任务状态。
//
// 发布约定（CI 与 README 同步维护）：
//   标签      v<MAJOR>.<MINOR>.<PATCH>，稳定版，禁止预发布标签
//   安装包    luci-app-online-upgrade_<版本>-r<发布号>_<架构>.apk
//   清单      online-upgrade-manifest.json（固定文件名，包含版本、架构、SHA256）
//   校验单    SHA256SUMS
//
// 自动更新只接受「清单 + SHA256」同时可验证的产物；缺任何一项都会
// 拒绝安装，而不是退回“下载了就装”的不安全路径。

use crate::compat::DeviceProfile;
use crate::downloader::{self, DownloadOptions, Progress};
use crate::github::{self, Cfg};
use crate::json::{parse, Value};
use crate::taskctl::{DownloadControl, Task, TaskStore, KIND_PLUGIN, ST_DONE, ST_FAILED};
use crate::{logline, uci};
use regex::Regex;
use std::fs;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 外部命令（apk / opkg）的默认等待上限。
///
/// 软件包管理器可能因为 `/lib/apk/db/lock` 被占用、等待仓库索引或
/// 网络不可达而长时间阻塞。旧实现使用无超时的 `Command::output()`，
/// 一旦阻塞，插件更新状态就会永久停留在「安装中」。这里统一加上限，
/// 超时即中止并把明确原因写回状态文件。
const CMD_TIMEOUT_INSTALL: Duration = Duration::from_secs(300);
const CMD_TIMEOUT_QUERY: Duration = Duration::from_secs(20);
const CMD_TIMEOUT_PROBE: Duration = Duration::from_secs(5);

/// 带超时执行外部命令。
///
/// stdout/stderr 直接重定向到临时文件而不是管道：管道写满会让子进程
/// 阻塞在 write()，反而制造出与「锁等待」一模一样的假死现象。
fn run_cmd_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(ExitStatus, String), String> {
    let out_path = format!("/tmp/online-upgrade-exec-{}.log", std::process::id());
    let f_out = fs::File::create(&out_path).map_err(|e| format!("创建临时输出文件失败: {e}"))?;
    let f_err = f_out
        .try_clone()
        .map_err(|e| format!("复制输出句柄失败: {e}"))?;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(f_out))
        .stderr(Stdio::from(f_err))
        .spawn()
        .map_err(|e| format!("调用 {program} 失败: {e}"))?;

    let started = Instant::now();
    let mut timed_out = false;
    let mut status: Option<ExitStatus> = None;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                status = Some(st);
                break;
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    // 先 kill 再 wait，避免留下僵尸进程占用 pid。
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&out_path);
                return Err(format!("等待 {program} 结束失败: {e}"));
            }
        }
    }

    let text = fs::read_to_string(&out_path).unwrap_or_default();
    let _ = fs::remove_file(&out_path);

    if timed_out {
        let brief: String = text
            .lines()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(format!(
            "{program} 超过 {} 秒未完成，已中止。常见原因是软件包数据库锁被其他进程占用、\
             等待仓库索引或网络不可达。{}",
            timeout.as_secs(),
            if brief.trim().is_empty() {
                "（该命令无任何输出）".to_string()
            } else {
                format!("最后输出：{brief}")
            }
        ));
    }

    Ok((status.expect("非超时路径必有退出状态"), text))
}

pub const PACKAGE: &str = "luci-app-online-upgrade";
pub const DEFAULT_REPO: &str = "LianXia233/luci-app-online-upgrade";
pub const STATUS_FILE: &str = "/tmp/online-upgrade-plugin-status.json";
const VERSION_FILE: &str = "/usr/share/online-upgrade/version";
const MANIFEST_NAME: &str = "online-upgrade-manifest.json";
const CHECKSUMS_NAME: &str = "SHA256SUMS";
const MAX_PKG_BYTES: u64 = 64 * 1024 * 1024;

/// 临时安装包必须保留真实扩展名：apk/opkg 会依据扩展名与文件头
/// 判断包格式，统一的 `.pkg` 后缀在部分版本上会被直接拒绝。
fn tmp_pkg_path(file: &str) -> String {
    let ext = if ext_of(file) == "ipk" { "ipk" } else { "apk" };
    format!("/tmp/online-upgrade-plugin.{ext}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Semver {
    major: u64,
    minor: u64,
    patch: u64,
}

impl std::fmt::Display for Semver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone)]
struct LocalInfo {
    display: String,
    version: Option<Semver>,
    pkg_release: String,
    source: &'static str,
    source_text: String,
}

#[derive(Debug, Clone)]
struct ManifestAsset {
    arch: String,
    manager: String,
    file: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Clone)]
struct Plan {
    local: LocalInfo,
    latest: Semver,
    latest_tag: String,
    release_url: String,
    published_at: String,
    asset: ManifestAsset,
    asset_url: String,
    asset_api_url: String,
    manifest_used: bool,
}

fn repo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$").unwrap())
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn plugin_repo() -> String {
    let raw = uci::get_or("plugin_repo", DEFAULT_REPO);
    if repo_re().is_match(&raw) {
        raw
    } else {
        logline!("[plugin] plugin_repo 非法，回退到默认仓库 {DEFAULT_REPO}");
        DEFAULT_REPO.to_string()
    }
}

/// 解析严格的 `vX.Y.Z` 标签；预发布、四段版本与日期标签一律不接受，
/// 保证「最新插件版本」不会被测试标签或旧固件命名习惯污染。
fn tag_version(tag: &str) -> Option<Semver> {
    let t = tag.strip_prefix('v')?;
    if t.contains('-') || t.contains('+') || t.contains('_') {
        return None;
    }
    // 日期型标签（v26.11.10）虽然形似三段版本，但它属于固件构建号，
    // 不能参与插件版本比较：26.11.10 会永远大于 2.5.0。
    if crate::version::pick(tag)
        .map(|v| v.is_date_tag())
        .unwrap_or(false)
    {
        return None;
    }
    let parts: Vec<&str> = t.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut nums = [0u64; 3];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        nums[i] = p.parse().ok()?;
    }
    Some(Semver {
        major: nums[0],
        minor: nums[1],
        patch: nums[2],
    })
}

fn parse_semver(text: &str) -> Option<Semver> {
    let v = crate::version::pick(text)?;
    if v.nums.len() != 3 || v.pre.is_some() {
        return None;
    }
    Some(Semver {
        major: v.nums[0],
        minor: v.nums[1],
        patch: v.nums[2],
    })
}

fn split_pkg_release(raw: &str) -> (String, String) {
    let raw = raw.trim();
    if let Some(pos) = raw.rfind("-r") {
        let (head, tail) = raw.split_at(pos);
        let rel = &tail[2..];
        if !rel.is_empty() && rel.chars().all(|c| c.is_ascii_digit()) {
            return (head.to_string(), rel.to_string());
        }
    }
    (raw.to_string(), String::new())
}

fn detect_manager() -> Option<&'static str> {
    for mgr in ["apk", "opkg"] {
        // 探测同样要限时：apk 在数据库锁被占用时连 --version 都可能阻塞。
        if let Ok((st, _)) = run_cmd_timeout(mgr, &["--version"], CMD_TIMEOUT_PROBE) {
            if st.success() {
                return Some(mgr);
            }
        }
    }
    None
}

fn parse_pkg_output(text: &str) -> Option<String> {
    let prefix = format!("{PACKAGE}-");
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
        if let Some(rest) = line.strip_prefix("Version:") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn installed_pkg_version(manager: &str) -> Option<String> {
    let args: Vec<&str> = if manager == "apk" {
        vec!["info", "-v", PACKAGE]
    } else {
        vec!["info", PACKAGE]
    };
    // 安装刚结束时 apk 数据库可能仍被持有；无超时会让「安装完成后的
    // 版本复核」永久阻塞，状态于是停在「安装中」。这里改为限时查询。
    let (status, text) = run_cmd_timeout(manager, &args, CMD_TIMEOUT_QUERY).ok()?;
    if !status.success() {
        return None;
    }
    parse_pkg_output(&text)
}

fn detect_local() -> LocalInfo {
    if let Ok(raw) = fs::read_to_string(VERSION_FILE) {
        let raw = raw.trim();
        if !raw.is_empty() {
            let (ver_text, rel) = split_pkg_release(raw);
            return LocalInfo {
                display: raw.to_string(),
                version: parse_semver(&ver_text),
                pkg_release: rel,
                source: "version_file",
                source_text: format!("安装包写入的版本文件 {VERSION_FILE}"),
            };
        }
    }

    if let Some(mgr) = detect_manager() {
        if let Some(raw) = installed_pkg_version(mgr) {
            let (ver_text, rel) = split_pkg_release(&raw);
            return LocalInfo {
                display: raw,
                version: parse_semver(&ver_text),
                pkg_release: rel,
                source: "package_manager",
                source_text: format!("{mgr} 已安装软件包版本"),
            };
        }
    }

    let raw = env!("CARGO_PKG_VERSION").to_string();
    LocalInfo {
        version: parse_semver(&raw),
        display: raw,
        pkg_release: String::new(),
        source: "binary",
        source_text: "当前运行二进制的编译版本（弱依据）".into(),
    }
}

fn local_json(local: &LocalInfo) -> Value {
    Value::Obj(vec![
        ("display".into(), Value::str(&local.display)),
        (
            "version".into(),
            match &local.version {
                Some(v) => Value::str(&v.to_string()),
                None => Value::Null,
            },
        ),
        ("pkg_release".into(), Value::str(&local.pkg_release)),
        ("source".into(), Value::str(local.source)),
        ("source_text".into(), Value::str(&local.source_text)),
    ])
}

fn write_status(state: &str, pct: u32, msg: &str, error: Option<&str>, result: Option<Value>) {
    let mut pairs = vec![
        ("state".into(), Value::str(state)),
        ("pct".into(), Value::num(pct as u64)),
        ("msg".into(), Value::str(msg)),
        (
            "error".into(),
            match error {
                Some(e) => Value::str(e),
                None => Value::Null,
            },
        ),
        ("ts".into(), Value::num(now_ts())),
        ("pid".into(), Value::num(std::process::id() as u64)),
    ];
    if let Some(r) = result {
        pairs.push(("result".into(), r));
    }
    let body = Value::Obj(pairs).dump();
    let tmp = format!("{STATUS_FILE}.tmp");
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, STATUS_FILE);
    }
}

/// 对插件任务记录做一次原子更新。
fn plugin_task_update(id: &str, f: impl FnOnce(&mut Task)) {
    let mut store = TaskStore::open();
    if let Some(t) = store.get_mut(id) {
        f(t);
    }
    let _ = store.save();
}

/// 将插件任务标记为终态。
fn plugin_task_finish(id: &str, state: &str, reason: &str, done: u64, speed: u64) {
    plugin_task_update(id, |t| {
        t.done = done;
        t.speed_bps = speed;
        t.finish(state, reason);
    });
}

/// 在下载开始前创建用户可见的插件更新任务。任务与固件下载共用
/// `/root/.online-upgrade-tasks.json`，通过 `kind=plugin` 区分。
fn create_plugin_task(plan: &Plan) -> String {
    let mut store = TaskStore::open();
    let mut t = Task::new(&plan.asset.file, &plan.asset_url, plan.asset.size)
        .with_kind(KIND_PLUGIN);
    t.reason = format!("插件更新 {} → v{}", plan.local.display, plan.latest);
    let id = t.id.clone();
    store.upsert(t);
    let _ = store.save();
    logline!("[plugin] 已创建插件更新任务 {id}");
    id
}

/// `plugin-info`：纯本地信息，不访问网络，供页面初始化使用。
pub fn info() -> Value {
    let local = detect_local();
    let manager = detect_manager().unwrap_or("");
    let device = DeviceProfile::current();
    let supported = !manager.is_empty() && local.version.is_some();
    // 资源缓存戳与模板中的 style.css?v= / app.js?v= 使用同一个
    // PKG_VERSION；自更新安装后版本文件变化，前端可据此判断
    // 当前页面是否仍是旧资源。
    let cache_version = local
        .version
        .map(|v| v.to_string())
        .unwrap_or_else(|| local.display.clone());
    let reason = if manager.is_empty() {
        "本机未检测到 apk 或 opkg，无法自动安装插件更新".to_string()
    } else if local.version.is_none() {
        format!(
            "无法从版本文件或 {manager} 识别当前插件版本（当前标记 {}），已禁用自动更新",
            local.display
        )
    } else {
        format!("将使用 {manager} 安装与 {} 架构匹配的插件包", device.pkg_arch)
    };
    Value::Obj(vec![
        ("ok".into(), Value::Bool(true)),
        ("package".into(), Value::str(PACKAGE)),
        ("repo".into(), Value::str(&plugin_repo())),
        ("manager".into(), Value::str(manager)),
        ("pkg_arch".into(), Value::str(&device.pkg_arch)),
        ("arch".into(), Value::str(&device.arch)),
        ("arch_family".into(), Value::str(device.arch_family().label())),
        ("supported".into(), Value::Bool(supported)),
        ("reason".into(), Value::str(&reason)),
        ("cache_version".into(), Value::str(&cache_version)),
        ("local".into(), local_json(&local)),
        (
            "convention".into(),
            Value::Obj(vec![
                ("tag".into(), Value::str("v<MAJOR>.<MINOR>.<PATCH>")),
                (
                    "asset".into(),
                    Value::str("luci-app-online-upgrade_<版本>-r<发布号>_<架构>.apk"),
                ),
                ("manifest".into(), Value::str(MANIFEST_NAME)),
                ("checksums".into(), Value::str(CHECKSUMS_NAME)),
            ]),
        ),
    ])
}

fn release_asset<'a>(release: &'a Value, name: &str) -> Option<&'a Value> {
    release
        .get("assets")
        .and_then(|a| a.as_arr())?
        .iter()
        .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(name))
}

fn asset_field(a: &Value, key: &str) -> String {
    a.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn asset_size(a: &Value) -> u64 {
    a.get("size").and_then(|v| v.as_u64()).unwrap_or(0)
}

fn bool_field(v: &Value, key: &str) -> bool {
    matches!(v.get(key), Some(Value::Bool(true)))
}

fn norm_arch(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace('-', "_")
}

fn arch_family(arch: &str) -> &'static str {
    let a = norm_arch(arch);
    if a.starts_with("aarch64") || a.starts_with("arm64") {
        "aarch64"
    } else if a.starts_with("x86_64") || a.starts_with("x86") || a.starts_with("amd64") {
        "x86_64"
    } else if a.starts_with("mips") {
        "mips"
    } else if a.starts_with("arm") {
        "arm32"
    } else {
        "unknown"
    }
}

fn ext_of(name: &str) -> &'static str {
    if name.to_ascii_lowercase().ends_with(".apk") {
        "apk"
    } else if name.to_ascii_lowercase().ends_with(".ipk") {
        "ipk"
    } else {
        ""
    }
}

fn manager_of(asset: &ManifestAsset) -> &str {
    if !asset.manager.is_empty() {
        &asset.manager
    } else {
        match ext_of(&asset.file) {
            "apk" => "apk",
            "ipk" => "opkg",
            _ => "",
        }
    }
}

fn asset_usable(asset: &ManifestAsset, manager: &str) -> bool {
    !asset.file.is_empty()
        && !asset.file.contains('/')
        && asset.sha256.len() == 64
        && asset.sha256.chars().all(|c| c.is_ascii_hexdigit())
        && manager_of(asset) == manager
        && !ext_of(&asset.file).is_empty()
}

fn select_asset(
    assets: &[ManifestAsset],
    manager: &str,
    device: &DeviceProfile,
) -> Result<(ManifestAsset, String), String> {
    let pool: Vec<&ManifestAsset> = assets
        .iter()
        .filter(|a| asset_usable(a, manager))
        .collect();
    if pool.is_empty() {
        return Err(format!(
            "清单中没有适用于 {manager} 的安装包（本机架构 {}）",
            device.pkg_arch
        ));
    }

    let dev_arch = norm_arch(&device.pkg_arch);
    let exact: Vec<&&ManifestAsset> = pool
        .iter()
        .filter(|a| norm_arch(&a.arch) == dev_arch)
        .collect();
    if exact.len() == 1 {
        return Ok(((*exact[0]).clone(), format!("架构精确匹配 {dev_arch}")));
    }
    if exact.len() > 1 {
        let names: Vec<String> = exact.iter().map(|a| a.file.clone()).collect();
        return Err(format!(
            "清单中有多个适用于 {dev_arch} 的安装包，无法安全选择：{}",
            names.join(", ")
        ));
    }

    let fam = device.arch_family().label();
    if fam != "unknown" {
        let family: Vec<&&ManifestAsset> = pool
            .iter()
            .filter(|a| arch_family(&a.arch) == fam)
            .collect();
        if family.len() == 1 {
            return Ok((
                (*family[0]).clone(),
                format!("架构家族匹配 {fam}（本机 {dev_arch}）"),
            ));
        }
        if family.len() > 1 {
            let names: Vec<String> = family.iter().map(|a| a.file.clone()).collect();
            return Err(format!(
                "清单中有多个 {fam} 架构安装包，无法确认与本机 {dev_arch} 匹配：{}",
                names.join(", ")
            ));
        }
    }

    let all: Vec<&&ManifestAsset> = pool
        .iter()
        .filter(|a| norm_arch(&a.arch) == "all" || a.arch.is_empty())
        .collect();
    if all.len() == 1 {
        return Ok(((*all[0]).clone(), "使用无架构限制（all）安装包".into()));
    }

    let names: Vec<String> = pool.iter().map(|a| a.file.clone()).collect();
    Err(format!(
        "清单中没有与本机架构 {dev_arch} 匹配的安装包，候选：{}",
        names.join(", ")
    ))
}

fn parse_manifest(text: &str, latest: &Semver, tag: &str) -> Result<Vec<ManifestAsset>, String> {
    let root = parse(text).map_err(|e| format!("manifest JSON 解析失败: {e}"))?;
    let schema = root.get("schema").and_then(|v| v.as_u64()).unwrap_or(0);
    if schema != 1 {
        return Err(format!("manifest schema 不受支持: {schema}"));
    }
    if let Some(pkg) = root.get("package").and_then(|v| v.as_str()) {
        if pkg != PACKAGE {
            return Err(format!("manifest 包名不匹配: {pkg}"));
        }
    }
    if let Some(v) = root.get("version").and_then(|v| v.as_str()) {
        if parse_semver(v) != Some(*latest) {
            return Err(format!("manifest 版本 {v} 与标签 {tag} 不一致"));
        }
    }
    if let Some(t) = root.get("tag").and_then(|v| v.as_str()) {
        if !t.is_empty() && t != tag {
            return Err(format!("manifest 标签 {t} 与 Release {tag} 不一致"));
        }
    }
    let arr = root
        .get("assets")
        .and_then(|v| v.as_arr())
        .ok_or_else(|| "manifest 缺少 assets 数组".to_string())?;
    let mut out = Vec::new();
    for a in arr {
        let asset = ManifestAsset {
            arch: asset_field(a, "arch"),
            manager: asset_field(a, "manager"),
            file: asset_field(a, "file"),
            sha256: asset_field(a, "sha256").to_ascii_lowercase(),
            size: asset_size(a),
        };
        if asset.file.is_empty() {
            return Err("manifest 条目缺少 file".into());
        }
        if asset.sha256.len() != 64
            || !asset.sha256.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(format!("manifest 条目 {} 缺少合法 SHA256", asset.file));
        }
        out.push(asset);
    }
    if out.is_empty() {
        return Err("manifest 未声明任何安装包".into());
    }
    Ok(out)
}

/// 没有 manifest 时的兜底：直接从 Release 资产名与 SHA256SUMS 构造候选。
/// 仍然要求 SHA256 可验证，绝不降级为「无校验安装」。
async fn fallback_assets(
    client: &reqwest::Client,
    cfg: &Cfg,
    release: &Value,
    manager: &str,
    device: &DeviceProfile,
) -> Result<Vec<ManifestAsset>, String> {
    let sums = match release_asset(release, CHECKSUMS_NAME) {
        Some(a) => {
            let url = asset_field(a, "browser_download_url");
            match github::fetch_text(client, &url, cfg).await {
                Some(t) => t,
                None => return Err(format!("无法读取 {CHECKSUMS_NAME}")),
            }
        }
        None => return Err("Release 缺少 online-upgrade-manifest.json 与 SHA256SUMS".into()),
    };

    let want_ext = if manager == "apk" { "apk" } else { "ipk" };
    let assets = release
        .get("assets")
        .and_then(|a| a.as_arr())
        .ok_or_else(|| "Release 无资产".to_string())?;

    let mut out = Vec::new();
    for a in assets {
        let name = asset_field(a, "name");
        if ext_of(&name) != want_ext {
            continue;
        }
        let lower = norm_arch(&name);
        let arch_ok = lower.contains(&norm_arch(&device.pkg_arch))
            || (device.arch_family().label() != "unknown"
                && lower.contains(device.arch_family().label()));
        if !arch_ok {
            continue;
        }
        let sha = crate::hash::find_sha256_in_text(&sums, &name)
            .ok_or_else(|| format!("{CHECKSUMS_NAME} 中没有 {name} 的校验值"))?;
        out.push(ManifestAsset {
            arch: device.pkg_arch.clone(),
            manager: manager.to_string(),
            file: name,
            sha256: sha,
            size: asset_size(a),
        });
    }
    if out.is_empty() {
        return Err(format!(
            "未找到适用于 {manager}/{0} 的插件安装包",
            device.pkg_arch
        ));
    }
    Ok(out)
}

fn release_json(rel: &Value) -> (String, String, String, String) {
    let tag = asset_field(rel, "tag_name");
    let published = asset_field(rel, "published_at");
    let url = asset_field(rel, "html_url");
    let name = asset_field(rel, "name");
    (tag, published, url, name)
}

fn notes_of(rel: &Value) -> String {
    let body = rel.get("body").and_then(|b| b.as_str()).unwrap_or("");
    let mut s: String = body.chars().take(1200).collect();
    if body.chars().count() > 1200 {
        s.push('…');
    }
    s
}

fn error_json(repo: &str, local: &LocalInfo, msg: &str) -> Value {
    Value::Obj(vec![
        ("ok".into(), Value::Bool(false)),
        ("state".into(), Value::str("error")),
        ("update_available".into(), Value::Bool(false)),
        ("repo".into(), Value::str(repo)),
        ("local".into(), local_json(local)),
        ("latest".into(), Value::Null),
        ("asset".into(), Value::Null),
        ("reason".into(), Value::str(msg)),
        ("checked_ts".into(), Value::num(now_ts())),
    ])
}

struct Outcome {
    json: Value,
    plan: Option<Plan>,
}

/// 联网检查插件最新版本。返回可直接写入状态文件的结果对象；
/// 只有确定「有新版本且安装包可验证」时才附带更新计划。
async fn check(client: &reqwest::Client) -> Outcome {
    let repo = plugin_repo();
    let local = detect_local();
    let manager = match detect_manager() {
        Some(m) => m,
        None => {
            return Outcome {
                json: error_json(&repo, &local, "本机未检测到 apk 或 opkg，无法自动更新插件"),
                plan: None,
            }
        }
    };
    let local_ver = match local.version {
        Some(v) => v,
        None => {
            return Outcome {
                json: error_json(
                    &repo,
                    &local,
                    &format!(
                        "无法识别本机插件版本（当前标记 {}），已停止自动更新；请用 {manager} 手动安装",
                        local.display
                    ),
                ),
                plan: None,
            }
        }
    };

    let fw = github::load_cfg();
    let cfg = Cfg {
        repo: repo.clone(),
        tag: "latest".into(),
        proxy: fw.proxy,
        pattern: String::new(),
        token: fw.token,
    };

    let api = format!("https://api.github.com/repos/{repo}/releases?per_page=30");
    let body = match github::api_fetch(client, &api, &cfg).await {
        Ok((_, b)) => b,
        Err(e) => {
            return Outcome {
                json: error_json(&repo, &local, &format!("获取插件 Release 失败: {e}")),
                plan: None,
            }
        }
    };
    let parsed = match parse(&body) {
        Ok(v) => v,
        Err(e) => {
            return Outcome {
                json: error_json(&repo, &local, &format!("Release JSON 解析失败: {e}")),
                plan: None,
            }
        }
    };
    let releases = match parsed.as_arr() {
        Some(a) => a,
        None => {
            return Outcome {
                json: error_json(&repo, &local, "Release 列表格式异常"),
                plan: None,
            }
        }
    };

    let mut best: Option<(Semver, String, Value)> = None;
    for rel in releases {
        if bool_field(rel, "draft") || bool_field(rel, "prerelease") {
            continue;
        }
        let tag = asset_field(rel, "tag_name");
        let Some(v) = tag_version(&tag) else {
            continue;
        };
        if best.as_ref().map(|(bv, _, _)| v > *bv).unwrap_or(true) {
            best = Some((v, tag, rel.clone()));
        }
    }
    let Some((latest, latest_tag, release)) = best else {
        return Outcome {
            json: error_json(
                &repo,
                &local,
                "未找到符合 vMAJOR.MINOR.PATCH 命名的稳定 Release，请检查发布标签规范",
            ),
            plan: None,
        };
    };

    let (tag_name, published, html_url, rel_name) = release_json(&release);
    let release_url = if html_url.is_empty() {
        format!("https://github.com/{repo}/releases/tag/{tag_name}")
    } else {
        html_url
    };
    let notes = notes_of(&release);
    let latest_json = |asset: Option<&ManifestAsset>, extra: Option<&str>| {
        Value::Obj(vec![
            ("version".into(), Value::str(&latest.to_string())),
            ("tag".into(), Value::str(&tag_name)),
            ("name".into(), Value::str(&rel_name)),
            ("published_at".into(), Value::str(&github::utc_to_local(&published))),
            ("release_url".into(), Value::str(&release_url)),
            ("notes".into(), Value::str(&notes)),
            (
                "asset".into(),
                match asset {
                    Some(a) => Value::Obj(vec![
                        ("name".into(), Value::str(&a.file)),
                        ("arch".into(), Value::str(&a.arch)),
                        ("size".into(), Value::num(a.size)),
                        ("size_human".into(), Value::str(&github::human_size(a.size))),
                        ("sha256".into(), Value::str(&a.sha256)),
                    ]),
                    None => Value::Null,
                },
            ),
            (
                "selection".into(),
                match extra {
                    Some(s) => Value::str(s),
                    None => Value::Null,
                },
            ),
        ])
    };

    if latest <= local_ver {
        let json = Value::Obj(vec![
            ("ok".into(), Value::Bool(true)),
            ("state".into(), Value::str("up_to_date")),
            ("update_available".into(), Value::Bool(false)),
            ("repo".into(), Value::str(&repo)),
            ("local".into(), local_json(&local)),
            ("latest".into(), latest_json(None, None)),
            ("asset".into(), Value::Null),
            (
                "reason".into(),
                Value::str(&format!(
                    "插件已是最新版本 v{latest}（当前 {}）",
                    local.display
                )),
            ),
            ("checked_ts".into(), Value::num(now_ts())),
        ]);
        return Outcome { json, plan: None };
    }

    // 新版本：优先读取固定名称的 manifest。
    let mut manifest_used = false;
    let assets: Vec<ManifestAsset> = if let Some(ma) = release_asset(&release, MANIFEST_NAME) {
        let murl = asset_field(ma, "browser_download_url");
        match github::fetch_text(client, &murl, &cfg).await {
            Some(txt) => match parse_manifest(&txt, &latest, &tag_name) {
                Ok(a) => {
                    manifest_used = true;
                    a
                }
                Err(e) => {
                    return Outcome {
                        json: error_json(&repo, &local, &format!("插件清单不可用: {e}")),
                        plan: None,
                    }
                }
            },
            None => {
                return Outcome {
                    json: error_json(&repo, &local, "无法下载 online-upgrade-manifest.json"),
                    plan: None,
                }
            }
        }
    } else {
        logline!("[plugin] 该 Release 缺少 manifest，改用 SHA256SUMS 兜底");
        match fallback_assets(client, &cfg, &release, manager, &DeviceProfile::current()).await {
            Ok(a) => a,
            Err(e) => {
                return Outcome {
                    json: error_json(&repo, &local, &e),
                    plan: None,
                }
            }
        }
    };

    let device = DeviceProfile::current();
    let (asset, selection) = match select_asset(&assets, manager, &device) {
        Ok(x) => x,
        Err(e) => {
            return Outcome {
                json: error_json(&repo, &local, &e),
                plan: None,
            }
        }
    };

    let rel_asset = match release_asset(&release, &asset.file) {
        Some(a) => a,
        None => {
            return Outcome {
                json: error_json(
                    &repo,
                    &local,
                    &format!("Release 中不存在清单声明的文件 {}", asset.file),
                ),
                plan: None,
            }
        }
    };
    let asset_url = asset_field(rel_asset, "browser_download_url");
    if asset_url.is_empty() {
        return Outcome {
            json: error_json(&repo, &local, &format!("{} 缺少下载地址", asset.file)),
            plan: None,
        };
    }
    let rel_size = asset_size(rel_asset);
    if asset.size > 0 && rel_size > 0 && asset.size != rel_size {
        return Outcome {
            json: error_json(
                &repo,
                &local,
                &format!(
                    "清单中 {} 的大小 {} 与 Release 实际大小 {} 不一致，已拒绝更新",
                    asset.file, asset.size, rel_size
                ),
            ),
            plan: None,
        };
    }

    let asset_api_url = asset_field(rel_asset, "url");
    let reason = format!(
        "发现插件新版本 v{latest}（当前 {}），可点击「更新插件」原地升级",
        local.display
    );
    let json = Value::Obj(vec![
        ("ok".into(), Value::Bool(true)),
        ("state".into(), Value::str("available")),
        ("update_available".into(), Value::Bool(true)),
        ("repo".into(), Value::str(&repo)),
        ("local".into(), local_json(&local)),
        ("latest".into(), latest_json(Some(&asset), Some(&selection))),
        (
            "asset".into(),
            Value::Obj(vec![
                ("name".into(), Value::str(&asset.file)),
                ("arch".into(), Value::str(&asset.arch)),
                ("manager".into(), Value::str(manager_of(&asset))),
                ("size".into(), Value::num(asset.size.max(rel_size))),
                (
                    "size_human".into(),
                    Value::str(&github::human_size(asset.size.max(rel_size))),
                ),
                ("sha256".into(), Value::str(&asset.sha256)),
                ("url".into(), Value::str(&asset_url)),
                ("source".into(), Value::str(if manifest_used { "manifest" } else { "sha256sums" })),
            ]),
        ),
        ("reason".into(), Value::str(&reason)),
        ("checked_ts".into(), Value::num(now_ts())),
    ]);

    logline!(
        "[plugin] 发现新版本 v{} -> v{}，资产 {}（{}）",
        local_ver,
        latest,
        asset.file,
        selection
    );

    Outcome {
        json,
        plan: Some(Plan {
            local,
            latest,
            latest_tag,
            release_url,
            published_at: github::utc_to_local(&published),
            asset,
            asset_url,
            asset_api_url,
            manifest_used,
        }),
    }
}

fn states_string(states: &[u8]) -> String {
    states
        .iter()
        .map(|&s| if s == 2 { 'D' } else if s == 1 { 'R' } else { 'P' })
        .collect()
}

/// 插件包下载：复用固件下载器的分块/worker/慢速保护，
/// 并把分块状态实时写入任务记录。
async fn download_one(
    client: &reqwest::Client,
    url: &str,
    out: &str,
    task_id: &str,
) -> Result<downloader::Downloaded, String> {
    let control = DownloadControl::start().map_err(|e| format!("初始化下载控制失败: {e}"))?;
    let started = now_ts();
    let last_persist = Arc::new(AtomicU64::new(0));
    let tid = task_id.to_string();
    let report: downloader::ProgressFn = Arc::new(move |p: Progress| {
        let total = p.total.max(p.done);
        let pct = if total > 0 {
            20 + ((p.done.min(total) * 55) / total) as u32
        } else {
            40
        };
        write_status(
            "downloading",
            pct,
            &format!(
                "正在下载插件安装包 {} / {} · 线程 {}/{} · 分块 {}/{}",
                github::human_size(p.done),
                if total > 0 {
                    github::human_size(total)
                } else {
                    "未知".into()
                },
                p.chunks_running.max(p.segments.min(p.workers)),
                p.workers,
                p.chunks_done,
                p.chunks_total
            ),
            None,
            None,
        );
        let now = now_ts();
        if now.saturating_sub(last_persist.load(Ordering::Relaxed)) >= 1 {
            last_persist.store(now, Ordering::Relaxed);
            let speed = if now > started {
                p.done / (now - started).max(1)
            } else {
                0
            };
            let states = states_string(&p.states);
            plugin_task_update(&tid, |t| {
                t.done = p.done;
                t.total = total;
                t.speed_bps = speed;
                t.workers = p.workers as u64;
                t.segments = p.segments as u64;
                t.chunk_size = p.chunk_size;
                t.chunks_total = p.chunks_total as u64;
                t.chunks_done = p.chunks_done as u64;
                t.chunks_running = p.chunks_running as u64;
                t.chunk_states = states;
                t.reason = "正在下载插件安装包".into();
            });
        }
    });
    let downloaded = downloader::download_with(
        client,
        url,
        out,
        report,
        &control,
        DownloadOptions { force_split: true },
    )
    .await
    .map_err(|e| e.message().to_string())?;
    plugin_task_update(task_id, |t| {
        t.done = downloaded.total;
        t.total = downloaded.total;
        t.speed_bps = 0;
        t.workers = downloaded.workers as u64;
        t.segments = downloaded.segments as u64;
        t.chunk_size = downloaded.chunk_size;
        t.chunks_total = downloaded.chunks_total as u64;
        t.chunks_done = downloaded.chunks_total as u64;
        t.chunks_running = 0;
        t.chunk_states = "D".repeat(downloaded.chunks_total);
        t.reason = "下载完成，正在校验 SHA256".into();
    });
    Ok(downloaded)
}

fn looks_like_package(path: &str, _file: &str) -> bool {
    // 只做空文件兜底。apk-tools 3 的包格式与 gzip 魔数并非一一对应，
    // 过严的魔数检查会把合法安装包误判为损坏；真正的完整性由
    // manifest 中的 SHA256 保证，格式最终交给 apk/opkg 自己校验。
    match fs::metadata(path) {
        Ok(m) => m.len() > 0,
        Err(_) => false,
    }
}

fn install_package(path: &str, manager: &str) -> Result<String, String> {
    let args: Vec<&str> = if manager == "apk" {
        vec!["add", "--allow-untrusted", path]
    } else {
        vec!["install", path]
    };
    let (status, text) = run_cmd_timeout(manager, &args, CMD_TIMEOUT_INSTALL)?;

    if status.success() {
        return Ok(text);
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("already installed") || lower.contains("is already installed") {
        return Ok(text);
    }
    let tail: String = text
        .lines()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ");
    Err(format!(
        "{manager} 安装失败（退出码 {}）: {}",
        status.code().unwrap_or(-1),
        if tail.is_empty() { "无输出" } else { &tail }
    ))
}

fn clear_luci_cache() {
    if let Ok(rd) = fs::read_dir("/tmp") {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("luci-indexcache") || name.starts_with("luci-modulecache") {
                let path = entry.path();
                if path.is_file() {
                    let _ = fs::remove_file(path);
                } else if path.is_dir() {
                    // 新版 LuCI 的索引缓存可能是目录；只递归删除这两个
                    // 明确前缀的临时路径，不触碰 /tmp 下的其他内容。
                    let _ = fs::remove_dir_all(path);
                }
            }
        }
    }
}

/// `plugin-check`：后台检查并写入插件专用状态文件。
pub async fn run_check() -> i32 {
    crate::status::trim_log();
    write_status("checking", 8, "正在获取插件 Release 信息...", None, None);
    let client = crate::http::client();
    let outcome = check(&client).await;
    let state = outcome
        .json
        .get("state")
        .and_then(|s| s.as_str())
        .unwrap_or("checked");
    if state == "error" {
        let err = outcome
            .json
            .get("reason")
            .and_then(|s| s.as_str())
            .unwrap_or("插件检查失败")
            .to_string();
        logline!("[plugin] 检查失败: {err}");
        write_status(
            "check_failed",
            100,
            "插件检查失败",
            Some(&err),
            Some(outcome.json),
        );
        1
    } else {
        let msg = if state == "available" {
            "发现插件新版本"
        } else if state == "up_to_date" {
            "插件已是最新版本"
        } else {
            "插件检查完成"
        };
        write_status("checked", 100, msg, None, Some(outcome.json));
        0
    }
}

/// `plugin-update`：重新检查 → 下载 → SHA256 校验 → 包管理器原地安装。
pub async fn run_update() -> i32 {
    crate::status::trim_log();
    write_status("updating", 5, "正在校验最新插件版本...", None, None);
    let client = crate::http::client();
    let outcome = check(&client).await;
    let Some(plan) = outcome.plan else {
        let state = outcome
            .json
            .get("state")
            .and_then(|s| s.as_str())
            .unwrap_or("error");
        if state == "up_to_date" {
            // 竞态下用户可能在检查后再次点击更新；这不是失败，
            // 应回到“已是最新”而不是留下“更新失败”的错误标识。
            write_status(
                "checked",
                100,
                "插件已是最新版本",
                None,
                Some(outcome.json),
            );
            return 0;
        }
        let err = outcome
            .json
            .get("reason")
            .and_then(|s| s.as_str())
            .unwrap_or("没有可安装的插件更新")
            .to_string();
        logline!("[plugin] 更新中止: {err}");
        write_status(
            "failed",
            100,
            "没有可安装的插件更新",
            Some(&err),
            Some(outcome.json),
        );
        return 1;
    };

    if plan.asset.size > MAX_PKG_BYTES {
        let msg = format!("插件安装包过大（{} 字节），已拒绝下载", plan.asset.size);
        write_status("failed", 100, "安装包过大", Some(&msg), Some(outcome.json));
        return 1;
    }

    // 内存盘环形缓存：更新包只允许落在 /tmp(tmpsf)，每次更新前先回收
    // 历史安装包与续传记录，保证任意时刻至多一个安装包镜像。
    let _ = crate::taskctl::cleanup_plugin_artifacts();
    let task_id = create_plugin_task(&plan);

    write_status(
        "downloading",
        20,
        &format!("正在下载 {} ...", plan.asset.file),
        None,
        None,
    );

    let tmp_pkg = tmp_pkg_path(&plan.asset.file);
    let _ = fs::remove_file(format!("{tmp_pkg}.parts.json"));
    let mut urls: Vec<String> = vec![plan.asset_url.clone()];
    if !plan.asset_api_url.is_empty() {
        urls.push(plan.asset_api_url.clone());
    }
    let cfg = cfg_for_download();
    let mut last_err = String::from("没有可用的下载线路");
    let mut downloaded = false;
    let mut downloaded_bytes = 0u64;
    let mut downloaded_sha = String::new();
    for url in &urls {
        // 有加速代理时优先代理，代理失败或内容校验不通过再回退直连。
        let mut candidates: Vec<(String, &str)> = Vec::new();
        if !cfg.proxy.is_empty() {
            let proxied = github::prefixed(&cfg.proxy, url);
            if proxied != *url {
                candidates.push((proxied, "加速代理"));
            }
        }
        if !candidates.iter().any(|(u, _)| u == url) {
            candidates.push((url.clone(), "直连"));
        }
        for (candidate, route) in candidates {
            logline!("[plugin] 下载线路: {route} {candidate}");
            match download_one(&client, &candidate, &tmp_pkg, &task_id).await {
                Ok(d) => {
                    let size_ok = d.total <= MAX_PKG_BYTES
                        && (plan.asset.size == 0 || d.total == plan.asset.size);
                    let hash = if size_ok {
                        crate::hash::sha256_file(&tmp_pkg).ok()
                    } else {
                        None
                    };
                    if size_ok && hash.as_deref() == Some(plan.asset.sha256.as_str()) {
                        downloaded_bytes = d.total;
                        downloaded_sha = hash.unwrap_or_default();
                        downloaded = true;
                        break;
                    }
                    last_err = format!(
                        "{route} 内容校验不通过（大小 {} / 期望 {}）",
                        d.total, plan.asset.size
                    );
                    let _ = fs::remove_file(&tmp_pkg);
                    let _ = fs::remove_file(format!("{tmp_pkg}.parts.json"));
                }
                Err(e) => {
                    last_err = format!("{route}: {e}");
                    let _ = fs::remove_file(&tmp_pkg);
                    let _ = fs::remove_file(format!("{tmp_pkg}.parts.json"));
                }
            }
        }
        if downloaded {
            break;
        }
    }
    if !downloaded {
        let msg = format!("插件安装包下载失败: {last_err}");
        plugin_task_finish(&task_id, ST_FAILED, &msg, 0, 0);
        write_status("failed", 100, "下载失败", Some(&msg), None);
        return 1;
    }

    write_status("verifying", 80, "正在校验安装包 SHA256...", None, None);
    let actual = if !downloaded_sha.is_empty() {
        downloaded_sha
    } else {
        match crate::hash::sha256_file(&tmp_pkg) {
            Ok(h) => h,
            Err(e) => {
                let msg = format!("SHA256 计算失败: {e}");
                let _ = fs::remove_file(&tmp_pkg);
                plugin_task_finish(&task_id, ST_FAILED, &msg, downloaded_bytes, 0);
                write_status("failed", 100, "校验失败", Some(&msg), None);
                return 1;
            }
        }
    };
    if actual != plan.asset.sha256 {
        let msg = format!(
            "SHA256 校验不匹配，已拒绝安装（期望 {}，实际 {}）",
            plan.asset.sha256, actual
        );
        let _ = fs::remove_file(&tmp_pkg);
        plugin_task_finish(&task_id, ST_FAILED, &msg, downloaded_bytes, 0);
        write_status("failed", 100, "校验失败", Some(&msg), None);
        return 1;
    }
    if !looks_like_package(&tmp_pkg, &plan.asset.file) {
        let msg = "下载文件不是有效的 apk/ipk 压缩包，已拒绝安装".to_string();
        let _ = fs::remove_file(&tmp_pkg);
        plugin_task_finish(&task_id, ST_FAILED, &msg, downloaded_bytes, 0);
        write_status("failed", 100, "校验失败", Some(&msg), None);
        return 1;
    }

    plugin_task_update(&task_id, |t| {
        t.done = downloaded_bytes;
        t.total = downloaded_bytes.max(t.total);
        t.speed_bps = 0;
        t.reason = "校验通过，正在安装插件包".into();
    });
    write_status(
        "installing",
        90,
        &format!("正在安装 {} ...", plan.asset.file),
        None,
        None,
    );
    let manager = manager_of(&plan.asset);
    match install_package(&tmp_pkg, manager) {
        Ok(out) => {
            if !out.trim().is_empty() {
                logline!("[plugin] {manager}: {}", out.trim().replace('\n', " | "));
            }
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp_pkg);
            plugin_task_finish(&task_id, ST_FAILED, &e, downloaded_bytes, 0);
            write_status("failed", 100, "安装失败", Some(&e), None);
            return 1;
        }
    }
    let _ = fs::remove_file(&tmp_pkg);
    clear_luci_cache();

    // 用安装后的真实版本复核，避免把「命令返回 0」当成成功。
    let after = detect_local();
    if let Some(v) = after.version {
        if v < plan.latest {
            let msg = format!(
                "安装命令已返回，但当前版本仍为 {}（期望 v{}），请检查包管理器状态",
                after.display, plan.latest
            );
            plugin_task_finish(&task_id, ST_FAILED, &msg, downloaded_bytes, 0);
            write_status("failed", 100, "版本复核失败", Some(&msg), None);
            return 1;
        }
    }

    let result = Value::Obj(vec![
        ("ok".into(), Value::Bool(true)),
        ("state".into(), Value::str("done")),
        ("task_id".into(), Value::str(&task_id)),
        ("from".into(), Value::str(&plan.local.display)),
        ("to".into(), Value::str(&plan.latest.to_string())),
        ("tag".into(), Value::str(&plan.latest_tag)),
        ("release_url".into(), Value::str(&plan.release_url)),
        ("published_at".into(), Value::str(&plan.published_at)),
        ("asset".into(), Value::str(&plan.asset.file)),
        ("sha256".into(), Value::str(&actual)),
        ("manager".into(), Value::str(manager)),
        (
            "manifest_used".into(),
            Value::Bool(plan.manifest_used),
        ),
        ("installed_version".into(), Value::str(&after.display)),
        (
            "cache_version".into(),
            Value::str(
                &after
                    .version
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| after.display.clone()),
            ),
        ),
        ("reload_required".into(), Value::Bool(true)),
        ("reboot_required".into(), Value::Bool(false)),
        (
            "message".into(),
            Value::str("插件已更新完成，刷新页面即可加载新界面；无需重启路由器"),
        ),
    ]);
    plugin_task_finish(
        &task_id,
        ST_DONE,
        &format!("插件已更新到 v{}；刷新页面加载新界面", plan.latest),
        downloaded_bytes,
        0,
    );
    // 更新成功后自动清理：删除任务记录与内存盘上的安装包，
    // 不在设备上留存可被重复安装的安装包副本。
    {
        let mut store = TaskStore::open();
        let _ = store.remove_any(&task_id);
        let _ = store.save();
        let removed = crate::taskctl::cleanup_plugin_artifacts();
        logline!("[plugin] 已清理更新任务与临时安装包（{} 个文件）", removed);
    }
    logline!(
        "[plugin] 更新完成: {} -> {}（{}）",
        plan.local.display,
        plan.latest,
        plan.asset.file
    );
    write_status("done", 100, "插件更新完成", None, Some(result));
    0
}

/// 下载阶段重新读取一次代理配置：避免把代理值复制到多处后失步。
fn cfg_for_download() -> Cfg {
    let fw = github::load_cfg();
    Cfg {
        repo: plugin_repo(),
        tag: "latest".into(),
        proxy: fw.proxy,
        pattern: String::new(),
        token: fw.token,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_tags_only_accept_three_part_stable_versions() {
        assert_eq!(
            tag_version("v2.5.0"),
            Some(Semver {
                major: 2,
                minor: 5,
                patch: 0
            })
        );
        assert_eq!(tag_version("2.5.0"), None);
        assert_eq!(tag_version("v2.5"), None);
        assert_eq!(tag_version("v2.5.0-rc1"), None);
        assert_eq!(tag_version("v26.11.10"), None);
        assert_eq!(tag_version("v2026.11.10"), None);
        assert_eq!(
            tag_version("v25.12.0"),
            Some(Semver {
                major: 25,
                minor: 12,
                patch: 0
            })
        );
        assert_eq!(tag_version("v2.5.0"), Some(Semver { major: 2, minor: 5, patch: 0 }));
    }

    #[test]
    fn local_package_version_is_split() {
        assert_eq!(
            split_pkg_release("2.5.0-r1"),
            ("2.5.0".to_string(), "1".to_string())
        );
        assert_eq!(
            split_pkg_release("2.5.0"),
            ("2.5.0".to_string(), String::new())
        );
        assert_eq!(parse_semver("2.5.0-r1"), Some(Semver { major: 2, minor: 5, patch: 0 }));
    }

    #[test]
    fn arch_family_normalizes_package_arch() {
        assert_eq!(arch_family("aarch64_cortex-a53"), "aarch64");
        assert_eq!(arch_family("x86_64"), "x86_64");
        assert_eq!(arch_family("mipsel_24kc"), "mips");
    }
}
