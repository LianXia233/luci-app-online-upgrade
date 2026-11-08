// Upgrade orchestration: check → download (segmented, real progress) →
// verify SHA256 + image integrity → record version → backup → package list →
// sysupgrade.

use crate::downloader;
use crate::github::{self, Cfg};
use crate::json::Value;
use crate::logline;
use crate::status;
use crate::uci;
use std::io::Write;
use std::process::Command;
use std::sync::Arc;

const TMP_FIRMWARE: &str = "/tmp/firmware.img.gz";
const TMP_FIRMWARE_TS: &str = "/tmp/firmware.img.gz.ts";
const TMP_FIRMWARE_PARTS: &str = "/tmp/firmware.img.gz.parts.json";
const PKG_LIST: &str = "/root/.pkg-list.txt";
const AUTO_REINSTALL_SCRIPT: &str = include_str!("../../root/etc/uci-defaults/90-online-upgrade-auto");
const UCI_DEFAULTS_AUTO: &str = "/etc/uci-defaults/90-online-upgrade-auto";

fn sh(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn now_stamp() -> String {
    sh("date", &["+%Y%m%d-%H%M%S"]).unwrap_or_else(|| "unknown".into())
}

fn full_url(cfg: &Cfg, url: &str) -> String {
    github::prefixed(&cfg.proxy, url)
}

/// Remove every artefact of a download attempt (firmware, timestamp marker and
/// resume sidecar). A sidecar left behind after the firmware was deleted is
/// exactly what produces a zero-filled "successful" download on the retry.
fn cleanup_download() {
    let _ = std::fs::remove_file(TMP_FIRMWARE);
    let _ = std::fs::remove_file(TMP_FIRMWARE_TS);
    let _ = std::fs::remove_file(TMP_FIRMWARE_PARTS);
}

/// Reuse a firmware file left over from a previous run only when the marker
/// proves the bytes on disk are exactly the ones that run finished with.
///
/// Length is not evidence: the downloader pre-allocates with `set_len`, so a
/// half-written file can have the right size while its tail is still zero
/// filled. The marker therefore stores the asset `updated` stamp *and* the
/// SHA256 of the completed file; a marker without the hash (an older run) is
/// never trusted. Callers must still run the full verification afterwards —
/// reuse skips the network transfer, not the hash check.
fn reuse_download(updated: &str, size: u64) -> bool {
    let meta = match std::fs::metadata(TMP_FIRMWARE) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if meta.len() == 0 || (size > 0 && meta.len() != size) {
        return false;
    }
    let marker = match std::fs::read_to_string(TMP_FIRMWARE_TS) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let mut lines = marker.lines().map(|l| l.trim()).filter(|l| !l.is_empty());
    let stamp = lines.next().unwrap_or("");
    let recorded = lines.next().unwrap_or("").to_lowercase();
    if stamp != updated || recorded.len() != 64 {
        logline!("[download] 拒绝对磁盘上的固件复用: 标记缺少哈希或时间戳不符");
        return false;
    }
    match crate::hash::sha256_file(TMP_FIRMWARE) {
        Ok(actual) => {
            if actual == recorded {
                logline!("[download] 固件复用判定通过: 标记哈希与磁盘一致 ({actual})");
                true
            } else {
                logline!("[download] 拒绝对磁盘上的固件复用: 标记哈希不符(标记 {recorded}, 实际 {actual})");
                false
            }
        }
        Err(e) => {
            logline!("[download] 固件复用判定失败: 无法计算磁盘 SHA256: {e}");
            false
        }
    }
}

/// Marker format: line 1 = asset `updated` stamp, line 2 = local SHA256 of the
/// finished file. Written only after the file passed verification.
fn write_download_marker(updated: &str, sha256: &str) {
    let _ = std::fs::write(TMP_FIRMWARE_TS, format!("{updated}\n{sha256}\n"));
}

struct Asset {
    url: String,
    api_url: String,
    updated: String,
    version: String,
    version_source: String,
    relation: String,
    name: String,
    size: u64,
    sha256: String,
    sha256_source: String,
    rule: Option<String>,
}

struct Integrity {
    verified: bool,
    source: String,
    sha256: String,
}

/// Try the configured proxy route first, then GitHub's API asset endpoint.
/// `browser_download_url` is served by github.com's web frontend, which can
/// return 504 while the API + release-assets CDN path still serves the file.
async fn fetch_firmware(
    client: &reqwest::Client,
    asset: &Asset,
    out: &str,
    report: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<downloader::Downloaded, String> {
    let mut candidates = vec![full_url(&github::load_cfg(), &asset.url)];
    if !asset.api_url.is_empty() {
        candidates.push(asset.api_url.clone());
    }
    let mut last = String::from("没有可用的下载地址");
    for (i, url) in candidates.iter().enumerate() {
        if i > 0 {
            logline!("[download] 上一线路失败({last}),回退到 GitHub API 资产直链");
            // the sidecar is keyed by URL, but a half-written file from another
            // route must not be mistaken for resumable progress
            let _ = std::fs::remove_file(out);
            let _ = std::fs::remove_file(format!("{out}.parts.json"));
        }
        // byte-slicing can panic on a non-ASCII URL (e.g. a custom proxy host)
        let shown = url.get(..url.len().min(90)).unwrap_or(url);
        logline!("[download] 线路{}: {}", i + 1, shown);
        match downloader::download(client, url, out, report.clone()).await {
            Ok(d) => return Ok(d),
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn fetch_asset(client: &reqwest::Client) -> Result<Asset, String> {
    status::set("checking", 3, "正在获取 Release 信息...");
    let info = github::check(client).await;
    if !matches!(info.get("ok"), Some(Value::Bool(true))) {
        let err = info
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("未知错误");
        return Err(err.to_string());
    }
    let latest = info.get("latest").cloned().unwrap_or(Value::Null);
    let url = latest
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    if url.is_empty() {
        return Err("下载地址为空".into());
    }
    let relation = info
        .get("version")
        .and_then(|v| v.get("relation"))
        .and_then(|r| r.as_str())
        .unwrap_or("unknown")
        .to_string();
    Ok(Asset {
        url,
        api_url: latest
            .get("api_url")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string(),
        updated: latest
            .get("updated")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string(),
        version: latest
            .get("version")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string(),
        version_source: latest
            .get("version_source")
            .and_then(|u| u.as_str())
            .unwrap_or("none")
            .to_string(),
        relation,
        name: latest
            .get("name")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string(),
        size: latest.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
        sha256: latest
            .get("sha256")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string(),
        sha256_source: latest
            .get("sha256_source")
            .and_then(|u| u.as_str())
            .unwrap_or("none")
            .to_string(),
        rule: info
            .get("matched_rule")
            .and_then(|r| r.as_str())
            .map(|s| s.to_string()),
    })
}

fn verify_firmware_integrity(
    device: &crate::compat::DeviceProfile,
    asset: &Asset,
    force: bool,
) -> Result<Integrity, String> {
    status::set("verifying", 91, "正在校验固件 SHA256 哈希完整性...");
    let actual_sha256 =
        crate::hash::sha256_file(TMP_FIRMWARE).map_err(|e| format!("SHA256 计算失败: {e}"))?;

    logline!("[verify] 固件实际 SHA256: {actual_sha256}");

    // Explicit UCI value first, then whatever the release published.
    let manual_expected = uci::get("expected_sha256")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let (expected, source) = if !manual_expected.is_empty() {
        (manual_expected, "config")
    } else {
        (asset.sha256.trim().to_lowercase(), asset.sha256_source.as_str())
    };

    let verified = if !expected.is_empty() {
        if actual_sha256 != expected {
            let err = format!("SHA256 校验不匹配! 期望: {expected}, 实际: {actual_sha256}");
            logline!("[verify] {err}");
            cleanup_download();
            return Err(err);
        }
        logline!("[verify] SHA256 校验一致 (来源 {source}: {expected})");
        true
    } else {
        // No expected hash anywhere. Do not claim "verified": the UI and the
        // status text must say "recorded but not verified".
        logline!(
            "[verify] 未提供校验依据(配置/校验单/Release 正文均无哈希),仅记录本地 SHA256: {actual_sha256}"
        );
        false
    };

    // Hardware & sysupgrade -T image compatibility verification
    if let Err(e) = crate::compat::verify_downloaded_image(device, TMP_FIRMWARE) {
        if !force {
            cleanup_download();
            return Err(e);
        }
        logline!("[verify] 强制模式生效，跳过未通过的固件兼容性预检: {e}");
    }

    Ok(Integrity {
        verified,
        source: source.to_string(),
        sha256: actual_sha256,
    })
}

/// `online-upgrade download` — check + segmented download + verification only.
/// Never touches flash, backup or sysupgrade.
pub async fn download_only() -> i32 {
    status::trim_log();
    let client = crate::http::client();
    let device = crate::compat::DeviceProfile::current();

    let asset = match fetch_asset(&client).await {
        Ok(a) => a,
        Err(e) => {
            status::set_error("failed", 100, "获取 Release 信息失败", &e);
            logline!("[download] check failed: {e}");
            return 1;
        }
    };

    let total: u64;
    let mut segments = 0usize;
    let mut resumed_chunks = 0usize;
    if reuse_download(&asset.updated, asset.size) {
        // A leftover file is reused only when its marker carries the hash of
        // the completed bytes; the hash is re-checked below either way.
        total = std::fs::metadata(TMP_FIRMWARE)
            .map(|m| m.len())
            .unwrap_or(0);
        logline!(
            "[download] 复用上一轮已校验的固件副本 {} ({}),跳过传输但仍重新校验哈希",
            asset.name,
            github::human_size(total)
        );
        status::set("verifying", 90, "复用已下载固件,正在重新校验...");
    } else {
        // Do not wipe the firmware + sidecar here: load_parts() already rejects
        // a sidecar whose url/etag/total/chunk grid no longer matches, and a
        // surviving sidecar is exactly what makes a download interrupted in a
        // previous process resumable. Wiping it forced every restart to begin
        // from byte 0.
        status::set("downloading", 5, "正在下载固件...");
        let total_hint = asset.size;
        let report: Arc<dyn Fn(u64, u64) + Send + Sync> = Arc::new(move |done: u64, total: u64| {
            let total_eff = if total > 0 { total } else { total_hint };
            let pct = if total_eff > 0 {
                5 + ((done as f64 / total_eff as f64) * 85.0).min(85.0) as u32
            } else {
                5
            };
            status::set(
                "downloading",
                pct,
                &format!(
                    "正在下载固件... {:.1}/{:.1} MB",
                    done as f64 / 1048576.0,
                    total_eff as f64 / 1048576.0
                ),
            );
        });
        match fetch_firmware(&client, &asset, TMP_FIRMWARE, report).await {
            Ok(d) => {
                total = d.total;
                segments = d.segments;
                resumed_chunks = d.resumed_chunks;
                logline!(
                    "[download] 完成 {} (分段 {} 并发, 续传 {} 块)",
                    github::human_size(d.total),
                    d.segments,
                    d.resumed_chunks
                );
            }
            Err(e) => {
                status::set_error("failed", 100, "固件下载失败", &e);
                logline!("[download] failed: {e}");
                // keep whatever the downloader judged resumable
                return 1;
            }
        }
    }

    let integrity = match verify_firmware_integrity(&device, &asset, false) {
        Ok(i) => i,
        Err(e) => {
            status::set_error("failed", 100, "固件校验或兼容性预检失败", &e);
            return 1;
        }
    };
    // Marker is written only after verification: it advertises "this file
    // passed the hash gate", which is what makes reuse safe next time.
    write_download_marker(&asset.updated, &integrity.sha256);
    let compat = crate::compat::evaluate_asset(&device, &asset.name);
    let msg = if integrity.verified {
        "固件下载完成,SHA256 校验一致(未执行任何升级操作)"
    } else {
        "固件下载完成,但无校验依据,仅记录本地 SHA256(未校验,未执行任何升级操作)"
    };
    status::set_result(
        "downloaded",
        100,
        msg,
        Value::Obj(vec![
            ("firmware".into(), Value::str(&asset.name)),
            ("size".into(), Value::num(total)),
            (
                "size_human".into(),
                Value::str(&github::human_size(total)),
            ),
            ("sha256".into(), Value::str(&integrity.sha256)),
            ("verified".into(), Value::Bool(integrity.verified)),
            ("hash_source".into(), Value::str(&integrity.source)),
            ("version".into(), Value::str(&asset.version)),
            ("version_source".into(), Value::str(&asset.version_source)),
            ("relation".into(), Value::str(&asset.relation)),
            ("segments".into(), Value::num(segments as u64)),
            ("resumed_chunks".into(), Value::num(resumed_chunks as u64)),
            ("reused".into(), Value::Bool(resumed_chunks == 0 && segments == 0)),
            ("compatibility".into(), compat.to_json()),
            ("dry_run".into(), Value::Bool(true)),
        ]),
    );
    0
}

pub async fn run(force: bool) -> i32 {
    status::trim_log();
    let client = crate::http::client();
    if force {
        logline!("[upgrade] 强制更新模式");
    }

    // ---- Step 0: check, get asset info ----
    let asset = match fetch_asset(&client).await {
        Ok(a) => a,
        Err(e) => {
            status::set_error("failed", 100, "获取 Release 信息失败", &e);
            logline!("[upgrade] check failed: {e}");
            return 1;
        }
    };
    let updated = asset.updated.clone();
    let fw_version = asset.version.clone();
    let name = asset.name.clone();
    let size_hint = asset.size;

    let device = crate::compat::DeviceProfile::current();
    let compat = match asset.rule.as_deref() {
        Some(rule) => crate::compat::evaluate_with_rule(&device, &name, rule),
        None => crate::compat::evaluate_asset(&device, &name),
    };
    logline!(
        "[upgrade] 本地设备: {} ({} / {}), 目标固件: {}, 识别判定: {} [{}]",
        device.model,
        device.uname_arch,
        device.target,
        name,
        compat.badge,
        compat.reason
    );
    for line in &compat.basis {
        logline!("[upgrade] 识别依据: {line}");
    }
    if !compat.can_upgrade && !force {
        let err = format!(
            "固件机型或架构未确认匹配 ({}): 拒绝刷写以防变砖",
            compat.reason
        );
        logline!("[upgrade] {err}");
        status::set_error("failed", 100, "固件不兼容，已拒绝刷机", &err);
        return 1;
    }
    if asset.relation == "older" && !force {
        let err = format!(
            "目标版本 v{} 低于当前系统版本，已拒绝降级(如需降级请强制升级)",
            fw_version
        );
        logline!("[upgrade] {err}");
        status::set_error("failed", 100, "拒绝降级", &err);
        return 1;
    }

    // ---- Step 1: download (segmented, real progress) ----
    // Reuse is gated on the marker's recorded hash (see reuse_download); the
    // verification below runs in both branches, so a reused file still has to
    // match the expected checksum before anything touches flash.
    if reuse_download(&updated, size_hint) {
        logline!("[upgrade] 复用上一轮已校验的固件副本,跳过传输({})", name);
        status::set("downloaded", 90, "复用已下载固件,正在重新校验...");
    } else {
        // See download_only(): the sidecar is what enables resume across
        // processes, and load_parts() already validates it.
        status::set("downloading", 5, "正在下载固件...");
        let total_hint = size_hint;
        let report: Arc<dyn Fn(u64, u64) + Send + Sync> = Arc::new(move |done: u64, total: u64| {
            let total_eff = if total > 0 { total } else { total_hint };
            let pct = if total_eff > 0 {
                5 + ((done as f64 / total_eff as f64) * 85.0).min(85.0) as u32
            } else {
                5
            };
            status::set(
                "downloading",
                pct,
                &format!(
                    "正在下载固件... {:.1}/{:.1} MB",
                    done as f64 / 1048576.0,
                    total_eff as f64 / 1048576.0
                ),
            );
        });
        match fetch_firmware(&client, &asset, TMP_FIRMWARE, report).await {
            Ok(d) => {
                logline!(
                    "[upgrade] 下载完成 {} (分段 {} 并发, 续传 {} 块)",
                    github::human_size(d.total),
                    d.segments,
                    d.resumed_chunks
                );
                status::set("downloaded", 90, "固件下载完成");
            }
            Err(e) => {
                status::set_error("failed", 100, "固件下载失败", &e);
                logline!("[upgrade] download failed: {e}");
                // keep whatever the downloader judged resumable
                return 1;
            }
        }
    }

    // ---- Step 1.5: verify SHA256 checksum and image integrity ----
    let integrity = match verify_firmware_integrity(&device, &asset, force) {
        Ok(i) => i,
        Err(e) => {
            status::set_error("failed", 100, "固件校验或预检失败", &e);
            return 1;
        }
    };
    logline!(
        "[upgrade] 校验结果: verified={} source={} sha256={}",
        integrity.verified,
        integrity.source,
        integrity.sha256
    );
    // Only a verified file earns the reuse marker.
    write_download_marker(&updated, &integrity.sha256);

    // ---- Step 2: record version after a successful backup ----
    // Order matters: writing first would leave a wrong "already upgraded"
    // marker when the backup fails, and writing after the backup without
    // rollback would do the same on flash failure below.
    let prev_ts = uci::get("last_upgrade_ts");
    let prev_ver = uci::get("last_upgrade_version");
    status::set("saving", 92, "记录固件版本...");
    uci::set("last_upgrade_ts", &updated);
    uci::set("last_upgrade_version", &fw_version);
    uci::commit();
    let _ = sh("sync", &[]);
    logline!(
        "[upgrade] 已记录版本 v{fw_version} ({})",
        github::utc_to_local(&updated)
    );

    // ---- Step 3: backup (sysupgrade -b, passed to -f later) ----
    status::set("backing_up", 94, "正在创建配置备份...");
    let _ = std::fs::create_dir_all("/etc/uci-defaults");
    let _ = std::fs::write(UCI_DEFAULTS_AUTO, AUTO_REINSTALL_SCRIPT);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            UCI_DEFAULTS_AUTO,
            std::fs::Permissions::from_mode(0o755),
        );
    }
    let stamp = now_stamp();
    let backup_tmp = format!("/tmp/pre-upgrade-backup-{stamp}.tar.gz");
    let backup_root = format!("/root/pre-upgrade-backup-{stamp}.tar.gz");
    logline!("[upgrade] 创建备份: {backup_root}");
    let r = {
        let backup_tmp = backup_tmp.clone();
        tokio::task::spawn_blocking(move || sh("sysupgrade", &["-b", &backup_tmp])).await
    };
    let backup_ok = matches!(r, Ok(Some(_)))
        && std::fs::metadata(&backup_tmp)
            .map(|m| m.len() > 0)
            .unwrap_or(false);
    if !backup_ok {
        // Roll the version marker back: no backup means no upgrade happened.
        match &prev_ts {
            Some(v) => uci::set("last_upgrade_ts", v),
            None => uci::unset("last_upgrade_ts"),
        };
        match &prev_ver {
            Some(v) => uci::set("last_upgrade_version", v),
            None => uci::unset("last_upgrade_version"),
        };
        uci::commit();
        status::set_error("failed", 100, "升级失败", "配置备份失败(sysupgrade -b)");
        logline!("[upgrade] backup failed, version marker rolled back");
        return 1;
    }
    let files = sh(
        "sh",
        &["-c", &format!("tar tzf {backup_tmp} 2>/dev/null | wc -l")],
    )
    .unwrap_or_default();
    match std::fs::copy(&backup_tmp, &backup_root) {
        Ok(_) => logline!(
            "[upgrade] 备份成功: {backup_root} ({}) 包含 {files} 个文件",
            github::human_size(std::fs::metadata(&backup_root).map(|m| m.len()).unwrap_or(0))
        ),
        Err(e) => logline!(
            "[upgrade] 警告: 备份已生成于 {backup_tmp},但复制到 {backup_root} 失败: {e} (刷机仍会使用临时备份)"
        ),
    }

    // ---- Step 4: save installed package list (for post-upgrade auto reinstall) ----
    // Package names only (no versions): the reinstall script must not depend
    // on a specific package manager's output format.
    let pkg_cmd = if which("apk") {
        "apk info"
    } else {
        "opkg list-installed | cut -d' ' -f1"
    };
    match sh("sh", &["-c", &format!("{pkg_cmd} 2>/dev/null")]) {
        Some(list) if !list.is_empty() => {
            if let Ok(mut f) = std::fs::File::create(PKG_LIST) {
                let _ = f.write_all(list.as_bytes());
                let _ = f.flush();
                logline!(
                    "[upgrade] 已保存 {} 个已安装软件包清单",
                    list.lines().count()
                );
            } else {
                logline!("[upgrade] 警告: 无法写入软件包清单 {PKG_LIST}");
            }
        }
        _ => logline!("[upgrade] 警告: 读取已安装软件包列表失败,升级后将无法自动重装插件"),
    }
    let _ = sh("sync", &[]);

    // ---- Step 5: flash (does not return on success) ----
    status::set("flashing", 100, "正在刷写固件,配置将自动恢复,即将重启...");
    logline!("[upgrade] 执行: sysupgrade -f {backup_tmp} {TMP_FIRMWARE}");
    let _ = sh("sync", &[]);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let _ = Command::new("/sbin/sysupgrade")
        .args(["-f", &backup_tmp, TMP_FIRMWARE])
        .status();

    // if sysupgrade returned, it failed
    logline!("[upgrade] sysupgrade 执行失败!");
    status::set_error("failed", 100, "升级失败", "sysupgrade 执行失败");
    match &prev_ts {
        Some(v) => uci::set("last_upgrade_ts", v),
        None => uci::unset("last_upgrade_ts"),
    };
    match &prev_ver {
        Some(v) => uci::set("last_upgrade_version", v),
        None => uci::unset("last_upgrade_version"),
    };
    uci::commit();
    1
}

fn which(cmd: &str) -> bool {
    sh("sh", &["-c", &format!("command -v {cmd} >/dev/null 2>&1 && echo y")]).as_deref()
        == Some("y")
}
