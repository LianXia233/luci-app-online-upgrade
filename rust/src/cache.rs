// 独立缓存槽：四套状态模型各自持有**不同的文件路径**，互不覆盖。
//
// 设计要点（对应任务「缓存与并发控制」）：
// 1. 每个模型一个文件，写入互不影响；一个模型刷新失败不会清除别人的数据。
// 2. 缓存带 Envelope 元数据：本次写入时间、上次成功时间、stale 标记、
//    API 状态与错误。失败时保留上次成功的 payload 并显式标记 stale，
//    **绝不把缓存伪装成实时数据**。
// 3. 原子写（tmp + rename），避免 rpcd 轮询读到半截 JSON。
// 4. 失败退避：记录失败时间，退避期内跳过主动刷新，防止限流下高频轮询。
// 5. 目录位于 /tmp（OpenWrt tmpfs），不占用闪存；单文件有大小上限。

use crate::json::Value;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const CACHE_DIR: &str = "/tmp/online-upgrade-cache";
pub const MAX_CACHE_BYTES: u64 = 512 * 1024;

// 各模型独立缓存文件。刻意分散命名，任何模块都不得引用他人路径。
pub const ACTIONS_FILE: &str = "/tmp/online-upgrade-cache/actions.json";
pub const RELEASES_FILE: &str = "/tmp/online-upgrade-cache/releases.json";
pub const ARTIFACTS_FILE: &str = "/tmp/online-upgrade-cache/artifacts.json";
pub const COMPAT_FILE: &str = "/tmp/online-upgrade-cache/compat.json";
pub const NETWORK_FILE: &str = "/tmp/online-upgrade-cache/network.json";
pub const DEVICE_FILE: &str = "/tmp/online-upgrade-cache/device.json";
pub const REPO_FILE: &str = "/tmp/online-upgrade-cache/repo.json";

// 独立默认 TTL（秒）。可由 UCI 分别覆盖。
pub const TTL_ACTIONS: u64 = 120;
pub const TTL_RELEASES: u64 = 300;
pub const TTL_ARTIFACTS: u64 = 180;
pub const TTL_COMPAT: u64 = 600;
pub const TTL_NETWORK: u64 = 60;
pub const TTL_DEVICE: u64 = 3600;
pub const TTL_REPO: u64 = 1800;

// 失败退避：连续失败时最短间隔，避免打满匿名 API 额度。
pub const FAILURE_BACKOFF_SECS: u64 = 60;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn ttl_from_uci(key: &str, default: u64) -> u64 {
    crate::uci::get(key)
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| v.clamp(15, 86400))
        .unwrap_or(default)
}

/// 缓存信封：payload 是模型自己的原始状态，元数据只描述"这份数据的新鲜度"。
#[derive(Debug, Clone)]
pub struct Envelope {
    pub payload: Value,
    pub fetched_at: u64,
    pub last_success_at: u64,
    pub stale: bool,
    pub api_status: String, // ok | error | rate_limited | cached
    pub api_error: String,
    pub source: String, // live | cache | failure_preserved
}

