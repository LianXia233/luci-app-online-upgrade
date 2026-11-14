// 进度状态文件：/tmp/online-upgrade-status.json
// 以原子方式写入（先写临时文件再 rename），确保 LuCI 页面轮询时不会读到半截内容。

use crate::json::Value;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

pub const STATUS_FILE: &str = "/tmp/online-upgrade-status.json";
pub const LOG_FILE: &str = "/tmp/online-upgrade.log";
pub const MAX_LOG_SIZE: u64 = 2 * 1024 * 1024; // tmpfs 中日志占用的内存上限为 2MB
pub const KEEP_LOG_SIZE: usize = 1024 * 1024; // 超过 2MB 触发裁剪时仅保留最近 1MB

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct State {
    pub stage: &'static str,
    pub pct: u32,
    pub msg: String,
    pub error: Option<String>,
    pub result: Option<Value>,
}

pub fn write(st: State) {
    let mut pairs: Vec<(String, Value)> = vec![
        ("stage".into(), Value::str(st.stage)),
        ("pct".into(), Value::num(st.pct as u64)),
        ("msg".into(), Value::str(&st.msg)),
        (
            "error".into(),
            match &st.error {
                Some(e) => Value::str(e),
                None => Value::Null,
            },
        ),
        ("ts".into(), Value::num(now())),
        // 写入者 pid 让界面能够区分新一轮运行与上一次运行留下的
        // 终态：rpcd 以 detached 方式启动 check/upgrade，
        // 否则点击后的第一次轮询会读到
        // 旧的 "failed"/"check_done" 状态并立即停止轮询。
        ("pid".into(), Value::num(std::process::id() as u64)),
    ];
    if let Some(r) = st.result {
        pairs.push(("result".into(), r));
    }
    let body = Value::Obj(pairs).dump();
    let tmp = format!("{}.tmp", STATUS_FILE);
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, STATUS_FILE);
    }
}

pub fn set(stage: &'static str, pct: u32, msg: &str) {
    write(State {
        stage,
        pct,
        msg: msg.to_string(),
        error: None,
        result: None,
    });
}

pub fn set_error(stage: &'static str, pct: u32, msg: &str, error: &str) {
    write(State {
        stage,
        pct,
        msg: msg.to_string(),
        error: Some(error.to_string()),
        result: None,
    });
}

pub fn set_result(stage: &'static str, pct: u32, msg: &str, result: Value) {
    write(State {
        stage,
        pct,
        msg: msg.to_string(),
        error: None,
        result: Some(result),
    });
}

/// 任务删除 / 孤儿收敛后，把残留的任务阶段复位为 idle。
/// 否则页面会同时显示「暂无下载任务」和上一次的「正在下载」。
pub fn clear_task_stage() {
    let text = match fs::read_to_string(STATUS_FILE) {
        Ok(t) => t,
        Err(_) => return,
    };
    let v = match crate::json::parse(&text) {
        Ok(v) => v,
        Err(_) => return,
    };
    let stage = v.get("stage").and_then(|x| x.as_str()).unwrap_or("");
    if matches!(stage, "downloading" | "verifying" | "downloaded" | "paused" | "failed" | "aborted") {
        set("idle", 0, "空闲（任务已清理）");
    }
}

/// 向日志文件追加一行可读日志。
pub fn log(line: &str) {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_FILE)
    {
        let _ = writeln!(f, "{}", line);
    }

    if COUNTER.fetch_add(1, Ordering::Relaxed).is_multiple_of(32) {
        trim_log();
    }
}

/// 将可读日志限制在 tmpfs 的内存上限内（≤ 2MB）：
/// 超过 2MB 时仅保留最近 1MB。
pub fn trim_log() {
    let len = fs::metadata(LOG_FILE).map(|m| m.len()).unwrap_or(0);
    if len <= MAX_LOG_SIZE {
        return;
    }
    if let Ok(all) = fs::read(LOG_FILE) {
        if all.len() > KEEP_LOG_SIZE {
            let cut = all.len() - KEEP_LOG_SIZE;
            // 对齐到下一条完整行的边界
            let cut = all[cut..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| cut + p + 1)
                .unwrap_or(cut);
            let _ = fs::write(LOG_FILE, &all[cut..]);
        }
    }
}

/// 立即清空日志文件内容。
pub fn clear_log() {
    let _ = fs::write(LOG_FILE, "");
}

pub fn logf(args: std::fmt::Arguments) {
    log(&args.to_string());
}

#[macro_export]
macro_rules! logline {
    ($($arg:tt)*) => {
        $crate::status::logf(format_args!($($arg)*))
    };
}

/// 读取当前状态（供 `status` 子命令在命令行排查时使用）。
pub fn read() -> Value {
    match fs::read_to_string(STATUS_FILE) {
        Ok(s) => crate::json::parse(&s).unwrap_or(Value::Null),
        Err(_) => Value::str("idle"),
    }
}
