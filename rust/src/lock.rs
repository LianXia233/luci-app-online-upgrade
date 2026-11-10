// 用于 check / upgrade / download / backup / restore 的单进程互斥锁。
//
// OpenWrt 的 rpcd 后端以 detached 方式启动二进制，因此两次点击（或
// check 与正在运行的 upgrade 并发）可能同时写入同一份临时固件
// 和同一个状态文件。陈旧锁（下载途中路由器重启）通过探测
// /proc/<pid> 识别，并回收一次。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOCK_FILE: &str = "/tmp/online-upgrade.lock";

/// 不可读的锁文件被视为陈旧之前所需的宽限期。
///
/// `acquire()` 先创建文件，再写入 `"<pid> <owner> <ts>"`。
/// 在这段窗口内进入的竞争者会看到空文件（或只写了一半的内容），
/// 若此时回收锁，就会让两个进程同时操作同一份固件
/// 和状态文件。因此，刚创建但不可读的锁按“忙”处理；
/// 该时长阈值可避免创建与写入之间的崩溃
/// 导致锁被永久占用。
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
    // 仅 Linux 可用的探测；其他平台（本地开发构建）上，
    // 为保守起见，将其他进程持有的锁视为仍然存活。
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

/// 锁文件的年龄（整秒），以文件 mtime 为基准计算，
/// 而不是比较进程间的墙上时钟。
fn lock_age_secs() -> Option<u64> {
    let modified = fs::metadata(LOCK_FILE).ok()?.modified().ok()?;
    let mtime = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now().saturating_sub(mtime))
}

/// 获取全局操作锁。失败时调用方**不得**写状态文件：
/// 持有锁的进程才负责上报进度。
pub fn acquire(owner: &str) -> Result<Lock, String> {
    for attempt in 0..2 {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(LOCK_FILE)
        {
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
