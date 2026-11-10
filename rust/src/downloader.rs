// Segmented concurrent downloader: dynamic chunk queue with work-stealing
// (fast connections naturally take over slow segments), per-chunk retry with a
// stall watchdog, shared progress counter, and a sidecar file for resuming.
//
// Sized for this plugin's use case: one firmware file, optional URL-prefix
// proxy, progress fed into the LuCI status JSON.
//
// Task control is Tokio-native: a `watch::Receiver<RunState>` drives pause /
// cancel across every chunk worker, a Tokio task polls the out-of-band control
// file written by `online-upgrade pause|resume|cancel`, and a throughput
// monitor stops the transfer when it stays abnormally slow.

use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::Client;
use std::collections::VecDeque;
use std::io::SeekFrom;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::taskctl::{DownloadControl, RunState, SlowPolicy, StopReason};

const MAX_WORKERS: usize = 6;
const SINGLE_STREAM_MAX: u64 = 2 * 1024 * 1024; // below this, don't bother splitting
const MAX_CHUNK_RETRIES: u32 = 3;

/// Failure of a download attempt.
///
/// `Stopped` is terminal for the whole task (user cancel or slow-speed guard)
/// and must NOT trigger a fallback to another mirror; `Other` is a route-level
/// failure that the caller may retry on a different URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadError {
    /// Terminal stop with a typed reason.
    Stopped(StopReason),
    /// Network / IO / protocol failure of this route.
    Other(String),
}

impl DownloadError {
    /// User-facing message (status file, log, task history).
    pub fn message(&self) -> String {
        match self {
            DownloadError::Stopped(r) => r.text().to_string(),
            DownloadError::Other(s) => s.clone(),
        }
    }

    /// Whether another download route may be tried.
    pub fn is_route_failure(&self) -> bool {
        matches!(self, DownloadError::Other(_))
    }
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Stopped(r) => f.write_str(r.text()),
            DownloadError::Other(s) => f.write_str(s),
        }
    }
}

/// Tunables read from UCI (`online-upgrade.settings.dl_*`), clamped to sane ranges.
struct DlCfg {
    workers: usize,
    chunk_size: u64,
    stall: Duration,
    retries: u32,
    slow: SlowPolicy,
}

impl DlCfg {
    fn load() -> DlCfg {
        let get_u32 = |key: &str, def: u32, lo: u32, hi: u32| -> u32 {
            crate::uci::get(key)
                .and_then(|v| v.parse::<u32>().ok())
                .map(|v| v.clamp(lo, hi))
                .unwrap_or(def)
        };
        DlCfg {
            workers: get_u32("dl_workers", MAX_WORKERS as u32, 1, 16) as usize,
            chunk_size: get_u32("dl_chunk_mb", 4, 1, 32) as u64 * 1024 * 1024,
            stall: Duration::from_secs(get_u32("dl_stall_sec", 30, 5, 600) as u64),
            retries: get_u32("dl_retries", MAX_CHUNK_RETRIES, 1, 10),
            slow: SlowPolicy::load(),
        }
    }
}

/// Result of a completed download.
pub struct Downloaded {
    /// Final file size in bytes.
    pub total: u64,
    /// Entity tag of the asset, when the server supplied one.
    #[allow(dead_code)] // reserved for future cache validation
    pub etag: Option<String>,
    /// Chunks restored from a previous run's sidecar.
    pub resumed_chunks: usize,
    /// Number of concurrent segments used.
    pub segments: usize,
}

struct Probe {
    total: Option<u64>,
    ranges: bool,
    etag: Option<String>,
}

/// HEAD-less range probe: asks for byte 0-0 and derives size / range support.
async fn probe(client: &Client, url: &str) -> Result<Probe, String> {
    let resp = client
        .get(url)
        // Required by api.github.com/releases/assets/<id>: without it the API
        // answers with JSON metadata instead of redirecting to the file.
        .header("Accept", "application/octet-stream")
        .header("Range", "bytes=0-0")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("探测失败: {e}"))?;

    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let status = resp.status().as_u16();
    if status == 206 {
        // Content-Range: bytes 0-0/123456
        let cr = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let total = cr.rsplit('/').next().and_then(|s| s.parse::<u64>().ok());
        return Ok(Probe {
            total,
            ranges: total.is_some(),
            etag,
        });
    }
    if status == 200 {
        let total = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        return Ok(Probe {
            total,
            ranges: false,
            etag,
        });
    }
    Err(format!("HTTP {status}"))
}

