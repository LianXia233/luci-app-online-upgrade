// 升级流程编排：检查 → 下载（分段、真实进度）→
// 校验 SHA256 与镜像完整性 → 记录版本 → 备份 → 软件包清单 →
// sysupgrade。

use crate::downloader::{self, DownloadError};
use crate::github::{self, Cfg};
use crate::json::Value;
use crate::logline;
use crate::status;
use crate::taskctl::{
    DownloadControl, RunState, StopReason, Task, TaskStore, ST_ABORTED, ST_DONE, ST_FAILED,
    ST_PAUSED, ST_RUNNING,
};
use crate::uci;
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const TMP_FIRMWARE: &str = "/tmp/firmware.img.gz";
const TMP_FIRMWARE_TS: &str = "/tmp/firmware.img.gz.ts";
const TMP_FIRMWARE_PARTS: &str = "/tmp/firmware.img.gz.parts.json";
const PKG_LIST: &str = "/root/.pkg-list.txt";
const AUTO_REINSTALL_SCRIPT: &str =
    include_str!("../../root/etc/uci-defaults/90-online-upgrade-auto");
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

/// 传给下载器的共享进度回调。
type ProgressFn = downloader::ProgressFn;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 对一条历史记录应用修改并原子持久化。
fn task_update(id: &str, f: impl FnOnce(&mut Task)) {
    let mut store = TaskStore::open();
    if let Some(t) = store.get_mut(id) {
        f(t);
    }
    let _ = store.save();
}

/// 记录任务的终态（done / failed / aborted）。
fn task_finish(id: &str, state: &str, reason: &str, done: u64, speed: u64) {
    task_update(id, |t| {
        t.done = done;
        t.speed_bps = speed;
        if state == ST_DONE {
            t.chunks_done = t.chunks_total;
            t.chunks_running = 0;
            if t.chunks_total > 0 {
                t.chunk_states = "D".repeat(t.chunks_total as usize);
            }
        }
        t.finish(state, reason);
    });
}

/// 将下载停止写入状态文件、日志与任务历史。
/// 慢速中止会使用独立的标题，并在日志中保留实测证据，
/// 避免原因被简化成笼统的“下载失败”。
fn report_download_stop(
    err: &DownloadError,
    task_id: Option<&str>,
    control: Option<&DownloadControl>,
    done: u64,
) {
    let msg = err.message();
    let (headline, state) = match err {
        DownloadError::Stopped(StopReason::Slow {
            bps,
            min_bps,
            window_secs,
            ..
        }) => {
            logline!(
                "[download] 慢速保护触发: 实测 {} B/s, 阈值 {} B/s, 窗口 {}s",
                bps,
                min_bps,
                window_secs
            );
            logline!(
                "[download] 建议切换加速节点后重试: {}",
                github::MIRRORS.join(" / ")
            );
            ("下载速度异常缓慢，已自动停止", ST_ABORTED)
        }
        DownloadError::Stopped(_) => ("下载已取消", ST_ABORTED),
        DownloadError::Other(_) => ("固件下载失败", ST_FAILED),
    };
    status::set_error("failed", 100, headline, &msg);
    logline!("[download] {headline}: {msg}");
    if let Some(id) = task_id {
        let speed = control.map(DownloadControl::current_speed).unwrap_or(0);
        task_finish(id, state, &msg, done, speed);
    }
}

/// 把 Tokio watch 状态同步到状态文件与历史记录。
/// 进度写入由下载器的上报任务负责；本任务只处理
/// 暂停 / 继续的状态切换（这些切换不会产生字节进度）。
fn spawn_state_reporter(mut rx: watch::Receiver<RunState>, task_id: String) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if rx.changed().await.is_err() {
                return;
            }
            // 先绑定克隆后的状态：`watch::Ref` 不是 Send，
            // 不能在这个 spawned future 中跨 await 持有。
            let st = rx.borrow_and_update().clone();
            match st {
                RunState::Paused => {
                    let id = task_id.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let pct = status::read()
                            .get("pct")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        status::set("paused", pct, "下载已暂停，点击「继续」恢复");
                        task_update(&id, |t| t.state = ST_PAUSED.to_string());
                    })
                    .await;
                }
                RunState::Running => {
                    let id = task_id.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        task_update(&id, |t| t.state = ST_RUNNING.to_string());
                    })
                    .await;
                }
                RunState::Stopped(_) => return,
            }
        }
    })
}

