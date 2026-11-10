// 分段并发下载器：动态分块队列结合工作窃取
// （连接较快的 worker 会自然接管较慢的分段），单块重试、
// 停滞看门狗、共享进度计数，以及用于断点续传的 sidecar 文件。
//
// 针对本插件的场景设计：单个固件文件、可选的 URL 前缀
// 代理，进度写入 LuCI 状态 JSON。
//
// 任务控制基于 Tokio 原生机制：`watch::Receiver<RunState>` 驱动所有分块
// worker 的暂停 / 取消；一个 Tokio 任务轮询带外控制文件
// （由 `online-upgrade pause|resume|cancel` 写入），另有吞吐量
// 监控在速度持续异常偏低时中止传输。

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

/// 单次下载尝试的失败类型。
///
/// `Stopped` 表示整个任务终止（用户取消或慢速保护），
/// 绝不能触发切换到其他镜像；`Other` 表示当前线路层面的
/// 失败，调用方可以换一个 URL 重试。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadError {
    /// 带明确原因类型的终止。
    Stopped(StopReason),
    /// 当前线路的网络 / IO / 协议失败。
    Other(String),
}

impl DownloadError {
    /// 面向用户的消息（状态文件、日志、任务历史）。
    pub fn message(&self) -> String {
        match self {
            DownloadError::Stopped(r) => r.text().to_string(),
            DownloadError::Other(s) => s.clone(),
        }
    }

    /// 是否可以尝试其他下载线路。
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

/// 从 UCI 读取的可调参数（`online-upgrade.settings.dl_*`），会限制到合理范围。
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

/// 下载完成后的结果。
pub struct Downloaded {
    /// 最终文件大小（字节）。
    pub total: u64,
    /// 资产响应的 ETag（服务器提供时）。
    #[allow(dead_code)] // 预留给后续的缓存校验
    pub etag: Option<String>,
    /// 从上一次运行的 sidecar 恢复的分块数量。
    pub resumed_chunks: usize,
    /// 实际使用的并发分段数。
    pub segments: usize,
    /// 实际分块大小。
    pub chunk_size: u64,
    /// 分块总数。
    pub chunks_total: usize,
    /// 配置的 worker 数。
    pub workers: usize,
}

/// 传给界面的实时下载进度快照。
#[derive(Clone, Debug)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub workers: usize,
    pub segments: usize,
    pub chunk_size: u64,
    pub chunks_total: usize,
    pub chunks_done: usize,
    pub chunks_running: usize,
    /// 每个分块的状态：0=等待，1=传输中，2=完成。
    pub states: Vec<u8>,
}

pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// 下载选项；插件包小于单流阈值时可强制分段，以便展示线程与分块。
#[derive(Clone, Copy, Default)]
pub struct DownloadOptions {
    pub force_split: bool,
}

struct Probe {
    total: Option<u64>,
    ranges: bool,
    etag: Option<String>,
}

/// 不使用 HEAD 的范围探测：请求 0-0 字节，据此推导文件大小与是否支持 Range。
async fn probe(client: &Client, url: &str) -> Result<Probe, String> {
    let resp = client
        .get(url)
        // api.github.com/releases/assets/<id> 要求带此头：否则 API
        // 会返回 JSON 元数据，而不是重定向到文件。
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
        // 响应头形如 Content-Range: bytes 0-0/123456
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

// ---- 断点续传 sidecar ----

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

/// 只有同时满足以下三点，sidecar 才可信：
/// 1. 它描述的数据文件仍然存在且长度符合预期
///    （被删除或截断的固件绝不能保留其“已完成”映射）；
/// 2. 分块网格未变——修改 `dl_chunk_mb` 会让索引映射到
///    完全不同的字节区间；
/// 3. 每个记录的索引都落在当前网格内。
///
/// 其他情况一律丢弃，
/// 确保续传绝不会跳过从未写入的字节。
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

// ---- 分块队列 ----

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

// ---- 任务控制辅助函数 ----

/// 等待任务恢复运行。暂停只在分块边界生效，
/// 避免 HTTP 响应流在空闲时被长时间挂起。
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

/// 慢速保护使用的纯滑动窗口统计器。与异步监控逻辑分离，
/// 使判定规则无需计时器即可做单元测试。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SlowTracker {
    low_secs: u64,
    low_bytes: u64,
}

