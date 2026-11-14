// GitHub API 通用请求层（阶段 3）。
//
// 职责：仓库地址解析与校验、统一 HTTP 请求、统一状态码/JSON/API 错误处理、
// 匿名优先 + 可选 Token、限流响应头解析与退避、进程内请求去重与并发限制、
// 加速源策略（API 与下载分离）。
//
// 安全约束：
// - Token 只在 host == api.github.com 时作为 Authorization 头发送；
//   绝不进入日志、错误消息、返回值或前端。
// - 不关闭 TLS 证书验证；不无限重试；不把加速源强加到 GitHub API。

use crate::json::{parse, Value};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};

pub const API_HOST: &str = "api.github.com";
pub const API_BASE: &str = "https://api.github.com";
pub const DEFAULT_API_TIMEOUT: u64 = 25;
pub const DEFAULT_CONNECT_TIMEOUT: u64 = 12;
pub const MAX_CONCURRENCY: usize = 4;

// ---------------------------------------------------------------- 仓库解析

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSpec {
    pub owner: String,
    pub repo: String,
    /// "owner/repo"
    pub slug: String,
    /// 用户可选：构建分支筛选（不填则跟随仓库 default_branch）
    pub build_branch: String,
    /// 用户可选：Workflow 名称/路径过滤（子串，不区分大小写）
    pub workflow_filter: String,
    /// 用户可选：固件匹配规则（正则）
    pub firmware_rule: String,
    /// 原始输入（用于诊断展示，已去除潜在凭据）
    pub source: String,
}

fn valid_owner(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !s.starts_with('-')
}

fn valid_repo(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 140
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && s != "."
        && s != ".."
}

/// 解析任意形式的公开 GitHub 仓库地址：
///   owner/repo
///   https://github.com/owner/repo[.git][/tree/branch]
///   http(s)://github.com/owner/repo/releases/tag/xxx
///   git@github.com:owner/repo.git
/// 只接受 github.com 主机；拒绝注入字符与拼接参数。
pub fn parse_repo(input: &str) -> Result<RepoSpec, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("仓库地址为空".into());
    }
    let mut branch_hint = String::new();

    let (owner, repo) = if raw.contains("github.com") {
        // 去掉协议 / 用户凭据 / 主机前缀
        let after_host = if let Some(pos) = raw.find("github.com") {
            let rest = &raw[pos + "github.com".len()..];
            rest.trim_start_matches(':').trim_start_matches('/')
        } else {
            return Err("不是有效的 GitHub 仓库地址".into());
        };
        // 主机后不允许残留 @（形如 user@github.com 已在上一行处理）
        let path = after_host.split(['?', '#']).next().unwrap_or("");
        let mut segs = path.split('/').filter(|s| !s.is_empty());
        let o = segs.next().unwrap_or("");
        let r = segs.next().unwrap_or("");
        // /tree/<branch> 携带分支提示
        let remaining: Vec<&str> = segs.collect();
        for w in remaining.windows(2) {
            if w[0].eq_ignore_ascii_case("tree") || w[0].eq_ignore_ascii_case("blob") {
                branch_hint = w[1].to_string();
                break;
            }
        }
        (o.to_string(), r.trim_end_matches(".git").to_string())
    } else if raw.contains('/') {
        let mut segs = raw.split('/').filter(|s| !s.is_empty());
        let o = segs.next().unwrap_or("").to_string();
        let r = segs.next().unwrap_or("").trim_end_matches(".git").to_string();
        if segs.next().is_some() {
            return Err("仓库格式不正确（应为 owner/repo 或完整 GitHub URL）".into());
        }
        (o, r)
    } else {
        return Err("仓库格式不正确（应为 owner/repo 或完整 GitHub URL）".into());
    };

    if !valid_owner(&owner) || !valid_repo(&repo) {
        return Err(format!("仓库标识不合法 [{owner}/{repo}]"));
    }

    Ok(RepoSpec {
        slug: format!("{owner}/{repo}"),
        owner,
        repo,
        build_branch: branch_hint,
        workflow_filter: String::new(),
        firmware_rule: String::new(),
        source: raw.to_string(),
    })
}

