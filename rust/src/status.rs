// Progress state file: /tmp/online-upgrade-status.json
// Written atomically (tmp + rename) so the LuCI page can poll it safely.

use crate::json::Value;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

pub const STATUS_FILE: &str = "/tmp/online-upgrade-status.json";
pub const LOG_FILE: &str = "/tmp/online-upgrade.log";
pub const MAX_LOG_SIZE: u64 = 2 * 1024 * 1024; // 2MB maximum RAM memory limit on tmpfs
pub const KEEP_LOG_SIZE: usize = 1024 * 1024; // Keep latest 1MB when trimming exceeds 2MB

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
        // The writer's pid lets the UI tell a fresh run apart from the terminal
        // state left behind by a previous run: rpcd spawns check/upgrade
        // detached, so the first poll after a click would otherwise read the
        // old "failed"/"check_done" entry and stop polling immediately.
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

/// Append a human-readable line to the log file.
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

/// Keep the human-readable log bounded in tmpfs memory (≤ 2MB):
/// when exceeding 2MB, keep only the latest 1MB.
pub fn trim_log() {
    let len = fs::metadata(LOG_FILE).map(|m| m.len()).unwrap_or(0);
    if len <= MAX_LOG_SIZE {
        return;
    }
    if let Ok(all) = fs::read(LOG_FILE) {
        if all.len() > KEEP_LOG_SIZE {
            let cut = all.len() - KEEP_LOG_SIZE;
            // align to the next line boundary
            let cut = all[cut..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| cut + p + 1)
                .unwrap_or(cut);
            let _ = fs::write(LOG_FILE, &all[cut..]);
        }
    }
}

/// Clear the log file contents immediately.
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

/// Read the current status (used by the `status` subcommand for CLI debugging).
pub fn read() -> Value {
    match fs::read_to_string(STATUS_FILE) {
        Ok(s) => crate::json::parse(&s).unwrap_or(Value::Null),
        Err(_) => Value::str("idle"),
    }
}