// ---- resume sidecar ----

#[derive(Clone)]
struct Parts {
    url: String,
    etag: String,
    total: u64,
    chunk: u64,
    done: Vec<usize>,
}

fn parts_path(out: &str) -> String {
    format!("{out}.parts.json")
}

/// A sidecar is only trustworthy when all three hold:
/// 1. the data file it describes still exists with the expected length
///    (a deleted/truncated firmware must never keep its "done" map);
/// 2. the chunk grid is unchanged — changing `dl_chunk_mb` remaps indices to
///    completely different byte ranges;
/// 3. every recorded index is inside the current grid.
///
/// Anything else is discarded, so a resume can never skip bytes that were
/// never written.
fn load_parts(
    out: &str,
    url: &str,
    etag: &Option<String>,
    total: u64,
    chunk: u64,
    n_chunks: usize,
) -> Option<Parts> {
    let len = std::fs::metadata(out).ok()?.len();
    if len != total {
        return None;
    }
    let text = std::fs::read_to_string(parts_path(out)).ok()?;
    let v = crate::json::parse(&text).ok()?;
    let same = v.get("url").and_then(|x| x.as_str()) == Some(url)
        && v.get("total").and_then(|x| x.as_u64()) == Some(total)
        && v.get("chunk").and_then(|x| x.as_u64()) == Some(chunk)
        && v.get("etag")
            .and_then(|x| x.as_str())
            .map(|s| Some(s.to_string()))
            .unwrap_or(None)
            == etag.clone();
    if !same {
        return None;
    }
    let mut done: Vec<usize> = v
        .get("done")
        .and_then(|x| x.as_arr())?
        .iter()
        .filter_map(|x| x.as_u64().map(|n| n as usize))
        .collect();
    if done.iter().any(|&i| i >= n_chunks) {
        return None;
    }
    done.sort_unstable();
    done.dedup();
    Some(Parts {
        url: url.to_string(),
        etag: etag.clone().unwrap_or_default(),
        total,
        chunk,
        done,
    })
}

fn save_parts(out: &str, p: &Parts) {
    let pairs: Vec<(String, crate::json::Value)> = vec![
        ("url".into(), crate::json::Value::str(&p.url)),
        ("etag".into(), crate::json::Value::str(&p.etag)),
        ("total".into(), crate::json::Value::num(p.total)),
        ("chunk".into(), crate::json::Value::num(p.chunk)),
        (
            "done".into(),
            crate::json::Value::Arr(
                p.done
                    .iter()
                    .map(|&i| crate::json::Value::num(i as u64))
                    .collect(),
            ),
        ),
    ];
    let body = crate::json::Value::Obj(pairs).dump();
    let tmp = format!("{}.tmp", parts_path(out));
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, parts_path(out));
    }
}

fn drop_parts(out: &str) {
    let _ = std::fs::remove_file(parts_path(out));
}

// ---- chunk queue ----

#[derive(Clone, Debug, PartialEq, Eq)]
struct Chunk {
    idx: usize,
    start: u64,
    end: u64, // inclusive
}

impl Chunk {
    fn len(&self) -> u64 {
        self.end - self.start + 1
    }
}

fn build_chunks(total: u64, chunk_size: u64) -> Vec<Chunk> {
    let mut v = Vec::new();
    let mut start = 0u64;
    while start < total {
        let end = (start + chunk_size).min(total) - 1;
        v.push(Chunk {
            idx: v.len(),
            start,
            end,
        });
        start = end + 1;
    }
    v
}

// ---- task control helpers ----

/// Wait until the task is running again. Pause is honoured only at chunk
/// boundaries so an HTTP response stream is never held open while idle.
async fn wait_running(state: &mut watch::Receiver<RunState>) -> Result<(), DownloadError> {
    loop {
        let cur = state.borrow_and_update().clone();
        match cur {
            RunState::Running => return Ok(()),
            RunState::Paused => {}
            RunState::Stopped(r) => return Err(DownloadError::Stopped(r)),
        }
        if state.changed().await.is_err() {
            return Err(DownloadError::Stopped(StopReason::UserCancel(
                "下载控制通道已关闭".to_string(),
            )));
        }
    }
}