/// 从 UCI 组装 RepoSpec。兼容旧 `repo` 字段与新 `repo_url` 字段；
/// 新增字段一律带默认值，不覆盖历史配置。
pub fn repo_from_uci() -> Result<RepoSpec, String> {
    let url = crate::uci::get("repo_url").unwrap_or_default();
    let slug = crate::uci::get("repo").unwrap_or_default();
    let input = if !url.trim().is_empty() { &url } else { &slug };
    let mut spec = parse_repo(input)?;
    if spec.build_branch.is_empty() {
        spec.build_branch = crate::uci::get("build_branch").unwrap_or_default();
    }
    spec.workflow_filter = crate::uci::get("workflow_filter").unwrap_or_default();
    spec.firmware_rule = crate::uci::get("firmware_pattern").unwrap_or_default();
    Ok(spec)
}

pub fn api_url(repo: &RepoSpec, suffix: &str) -> String {
    let s = suffix.trim_start_matches('/');
    // GitHub REST API 对 /repos/{slug}/（尾斜杠）返回 404 Not Found，
    // 不带斜杠才返回仓库元数据；查询仓库本身时 suffix 为空，必须去掉尾斜杠。
    if s.is_empty() {
        format!("{API_BASE}/repos/{}", repo.slug)
    } else {
        format!("{API_BASE}/repos/{}/{}", repo.slug, s)
    }
}

// ---------------------------------------------------------------- 错误模型

/// 网络层故障分类（供连通性检测与日志使用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetClass {
    Dns,
    Tcp,
    Tls,
    Timeout,
    Http403,
    Http404,
    Http429,
    Http5xx,
    BadContent,
    RateLimited,
    MirrorDown,
    OfficialDown,
    Ok,
    Unknown,
}

impl NetClass {
    pub fn label(self) -> &'static str {
        match self {
            NetClass::Dns => "DNS 解析失败",
            NetClass::Tcp => "TCP 连接失败",
            NetClass::Tls => "TLS 握手/证书失败",
            NetClass::Timeout => "HTTP 请求超时",
            NetClass::Http403 => "HTTP 403",
            NetClass::Http404 => "HTTP 404",
            NetClass::Http429 => "HTTP 429",
            NetClass::Http5xx => "HTTP 5xx",
            NetClass::BadContent => "响应内容异常",
            NetClass::RateLimited => "GitHub API 限流",
            NetClass::MirrorDown => "加速源不可用",
            NetClass::OfficialDown => "GitHub 官方服务不可用",
            NetClass::Ok => "正常",
            NetClass::Unknown => "未知网络错误",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            NetClass::Dns => "dns",
            NetClass::Tcp => "tcp",
            NetClass::Tls => "tls",
            NetClass::Timeout => "timeout",
            NetClass::Http403 => "http403",
            NetClass::Http404 => "http404",
            NetClass::Http429 => "http429",
            NetClass::Http5xx => "http5xx",
            NetClass::BadContent => "bad_content",
            NetClass::RateLimited => "rate_limited",
            NetClass::MirrorDown => "mirror_down",
            NetClass::OfficialDown => "official_down",
            NetClass::Ok => "ok",
            NetClass::Unknown => "unknown",
        }
    }
    pub fn retryable(self) -> bool {
        matches!(
            self,
            NetClass::Timeout | NetClass::Tcp | NetClass::Http5xx | NetClass::MirrorDown
        )
    }
}

/// 统一的 API 结果错误。所有变体的 Display 都经过脱敏处理。
#[derive(Debug, Clone)]
pub enum ApiError {
    /// 匿名/令牌额度耗尽，含可重试时间
    RateLimited { reset_secs: u64, retry_after: Option<u64> },
    /// 403 但不是限流（私有仓库、SSO、权限不足）
    Forbidden(String),
    NotFound(String),
    Unauthorized(String),
    ServerError(u16, String),
    Network(NetClass, String),
    BadJson(String),
    BadContent(String),
    Config(String),
}

