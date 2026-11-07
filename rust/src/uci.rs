// UCI access via the `uci` CLI (arguments passed directly, no shell involved).

use std::process::Command;

const BASE: &str = "online-upgrade.settings";

pub fn get(key: &str) -> Option<String> {
    let out = Command::new("uci")
        .args(["-q", "get", &format!("{}.{}", BASE, key)])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub fn get_or(key: &str, default: &str) -> String {
    get(key).unwrap_or_else(|| default.to_string())
}

pub fn set(key: &str, val: &str) -> bool {
    Command::new("uci")
        .args(["set", &format!("{}.{}", BASE, key), val])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn unset(key: &str) -> bool {
    Command::new("uci")
        .args(["-q", "delete", &format!("{}.{}", BASE, key)])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn commit() -> bool {
    Command::new("uci")
        .args(["commit", "online-upgrade"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
