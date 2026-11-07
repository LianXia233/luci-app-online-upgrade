// Upgrade orchestration: check → download (segmented, real progress) →
// record version → backup → package list → sysupgrade.

use crate::downloader;
use crate::github::{self, Cfg};
use crate::json::Value;
use crate::status;
use crate::logline;
use crate::uci;
use std::io::Write;
use std::process::Command;
use std::sync::Arc;

const TMP_FIRMWARE: &str = "/tmp/firmware.img.gz";
const TMP_FIRMWARE_TS: &str = "/tmp/firmware.img.gz.ts";
const PKG_LIST: &str = "/root/.pkg-list.txt";

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
    format!("{}{}", cfg.proxy, url)
}

struct Asset {
    url: String,
    updated: String,
    version: String,
    name: String,
    size: u64,
    sha256: String,
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
    Ok(Asset {
        url,
        updated: latest
            .get("updated")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string(),
        version: latest
            .get("version")
            .and_then(|u| u.as_str())
            .unwrap_or("0")
            .to_string(),
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
    })
}

fn verify_firmware_integrity(asset: &Asset) -> Result<(), String> {
    status::set("verifying", 91, "正在校验固件 SHA256 哈希完整性...");
    let actual_sha256 = crate::hash::sha256_file(TMP_FIRMWARE)
        .map_err(|e| format!("SHA256 计算失败: {e}"))?;

    logline!("[verify] 固件实际 SHA256: {actual_sha256}");

    // Check custom UCI expected_sha256 first, then release asset's sha256
    let manual_expected = uci::get("expected_sha256").unwrap_or_default().trim().to_lowercase();
    let expected = if !manual_expected.is_empty() {
        manual_expected
    } else {
        asset.sha256.trim().to_lowercase()
    };

    if !expected.is_empty() {
        if actual_sha256 != expected {
            let err = format!("SHA256 校验不匹配! 期望: {expected}, 实际: {actual_sha256}");
            logline!("[verify] ❌ {err}");
            let _ = std::fs::remove_file(TMP_FIRMWARE);
            let _ = std::fs::remove_file(TMP_FIRMWARE_TS);
            return Err(err);
        }
        logline!("[verify] ✅ SHA256 校验一致 (匹配通过: {expected})");
    } else {
        logline!("[verify] ℹ️ 云端未提供校验单，已完成本地完整性记录 (SHA256: {actual_sha256})");
    }

    // Run sysupgrade -T image compatibility test if available
    if which("sysupgrade") {
        logline!("[verify] 执行 sysupgrade -T 测试固件兼容性...");
        if let Some(out) = sh("sysupgrade", &["-T", TMP_FIRMWARE]) {
            logline!("[verify] sysupgrade -T 兼容性通过: {}", out.lines().next().unwrap_or(""));
        }
    }

    Ok(())
}