impl ApiError {
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::RateLimited { .. } => "rate_limited",
            ApiError::Forbidden(_) => "forbidden",
            ApiError::NotFound(_) => "not_found",
            ApiError::Unauthorized(_) => "unauthorized",
            ApiError::ServerError(_, _) => "server_error",
            ApiError::Network(c, _) => c.code(),
            ApiError::BadJson(_) => "bad_json",
            ApiError::BadContent(_) => "bad_content",
            ApiError::Config(_) => "config",
        }
    }

    pub fn net_class(&self) -> NetClass {
        match self {
            ApiError::RateLimited { .. } => NetClass::RateLimited,
            ApiError::Forbidden(_) => NetClass::Http403,
            ApiError::NotFound(_) => NetClass::Http404,
            ApiError::ServerError(c, _) => {
                if *c >= 500 { NetClass::Http5xx } else { NetClass::Unknown }
            }
            ApiError::Network(c, _) => *c,
            ApiError::BadContent(_) => NetClass::BadContent,
            _ => NetClass::Unknown,
        }
    }

    /// 面向用户的中文解释。绝不携带 Token 或敏感请求头。
    pub fn user_message(&self) -> String {
        match self {
            ApiError::RateLimited { reset_secs, retry_after } => {
                let wait = retry_after.unwrap_or(*reset_secs);
                format!(
                    "GitHub API 请求受到限流，暂时无法获取最新状态（约 {} 秒后可重试）；已保留上次成功查询的数据",
                    wait
                )
            }
            ApiError::Forbidden(m) => format!("GitHub API 拒绝访问（权限或仓库可见性）：{m}"),
            ApiError::NotFound(m) => format!("GitHub 资源不存在：{m}"),
            ApiError::Unauthorized(m) => format!("GitHub 鉴权失败（请检查 Token）：{m}"),
            ApiError::ServerError(c, m) => format!("GitHub 服务暂时不可用（HTTP {c}）：{m}"),
            ApiError::Network(c, m) => format!("网络请求失败（{}）：{m}", c.label()),
            ApiError::BadJson(m) => format!("GitHub API 返回内容无法解析：{m}"),
            ApiError::BadContent(m) => format!("GitHub API 返回异常内容：{m}"),
            ApiError::Config(m) => format!("配置有误：{m}"),
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, ApiError::Network(c, _) if c.retryable())
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, ApiError::RateLimited { .. })
    }

    /// 结构化错误输出（供日志/诊断层按 module / stage 消费）。
    #[allow(dead_code)]
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("code".into(), Value::str(self.code())),
            ("net_class".into(), Value::str(self.net_class().code())),
            ("message".into(), Value::str(&self.user_message())),
            ("retryable".into(), Value::Bool(self.retryable())),
            ("is_rate_limited".into(), Value::Bool(self.is_rate_limited())),
            ("has_cached_result".into(), Value::Bool(false)),
        ])
    }
}

/// 从 reqwest 错误推断网络故障类型（best effort，绝不吞掉原始信息）。
pub fn classify_reqwest_error(e: &reqwest::Error) -> NetClass {
    let s = e.to_string().to_ascii_lowercase();
    if e.is_timeout() {
        return NetClass::Timeout;
    }
    if s.contains("certificate") || s.contains("tls") || s.contains("ssl") || s.contains("handshake")
    {
        return NetClass::Tls;
    }
    if s.contains("dns") || s.contains("resolve") || s.contains("name or service") {
        return NetClass::Dns;
    }
    if e.is_connect() || s.contains("connection refused") || s.contains("connection reset") {
        return NetClass::Tcp;
    }
    if e.is_builder() {
        return NetClass::Unknown;
    }
    NetClass::Unknown
}

/// 脱敏兜底：若任何文本意外包含已配置 Token，替换为占位符。
/// 用于所有对外输出前的最后一道防线。
pub fn redact(text: &str, token: Option<&str>) -> String {
    match token {
        Some(t) if !t.is_empty() => text.replace(t, "***REDACTED***"),
        _ => text.to_string(),
    }
}

// ---------------------------------------------------------------- 限流信息

#[derive(Debug, Clone, Default)]
pub struct RateInfo {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset: Option<u64>,
    pub retry_after: Option<u64>,
}