/// Pure sliding-window tracker for the slow-speed guard. Kept separate from
/// the async monitor so the decision rule is unit-testable without timers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SlowTracker {
    low_secs: u64,
    low_bytes: u64,
}

impl SlowTracker {
    /// Feed one second of throughput. Returns `(avg_bps, low_secs)` once the
    /// throughput stayed below the floor for the whole observation window.
    fn observe(&mut self, bps: u64, policy: &SlowPolicy, in_grace: bool) -> Option<(u64, u64)> {
        if in_grace || !policy.enabled() {
            self.reset();
            return None;
        }
        if bps < policy.min_bps {
            self.low_secs = self.low_secs.saturating_add(1);
            self.low_bytes = self.low_bytes.saturating_add(bps);
            if self.low_secs >= policy.window_secs {
                return Some((self.low_bytes / self.low_secs, self.low_secs));
            }
        } else {
            self.reset();
        }
        None
    }

    fn reset(&mut self) {
        self.low_secs = 0;
        self.low_bytes = 0;
    }
}

/// Shorten a URL for logs without slicing in the middle of a UTF-8 character.
pub(crate) fn short_url(url: &str) -> String {
    let mut s: String = url.chars().take(90).collect();
    if url.chars().count() > 90 {
        s.push('…');
    }
    s
}

/// Tokio task that samples the shared byte counter once per second and stops
/// the download when throughput stays below the configured floor.
fn spawn_slow_monitor(
    downloaded: Arc<AtomicU64>,
    policy: SlowPolicy,
    control: &DownloadControl,
    route: String,
) -> Option<JoinHandle<()>> {
    if !policy.enabled() {
        return None;
    }
    let tx = control.tx();
    let speed = control.speed();
    Some(tokio::spawn(async move {
        let start = Instant::now();
        let mut last = downloaded.load(Ordering::Relaxed);
        let mut tracker = SlowTracker::default();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let now = downloaded.load(Ordering::Relaxed);
            let delta = now.saturating_sub(last);
            last = now;
            speed.store(delta, Ordering::Relaxed);

            let st = tx.borrow().clone();
            if matches!(st, RunState::Stopped(_)) {
                return;
            }
            if matches!(st, RunState::Paused) {
                tracker.reset();
                continue;
            }
            let in_grace = start.elapsed() < Duration::from_secs(policy.grace_secs);
            if let Some((avg, secs)) = tracker.observe(delta, &policy, in_grace) {
                let reason = format!(
                    "下载速度异常缓慢，已自动停止：近 {secs} 秒平均 {:.1} KB/s，低于阈值 {:.1} KB/s；线路 {}。请切换加速节点或稍后重试（当前线路的断点记录已保留）",
                    avg as f64 / 1024.0,
                    policy.min_bps as f64 / 1024.0,
                    short_url(&route)
                );
                let _ = tx.send(RunState::Stopped(StopReason::Slow {
                    reason,
                    bps: avg,
                    min_bps: policy.min_bps,
                    window_secs: secs,
                }));
                return;
            }
        }
    }))
}

// ---- core ----

