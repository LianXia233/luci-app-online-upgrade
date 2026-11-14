// 网络连通性检测与线路策略（阶段 7）。
//
// - 用真实 HTTP 请求检测，不用 Ping 判断 GitHub 可用性。
// - 区分 DNS / TCP / TLS / 超时 / 403 / 404 / 429 / 5xx / 响应异常 /
//   API 限流 / 加速源不可用 / 官方服务不可用。
// - 结果独立缓存到 NETWORK_FILE，带 TTL；同一地址在 TTL 内不重复探测。
// - 加速源优先（下载链路），失败可靠回退官方直连；API 与下载策略分离。
// - 绝不关闭 TLS 证书验证。

use crate::cache::{self, Envelope, NETWORK_FILE, TTL_NETWORK};
use crate::ghapi::{ApiError, Hub, NetClass, API_BASE};
use crate::json::Value;
use crate::model::{self, ApiMeta};

pub const MODEL: &str = "network";

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub target: String,
    pub class: NetClass,
    pub label: String,
    pub detail: String,
    pub http_status: u64,
    pub latency_ms: u64,
    pub retryable: bool,
    pub from_cache: bool,
    pub last_success_at: u64,
    pub stale: bool,
    pub rate_remaining: Option<u64>,
}

impl ProbeResult {
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("target".into(), Value::str(&self.target)),
            ("class".into(), Value::str(self.class.code())),
            ("label".into(), Value::str(&self.label)),
            ("detail".into(), Value::str(&self.detail)),
            ("http_status".into(), Value::num(self.http_status)),
            ("latency_ms".into(), Value::num(self.latency_ms)),
            ("retryable".into(), Value::Bool(self.retryable)),
            ("from_cache".into(), Value::Bool(self.from_cache)),
            ("last_success_at".into(), Value::num(self.last_success_at)),
            ("last_success_local".into(), Value::str(&cache::human_ts(self.last_success_at))),
            ("stale".into(), Value::Bool(self.stale)),
            (
                "rate_remaining".into(),
                match self.rate_remaining {
                    Some(n) => Value::num(n),
                    None => Value::Null,
                },
            ),
        ])
    }
}

/// 单次真实 HTTP 探测（GET /rate_limit，最轻量且能暴露限流头）。
/// 结果写入独立网络缓存；失败时保留上次成功探测并标记 stale。
pub async fn probe(hub: &Hub, force: bool) -> ProbeResult {
    let ttl = cache::ttl_from_uci("ttl_network", TTL_NETWORK);
    let target = format!("{API_BASE}/rate_limit");
    if !force {
        if let Some(env) = cache::usable(NETWORK_FILE, ttl) {
            return result_from_cache(&env, &target);
        }
        if cache::in_backoff(NETWORK_FILE, cache::FAILURE_BACKOFF_SECS) {
            if let Some(env) = cache::read(NETWORK_FILE) {
                return result_from_cache(&env, &target);
            }
        }
    }

    let started = std::time::Instant::now();
    let outcome = hub.get(&target, false).await;
    let latency = started.elapsed().as_millis() as u64;

    let (env, result) = match outcome {
        Ok(resp) => {
            let r = ProbeResult {
                target: target.clone(),
                class: NetClass::Ok,
                label: NetClass::Ok.label().into(),
                detail: format!("GitHub API 可达（HTTP {}，{} ms）", resp.status, latency),
                http_status: resp.status as u64,
                latency_ms: latency,
                retryable: false,
                from_cache: false,
                last_success_at: cache::now(),
                stale: false,
                rate_remaining: resp.rate.remaining,
            };
            (
                Envelope::fresh(
                    model::frame(
                        MODEL,
                        "ok",
                        "网络正常",
                        vec![("probe".into(), r.to_json())],
                        &ApiMeta::ok(resp.rate),
                        ttl,
                    ),
                    "ok",
                ),
                r,
            )
        }
        Err(e) => {
            let class = e.net_class();
            let r = ProbeResult {
                target: target.clone(),
                class,
                label: class.label().into(),
                detail: e.user_message(),
                http_status: match &e {
                    ApiError::ServerError(c, _) => *c as u64,
                    ApiError::Forbidden(_) => 403,
                    ApiError::NotFound(_) => 404,
                    _ => 0,
                },
                latency_ms: latency,
                retryable: e.retryable(),
                from_cache: false,
                last_success_at: 0,
                stale: false,
                rate_remaining: None,
            };
            let status = if e.is_rate_limited() { "rate_limited" } else { "error" };
            let mut env = cache::preserve_on_failure(NETWORK_FILE, status, &e.user_message());
            env.payload = model::replace_api(env.payload.clone(), &ApiMeta::from_error(&e, &env));
            (env, r)
        }
    };
    let _ = cache::write(NETWORK_FILE, &env);
    result
}

fn result_from_cache(env: &Envelope, target: &str) -> ProbeResult {
    let p = env.payload.get("probe").cloned().unwrap_or(Value::Obj(vec![]));
    let class = NetClass::from_code(p.get("class").and_then(|v| v.as_str()).unwrap_or("unknown"));
    ProbeResult {
        target: target.to_string(),
        class,
        label: p.get("label").and_then(|v| v.as_str()).unwrap_or(class.label()).to_string(),
        detail: p.get("detail").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        http_status: p.get("http_status").and_then(|v| v.as_u64()).unwrap_or(0),
        latency_ms: p.get("latency_ms").and_then(|v| v.as_u64()).unwrap_or(0),
        retryable: matches!(p.get("retryable"), Some(Value::Bool(true))),
        from_cache: true,
        last_success_at: env.last_success_at,
        stale: env.stale,
        rate_remaining: None,
    }
}