impl RateInfo {
    fn from_headers(h: &reqwest::header::HeaderMap) -> RateInfo {
        let get = |k: &str| -> Option<u64> {
            h.get(k)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
        };
        RateInfo {
            limit: get("x-ratelimit-limit"),
            remaining: get("x-ratelimit-remaining"),
            reset: get("x-ratelimit-reset"),
            retry_after: get("retry-after"),
        }
    }

    /// 距离额度重置的秒数。
    pub fn wait_secs(&self) -> u64 {
        if let Some(r) = self.retry_after {
            return r;
        }
        if let Some(reset) = self.reset {
            return reset.saturating_sub(crate::cache::now());
        }
        60
    }

    pub fn is_exhausted(&self) -> bool {
        self.remaining == Some(0)
    }

    pub fn to_json(&self) -> Value {
        let opt = |v: Option<u64>| match v {
            Some(x) => Value::num(x),
            None => Value::Null,
        };
        Value::Obj(vec![
            ("limit".into(), opt(self.limit)),
            ("remaining".into(), opt(self.remaining)),
            ("reset".into(), opt(self.reset)),
            ("retry_after".into(), opt(self.retry_after)),
            ("anonymous".into(), Value::Bool(self.limit == Some(60))),
        ])
    }
}

// ---------------------------------------------------------------- 请求 Hub

#[derive(Debug, Clone)]
pub struct ApiCfg {
    /// 绝不序列化、绝不打印
    pub token: Option<String>,
    /// 已归一化的下载加速源（仅用于下载链路）
    pub proxy: String,
    /// 是否允许把加速源用于 API（默认 false：镜像多数不支持 api.github.com）
    pub api_via_proxy: bool,
    pub request_timeout: u64,
    pub connect_timeout: u64,
}

impl ApiCfg {
    pub fn from_uci() -> ApiCfg {
        let token = crate::uci::get("github_token").filter(|s| !s.trim().is_empty());
        ApiCfg {
            token,
            proxy: crate::github::normalize_proxy(&crate::uci::get_or("proxy", "")),
            api_via_proxy: crate::uci::get("api_via_proxy")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            request_timeout: crate::uci::get("api_timeout")
                .and_then(|v| v.parse::<u64>().ok())
                .map(|v| v.clamp(5, 120))
                .unwrap_or(DEFAULT_API_TIMEOUT),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }

    pub fn anonymous(&self) -> bool {
        self.token.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub status: u16,
    pub body: String,
    pub rate: RateInfo,
    /// 请求 URL，供错误诊断与去重键核对（不进入前端）
    #[allow(dead_code)]
    pub url: String,
    /// 是否来自进程内去重缓存（本次调用未发起网络请求）
    pub deduplicated: bool,
}

pub struct Hub {
    client: Client,
    cfg: ApiCfg,
    /// 进程内响应去重：同一 URL 在一次检查中只请求一次
    memo: Mutex<HashMap<String, ApiResponse>>,
    gate: Semaphore,
}

static SHARED_CLIENT: OnceLock<Client> = OnceLock::new();

fn shared_client(cfg: &ApiCfg) -> Client {
    SHARED_CLIENT
        .get_or_init(|| {
            Client::builder()
                .user_agent("online-upgrade/2.6")
                .connect_timeout(Duration::from_secs(cfg.connect_timeout))
                .timeout(Duration::from_secs(cfg.request_timeout))
                // 连接池复用，限制每主机空闲连接，避免资源占用
                .pool_max_idle_per_host(4)
                .pool_idle_timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::limited(10))
                // 注意：绝不调用 danger_accept_invalid_certs / danger_accept_invalid_hostnames
                .build()
                .expect("failed to build shared GitHub API client")
        })
        .clone()
}

impl Hub {
    pub fn new(cfg: ApiCfg) -> Hub {
        Hub {
            client: shared_client(&cfg),
            cfg,
            memo: Mutex::new(HashMap::new()),
            gate: Semaphore::new(MAX_CONCURRENCY),
        }
    }

    /// 供外部只读检查 API 配置（是否匿名、超时等），不暴露 Token 内容。
    #[allow(dead_code)]
    pub fn cfg(&self) -> &ApiCfg {
        &self.cfg
    }