impl Envelope {
    pub fn fresh(live: Value, api_status: &str) -> Envelope {
        let t = now();
        Envelope {
            payload: live,
            fetched_at: t,
            last_success_at: t,
            stale: false,
            api_status: api_status.to_string(),
            api_error: String::new(),
            source: "live".into(),
        }
    }

    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("payload".into(), self.payload.clone()),
            ("fetched_at".into(), Value::num(self.fetched_at)),
            ("last_success_at".into(), Value::num(self.last_success_at)),
            ("stale".into(), Value::Bool(self.stale)),
            ("api_status".into(), Value::str(&self.api_status)),
            ("api_error".into(), Value::str(&self.api_error)),
            ("source".into(), Value::str(&self.source)),
            ("schema".into(), Value::num(1)),
        ])
    }

    pub fn from_json(v: &Value) -> Option<Envelope> {
        let payload = v.get("payload")?.clone();
        let num = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        Some(Envelope {
            payload,
            fetched_at: num("fetched_at"),
            last_success_at: num("last_success_at"),
            stale: matches!(v.get("stale"), Some(Value::Bool(true))),
            api_status: v
                .get("api_status")
                .and_then(|x| x.as_str())
                .unwrap_or("cached")
                .to_string(),
            api_error: v
                .get("api_error")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            source: v
                .get("source")
                .and_then(|x| x.as_str())
                .unwrap_or("cache")
                .to_string(),
        })
    }

    /// 是否仍在 TTL 内。stale 数据永远不算新鲜（即使时间没到）。
    pub fn is_fresh(&self, ttl: u64) -> bool {
        !self.stale && self.fetched_at > 0 && now().saturating_sub(self.fetched_at) < ttl
    }
}

fn ensure_dir() {
    let _ = fs::create_dir_all(CACHE_DIR);
}

/// 原子写入指定槽位。文件过大（异常数据）时拒绝写入，避免撑爆 tmpfs。
pub fn write(path: &str, env: &Envelope) -> Result<(), String> {
    ensure_dir();
    let body = env.to_json().dump();
    if body.len() as u64 > MAX_CACHE_BYTES {
        return Err(format!("缓存数据过大，拒绝写入 {path}"));
    }
    let tmp = format!("{path}.tmp.{}", std::process::id());
    fs::write(&tmp, body).map_err(|e| format!("写缓存失败 {path}: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("缓存落盘失败 {path}: {e}")
    })?;
    Ok(())
}

pub fn read(path: &str) -> Option<Envelope> {
    let s = fs::read_to_string(path).ok()?;
    if s.len() as u64 > MAX_CACHE_BYTES {
        return None;
    }
    let v = crate::json::parse(&s).ok()?;
    Envelope::from_json(&v)
}

/// 主动刷新时复用有效缓存；未过期直接返回 None（调用方无需请求）。
pub fn usable(path: &str, ttl: u64) -> Option<Envelope> {
    let env = read(path)?;
    if env.is_fresh(ttl) {
        Some(env)
    } else {
        None
    }
}

/// 查询失败时：保留上次成功 payload，标记 stale，记录错误与失败时刻。
/// 返回可直接写回的信封；若无历史数据则返回"仅含错误"的空 payload。
pub fn preserve_on_failure(path: &str, api_status: &str, error: &str) -> Envelope {
    let mut env = read(path).unwrap_or(Envelope {
        payload: Value::Obj(vec![]),
        fetched_at: 0,
        last_success_at: 0,
        stale: true,
        api_status: "none".into(),
        api_error: String::new(),
        source: "none".into(),
    });
    env.stale = true;
    env.fetched_at = now();
    env.api_status = api_status.to_string();
    env.api_error = error.to_string();
    env.source = "failure_preserved".into();
    env
}

/// 是否处于失败退避期（连续失败后不要立刻重试）。
pub fn in_backoff(path: &str, backoff: u64) -> bool {
    match read(path) {
        Some(env) => {
            env.stale
                && env.api_status != "ok"
                && now().saturating_sub(env.fetched_at) < backoff.max(FAILURE_BACKOFF_SECS)
        }
        None => false,
    }
}

pub fn invalidate(path: &str) {
    if Path::new(path).exists() {
        let _ = fs::remove_file(path);
    }
}

/// 供 CLI `states --clear-cache` 使用：只清四套模型与网络缓存，
/// 不动 status.json / 任务历史 / 固件文件。
pub fn invalidate_all() -> usize {
    let mut n = 0;
    for p in [
        ACTIONS_FILE,
        RELEASES_FILE,
        ARTIFACTS_FILE,
        COMPAT_FILE,
        NETWORK_FILE,
        DEVICE_FILE,
        REPO_FILE,
    ] {
        if Path::new(p).exists() {
            invalidate(p);
            n += 1;
        }
    }
    n
}