impl NetClass {
    pub fn from_code(s: &str) -> NetClass {
        match s {
            "dns" => NetClass::Dns,
            "tcp" => NetClass::Tcp,
            "tls" => NetClass::Tls,
            "timeout" => NetClass::Timeout,
            "http403" => NetClass::Http403,
            "http404" => NetClass::Http404,
            "http429" => NetClass::Http429,
            "http5xx" => NetClass::Http5xx,
            "bad_content" => NetClass::BadContent,
            "rate_limited" => NetClass::RateLimited,
            "mirror_down" => NetClass::MirrorDown,
            "official_down" => NetClass::OfficialDown,
            "ok" => NetClass::Ok,
            _ => NetClass::Unknown,
        }
    }
}

/// 生成固件下载线路候选：加速源优先，官方直连兜底。
/// 只对**下载 URL**（github.com / release-assets / raw）加前缀，
/// 绝不修改 API 参数，也不把加速源强加到 api.github.com。
pub fn download_routes(url: &str, proxy: &str) -> Vec<String> {
    let mut out = Vec::new();
    let p = crate::github::normalize_proxy(proxy);
    if !p.is_empty() && is_download_host(url) {
        let prefixed = crate::github::prefixed(&p, url);
        if prefixed != url {
            out.push(prefixed);
        }
    }
    if !out.iter().any(|u| u == url) {
        out.push(url.to_string());
    }
    out
}

fn is_download_host(url: &str) -> bool {
    let h = url
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    h == "github.com"
        || h == "objects.githubusercontent.com"
        || h == "release-assets.githubusercontent.com"
        || h == "raw.githubusercontent.com"
        || h == "codeload.github.com"
}

/// 线路策略说明（供前端/文档展示），明确 API 与下载分离。
pub fn policy_json(cfg: &crate::ghapi::ApiCfg) -> Value {
    Value::Obj(vec![
        ("api_via_proxy".into(), Value::Bool(cfg.api_via_proxy)),
        ("anonymous_api".into(), Value::Bool(cfg.anonymous())),
        ("download_proxy_priority".into(), Value::Bool(true)),
        ("official_fallback".into(), Value::Bool(true)),
        ("tls_verification_disabled".into(), Value::Bool(false)),
        ("token_exposed_to_frontend".into(), Value::Bool(false)),
        ("api_timeout_secs".into(), Value::num(cfg.request_timeout)),
        ("connect_timeout_secs".into(), Value::num(cfg.connect_timeout)),
        ("max_concurrency".into(), Value::num(crate::ghapi::MAX_CONCURRENCY as u64)),
        ("note".into(), Value::str(
            "GitHub API 默认直连 api.github.com（多数加速源不支持 API）；下载链路优先加速源、失败自动回退官方直连",
        )),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_routes_put_mirror_first_then_official() {
        let url = "https://github.com/o/r/releases/download/v1/fw.bin";
        let routes = download_routes(url, "https://ghproxy.example/");
        assert_eq!(routes.len(), 2);
        assert!(routes[0].starts_with("https://ghproxy.example/https://github.com/"));
        assert_eq!(routes[1], url, "官方直连必须始终保留为回退");
    }

    #[test]
    fn empty_proxy_means_direct_only() {
        let url = "https://github.com/o/r/releases/download/v1/fw.bin";
        assert_eq!(download_routes(url, ""), vec![url.to_string()]);
        assert_eq!(download_routes(url, "not-a-url"), vec![url.to_string()]);
    }

    #[test]
    fn api_urls_are_never_rewritten_by_download_proxy() {
        let api = "https://api.github.com/repos/o/r/actions/runs?per_page=30";
        let routes = download_routes(api, "https://ghproxy.example/");
        assert_eq!(routes, vec![api.to_string()], "API 参数不得被加速源拼坏");
    }

    #[test]
    fn proxy_does_not_duplicate_official_url() {
        let url = "https://github.com/o/r/releases/download/v1/fw.bin";
        let routes = download_routes(url, "https://ghproxy.example/");
        assert_eq!(routes.iter().filter(|u| *u == url).count(), 1);
    }

    #[test]
    fn net_class_codes_roundtrip() {
        for c in [
            NetClass::Dns, NetClass::Tcp, NetClass::Tls, NetClass::Timeout,
            NetClass::Http403, NetClass::Http404, NetClass::Http429, NetClass::Http5xx,
            NetClass::BadContent, NetClass::RateLimited, NetClass::MirrorDown,
            NetClass::OfficialDown, NetClass::Ok, NetClass::Unknown,
        ] {
            assert_eq!(NetClass::from_code(c.code()), c);
        }
    }

    #[test]
    fn policy_never_disables_tls_or_exposes_token() {
        let cfg = crate::ghapi::ApiCfg {
            token: Some("ghp_SECRET".into()),
            proxy: String::new(),
            api_via_proxy: false,
            request_timeout: 25,
            connect_timeout: 12,
        };
        let j = policy_json(&cfg);
        assert_eq!(j.get("tls_verification_disabled"), Some(&Value::Bool(false)));
        assert_eq!(j.get("token_exposed_to_frontend"), Some(&Value::Bool(false)));
        assert_eq!(j.get("api_via_proxy"), Some(&Value::Bool(false)));
        assert!(!j.dump().contains("ghp_SECRET"), "策略输出不得包含 Token");
    }
}