    pub fn token(&self) -> Option<&str> {
        self.cfg.token.as_deref()
    }

    /// 统一 GET。返回 (状态码, 响应体, 限流信息)；错误全部收敛到 ApiError。
    /// `use_memo=true` 时同一 URL 在一次进程内只请求一次（避免多模型重复查询）。
    pub async fn get(&self, url: &str, use_memo: bool) -> Result<ApiResponse, ApiError> {
        if use_memo {
            if let Some(hit) = self.memo.lock().await.get(url) {
                let mut r = hit.clone();
                r.deduplicated = true;
                return Ok(r);
            }
        }

        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| ApiError::Config("并发控制已关闭".into()))?;

        // 并发许可到手后再查一次 memo，避免并发重复请求
        if use_memo {
            if let Some(hit) = self.memo.lock().await.get(url) {
                let mut r = hit.clone();
                r.deduplicated = true;
                return Ok(r);
            }
        }

        let resp = self.execute(url, false).await?;
        if use_memo {
            self.memo.lock().await.insert(url.to_string(), resp.clone());
        }
        Ok(resp)
    }

    /// 实际执行：API 默认直连；仅当用户显式开启 api_via_proxy 时
    /// 才先试加速源，且失败一定回退官方地址。绝不破坏 URL 参数。
    async fn execute(&self, url: &str, _mirror_tried: bool) -> Result<ApiResponse, ApiError> {
        let token = self.cfg.token.as_deref();
        let mut candidates: Vec<String> = Vec::new();
        if self.cfg.api_via_proxy && !self.cfg.proxy.is_empty() && url.contains(API_HOST) {
            candidates.push(crate::github::prefixed(&self.cfg.proxy, url));
        }
        candidates.push(url.to_string());

        let mut last_err: Option<ApiError> = None;
        let mut mirror_failed = false;
        for (i, u) in candidates.iter().enumerate() {
            let is_mirror = i == 0 && candidates.len() > 1;
            let mut req = self.client.get(u);
            // Token 只允许发给 api.github.com；镜像域名绝不携带凭据
            if let Some(t) = token {
                if host_of(u).eq_ignore_ascii_case(API_HOST) {
                    req = req.header("Authorization", format!("Bearer {t}"));
                }
            }
            req = req.header("Accept", "application/vnd.github+json");
            req = req.header("X-GitHub-Api-Version", "2022-11-28");

            let raw = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    let class = if is_mirror { NetClass::MirrorDown } else { classify_reqwest_error(&e) };
                    let msg = redact(&e.to_string(), token);
                    let err = ApiError::Network(class, msg);
                    if is_mirror {
                        mirror_failed = true;
                        crate::logline!("[ghapi] 加速源请求失败，回退官方地址: {}", class.label());
                        continue;
                    }
                    last_err = Some(err);
                    continue;
                }
            };

            let rate = RateInfo::from_headers(raw.headers());
            let status = raw.status().as_u16();
            let body = raw
                .text()
                .await
                .map_err(|e| ApiError::Network(NetClass::BadContent, redact(&e.to_string(), token)))?;
            let body = redact(&body, token);

            // 镜像返回非 2xx 或异常内容时同样回退官方
            if is_mirror && (status >= 400 || !looks_like_json(&body)) {
                mirror_failed = true;
                crate::logline!("[ghapi] 加速源返回 HTTP {status}，回退官方地址");
                continue;
            }

