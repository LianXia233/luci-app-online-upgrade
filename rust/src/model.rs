// 状态模型的**元数据框架**：只负责"这份数据什么时候查的、可不可信"，
// 不持有也不推导任何业务状态。四套模型各自独立定义状态枚举与数据结构，
// 仅复用这里的展示格式，避免四份重复样板代码。
//
// 注意：这里没有、也绝不允许出现统一的 upgrade_status / 单一状态机。

use crate::cache::Envelope;
use crate::ghapi::{ApiError, RateInfo};
use crate::json::Value;

pub const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone)]
pub struct ApiMeta {
    /// ok | error | rate_limited | not_checked | querying | cached
    pub status: String,
    pub error: String,
    pub queried_at: u64,
    pub last_success_at: u64,
    /// 当前数据是缓存且已过期/来自失败保留，不能当实时数据
    pub stale: bool,
    pub from_cache: bool,
    pub rate: RateInfo,
}

impl ApiMeta {
    pub fn not_checked() -> ApiMeta {
        ApiMeta {
            status: "not_checked".into(),
            error: String::new(),
            queried_at: 0,
            last_success_at: 0,
            stale: false,
            from_cache: false,
            rate: RateInfo::default(),
        }
    }

    /// 预留「正在查询」中间态；当前刷新同步完成，不落盘该状态。
    #[allow(dead_code)]
    pub fn querying() -> ApiMeta {
        let mut m = ApiMeta::not_checked();
        m.status = "querying".into();
        m
    }

    pub fn ok(rate: RateInfo) -> ApiMeta {
        let t = crate::cache::now();
        ApiMeta {
            status: "ok".into(),
            error: String::new(),
            queried_at: t,
            last_success_at: t,
            stale: false,
            from_cache: false,
            rate,
        }
    }

    pub fn from_envelope(env: &Envelope) -> ApiMeta {
        ApiMeta {
            status: env.api_status.clone(),
            error: env.api_error.clone(),
            queried_at: env.fetched_at,
            last_success_at: env.last_success_at,
            stale: env.stale,
            from_cache: env.source != "live",
            rate: RateInfo::default(),
        }
    }

    pub fn from_error(e: &ApiError, env: &Envelope) -> ApiMeta {
        ApiMeta {
            status: if e.is_rate_limited() { "rate_limited" } else { "error" }.into(),
            error: e.user_message(),
            queried_at: env.fetched_at,
            last_success_at: env.last_success_at,
            stale: true,
            from_cache: true,
            rate: RateInfo::default(),
        }
    }

    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("status".into(), Value::str(&self.status)),
            ("error".into(), Value::str(&self.error)),
            ("queried_at".into(), Value::num(self.queried_at)),
            (
                "queried_at_local".into(),
                Value::str(&crate::cache::human_ts(self.queried_at)),
            ),
            ("last_success_at".into(), Value::num(self.last_success_at)),
            (
                "last_success_local".into(),
                Value::str(&crate::cache::human_ts(self.last_success_at)),
            ),
            ("stale".into(), Value::Bool(self.stale)),
            ("from_cache".into(), Value::Bool(self.from_cache)),
            ("rate_limit".into(), self.rate.to_json()),
        ])
    }
}

/// 组装一个模型的对外 JSON。每个模型传入自己的 state / label / data。
pub fn frame(
    model: &str,
    state: &str,
    label: &str,
    data: Vec<(String, Value)>,
    meta: &ApiMeta,
    ttl: u64,
) -> Value {
    let mut pairs: Vec<(String, Value)> = vec![
        ("schema_version".into(), Value::num(SCHEMA_VERSION)),
        ("model".into(), Value::str(model)),
        ("state".into(), Value::str(state)),
        ("state_label".into(), Value::str(label)),
    ];
    pairs.extend(data);
    pairs.push(("api".into(), meta.to_json()));
    pairs.push(("ttl".into(), Value::num(ttl)));
    pairs.push(("age_secs".into(), Value::num(crate::cache::now().saturating_sub(meta.queried_at))));
    Value::Obj(pairs)
}

/// 只替换模型 JSON 里的 `api` 元数据，业务状态字段一字不动。
/// 这是「API 失败不污染远程真实状态」的关键：失败时保留上次成功的
/// state/data，仅把 api.status 标成 error/rate_limited + stale。
pub fn replace_api(value: Value, meta: &ApiMeta) -> Value {
    let ttl = value.get("ttl").and_then(|v| v.as_u64()).unwrap_or(0);
    match value {
        Value::Obj(mut pairs) => {
            let api = meta.to_json();
            if let Some(p) = pairs.iter_mut().find(|(k, _)| k == "api") {
                p.1 = api;
            } else {
                pairs.push(("api".into(), api));
            }
            if let Some(p) = pairs.iter_mut().find(|(k, _)| k == "age_secs") {
                p.1 = Value::num(crate::cache::now().saturating_sub(meta.queried_at));
            }
            Value::Obj(pairs)
        }
        other => frame("unknown", "unknown", "未知", vec![("raw".into(), other)], meta, ttl),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_never_invents_state_it_only_labels() {
        let v = frame(
            "actions",
            "building",
            "正在编译",
            vec![("run_id".into(), Value::num(42))],
            &ApiMeta::ok(RateInfo::default()),
            120,
        );
        assert_eq!(v.get("model").and_then(|x| x.as_str()), Some("actions"));
        assert_eq!(v.get("state").and_then(|x| x.as_str()), Some("building"));
        assert_eq!(v.get("state_label").and_then(|x| x.as_str()), Some("正在编译"));
        assert_eq!(v.get("run_id").and_then(|x| x.as_u64()), Some(42));
    }

    #[test]
    fn error_meta_marks_stale_and_cached() {
        let env = Envelope::fresh(Value::Obj(vec![]), "ok");
        let e = ApiError::RateLimited { reset_secs: 60, retry_after: None };
        let m = ApiMeta::from_error(&e, &env);
        assert_eq!(m.status, "rate_limited");
        assert!(m.stale);
        assert!(m.from_cache, "失败时展示的必须是缓存数据，不能冒充实时");
        assert!(m.error.contains("限流"));
    }
}