impl SlowTracker {
    /// 输入一秒的吞吐量。当吞吐量在整个观察窗口内持续低于下限时，
    /// 返回 `(avg_bps, low_secs)`。
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

/// 缩短 URL 以便记录日志，且不会从 UTF-8 字符中间截断。
pub(crate) fn short_url(url: &str) -> String {
    let mut s: String = url.chars().take(90).collect();
    if url.chars().count() > 90 {
        s.push('…');
    }
    s
}

/// 每秒采样一次共享字节计数器的 Tokio 任务，当吞吐量持续低于
/// 配置下限时中止下载。
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

// ---- 核心逻辑 ----

/// 并发地把 `url` 下载到 `out`。`report(done_bytes, total)` 的调用
/// 间隔最多约 500 毫秒。返回最终大小。
///
/// `control` 携带用于暂停 / 取消和慢速保护的 Tokio watch 通道；
/// 它由调用方持有，使同一个任务可以跨多条备用线路
/// 持续工作。
pub async fn download(
    client: &Client,
    url: &str,
    out: &str,
    report: ProgressFn,
    control: &DownloadControl,
) -> Result<Downloaded, DownloadError> {
    download_with(client, url, out, report, control, DownloadOptions::default()).await
}

pub async fn download_with(
    client: &Client,
    url: &str,
    out: &str,
    report: ProgressFn,
    control: &DownloadControl,
    options: DownloadOptions,
) -> Result<Downloaded, DownloadError> {
    // 在收到第一个字节之前就已到达的取消请求，
    // 绝不能再去建立新连接。
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
    if dl.workers <= 1 || !p.ranges || (!options.force_split
        && p.total.map(|t| t <= SINGLE_STREAM_MAX).unwrap_or(true))
    {
        return single_stream(client, url, out, p.total, p.etag, report, control, &dl).await;
    }
    let total = p
        .total
        .ok_or_else(|| DownloadError::Other("服务器未返回文件大小,无法分段下载".to_string()))?;
    // 插件包通常小于单流阈值；强制分段时按 worker 数切出可观察的分块，
    // 但不小于 64KB，避免生成过多微小分片。
    let mut chunk_size = dl.chunk_size;
    if options.force_split && total <= SINGLE_STREAM_MAX {
        let target = ((total + dl.workers.max(2) as u64 - 1) / dl.workers.max(2) as u64)
            .max(64 * 1024);
        chunk_size = chunk_size.min(target).max(64 * 1024);
    }
    let all_chunks = build_chunks(total, chunk_size);
    let segments = ((total / chunk_size).min(dl.workers as u64))
        .max(2)
        .min(all_chunks.len() as u64) as usize;

    // 续传状态
    let prev = load_parts(out, url, &p.etag, total, chunk_size, all_chunks.len());
    if prev.is_none() {
        // 不可信的 sidecar（文件不存在、分块网格变化等）
        // 不能留在新开始的下载旁边
        drop_parts(out);
    }
    let mut queue: VecDeque<Chunk> = VecDeque::new();
    let mut already_done: u64 = 0;
    let mut resumed_chunks = 0usize;
    let mut done_flags = vec![false; all_chunks.len()];
    let chunk_states = Arc::new(Mutex::new(vec![0u8; all_chunks.len()]));
    if let Some(prev) = &prev {
        for c in &all_chunks {
            if prev.done.contains(&c.idx) {
                done_flags[c.idx] = true;
                if let Ok(mut states) = chunk_states.lock() {
                    states[c.idx] = 2;
                }
                already_done += c.len();
                resumed_chunks += 1;
            } else {
                queue.push_back(c.clone());
            }
        }
    } else {
        queue.extend(all_chunks.iter().cloned());
    }
    // 预分配文件
    File::options()
        .write(true)
        .create(true)
        // 续传会复用已预分配的文件：截断会破坏
        // 已写入的分块。下方用 `set_len` 设定完整大小。
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
        chunk: chunk_size,
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
        let chunk_states = Arc::clone(&chunk_states);
        let downloaded = Arc::clone(&downloaded);
        let parts_state = Arc::clone(&parts_state);
        let mut state = control.rx();
        let stall = dl.stall;
        let retries = dl.retries;
        handles.push(tokio::spawn(async move {
            loop {
                // 暂停在分块之间检查；停止还会在字节循环内部检查，
                // 因此取消能够及时生效。
                wait_running(&mut state).await?;
                let chunk = {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    q.pop_front()
                };
                let Some(chunk) = chunk else { break };
                // 让详情视图看到当前 worker 正在处理哪个分块。
                if let Ok(mut states) = chunk_states.lock() {
                    states[chunk.idx] = 1;
                }
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
                        // 终止性停止：保留原始的原因类型，
                        // 不消耗剩余重试次数。
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
                // 标记完成并持久化 sidecar
                {
                    let mut flags = done_flags.lock().unwrap_or_else(|e| e.into_inner());
                    flags[chunk.idx] = true;
                    if let Ok(mut states) = chunk_states.lock() {
                        states[chunk.idx] = 2;
                    }
                    let mut ps = parts_state.lock().unwrap_or_else(|e| e.into_inner());
                    ps.done.push(chunk.idx);
                    save_parts(&out, &ps);
                }
            }
            Ok::<(), DownloadError>(())
        }));
    }