            return self.classify_status(status, body, rate, u.clone(), mirror_failed);
        }

        Err(last_err.unwrap_or_else(|| {
            ApiError::Network(
                if mirror_failed { NetClass::MirrorDown } else { NetClass::OfficialDown },
                "所有请求线路均失败".into(),
            )
        }))
    }

    fn classify_status(
        &self,
        status: u16,
        body: String,
        rate: RateInfo,
        url: String,
        mirror_failed: bool,
    ) -> Result<ApiResponse, ApiError> {
        let api_msg = github_message(&body);
        match status {
            200 | 201 => {
                if !looks_like_json(&body) {
                    return Err(ApiError::BadContent("响应不是 JSON（可能是错误页面）".into()));
                }
                Ok(ApiResponse { status, body, rate, url, deduplicated: false })
            }
            304 => Ok(ApiResponse { status, body: String::new(), rate, url, deduplicated: false }),
            403 | 429 => {
                // 严格区分「限流」与「权限」：
                // remaining=0 / Retry-After 存在 / body 明确含 rate limit → 限流
                let limited = rate.is_exhausted()
                    || rate.retry_after.is_some()
                    || status == 429
                    || api_msg.to_ascii_lowercase().contains("rate limit")
                    || api_msg.to_ascii_lowercase().contains("abuse");
                if limited {
                    Err(ApiError::RateLimited {
                        reset_secs: rate.wait_secs(),
                        retry_after: rate.retry_after,
                    })
                } else if status == 403 {
                    Err(ApiError::Forbidden(api_msg))
                } else {
                    Err(ApiError::RateLimited {
                        reset_secs: rate.wait_secs(),
                        retry_after: rate.retry_after,
                    })
                }
            }
            401 => Err(ApiError::Unauthorized(api_msg)),
            404 => Err(ApiError::NotFound(api_msg)),
            400 | 422 => Err(ApiError::BadContent(api_msg)),
            500..=599 => Err(ApiError::ServerError(status, api_msg)),
            _ => Err(ApiError::Network(
                if mirror_failed { NetClass::MirrorDown } else { NetClass::Unknown },
                format!("HTTP {status}: {api_msg}"),
            )),
        }
    }

    /// 拉取并解析 JSON（统一处理解析错误）。
    pub async fn get_json(&self, url: &str) -> Result<(Value, RateInfo), ApiError> {
        let resp = self.get(url, true).await?;
        let v = parse(&resp.body).map_err(|e| ApiError::BadJson(e))?;
        Ok((v, resp.rate))
    }
}

fn host_of(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
}

fn looks_like_json(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('{') || t.starts_with('[')
}

/// 提取 GitHub API 错误体中的 message（限长，脱敏后的 body）。
fn github_message(body: &str) -> String {
    if let Ok(v) = parse(body) {
        if let Some(m) = v.get("message").and_then(|x| x.as_str()) {
            return m.chars().take(240).collect();
        }
    }
    let t = body.trim();
    if t.is_empty() {
        return "无响应详情".into();
    }
    t.chars().take(160).collect()
}

/// 从 `GET /repos/{owner}/{repo}` 读取仓库公开信息与默认分支。
pub async fn fetch_repo_meta(hub: &Hub, repo: &RepoSpec) -> Result<RepoMeta, ApiError> {
    let url = api_url(repo, "");
    let (v, rate) = hub.get_json(&url).await?;
    Ok(RepoMeta {
        slug: v
            .get("full_name")
            .and_then(|x| x.as_str())
            .unwrap_or(&repo.slug)
            .to_string(),
        default_branch: v
            .get("default_branch")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        private: matches!(v.get("private"), Some(Value::Bool(true))),
        archived: matches!(v.get("archived"), Some(Value::Bool(true))),
        pushed_at: v
            .get("pushed_at")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        html_url: v
            .get("html_url")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        rate,
    })
}

#[derive(Debug, Clone)]
pub struct RepoMeta {
    pub slug: String,
    pub default_branch: String,
    pub private: bool,
    pub archived: bool,
    pub pushed_at: String,
    pub html_url: String,
    pub rate: RateInfo,
}

