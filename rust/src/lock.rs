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
                    // unreadable lock file: treat it as stale once
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