/// Download `url` to `out`, concurrently. `report(done_bytes, total)` is called
/// at most every ~500 ms. Returns final size.
///
/// `control` carries the Tokio watch channel used for pause / cancel and the
/// slow-speed guard; the caller owns it so a single task can span multiple
/// fallback routes.
pub async fn download(
    client: &Client,
    url: &str,
    out: &str,
    report: Arc<dyn Fn(u64, u64) + Send + Sync>,
    control: &DownloadControl,
) -> Result<Downloaded, DownloadError> {
    // A cancel that arrived before the first byte must not start a new
    // connection at all.
    if let RunState::Stopped(r) = control.state() {
        return Err(DownloadError::Stopped(r));
    }
    let p = probe(client, url).await.map_err(DownloadError::Other)?;

    let dl = DlCfg::load();
    crate::logline!(
        "[download] 并发 {} / 分块 {} KB / 停滞 {}s / 重试 {} / 低速阈值 {} KB/s (窗口 {}s, 宽限 {}s)",
        dl.workers,
        dl.chunk_size / 1024,
        dl.stall.as_secs(),
        dl.retries,
        dl.slow.min_bps / 1024,
        dl.slow.window_secs,
        dl.slow.grace_secs
    );
    if dl.workers <= 1 || !p.ranges || p.total.map(|t| t <= SINGLE_STREAM_MAX).unwrap_or(true) {
        return single_stream(client, url, out, p.total, p.etag, report, control, &dl).await;
    }
    let total = p
        .total
        .ok_or_else(|| DownloadError::Other("服务器未返回文件大小,无法分段下载".to_string()))?;
    let segments = ((total / dl.chunk_size).min(dl.workers as u64)).max(2) as usize;
    let all_chunks = build_chunks(total, dl.chunk_size);

    // resume state
    let prev = load_parts(out, url, &p.etag, total, dl.chunk_size, all_chunks.len());
    if prev.is_none() {
        // untrusted sidecar (file gone, chunk grid changed, ...) must not
        // linger next to a freshly started download
        drop_parts(out);
    }
    let mut queue: VecDeque<Chunk> = VecDeque::new();
    let mut already_done: u64 = 0;
    let mut resumed_chunks = 0usize;
    let mut done_flags = vec![false; all_chunks.len()];
    if let Some(prev) = &prev {
        for c in &all_chunks {
            if prev.done.contains(&c.idx) {
                done_flags[c.idx] = true;
                already_done += c.len();
                resumed_chunks += 1;
            } else {
                queue.push_back(c.clone());
            }
        }
    } else {
        queue.extend(all_chunks.iter().cloned());
    }
    // preallocate
    File::options()
        .write(true)
        .create(true)
        // Resume reuses the preallocated file: truncating it would destroy
        // already-written chunks. `set_len` below establishes the full size.
        .truncate(false)
        .open(out)
        .await
        .map_err(|e| DownloadError::Other(format!("无法创建文件 {out}: {e}")))?
        .set_len(total)
        .await
        .map_err(|e| DownloadError::Other(format!("预分配失败: {e}")))?;

    let downloaded = Arc::new(AtomicU64::new(already_done));
    let queue = Arc::new(Mutex::new(queue));
    let done_flags = Arc::new(Mutex::new(done_flags));

    let parts_state = Arc::new(Mutex::new(Parts {
        url: url.to_string(),
        etag: p.etag.clone().unwrap_or_default(),
        total,
        chunk: dl.chunk_size,
        done: prev.map(|x| x.done).unwrap_or_default(),
    }));

    let slow_monitor =
        spawn_slow_monitor(Arc::clone(&downloaded), dl.slow, control, url.to_string());

    let mut handles = Vec::with_capacity(segments);
    for _ in 0..segments {
        let client = client.clone();
        let url = url.to_string();
        let out = out.to_string();
        let queue = Arc::clone(&queue);
        let done_flags = Arc::clone(&done_flags);
        let downloaded = Arc::clone(&downloaded);
        let parts_state = Arc::clone(&parts_state);
        let mut state = control.rx();
        let stall = dl.stall;
        let retries = dl.retries;
        handles.push(tokio::spawn(async move {
            loop {
                // Pause is observed between chunks; a stop is observed inside
                // the byte loop as well, so cancellation is prompt.
                wait_running(&mut state).await?;
                let chunk = {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    q.pop_front()
                };
                let Some(chunk) = chunk else { break };
                let mut ok = false;
                for attempt in 1..=retries {
                    match fetch_range(
                        &client,
                        &url,
                        chunk.start,
                        chunk.end,
                        out.as_str(),
                        &downloaded,
                        stall,
                        &mut state,
                    )
                    .await
                    {
                        Ok(()) => {
                            ok = true;
                            break;
                        }
                        // Terminal stop: keep the original typed reason and do
                        // not burn the remaining retries.
                        Err(e @ DownloadError::Stopped(_)) => return Err(e),
                        Err(DownloadError::Other(e)) => {
                            crate::logline!("  分段 {} 第{}次重试失败: {}", chunk.idx, attempt, e);
                            tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                        }
                    }
                }
                if !ok {
                    return Err(DownloadError::Other(format!("分段 {} 下载失败", chunk.idx)));
                }
                // mark done + persist sidecar
                {
                    let mut flags = done_flags.lock().unwrap_or_else(|e| e.into_inner());
                    flags[chunk.idx] = true;
                    let mut ps = parts_state.lock().unwrap_or_else(|e| e.into_inner());
                    ps.done.push(chunk.idx);
                    save_parts(&out, &ps);
                }
            }
            Ok::<(), DownloadError>(())
        }));
    }

    // progress reporter
    let rep_handle = {
        let downloaded = Arc::clone(&downloaded);
        let report = Arc::clone(&report);
        tokio::spawn(async move {
            let mut last = 0u64;
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let d = downloaded.load(Ordering::Relaxed);
                if d != last {
                    report(d, total);
                    last = d;
                }
                if d >= total {
                    break;
                }
            }
        })
    };

    let mut err: Option<DownloadError> = None;
    for h in handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if err.is_none() {
                    err = Some(e);
                }
            }
            Err(join_err) => {
                if err.is_none() {
                    err = Some(DownloadError::Other(format!(
                        "下载任务异常退出: {join_err}"
                    )));
                }
            }
        }
    }
    rep_handle.abort();
    if let Some(h) = slow_monitor {
        h.abort();
    }
    if let Some(e) = err {
        // Keep the sidecar only when the data file is still intact at full
        // length; otherwise a later resume would trust a "done" map that no
        // longer describes this file (the classic zero-filled hole bug).
        keep_resumable(out, total);
        return Err(e);
    }

    // Every chunk must be marked done. Length alone cannot prove that: a
    // preallocated file has the right length even with unwritten regions.
    {
        let flags = done_flags.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(missing) = flags.iter().position(|&d| !d) {
            keep_resumable(out, total);
            return Err(DownloadError::Other(format!(
                "分段 {} 未完成,已保留续传记录",
                missing
            )));
        }
    }

    let final_size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    if final_size != total {
        let _ = std::fs::remove_file(out);
        drop_parts(out);
        return Err(DownloadError::Other(format!(
            "文件大小不匹配: 期望 {total}, 实际 {final_size}"
        )));
    }
    report(total, total);
    drop_parts(out);
    Ok(Downloaded {
        total,
        etag: p.etag,
        resumed_chunks,
        segments,
    })
}

