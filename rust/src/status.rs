// Progress state file: /tmp/online-upgrade-status.json
// Written atomically (tmp + rename) so the LuCI page can poll it safely.

use crate::json::Value;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

pub const STATUS_FILE: &str = "/tmp/online-upgrade-status.json";
pub const LOG_FILE: &str = "/tmp/online-upgrade.log";

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
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = writeln!(f, "{}", line);
    }
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