    // 进度上报任务：每 500ms 提供分块状态快照，供任务详情展示。
    let rep_handle = {
        let downloaded = Arc::clone(&downloaded);
        let states_ref = Arc::clone(&chunk_states);
        let report = Arc::clone(&report);
        let chunks_total = all_chunks.len();
        let workers = dl.workers;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let d = downloaded.load(Ordering::Relaxed);
                let states = states_ref
                    .lock()
                    .map(|s| s.clone())
                    .unwrap_or_default();
                let chunks_done = states.iter().filter(|&&s| s == 2).count();
                let chunks_running = states.iter().filter(|&&s| s == 1).count();
                report(Progress {
                    done: d,
                    total,
                    workers,
                    segments,
                    chunk_size,
                    chunks_total,
                    chunks_done,
                    chunks_running,
                    states,
                });
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
        // 仅当数据文件仍完整且长度正确时才保留 sidecar；
        // 否则后续续传会信任一个已不再描述该文件的
        // “已完成”映射（典型的空洞填零问题）。
        keep_resumable(out, total);
        return Err(e);
    }

    // 每个分块都必须标记为完成。仅凭文件长度无法证明这一点：
    // 预分配的文件即使存在未写入区域，长度也会是正确的。
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
    report(Progress {
        done: total,
        total,
        workers: dl.workers,
        segments,
        chunk_size,
        chunks_total: all_chunks.len(),
        chunks_done: all_chunks.len(),
        chunks_running: 0,
        states: vec![2u8; all_chunks.len()],
    });
    drop_parts(out);
    Ok(Downloaded {
        total,
        etag: p.etag,
        resumed_chunks,
        segments,
        chunk_size,
        chunks_total: all_chunks.len(),
        workers: dl.workers,
    })
}

/// 失败时：仅当确实能从该文件续传时才保留 sidecar，
/// 否则将两者都删除，让重试从干净状态开始。
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
        // `state.changed()` 是取消安全的，因此与数据流一起 select
        // 时，即使数据块先就绪也不会丢失停止通知。
        let item = tokio::select! {
            biased;
            _res = state.changed() => {
                match state.borrow_and_update().clone() {
                    RunState::Stopped(r) => return Err(DownloadError::Stopped(r)),
                    // 暂停在下一个分块边界生效；期间继续
                    // 读取完当前响应，避免连接空转。
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
    report: ProgressFn,
    control: &DownloadControl,
    dl: &DlCfg,
) -> Result<Downloaded, DownloadError> {
    let res = single_stream_inner(client, url, out, total, etag, report, control, dl).await;
    match res {
        Ok(d) => {
            // 单流下载不适用续传状态：确保此前分段下载留下的
            // 旧 sidecar 不会在之后被误用
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
    report: ProgressFn,
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
            let total_eff = total.max(done);
            report(Progress {
                done,
                total: total_eff,
                workers: 1,
                segments: 1,
                chunk_size: total_eff.max(1),
                chunks_total: 1,
                chunks_done: if done >= total_eff { 1 } else { 0 },
                chunks_running: if done < total_eff { 1 } else { 0 },
                states: vec![if done >= total_eff { 2 } else { 1 }],
            });
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
    let total_eff = total.max(done);
    report(Progress {
        done,
        total: total_eff,
        workers: 1,
        segments: 1,
        chunk_size: total_eff.max(1),
        chunks_total: 1,
        chunks_done: 1,
        chunks_running: 0,
        states: vec![2],
    });
    Ok(Downloaded {
        total: done,
        etag,
        resumed_chunks: 0,
        segments: 1,
        chunk_size: total_eff.max(1),
        chunks_total: 1,
        workers: 1,
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
        let p = SlowPolicy::new(32, 5, 0); // 32 KiB/s，5 秒窗口
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