/// On failure: keep the sidecar only if the file can actually be resumed from
/// it, otherwise delete both so a retry starts from a clean slate.
fn keep_resumable(out: &str, total: u64) {
    let ok = std::fs::metadata(out)
        .map(|m| m.len() == total)
        .unwrap_or(false);
    if ok {
        crate::logline!("[download] 中断,已保留续传记录 {}", parts_path(out));
    } else {
        let _ = std::fs::remove_file(out);
        drop_parts(out);
        crate::logline!("[download] 中断且文件不可续传,已清理临时文件");
    }
}

#[allow(clippy::too_many_arguments)]
async fn fetch_range(
    client: &Client,
    url: &str,
    start: u64,
    end: u64,
    out: &str,
    downloaded: &AtomicU64,
    stall_timeout: Duration,
    state: &mut watch::Receiver<RunState>,
) -> Result<(), DownloadError> {
    let resp = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .header("Range", format!("bytes={start}-{end}"))
        .send()
        .await
        .map_err(|e| DownloadError::Other(format!("{e}")))?;
    if resp.status().as_u16() != 206 {
        return Err(DownloadError::Other(format!(
            "HTTP {}",
            resp.status().as_u16()
        )));
    }
    let mut file = File::options()
        .write(true)
        .open(out)
        .await
        .map_err(|e| DownloadError::Other(format!("打开文件失败: {e}")))?;
    file.seek(SeekFrom::Start(start))
        .await
        .map_err(|e| DownloadError::Other(format!("seek 失败: {e}")))?;

    let mut stream = resp.bytes_stream();
    let mut written: u64 = 0;
    loop {
        // `state.changed()` is cancel-safe, so selecting it against the stream
        // does not lose a stop notification when a data chunk wins the race.
        let item = tokio::select! {
            biased;
            _res = state.changed() => {
                match state.borrow_and_update().clone() {
                    RunState::Stopped(r) => return Err(DownloadError::Stopped(r)),
                    // Paused takes effect at the next chunk boundary; keep
                    // draining this response so the connection is not idle.
                    _ => continue,
                }
            }
            res = tokio::time::timeout(stall_timeout, stream.next()) => res,
        };
        let item = match item {
            Ok(Some(item)) => item.map_err(|e| DownloadError::Other(format!("读取失败: {e}")))?,
            Ok(None) => break,
            Err(_) => {
                return Err(DownloadError::Other(format!(
                    "停滞超过 {}s",
                    stall_timeout.as_secs()
                )))
            }
        };
        let b: Bytes = item;
        file.write_all(&b)
            .await
            .map_err(|e| DownloadError::Other(format!("写入失败: {e}")))?;
        written += b.len() as u64;
        downloaded.fetch_add(b.len() as u64, Ordering::Relaxed);
    }
    file.flush()
        .await
        .map_err(|e| DownloadError::Other(format!("flush 失败: {e}")))?;
    if written != end - start + 1 {
        return Err(DownloadError::Other(format!(
            "分段不完整: 期望 {}, 实际 {}",
            end - start + 1,
            written
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn single_stream(
    client: &Client,
    url: &str,
    out: &str,
    total: Option<u64>,
    etag: Option<String>,
    report: Arc<dyn Fn(u64, u64) + Send + Sync>,
    control: &DownloadControl,
    dl: &DlCfg,
) -> Result<Downloaded, DownloadError> {
    let res = single_stream_inner(client, url, out, total, etag, report, control, dl).await;
    match res {
        Ok(d) => {
            // no resume state applies to a single stream: make sure an old
            // sidecar from a segmented attempt cannot be reused later
            drop_parts(out);
            Ok(d)
        }
        Err(e) => {
            let _ = std::fs::remove_file(out);
            drop_parts(out);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn single_stream_inner(
    client: &Client,
    url: &str,
    out: &str,
    total: Option<u64>,
    etag: Option<String>,
    report: Arc<dyn Fn(u64, u64) + Send + Sync>,
    control: &DownloadControl,
    dl: &DlCfg,
) -> Result<Downloaded, DownloadError> {
    let resp = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .timeout(Duration::from_secs(3600))
        .send()
        .await
        .map_err(|e| DownloadError::Other(format!("{e}")))?;
    if !resp.status().is_success() {
        return Err(DownloadError::Other(format!(
            "HTTP {}",
            resp.status().as_u16()
        )));
    }
    let total = resp.content_length().or(total).unwrap_or(0);
    let mut file = File::create(out)
        .await
        .map_err(|e| DownloadError::Other(format!("创建文件失败: {e}")))?;
    let mut stream = resp.bytes_stream();
    let mut done: u64 = 0;
    let mut last_report = Instant::now();
    let counter = Arc::new(AtomicU64::new(0));
    let mut state = control.rx();
    let slow_monitor = spawn_slow_monitor(Arc::clone(&counter), dl.slow, control, url.to_string());
    let result = loop {
        wait_running(&mut state).await?;
        let item = tokio::select! {
            biased;
            _res = state.changed() => {
                match state.borrow_and_update().clone() {
                    RunState::Stopped(r) => break Err(DownloadError::Stopped(r)),
                    _ => continue,
                }
            }
            res = tokio::time::timeout(dl.stall, stream.next()) => res,
        };
        let item = match item {
            Ok(Some(item)) => item.map_err(|e| DownloadError::Other(format!("读取失败: {e}")))?,
            Ok(None) => break Ok(()),
            Err(_) => {
                break Err(DownloadError::Other(format!(
                    "停滞超过 {}s",
                    dl.stall.as_secs()
                )))
            }
        };
        let b: Bytes = item;
        file.write_all(&b)
            .await
            .map_err(|e| DownloadError::Other(format!("写入失败: {e}")))?;
        done += b.len() as u64;
        counter.fetch_add(b.len() as u64, Ordering::Relaxed);
        if last_report.elapsed() >= Duration::from_millis(500) {
            report(done, total);
            last_report = Instant::now();
        }
    };
    if let Some(h) = slow_monitor {
        h.abort();
    }
    result?;
    file.flush()
        .await
        .map_err(|e| DownloadError::Other(format!("flush 失败: {e}")))?;
    if total > 0 && done != total {
        return Err(DownloadError::Other(format!(
            "下载不完整: 期望 {total}, 实际 {done}"
        )));
    }
    report(done, total.max(done));
    Ok(Downloaded {
        total: done,
        etag,
        resumed_chunks: 0,
        segments: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;

    #[tokio::test]
    async fn wait_running_blocks_until_resume() {
        let (tx, mut rx) = watch::channel(RunState::Paused);
        let handle = tokio::spawn(async move { wait_running(&mut rx).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(RunState::Running).unwrap();
        assert!(handle.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn wait_running_propagates_stop_reason() {
        let reason = StopReason::Slow {
            reason: "太慢".to_string(),
            bps: 100,
            min_bps: 32768,
            window_secs: 15,
        };
        let (_tx, mut rx) = watch::channel(RunState::Stopped(reason.clone()));
        match wait_running(&mut rx).await {
            Err(DownloadError::Stopped(r)) => assert_eq!(r, reason),
            other => panic!("expected a terminal stop, got {other:?}"),
        }
    }

    #[test]
    fn chunks_cover_exact_range() {
        let cs = build_chunks(10 * 1024 * 1024 + 1, 4 * 1024 * 1024);
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].start, 0);
        assert_eq!(cs[0].end, 4 * 1024 * 1024 - 1);
        assert_eq!(cs[2].end, 10 * 1024 * 1024);
        assert_eq!(cs.iter().map(Chunk::len).sum::<u64>(), 10 * 1024 * 1024 + 1);
        assert!(build_chunks(0, 1024).is_empty());
    }

    #[test]
    fn slow_tracker_fires_after_window() {
        let p = SlowPolicy::new(32, 5, 0); // 32 KiB/s, 5s window
        let mut t = SlowTracker::default();
        for _ in 0..4 {
            assert!(t.observe(1024, &p, false).is_none());
        }
        let (avg, secs) = t.observe(1024, &p, false).unwrap();
        assert_eq!(avg, 1024);
        assert_eq!(secs, 5);
    }

    #[test]
    fn slow_tracker_resets_on_recovery() {
        let p = SlowPolicy::new(32, 5, 0);
        let mut t = SlowTracker::default();
        t.observe(100, &p, false);
        t.observe(100, &p, false);
        t.observe(32 * 1024 * 1024, &p, false); // fast second resets
        assert_eq!(t, SlowTracker::default());
        assert!(t.observe(100, &p, false).is_none());
    }

    #[test]
    fn slow_tracker_ignores_grace_and_disabled() {
        let p = SlowPolicy::new(32, 3, 10);
        let mut t = SlowTracker::default();
        for _ in 0..10 {
            assert!(t.observe(0, &p, true).is_none());
        }
        assert_eq!(t, SlowTracker::default());
        let off = SlowPolicy::new(0, 3, 0);
        assert!(t.observe(0, &off, false).is_none());
    }

    #[test]
    fn error_classification() {
        let cancel = DownloadError::Stopped(StopReason::UserCancel("用户取消下载".into()));
        assert!(!cancel.is_route_failure());
        assert_eq!(cancel.message(), "用户取消下载");
        let slow = DownloadError::Stopped(StopReason::Slow {
            reason: "太慢".into(),
            bps: 100,
            min_bps: 32768,
            window_secs: 15,
        });
        assert!(!slow.is_route_failure());
        assert_eq!(slow.message(), "太慢");
        let net = DownloadError::Other("HTTP 504".into());
        assert!(net.is_route_failure());
        assert_eq!(net.message(), "HTTP 504");
    }

    #[test]
    fn short_url_is_utf8_safe() {
        let u = format!("https://镜像.example/{}", "x".repeat(200));
        let s = short_url(&u);
        assert!(s.chars().count() <= 91);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn parts_sidecar_rejects_wrong_size() {
        let dir = std::env::temp_dir();
        let out = dir.join(format!("ou-parts-{}.bin", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::write(&out, vec![0u8; 10]);
        let _ = std::fs::write(
            format!("{}.parts.json", out.display()),
            r#"{"url":"u","total":999,"chunk":4,"etag":"","done":[0]}"#,
        );
        assert!(load_parts(out.to_str().unwrap_or(""), "u", &None, 999, 4, 10).is_none());
        drop_parts(out.to_str().unwrap_or(""));
        let _ = std::fs::remove_file(&out);
    }
}