/// 构造进度回调：实时显示吞吐量，并把历史写入
/// 限制为每 5 秒一次（历史记录存放在闪存上）。
fn progress_reporter(
    total_hint: u64,
    control: &DownloadControl,
    task_id: &str,
) -> (ProgressFn, Arc<AtomicU64>) {
    let speed = control.speed();
    let last_hist = Arc::new(AtomicU64::new(0));
    let last_done = Arc::new(AtomicU64::new(0));
    let hist_id = task_id.to_string();
    let hist_ts = Arc::clone(&last_hist);
    let done_ref = Arc::clone(&last_done);
    let bps_ref = Arc::clone(&speed);
    let report: ProgressFn = Arc::new(move |p: downloader::Progress| {
        let done = p.done;
        let total_eff = if p.total > 0 { p.total } else { total_hint };
        let pct = if total_eff > 0 {
            5 + ((done as f64 / total_eff as f64) * 85.0).min(85.0) as u32
        } else {
            5
        };
        let bps = bps_ref.load(Ordering::Relaxed);
        done_ref.store(done, Ordering::Relaxed);
        status::set(
            "downloading",
            pct,
            &format!(
                "正在下载固件... {:.1}/{:.1} MB · {:.1} KB/s · 线程 {}/{} · 分块 {}/{}",
                done as f64 / 1048576.0,
                total_eff as f64 / 1048576.0,
                bps as f64 / 1024.0,
                p.chunks_running.max(p.segments.min(p.workers)),
                p.workers,
                p.chunks_done,
                p.chunks_total
            ),
        );
        let now = unix_now();
        if now.saturating_sub(hist_ts.load(Ordering::Relaxed)) >= 5 {
            hist_ts.store(now, Ordering::Relaxed);
            let states: String = p
                .states
                .iter()
                .map(|&s| if s == 2 { 'D' } else if s == 1 { 'R' } else { 'P' })
                .collect();
            task_update(&hist_id, |t| {
                t.done = done;
                t.total = total_eff;
                t.speed_bps = bps;
                t.workers = p.workers as u64;
                t.segments = p.segments as u64;
                t.chunk_size = p.chunk_size;
                t.chunks_total = p.chunks_total as u64;
                t.chunks_done = p.chunks_done as u64;
                t.chunks_running = p.chunks_running as u64;
                t.chunk_states = states;
            });
        }
    });
    (report, last_done)
}

/// 清除一次下载尝试的全部产物（固件、时间戳标记与
/// 续传 sidecar）。固件已删除却留下 sidecar，
/// 正是重试时得到“成功”的空洞文件的原因。
fn cleanup_download() {
    let _ = std::fs::remove_file(TMP_FIRMWARE);
    let _ = std::fs::remove_file(TMP_FIRMWARE_TS);
    let _ = std::fs::remove_file(TMP_FIRMWARE_PARTS);
}

/// 仅当标记能够证明磁盘上的字节确实是上一次运行完成时的内容，
/// 才复用上次运行留下的固件文件。
///
/// 文件长度不能作为证据：下载器会用 `set_len` 预分配，
/// 半写入的文件也可能长度正确，而尾部仍是零填充。
/// 因此标记中同时保存资产的 `updated` 时间戳*和*
/// 完成文件的 SHA256；不含哈希的标记（旧版本写入）
/// 一律不可信。调用方之后仍必须执行完整校验——
/// 复用只跳过网络传输，不跳过哈希校验。
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

/// 标记格式：第 1 行为资产的 `updated` 时间戳，第 2 行为完成文件的
/// 本地 SHA256。仅在文件通过校验后写入。
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

