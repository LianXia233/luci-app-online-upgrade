// Segmented concurrent downloader — architecture inspired by FluxDown's engine
// (https://github.com/zerx-lab/FluxDown): dynamic chunk queue with work-stealing
// (fast connections naturally take over slow segments), per-chunk retry with a
// stall watchdog, shared progress counter, and a sidecar file for resuming.
//
// Sized for this plugin's use case: one firmware file, optional URL-prefix
// proxy, progress fed into the LuCI status JSON.

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

const MAX_WORKERS: usize = 6;
const SINGLE_STREAM_MAX: u64 = 2 * 1024 * 1024; // below this, don't bother splitting
const MAX_CHUNK_RETRIES: u32 = 3;
const STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Tunables read from UCI (`online-upgrade.settings.dl_*`), clamped to sane ranges.
struct DlCfg {
    workers: usize,
    chunk_size: u64,
    stall: Duration,
    retries: u32,
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
        }
    }
}

pub struct Downloaded {
    pub total: u64,
    #[allow(dead_code)] // reserved for future cache validation
    pub etag: Option<String>,
    pub resumed_chunks: usize,
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

fn load_parts(out: &str, url: &str, etag: &Option<String>, total: u64) -> Option<Parts> {
    let text = std::fs::read_to_string(parts_path(out)).ok()?;
    let v = crate::json::parse(&text).ok()?;
    let same = v.get("url").and_then(|x| x.as_str()) == Some(url)
        && v.get("total").and_then(|x| x.as_u64()) == Some(total)
        && v.get("etag")
            .and_then(|x| x.as_str())
            .map(|s| Some(s.to_string()))
            .unwrap_or(None)
            == etag.clone();
    if !same {
        return None;
    }
    let chunk = v.get("chunk").and_then(|x| x.as_u64())?;
    let done = v
        .get("done")
        .and_then(|x| x.as_arr())?
        .iter()
        .filter_map(|x| x.as_u64().map(|n| n as usize))
        .collect();
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
            crate::json::Value::Arr(p.done.iter().map(|&i| crate::json::Value::num(i as u64)).collect()),
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

#[derive(Clone)]
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

// ---- core ----

/// Download `url` to `out`, concurrently. `report(done_bytes, total)` is called
/// at most every ~500 ms. Returns final size.
pub async fn download(
    client: &Client,
    url: &str,
    out: &str,
    report: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<Downloaded, String> {
    let p = probe(client, url).await?;

    let dl = DlCfg::load();
    crate::logline!(
        "[download] 并发 {} / 分块 {} KB / 停滞 {}s / 重试 {}",
        dl.workers,
        dl.chunk_size / 1024,
        dl.stall.as_secs(),
        dl.retries
    );
    if dl.workers <= 1 || !p.ranges || p.total.map(|t| t <= SINGLE_STREAM_MAX).unwrap_or(true) {
        return single_stream(client, url, out, p.total, p.etag, report).await;
    }
    let total = p.total.ok_or_else(|| "服务器未返回文件大小,无法分段下载".to_string())?;
    let segments = ((total / dl.chunk_size).min(dl.workers as u64)).max(2) as usize;
    let all_chunks = build_chunks(total, dl.chunk_size);

    // resume state
    let prev = load_parts(out, url, &p.etag, total);
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
        .open(out)
        .await
        .map_err(|e| format!("无法创建文件 {out}: {e}"))?
        .set_len(total)
        .await
        .map_err(|e| format!("预分配失败: {e}"))?;

    let downloaded = Arc::new(AtomicU64::new(already_done));
    let chunks_left = Arc::new(AtomicU64::new(queue.len() as u64));
    let queue = Arc::new(Mutex::new(queue));
    let done_flags = Arc::new(Mutex::new(done_flags));

    let parts_state = Arc::new(Mutex::new(Parts {
        url: url.to_string(),
        etag: p.etag.clone().unwrap_or_default(),
        total,
        chunk: dl.chunk_size,
        done: prev.map(|x| x.done).unwrap_or_default(),
    }));

    let mut handles = Vec::with_capacity(segments);
    for _ in 0..segments {
        let client = client.clone();
        let url = url.to_string();
        let out = out.to_string();
        let queue = queue.clone();
        let done_flags = done_flags.clone();
        let downloaded = downloaded.clone();
        let parts_state = parts_state.clone();
        let chunks_left = chunks_left.clone();
        let stall = dl.stall;
        let retries = dl.retries;
        handles.push(tokio::spawn(async move {
            loop {
                let chunk = {
                    let mut q = queue.lock().unwrap();
                    q.pop_front()
                };
                let Some(chunk) = chunk else { break };
                let mut ok = false;
                for attempt in 1..=retries {
                    match fetch_range(&client, &url, chunk.start, chunk.end, out.as_str(), &downloaded, stall).await {
                        Ok(()) => {
                            ok = true;
                            break;
                        }
                        Err(e) => {
                            crate::logline!("  分段 {} 第{}次重试失败: {}", chunk.idx, attempt, e);
                            tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                        }
                    }
                }
                if !ok {
                    return Err(format!("分段 {} 下载失败", chunk.idx));
                }
                // mark done + persist sidecar
                {
                    let mut flags = done_flags.lock().unwrap();
                    flags[chunk.idx] = true;
                    let mut ps = parts_state.lock().unwrap();
                    ps.done.push(chunk.idx);
                    save_parts(&out, &ps);
                }
                if chunks_left.fetch_sub(1, Ordering::SeqCst) == 1 {
                    // last chunk finished — everything done
                }
            }
            Ok::<(), String>(())
        }));
    }

    // progress reporter
    let rep_handle = {
        let downloaded = downloaded.clone();
        let report = report.clone();
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

    let mut err: Option<String> = None;
    for h in handles {
        if let Ok(Err(e)) = h.await {
            err = Some(e);
        }
    }
    let _ = rep_handle.abort();
    if let Some(e) = err {
        drop_parts_done(out); // keep sidecar for resume
        return Err(e);
    }

    let final_size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    if final_size != total {
        return Err(format!("文件大小不匹配: 期望 {total}, 实际 {final_size}"));
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

fn drop_parts_done(_out: &str) {
    // sidecar intentionally kept: retrying the download resumes from it
}

async fn fetch_range(
    client: &Client,
    url: &str,
    start: u64,
    end: u64,
    out: &str,
    downloaded: &AtomicU64,
    stall_timeout: Duration,
) -> Result<(), String> {
    let resp = client
        .get(url)
        .header("Range", format!("bytes={start}-{end}"))
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    if resp.status().as_u16() != 206 {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let mut file = File::options()
        .write(true)
        .open(out)
        .await
        .map_err(|e| format!("打开文件失败: {e}"))?;
    file.seek(SeekFrom::Start(start))
        .await
        .map_err(|e| format!("seek 失败: {e}"))?;

    let mut stream = resp.bytes_stream();
    let mut written: u64 = 0;
    loop {
        let item = match tokio::time::timeout(stall_timeout, stream.next()).await {
            Ok(Some(item)) => item.map_err(|e| format!("读取失败: {e}"))?,
            Ok(None) => break,
            Err(_) => return Err(format!("停滞超过 {}s", stall_timeout.as_secs())),
        };
        let b: Bytes = item;
        file.write_all(&b).await.map_err(|e| format!("写入失败: {e}"))?;
        written += b.len() as u64;
        downloaded.fetch_add(b.len() as u64, Ordering::Relaxed);
    }
    file.flush().await.map_err(|e| format!("flush 失败: {e}"))?;
    if written != end - start + 1 {
        return Err(format!("分段不完整: 期望 {}, 实际 {}", end - start + 1, written));
    }
    Ok(())
}

async fn single_stream(
    client: &Client,
    url: &str,
    out: &str,
    total: Option<u64>,
    etag: Option<String>,
    report: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<Downloaded, String> {
    let resp = client
        .get(url)
        .timeout(Duration::from_secs(3600))
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let total = resp
        .content_length()
        .or(total)
        .unwrap_or(0);
    let mut file = File::create(out).await.map_err(|e| format!("创建文件失败: {e}"))?;
    let mut stream = resp.bytes_stream();
    let mut done: u64 = 0;
    let mut last_report = Instant::now();
    loop {
        let item = match tokio::time::timeout(STALL_TIMEOUT, stream.next()).await {
            Ok(Some(item)) => item.map_err(|e| format!("读取失败: {e}"))?,
            Ok(None) => break,
            Err(_) => return Err(format!("停滞超过 {}s", STALL_TIMEOUT.as_secs())),
        };
        let b: Bytes = item;
        file.write_all(&b).await.map_err(|e| format!("写入失败: {e}"))?;
        done += b.len() as u64;
        if last_report.elapsed() >= Duration::from_millis(500) {
            report(done, total);
            last_report = Instant::now();
        }
    }
    file.flush().await.map_err(|e| format!("flush 失败: {e}"))?;
    report(done, total.max(done));
    Ok(Downloaded {
        total: done,
        etag,
        resumed_chunks: 0,
        segments: 1,
    })
}
