// Single-process mutex for check / upgrade / download / backup / restore.
//
// OpenWrt's rpcd backend spawns each binary detached, so two clicks (or a
// check racing a running upgrade) can otherwise write the same temp firmware
// and the same status file. A stale lock (router rebooted mid-download) is
// detected by probing /proc/<pid> and reclaimed once.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOCK_FILE: &str = "/tmp/online-upgrade.lock";

/// Grace period before an unreadable lock file is treated as stale.
///
/// `acquire()` creates the file and only then writes `"<pid> <owner> <ts>"` into
/// it. A contender that lands in that window sees an empty (or half-written)
/// lock, and reclaiming it would let two processes run at once against the same
/// firmware and status files. So a fresh-but-unreadable lock is reported as
/// busy. The age threshold keeps a crash between create and write from holding
/// the lock forever.
const UNREADABLE_LOCK_GRACE_SECS: u64 = 2;

pub struct Lock {
    _priv: (),
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn pid_alive(pid: u32) -> bool {
    // Linux-only probe; on other platforms (local dev builds) treat a lock
    // file owned by another process id as alive to stay conservative.
    let path = format!("/proc/{pid}");
    if !std::path::Path::new("/proc").exists() {
        return true;
    }
    std::path::Path::new(&path).exists()
}

fn read_owner() -> Option<(u32, String)> {
    let text = fs::read_to_string(LOCK_FILE).ok()?;
    let mut it = text.split_whitespace();
    let pid = it.next()?.parse::<u32>().ok()?;
    let who = it.next().unwrap_or("unknown").to_string();
    Some((pid, who))
}

/// Age of the lock file in whole seconds, measured on the file's mtime rather
/// than on wall-clock differences between processes.
fn lock_age_secs() -> Option<u64> {
    let modified = fs::metadata(LOCK_FILE).ok()?.modified().ok()?;
    let mtime = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now().saturating_sub(mtime))
}

/// Acquire the global operation lock. On failure the caller must NOT write the
/// status file: the process that owns the lock is the one reporting progress.
pub fn acquire(owner: &str) -> Result<Lock, String> {
    for attempt in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(LOCK_FILE) {
            Ok(mut f) => {
                let _ = writeln!(f, "{} {} {}", std::process::id(), owner, now());
                let _ = f.flush();
                return Ok(Lock { _priv: () });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if attempt == 0 {
                    if let Some((pid, who)) = read_owner() {
                        if !pid_alive(pid) || pid == std::process::id() {
                            let _ = fs::remove_file(LOCK_FILE);
                            continue;
                        }
                        return Err(format!("已有任务正在运行 (pid {pid}, {who})"));
                    }
                    // 锁文件存在但读不出 PID：可能正好落在 owner 的
                    // create_new 与 writeln 之间（空文件或首行只写了一半）。
                    // 这时删锁会让两个进程同时持有锁，所以宽限期内一律按忙处理；
                    // 超过宽限期仍不可读，才认定是崩溃残留并回收，避免永久占锁。
                    let fresh = lock_age_secs()
                        .map(|age| age < UNREADABLE_LOCK_GRACE_SECS)
                        .unwrap_or(false);
                    if fresh {
                        return Err("已有任务正在运行 (锁文件刚被创建)".into());
                    }
                    let _ = fs::remove_file(LOCK_FILE);
                    continue;
                }
                return Err("已有任务正在运行 (无法读取锁文件)".into());
            }
            Err(e) => return Err(format!("无法创建锁文件 {LOCK_FILE}: {e}")),
        }
    }
    Err("无法获取运行锁".into())
}

impl Drop for Lock {
    fn drop(&mut self) {
        let mine = fs::read_to_string(LOCK_FILE)
            .ok()
            .and_then(|t| t.split_whitespace().next().map(|s| s.to_string()))
            .map(|s| s == std::process::id().to_string())
            .unwrap_or(false);
        if mine {
            let _ = fs::remove_file(LOCK_FILE);
        }
    }
}