/// 线路优先级：加速源 → 官方直连 → GitHub API 资产端点。
/// `browser_download_url` 由 github.com 的 Web 前端提供，
/// 它可能返回 504，而 API + release-assets CDN 路径仍能提供文件。
///
/// 只有线路层面的失败才会继续尝试下一个 URL。用户取消或
/// 慢速中止对整个任务都是终止性的，
/// 不能被其他镜像的静默重试掩盖。
async fn fetch_firmware(
    client: &reqwest::Client,
    asset: &Asset,
    out: &str,
    report: ProgressFn,
    control: &DownloadControl,
) -> Result<downloader::Downloaded, DownloadError> {
    let cfg = github::load_cfg();
    let mut candidates: Vec<String> = Vec::new();
    // v2.6：统一线路策略——加速源优先、官方直连兜底。
    // download_routes 只对下载主机加前缀，绝不把加速源套到
    // api.github.com，也不改写 URL 参数；TLS 校验保持开启。
    for u in crate::network::download_routes(&asset.url, &cfg.proxy) {
        if !candidates.iter().any(|x| x == &u) {
            candidates.push(u);
        }
    }
    // 兼容旧行为：保留直接前缀线路，覆盖非 GitHub 下载主机。
    let legacy = full_url(&cfg, &asset.url);
    if !candidates.iter().any(|x| x == &legacy) {
        candidates.push(legacy);
    }
    if !asset.api_url.is_empty() && !candidates.iter().any(|x| x == &asset.api_url) {
        candidates.push(asset.api_url.clone());
    }
    let mut last = DownloadError::Other("没有可用的下载地址".to_string());
    for (i, url) in candidates.iter().enumerate() {
        if i > 0 {
            logline!(
                "[download] 上一线路失败({}),回退到 GitHub API 资产直链",
                last.message()
            );
            // sidecar 以 URL 为键，但来自其他线路的半成品文件
            // 不能被误认为可续传的进度
            let _ = std::fs::remove_file(out);
            let _ = std::fs::remove_file(format!("{out}.parts.json"));
        }
        let shown = downloader::short_url(url);
        logline!("[download] 线路{}: {}", i + 1, shown);
        match downloader::download(client, url, out, report.clone(), control).await {
            Ok(d) => return Ok(d),
            Err(e) => {
                if !e.is_route_failure() {
                    return Err(e);
                }
                last = e;
            }
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

/// v2.6：下载完成后，若目标明确是归档包（ZIP / tar 系列），先做只读安全清单
/// 解析。普通 `.bin` / `.img` / `.gz` 镜像零额外开销。
///
/// - 恶意路径、解压炸弹、条目数超限、结构损坏一律拒绝，`force` 也不豁免；
/// - 归档内没有任何可刷写镜像时拒绝直接刷写（可由 `force` 豁免）；
/// - 其余情况只记录诊断，旧 `fwtool` / `sysupgrade -T` 决策链完全不变。
fn inspect_downloaded_archive(asset: &Asset, force: bool) -> Result<(), String> {
    const MAX_INSPECT_BYTES: u64 = 256 * 1024 * 1024;
    let lower = asset.name.to_ascii_lowercase();
    let is_archive = [
        ".zip", ".tar", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.zst", ".7z",
    ]
    .iter()
    .any(|e| lower.ends_with(e));
    if !is_archive {
        return Ok(());
    }

    let size = std::fs::metadata(TMP_FIRMWARE)
        .map(|m| m.len())
        .unwrap_or(0);
    if size > MAX_INSPECT_BYTES {
        return Err(format!(
            "归档包体积 {} 超过安全解析上限 {}，已拒绝（避免耗尽内存）",
            github::human_size(size),
            github::human_size(MAX_INSPECT_BYTES)
        ));
    }

    match crate::firmware::inspect_archive_file(TMP_FIRMWARE) {
        Ok(entries) => {
            let flashable = crate::firmware::flashable_from_entries(&entries);
            logline!(
                "[verify] 归档包只读清单解析: {} 个条目, 可刷写镜像候选 {} 个",
                entries.len(),
                flashable.len()
            );
            for e in flashable.iter().take(5) {
                logline!("[verify] 归档内固件候选: {} ({} bytes, 压缩 {} bytes)", e.name, e.size, e.compressed_size);
            }
            if flashable.is_empty() && !force {
                return Err(format!(
                    "归档包 [{}] 内未发现可刷写的 sysupgrade/factory 镜像，请勿直接刷写压缩包",
                    asset.name
                ));
            }
            Ok(())
        }
        Err(crate::firmware::ArchiveError::Unsupported) => Ok(()),
        Err(e) => Err(format!("固件归档包安全校验未通过: {}", e.message())),
    }
}

fn verify_firmware_integrity(
    device: &crate::compat::DeviceProfile,
    asset: &Asset,
    force: bool,
) -> Result<Integrity, String> {
    status::set("verifying", 91, "正在校验固件 SHA256 哈希完整性...");

    if let Err(e) = inspect_downloaded_archive(asset, force) {
        cleanup_download();
        return Err(e);
    }

    let actual_sha256 =
        crate::hash::sha256_file(TMP_FIRMWARE).map_err(|e| format!("SHA256 计算失败: {e}"))?;

    logline!("[verify] 固件实际 SHA256: {actual_sha256}");

    // 优先使用 UCI 中显式配置的值，其次使用 Release 发布的值。
    let manual_expected = uci::get("expected_sha256")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let (expected, source) = if !manual_expected.is_empty() {
        (manual_expected, "config")
    } else {
        (
            asset.sha256.trim().to_lowercase(),
            asset.sha256_source.as_str(),
        )
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
        // 任何来源都没有期望哈希。此时不能宣称“已校验”：
        // 界面与状态文案必须写成“已记录但未校验”。
        logline!(
            "[verify] 未提供校验依据(配置/校验单/Release 正文均无哈希),仅记录本地 SHA256: {actual_sha256}"
        );
        false
    };

    // 硬件与 sysupgrade -T 镜像兼容性校验
    let mut l5_passed = true;
    let mut l5_detail = "本机 fwtool / sysupgrade -T 预检通过".to_string();
    if let Err(e) = crate::compat::verify_downloaded_image(device, TMP_FIRMWARE) {
        l5_passed = false;
        l5_detail = e.clone();
        if !force {
            cleanup_download();
            return Err(e);
        }
        logline!("[verify] 强制模式生效，跳过未通过的固件兼容性预检: {e}");
    }

    // v2.6：把 L4/L5 的真实结果回写到四套状态模型中 compat 自己的缓存。
    // 这里只做状态记录，不改变本函数返回值，也不参与刷写决策。
    let l4_basis = !expected.is_empty();
    let l4_detail = if l4_basis {
        format!("SHA256 校验一致（来源 {source}）")
    } else {
        "未提供校验依据，仅记录本地 SHA256，未宣称已校验".to_string()
    };
    crate::compatstate::record_download_result(
        &asset.name,
        asset.size,
        &actual_sha256,
        asset.rule.as_deref(),
        l4_basis,
        verified,
        &l4_detail,
        l5_passed,
        &l5_detail,
    );

    Ok(Integrity {
        verified,
        source: source.to_string(),
        sha256: actual_sha256,
    })
}

/// `online-upgrade download` —— 仅执行检查、分段下载与校验。
/// 不会写入闪存，也不执行备份或 sysupgrade。
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
    let task_id: Option<String>;
    let mut control: Option<DownloadControl> = None;
    if reuse_download(&asset.updated, asset.size) {
        // 仅当残留文件的标记带有完成字节的哈希时才复用；
        // 无论是否复用，下面都会重新校验该哈希。
        total = std::fs::metadata(TMP_FIRMWARE)
            .map(|m| m.len())
            .unwrap_or(0);
        logline!(
            "[download] 复用上一轮已校验的固件副本 {} ({}),跳过传输但仍重新校验哈希",
            asset.name,
            github::human_size(total)
        );
        status::set("verifying", 90, "复用已下载固件,正在重新校验...");
        // 复用已有下载时，仍会创建用户可见、可删除的任务记录。
        let mut store = TaskStore::open();
        let mut t = Task::new(
            &asset.name,
            &full_url(&github::load_cfg(), &asset.url),
            total,
        );
        t.done = total;
        t.finish(ST_DONE, "复用上一轮已校验固件");
        task_id = Some(t.id.clone());
        store.upsert(t);
        let _ = store.save();
    } else {
        // 这里不要清除固件和 sidecar：load_parts() 会拒绝
        // url/etag/total/分块网格已不匹配的 sidecar，
        // 而保留下来的 sidecar 正是让上次进程中中断的下载
        // 能够续传的关键。清除它会导致每次重启都从
        // 第 0 字节重新开始。
        let ctl = match DownloadControl::start() {
            Ok(c) => c,
            Err(e) => {
                status::set_error("failed", 100, "无法启动下载任务", &e);
                logline!("[download] control init failed: {e}");
                return 1;
            }
        };
        let mut store = TaskStore::open();
        let t = Task::new(
            &asset.name,
            &full_url(&github::load_cfg(), &asset.url),
            asset.size,
        );
        let id = t.id.clone();
        store.upsert(t);
        let _ = store.save();
        task_id = Some(id.clone());
        let state_reporter = spawn_state_reporter(ctl.rx(), id.clone());

        status::set("downloading", 5, "正在下载固件...");
        let (report, last_done) = progress_reporter(asset.size, &ctl, &id);
        match fetch_firmware(&client, &asset, TMP_FIRMWARE, report, &ctl).await {
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
                state_reporter.abort();
                report_download_stop(
                    &e,
                    task_id.as_deref(),
                    Some(&ctl),
                    last_done.load(Ordering::Relaxed),
                );
                // 保留下载器判定为可续传的内容
                return 1;
            }
        }
        state_reporter.abort();
        control = Some(ctl);
    }

    let integrity = match verify_firmware_integrity(&device, &asset, false) {
        Ok(i) => i,
        Err(e) => {
            status::set_error("failed", 100, "固件校验或兼容性预检失败", &e);
            if let Some(id) = &task_id {
                task_finish(id, ST_FAILED, &e, total, 0);
            }
            return 1;
        }
    };
    // 标记只在校验通过后写入：它表示“该文件
    // 已通过哈希校验”，这正是下次复用安全的前提。
    write_download_marker(&asset.updated, &integrity.sha256);
    if let Some(id) = &task_id {
        let speed = control
            .as_ref()
            .map(DownloadControl::current_speed)
            .unwrap_or(0);
        task_update(id, |t| {
            if t.state != ST_DONE {
                t.done = total;
                t.speed_bps = speed;
                t.finish(ST_DONE, "");
            }
        });
    }
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
            ("size_human".into(), Value::str(&github::human_size(total))),
            ("sha256".into(), Value::str(&integrity.sha256)),
            ("verified".into(), Value::Bool(integrity.verified)),
            ("hash_source".into(), Value::str(&integrity.source)),
            ("version".into(), Value::str(&asset.version)),
            ("version_source".into(), Value::str(&asset.version_source)),
            ("relation".into(), Value::str(&asset.relation)),
            ("segments".into(), Value::num(segments as u64)),
            ("resumed_chunks".into(), Value::num(resumed_chunks as u64)),
            (
                "reused".into(),
                Value::Bool(resumed_chunks == 0 && segments == 0),
            ),
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

    // ---- 步骤 0：检查更新并获取资产信息 ----
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
    // 同版本不是「可升级」：普通升级路径直接拒绝，重装必须显式 --force
    // （前端在同版本时也只保留强制升级入口，两处口径一致）。
    if asset.relation == "same" && !force {
        let err = format!(
            "目标版本 v{} 与当前固件同版本，已跳过普通升级(如需同版本重装请使用强制升级)",
            fw_version
        );
        logline!("[upgrade] {err}");
        status::set_error("failed", 100, "已是最新版本", &err);
        return 1;
    }

    // ---- 步骤 1：下载（分段、真实进度）----
    // 复用与否取决于标记中记录的哈希（见 reuse_download）；
    // 下面的校验在两条分支上都会执行，因此复用文件仍必须
    // 在写入闪存之前匹配期望的校验和。
    let task_id: Option<String>;
    let mut control: Option<DownloadControl> = None;
    if reuse_download(&updated, size_hint) {
        logline!("[upgrade] 复用上一轮已校验的固件副本,跳过传输({})", name);
        status::set("downloaded", 90, "复用已下载固件,正在重新校验...");
        task_id = None;
    } else {
        // 参见 download_only()：sidecar 是实现跨进程续传的关键，
        // 且 load_parts() 已经对它做过校验。
        let ctl = match DownloadControl::start() {
            Ok(c) => c,
            Err(e) => {
                status::set_error("failed", 100, "无法启动下载任务", &e);
                return 1;
            }
        };
        let mut store = TaskStore::open();
        let t = Task::new(&name, &full_url(&github::load_cfg(), &asset.url), size_hint);
        let id = t.id.clone();
        store.upsert(t);
        let _ = store.save();
        task_id = Some(id.clone());
        let state_reporter = spawn_state_reporter(ctl.rx(), id.clone());

        status::set("downloading", 5, "正在下载固件...");
        let (report, last_done) = progress_reporter(size_hint, &ctl, &id);
        match fetch_firmware(&client, &asset, TMP_FIRMWARE, report, &ctl).await {
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
                state_reporter.abort();
                report_download_stop(
                    &e,
                    task_id.as_deref(),
                    Some(&ctl),
                    last_done.load(Ordering::Relaxed),
                );
                // 保留下载器判定为可续传的内容
                return 1;
            }
        }
        state_reporter.abort();
        control = Some(ctl);
    }

    // ---- 步骤 1.5：校验 SHA256 与镜像完整性 ----
    let integrity = match verify_firmware_integrity(&device, &asset, force) {
        Ok(i) => i,
        Err(e) => {
            status::set_error("failed", 100, "固件校验或预检失败", &e);
            if let Some(id) = &task_id {
                task_finish(id, ST_FAILED, &e, 0, 0);
            }
            return 1;
        }
    };
    if let Some(id) = &task_id {
        let speed = control
            .as_ref()
            .map(DownloadControl::current_speed)
            .unwrap_or(0);
        task_finish(id, ST_DONE, "", size_hint, speed);
    }
    logline!(
        "[upgrade] 校验结果: verified={} source={} sha256={}",
        integrity.verified,
        integrity.source,
        integrity.sha256
    );
    // 只有通过校验的文件才写入复用标记。
    write_download_marker(&updated, &integrity.sha256);

    // ---- 步骤 2：备份成功后记录版本 ----
    // 顺序很重要：先写版本会在备份失败时留下错误的
    // “已升级”标记；备份后写入若不回滚，
    // 在下方刷写失败时也会产生同样的问题。
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

    // ---- 步骤 3：备份（sysupgrade -b，稍后传给 -f）----
    status::set("backing_up", 94, "正在创建配置备份...");
    let _ = std::fs::create_dir_all("/etc/uci-defaults");
    let _ = std::fs::write(UCI_DEFAULTS_AUTO, AUTO_REINSTALL_SCRIPT);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(UCI_DEFAULTS_AUTO, std::fs::Permissions::from_mode(0o755));
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
        // 回滚版本标记：没有备份就等同于没有发生升级。
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

    // ---- 步骤 4：保存已安装软件包清单（供升级后自动重装）----
    // 只记录包名（不含版本）：重装脚本不能依赖
    // 特定包管理器的输出格式。
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

    // ---- 步骤 5：刷写（成功时不会返回）----
    status::set("flashing", 100, "正在刷写固件,配置将自动恢复,即将重启...");
    logline!("[upgrade] 执行: sysupgrade -f {backup_tmp} {TMP_FIRMWARE}");
    let _ = sh("sync", &[]);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let _ = Command::new("/sbin/sysupgrade")
        .args(["-f", &backup_tmp, TMP_FIRMWARE])
        .status();

    // sysupgrade 若返回，说明执行失败
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
    sh(
        "sh",
        &["-c", &format!("command -v {cmd} >/dev/null 2>&1 && echo y")],
    )
    .as_deref()
        == Some("y")
}