impl RepoMeta {
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("slug".into(), Value::str(&self.slug)),
            ("default_branch".into(), Value::str(&self.default_branch)),
            ("private".into(), Value::Bool(self.private)),
            ("archived".into(), Value::Bool(self.archived)),
            ("pushed_at".into(), Value::str(&self.pushed_at)),
            ("html_url".into(), Value::str(&self.html_url)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_owner_repo_slug() {
        let r = parse_repo("LianXia233/OpenWRT-CI").unwrap();
        assert_eq!(r.owner, "LianXia233");
        assert_eq!(r.repo, "OpenWRT-CI");
        assert_eq!(r.slug, "LianXia233/OpenWRT-CI");
    }

    #[test]
    fn parse_full_url_with_tree_branch() {
        let r = parse_repo("https://github.com/LianXia233/OpenWRT-CI/tree/main").unwrap();
        assert_eq!(r.slug, "LianXia233/OpenWRT-CI");
        assert_eq!(r.build_branch, "main");
    }

    #[test]
    fn parse_url_with_git_suffix_and_query() {
        let r = parse_repo("https://github.com/owner/repo.git?tab=readme").unwrap();
        assert_eq!(r.slug, "owner/repo");
    }

    #[test]
    fn parse_git_ssh_form() {
        let r = parse_repo("git@github.com:owner/repo.git").unwrap();
        assert_eq!(r.slug, "owner/repo");
    }

    #[test]
    fn rejects_injection_and_bad_shapes() {
        for bad in [
            "",
            "owner",
            "owner/repo/extra",
            "own er/repo",
            "../etc/passwd",
            "https://evil.com/owner/repo",
            "owner/repo;rm -rf /",
        ] {
            // evil.com 不含 github.com 且为 owner/repo/extra 形态，应被拒绝
            if bad == "https://evil.com/owner/repo" {
                assert!(parse_repo(bad).is_err(), "{bad} 非 GitHub 主机必须拒绝");
            } else {
                assert!(parse_repo(bad).is_err(), "{bad} 应被拒绝");
            }
        }
    }

    #[test]
    fn api_url_does_not_corrupt_query() {
        let r = parse_repo("owner/repo").unwrap();
        assert_eq!(
            api_url(&r, "actions/runs?per_page=20"),
            "https://api.github.com/repos/owner/repo/actions/runs?per_page=20"
        );
    }

    #[test]
    fn api_url_empty_suffix_has_no_trailing_slash() {
        let r = parse_repo("owner/repo").unwrap();
        let root = api_url(&r, "");
        assert_eq!(root, "https://api.github.com/repos/owner/repo");
        assert!(!root.ends_with('/'), "尾斜杠会被 GitHub 判为 404 Not Found");
        assert_eq!(
            api_url(&r, "/releases?per_page=30"),
            "https://api.github.com/repos/owner/repo/releases?per_page=30"
        );
    }

    #[test]
    fn rate_info_wait_uses_retry_after_first() {
        let r = RateInfo {
            limit: Some(60),
            remaining: Some(0),
            reset: Some(crate::cache::now() + 3600),
            retry_after: Some(12),
        };
        assert_eq!(r.wait_secs(), 12);
        assert!(r.is_exhausted());
    }

    #[test]
    fn redact_never_leaks_token() {
        let t = "ghp_SECRET_VALUE_123";
        let s = format!("request failed with {t} in header");
        assert!(!redact(&s, Some(t)).contains(t));
        assert!(redact(&s, Some(t)).contains("***REDACTED***"));
        assert_eq!(redact(&s, None), s);
    }

    #[test]
    fn api_error_messages_are_desensitized_and_actionable() {
        let e = ApiError::RateLimited { reset_secs: 90, retry_after: None };
        assert!(e.user_message().contains("限流"));
        assert!(e.user_message().contains("已保留上次成功"));
        assert!(e.to_json().get("is_rate_limited").is_some());
        let f = ApiError::Forbidden("Resource not accessible by integration".into());
        assert!(!f.is_rate_limited(), "权限 403 不得误判为限流");
        assert_eq!(f.net_class(), NetClass::Http403);
    }

    #[test]
    fn server_error_is_retryable_but_not_build_failure() {
        let e = ApiError::ServerError(502, "bad gateway".into());
        assert_eq!(e.net_class(), NetClass::Http5xx);
        assert!(!e.is_rate_limited());
        assert!(e.user_message().contains("暂时不可用"));
    }

    #[test]
    fn net_class_labels_cover_required_categories() {
        let all = [
            NetClass::Dns, NetClass::Tcp, NetClass::Tls, NetClass::Timeout,
            NetClass::Http403, NetClass::Http404, NetClass::Http429, NetClass::Http5xx,
            NetClass::BadContent, NetClass::RateLimited, NetClass::MirrorDown,
            NetClass::OfficialDown,
        ];
        for c in all {
            assert!(!c.label().is_empty());
            assert!(!c.code().is_empty());
        }
    }
}