/// 人类可读的时间戳（UTC+8），用于界面显示"最后成功查询时间"。
pub fn human_ts(ts: u64) -> String {
    if ts == 0 {
        return String::new();
    }
    let days = (ts as i64).div_euclid(86400);
    let sec = (ts as i64).rem_euclid(86400);
    let (y, m, d) = crate::version::civil_from_days(days);
    let iso = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        sec / 3600,
        (sec % 3600) / 60,
        sec % 60
    );
    crate::github::utc_to_local(&iso)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> String {
        format!("/tmp/ou-cache-test-{}-{}.json", name, std::process::id())
    }

    #[test]
    fn envelope_roundtrip_preserves_payload_and_meta() {
        let p = tmp("roundtrip");
        let _ = fs::remove_file(&p);
        let payload = Value::Obj(vec![
            ("state".into(), Value::str("success")),
            ("count".into(), Value::num(3)),
        ]);
        let env = Envelope::fresh(payload.clone(), "ok");
        write(&p, &env).unwrap();
        let back = read(&p).unwrap();
        assert_eq!(back.payload, payload);
        assert!(!back.stale);
        assert_eq!(back.api_status, "ok");
        assert_eq!(back.source, "live");
        assert!(back.is_fresh(300));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn failure_preserves_last_success_payload_but_marks_stale() {
        let p = tmp("failure");
        let _ = fs::remove_file(&p);
        let good = Envelope::fresh(
            Value::Obj(vec![("state".into(), Value::str("success"))]),
            "ok",
        );
        write(&p, &good).unwrap();

        let preserved = preserve_on_failure(&p, "rate_limited", "HTTP 403");
        assert!(preserved.stale);
        assert_eq!(preserved.api_status, "rate_limited");
        assert_eq!(preserved.source, "failure_preserved");
        assert_eq!(
            preserved.payload.get("state").and_then(|v| v.as_str()),
            Some("success"),
            "失败时不得丢弃上次成功的真实状态"
        );
        // stale 数据永不算新鲜，也不得伪装成实时数据
        assert!(!preserved.is_fresh(86400));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn one_model_refresh_never_touches_other_models() {
        let a = tmp("actions");
        let r = tmp("releases");
        for p in [&a, &r] {
            let _ = fs::remove_file(p);
        }
        write(&a, &Envelope::fresh(Value::str("actions-data"), "ok")).unwrap();
        write(&r, &Envelope::fresh(Value::str("releases-data"), "ok")).unwrap();

        // 模拟 actions 刷新失败被保留
        let preserved = preserve_on_failure(&a, "error", "timeout");
        write(&a, &preserved).unwrap();

        assert!(read(&a).unwrap().stale);
        let rel = read(&r).unwrap();
        assert!(!rel.stale, "释放缓存被其他模型的刷新污染");
        assert_eq!(rel.payload.as_str(), Some("releases-data"));
        for p in [&a, &r] {
            let _ = fs::remove_file(p);
        }
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let p = tmp("oversized");
        let _ = fs::remove_file(&p);
        let big = Value::str(&"x".repeat(MAX_CACHE_BYTES as usize + 10));
        let env = Envelope::fresh(big, "ok");
        assert!(write(&p, &env).is_err());
        assert!(read(&p).is_none());
    }

    #[test]
    fn ttl_is_per_slot_independent() {
        let p = tmp("ttl");
        let _ = fs::remove_file(&p);
        let mut env = Envelope::fresh(Value::str("x"), "ok");
        env.fetched_at = now().saturating_sub(200);
        write(&p, &env).unwrap();
        assert!(usable(&p, 300).is_some(), "TTL 300 内应可复用");
        assert!(usable(&p, 120).is_none(), "TTL 120 已过期应重新查询");
        let _ = fs::remove_file(&p);
    }
}