/// `online-upgrade download` — check + segmented download only (no backup/flash).
pub async fn download_only() -> i32 {
    let client = crate::http::client();
    let asset = match fetch_asset(&client).await {
        Ok(a) => a,
        Err(e) => {
            status::set_error("failed", 100, "获取 Release 信息失败", &e);
            logline!("[download] check failed: {e}");
            return 1;
        }
    };
    let download_url = full_url(&github::load_cfg(), &asset.url);

    let skip = std::fs::metadata(TMP_FIRMWARE).map(|m| m.len()).unwrap_or(0) > 0
        && std::fs::read_to_string(TMP_FIRMWARE_TS)
            .map(|s| s.trim() == asset.updated)
            .unwrap_or(false);
    if skip {
        logline!("[download] 固件已下载,跳过({})", asset.name);
        status::set("downloaded", 100, "固件已下载,跳过");
        return 0;
    }

    status::set("downloading", 5, "正在下载固件...");
    logline!("[download] {}", &download_url[..download_url.len().min(90)]);
    let total_hint = asset.size;
    let report: Arc<dyn Fn(u64, u64) + Send + Sync> = Arc::new(move |done: u64, total: u64| {
        let total_eff = if total > 0 { total } else { total_hint };
        let pct = if total_eff > 0 {
            5 + ((done as f64 / total_eff as f64) * 95.0).min(95.0) as u32
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
    match downloader::download(&client, &download_url, TMP_FIRMWARE, report).await {
        Ok(d) => {
            logline!(
                "[download] 完成 {} (分段 {} 并发, 续传 {} 块)",
                github::human_size(d.total),
                d.segments,
                d.resumed_chunks
            );
            status::set("downloaded", 100, "固件下载完成");
            let _ = std::fs::write(TMP_FIRMWARE_TS, &asset.updated);
            if let Err(e) = verify_firmware_integrity(&asset) {
                status::set_error("failed", 100, "固件哈希校验失败", &e);
                return 1;
            }
            0
        }
        Err(e) => {
            status::set_error("failed", 100, "固件下载失败", &e);
            logline!("[download] failed: {e}");
            let _ = std::fs::remove_file(TMP_FIRMWARE);
            1
        }
    }
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
    let url = asset.url.clone();
    let updated = asset.updated.clone();
    let fw_version = asset.version.clone();
    let name = asset.name.clone();
    let size_hint = asset.size;
    let download_url = full_url(&github::load_cfg(), &url);

    // ---- Step 1: download (segmented, real progress) ----
    let skip = std::fs::metadata(TMP_FIRMWARE).map(|m| m.len()).unwrap_or(0) > 0
        && std::fs::read_to_string(TMP_FIRMWARE_TS)
            .map(|s| s.trim() == updated)
            .unwrap_or(false);
    if skip {
        logline!("[upgrade] 固件已下载,跳过({})", name);
        status::set("downloaded", 90, "固件已下载,跳过");
    } else {
        let _ = std::fs::remove_file(TMP_FIRMWARE);
        let _ = std::fs::remove_file(TMP_FIRMWARE_TS);
        let _ = std::fs::remove_file(format!("{TMP_FIRMWARE}.parts.json"));
        status::set("downloading", 5, "正在下载固件...");
        logline!("[upgrade] 下载: {}", &download_url[..download_url.len().min(90)]);
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
        match downloader::download(&client, &download_url, TMP_FIRMWARE, report).await {
            Ok(d) => {
                logline!(
                    "[upgrade] 下载完成 {} (分段 {} 并发, 续传 {} 块)",
                    github::human_size(d.total),
                    d.segments,
                    d.resumed_chunks
                );
                status::set("downloaded", 90, "固件下载完成");
                let _ = std::fs::write(TMP_FIRMWARE_TS, &updated);
            }
            Err(e) => {
                status::set_error("failed", 100, "固件下载失败", &e);
                logline!("[upgrade] download failed: {e}");
                let _ = std::fs::remove_file(TMP_FIRMWARE);
                return 1;
            }
        }
    }

    // ---- Step 1.5: verify SHA256 checksum and image integrity ----
    if let Err(e) = verify_firmware_integrity(&asset) {
        status::set_error("failed", 100, "固件哈希校验失败", &e);
        return 1;
    }

    // ---- Step 2: record version (before backup so the backup contains it) ----
    status::set("saving", 92, "记录固件版本...");
    uci::set("last_upgrade_ts", &updated);
    uci::set("last_upgrade_version", &fw_version);
    uci::commit();
    let _ = sh("sync", &[]);
    logline!("[upgrade] 已记录版本 v{fw_version} ({})", github::utc_to_local(&updated));

    // ---- Step 3: backup (sysupgrade -b, passed to -f later) ----
    status::set("backing_up", 94, "正在创建配置备份...");
    let stamp = now_stamp();
    let backup_tmp = format!("/tmp/pre-upgrade-backup-{stamp}.tar.gz");
    let backup_root = format!("/root/pre-upgrade-backup-{stamp}.tar.gz");
    logline!("[upgrade] 创建备份: {backup_root}");
    let r = {
        let backup_tmp = backup_tmp.clone();
        tokio::task::spawn_blocking(move || sh("sysupgrade", &["-b", &backup_tmp])).await
    };
    let backup_ok = matches!(r, Ok(Some(_)))
        && std::fs::metadata(&backup_tmp).map(|m| m.len() > 0).unwrap_or(false);
    if !backup_ok {
        status::set_error("failed", 100, "升级失败", "配置备份失败(sysupgrade -b)");
        logline!("[upgrade] backup failed");
        return 1;
    }
    let files = sh("sh", &["-c", &format!("tar tzf {backup_tmp} 2>/dev/null | wc -l")]).unwrap_or_default();
    if std::fs::copy(&backup_tmp, &backup_root).is_ok() {
        logline!(
            "[upgrade] 备份成功: {backup_root} ({}) 包含 {files} 个文件",
            github::human_size(std::fs::metadata(&backup_root).map(|m| m.len()).unwrap_or(0))
        );
    }

    // ---- Step 4: save installed package list (for post-upgrade auto reinstall) ----
    let pkg_cmd = if which("apk") { "apk info" } else { "opkg list-installed" };
    if let Some(list) = sh("sh", &["-c", &format!("{pkg_cmd} 2>/dev/null")]) {
        if let Ok(mut f) = std::fs::File::create(PKG_LIST) {
            let _ = f.write_all(list.as_bytes());
            let _ = f.flush();
        }
    }
    let _ = sh("sync", &[]);

    // ---- Step 5: flash (does not return on success) ----
    status::set("flashing", 100, "正在刷写固件,配置将自动恢复,即将重启...");
    logline!("[upgrade] 执行: sysupgrade -f {backup_tmp} {TMP_FIRMWARE}");
    let _ = sh("sync", &[]);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let _ = Command::new("/sbin/sysupgrade").args(["-f", &backup_tmp, TMP_FIRMWARE]).status();

    // if sysupgrade returned, it failed
    logline!("[upgrade] sysupgrade 执行失败!");
    status::set_error("failed", 100, "升级失败", "sysupgrade 执行失败");
    uci::unset("last_upgrade_ts");
    uci::unset("last_upgrade_version");
    uci::commit();
    1
}

fn which(cmd: &str) -> bool {
    sh("sh", &["-c", &format!("command -v {cmd} >/dev/null 2>&1 && echo y")]).as_deref() == Some("y")
}
