// GitHub Release 检查：拉取 API（带代理回退与限流重试）、
// 匹配固件资产并比较版本。
//
// 设计上不绑定固定来源：仓库 / 标签 / 资产规则 / 代理均来自 UCI
// （`online-upgrade.settings.*`）。取值为空时会明确报“请先配置”，
// 而不是静默使用内置仓库。

use crate::json::{parse, Value};
use crate::logline;
use crate::status;
use crate::uci;
use regex::Regex;
use std::time::Duration;

pub const DEFAULT_TAG: &str = "latest"; // neutral mode default, not a source
pub const DEFAULT_PROXY: &str = "https://gh.acg2.mom/";

/// 在 LuCI 界面中作为快捷选项提供、并在慢速中止日志中建议的
/// URL 前缀式 GitHub 镜像。每个条目均在
/// 2026-11-10 实测：对 raw.githubusercontent.com 与
/// github.com/.../raw/... 两类地址，均返回完全相同的 709 字节内容
/// （SHA-256 前缀 `b96903f063dd`）。此前配置的
/// `github.moeyy.xyz` 已无法解析，因此被移除。
pub const MIRRORS: &[&str] = &[
    DEFAULT_PROXY,
    "https://ghfast.top/",
    "https://ghproxy.net/",
    "https://gh-proxy.com/",
    "https://ghproxy.imciel.com/",
    "https://ghproxy.monkeyray.net/",
    "https://ghproxy.vip/",
];

pub struct Cfg {
    pub repo: String,
    pub tag: String,
    pub proxy: String,
    pub pattern: String,
    pub token: Option<String>,
}

/// 归一化 URL 前缀代理：去除首尾空白、要求为 http(s) 主机，并以
/// 恰好一个 `/` 结尾，确保 `format!("{proxy}{url}")` 不会拼出
/// `https://mirrorhttps://github.com/...`。
pub fn normalize_proxy(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    let lower = t.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return String::new();
    }
    let host = t
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if host.is_empty() {
        return String::new();
    }
    let mut s = t.to_string();
    while s.ends_with('/') {
        s.pop();
    }
    s.push('/');
    s
}

/// 为 `url` 加上（已归一化的）代理前缀。代理为空表示直连。
pub fn prefixed(proxy: &str, url: &str) -> String {
    let p = normalize_proxy(proxy);
    if p.is_empty() {
        url.to_string()
    } else {
        format!("{p}{url}")
    }
}

pub fn load_cfg() -> Cfg {
    let raw_proxy = uci::get_or("proxy", DEFAULT_PROXY);
    let proxy = normalize_proxy(&raw_proxy);
    if !raw_proxy.trim().is_empty() && proxy.is_empty() {
        logline!(
            "[cfg] 代理地址 [{}] 无效（需 http(s)://主机名），本次改为直连",
            raw_proxy
        );
    }
    Cfg {
        repo: uci::get_or("repo", ""),
        tag: uci::get_or("tag", DEFAULT_TAG),
        proxy,
        pattern: uci::get_or("firmware_pattern", ""),
        token: uci::get("github_token"),
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

/// 只有 api.github.com 可以收到用户令牌：前缀镜像属于第三方域名，
/// 绝不能看到 Authorization 请求头。
async fn api_get(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<(u16, String), String> {
    let mut req = client.get(url).timeout(Duration::from_secs(30));
    if let Some(t) = token {
        if host_of(url).eq_ignore_ascii_case("api.github.com") {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let code = resp.status().as_u16();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    Ok((code, body))
}

/// 按完整容错链路拉取 GitHub API URL：
/// 直连 → 连接失败时回退代理前缀 → 403 限流重试。
async fn api_fetch(
    client: &reqwest::Client,
    url: &str,
    cfg: &Cfg,
) -> Result<(u16, String), String> {
    let (mut code, mut body) = match api_get(client, url, cfg.token.as_deref()).await {
        Ok(x) => x,
        Err(e) => {
            // TLS/DNS/连接层失败 → 走代理重试（不携带令牌）
            if cfg.proxy.is_empty() {
                return Err(format!("直连失败且未配置可用代理: {e}"));
            }
            logline!("[check] direct failed: {e}, trying proxy");
            let proxied = prefixed(&cfg.proxy, url);
            match api_get(client, &proxied, cfg.token.as_deref()).await {
                Ok(x) => x,
                Err(e) => return Err(format!("直连与代理均失败: {e}")),
            }
        }
    };
    if code == 403 {
        status::set("checking", 30, "GitHub API 限速,15 秒后重试...");
        tokio::time::sleep(Duration::from_secs(15)).await;
        match api_get(client, url, cfg.token.as_deref()).await {
            Ok(x) => {
                code = x.0;
                body = x.1;
            }
            Err(e) => return Err(format!("API 请求失败: {e}")),
        }
    }
    if code == 403 {
        return Err("GitHub API 限速(HTTP 403),请等待 1 小时或配置 github_token".into());
    }
    if code != 200 {
        return Err(format!("GitHub API 返回 HTTP {code}"));
    }
    Ok((code, body))
}

/// 按与 API 相同的线路策略拉取小型文本资源（校验清单）：
/// 优先直连，失败回退代理，且只接受 2xx 响应体。
async fn fetch_text(client: &reqwest::Client, url: &str, cfg: &Cfg) -> Option<String> {
    let mut urls = vec![url.to_string()];
    if !cfg.proxy.is_empty() {
        urls.push(prefixed(&cfg.proxy, url));
    }
    for u in urls {
        let resp = match client.get(&u).timeout(Duration::from_secs(15)).send().await {
            Ok(r) => r,
            Err(e) => {
                logline!("[check] 校验单请求失败({}): {e}", u);
                continue;
            }
        };
        if !resp.status().is_success() {
            logline!("[check] 校验单返回 HTTP {}", resp.status().as_u16());
            continue;
        }
        match resp.text().await {
            Ok(t) => return Some(t),
            Err(e) => logline!("[check] 校验单读取失败: {e}"),
        }
    }
    None
}

/// 候选 Release：`latest` 模式返回最近的多个 Release（GitHub 已按
/// 最新在前排序），可选地按标签名正则过滤，写法为
/// `latest@<正则>`；指定标签模式只返回单个条目。
fn release_candidates<'a>(
    parsed: &'a Value,
    use_latest: bool,
    cfg_tag: &str,
    tag_filter: &Option<Regex>,
) -> Vec<(String, &'a Value)> {
    if use_latest {
        parsed
            .as_arr()
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| {
                        let tag = r.get("tag_name").and_then(|t| t.as_str())?;
                        if let Some(re) = tag_filter {
                            if !re.is_match(tag) {
                                return None;
                            }
                        }
                        Some((tag.to_string(), r))
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        vec![(cfg_tag.to_string(), parsed)]
    }
}

fn asset_name(a: &Value) -> String {
    a.get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string()
}

fn asset_size(a: &Value) -> u64 {
    a.get("size").and_then(|s| s.as_u64()).unwrap_or(0)
}

struct Decision {
    has_update: bool,
    reason: String,
    /// 取值：newer | same | older | unknown
    relation: &'static str,
    /// 取值：decidable | incomparable | suspect
    state: &'static str,
    /// 是否需要用户二次确认（目前仅降级需要）。
    confirm_required: bool,
}

/// 同版本统一文案：同版本不构成「有更新」，不引导普通升级，并指出重装只能显式强制。
/// `rebuilt` 传入重新构建的发布时间（`Some`）时把这一点也写清楚，但不因此变成「有新版本」。
fn same_version_reason(target: &crate::version::Version, rebuilt: Option<&str>) -> String {
    match rebuilt {
        Some(ts) => format!(
            "已是最新 v{}（与当前固件同版本）；该发布已重新构建（发布时间 {}），默认不做普通升级，如需重装请手动点击「强制升级」",
            target.as_display(),
            utc_to_local(ts)
        ),
        None => format!(
            "已是最新 v{}（与当前固件同版本），默认不做普通升级；如需同版本重装请手动点击「强制升级」",
            target.as_display()
        ),
    }
}

/// 判定依据说明：把「本地版本是多少、从哪取的」写进文案，让用户知道自己在确认什么。
fn local_basis(local: &crate::localver::LocalVersion, cur: &crate::version::Version) -> String {
    if local.source_text.is_empty() {
        format!("当前 {}", cur.as_display())
    } else {
        format!("当前 {}，依据：{}", cur.as_display(), local.source_text)
    }
}

/// 版本与更新判定。
///
/// `target` 是从资产名解析出的版本（解析失败时回退到标签）；
/// `local_ver` 是 `localver::detect()` 解析出的本地固件版本
/// （SNAPSHOT 镜像没有发布号时会回退到构建日期）。
/// 这里区分三种结果，而不是全部归入一个
/// “无法比较”分类：
///   - decidable  本地版本可信，可判定为新版/旧版/同版
///   - suspect    本地版本只能靠弱依据（构建时间、修订号、包版本）推断
///   - incomparable 确实取不到版本号：按「将升级到该发布版本」放行，但明确说明原因
///
/// 只有降级需要二次确认；不确定本身从不阻断，
/// 且机型/架构/哈希校验在任何情况下都仍然生效。
fn decide(
    last_ts: Option<&str>,
    last_version: Option<&str>,
    asset_updated: &str,
    target: Option<&crate::version::Version>,
    local: &crate::localver::LocalVersion,
    local_ver: Option<&crate::version::Version>,
) -> Decision {
    let weak = local.confidence != crate::localver::Confidence::Strong;
    match (local_ver, target) {
        (Some(c), Some(t)) => match c.compare(t) {
            std::cmp::Ordering::Less => Decision {
                has_update: true,
                reason: if weak {
                    format!(
                        "发现新版本 v{}（{}）",
                        t.as_display(),
                        local_basis(local, c)
                    )
                } else {
                    format!("发现新版本 v{}（当前 v{}）", t.as_display(), c.as_display())
                },
                relation: "newer",
                state: if weak { "suspect" } else { "decidable" },
                confirm_required: false,
            },
            std::cmp::Ordering::Greater => Decision {
                has_update: false,
                reason: if weak {
                    format!(
                        "Release 版本 v{} 低于当前 {}，已拒绝降级（依据较弱，如确认可强制升级）",
                        t.as_display(),
                        local_basis(local, c)
                    )
                } else {
                    format!(
                        "Release 版本 v{} 低于当前 v{}，不执行降级（如需降级请强制升级）",
                        t.as_display(),
                        c.as_display()
                    )
                },
                relation: "older",
                state: if weak { "suspect" } else { "decidable" },
                confirm_required: true,
            },
            std::cmp::Ordering::Equal => {
                // 同版本一律不算「有更新」：Release 被重新构建也只是重刷同一版本，
                // 重装必须由用户显式点「强制升级」，不能摆成普通升级入口。
                let recorded = last_version == Some(t.as_display().as_str());
                let rebuilt =
                    recorded && !asset_updated.is_empty() && last_ts != Some(asset_updated);
                Decision {
                    has_update: false,
                    reason: same_version_reason(t, rebuilt.then_some(asset_updated)),
                    relation: "same",
                    state: if weak { "suspect" } else { "decidable" },
                    confirm_required: false,
                }
            }
        },
        (None, Some(t)) => {
            let already =
                last_version == Some(t.as_display().as_str()) && last_ts == Some(asset_updated);
            if already {
                Decision {
                    has_update: false,
                    reason: same_version_reason(t, None),
                    relation: "same",
                    state: "incomparable",
                    confirm_required: false,
                }
            } else {
                let marker = if local.raw.is_empty() {
                    "未知"
                } else {
                    local.raw.as_str()
                };
                Decision {
                    has_update: true,
                    reason: format!(
                        "自用构建固件没有可比较的版本号（本地标记 {}），将按「升级到 v{}」处理；机型、架构与哈希校验仍然生效",
                        marker,
                        t.as_display()
                    ),
                    relation: "unknown",
                    state: "incomparable",
                    confirm_required: false,
                }
            }
        }
        (_, None) => {
            if !asset_updated.is_empty() && last_ts.map(|t| t != asset_updated).unwrap_or(true) {
                Decision {
                    has_update: true,
                    reason: format!(
                        "未能从文件名或标签解析出版本号，按 Release 发布时间判定（{}）",
                        utc_to_local(asset_updated)
                    ),
                    relation: "unknown",
                    state: "incomparable",
                    confirm_required: false,
                }
            } else {
                Decision {
                    has_update: false,
                    reason: "已是最新（按发布时间判定）".into(),
                    relation: "unknown",
                    state: "incomparable",
                    confirm_required: false,
                }
            }
        }
    }
}

/// 解析 `latest@<正则>` 通道过滤器。
///
/// 非法正则必须作为配置错误上报，绝不能退化为“不过滤”：若静默
/// 忽略它，`release_candidates` 会检查所有近期 Release，
/// 可能选中非预期通道的版本。通道选择器中的拼写错误
/// 正是本插件在其他地方也拒绝的静默降级，
/// 因此这里同样选择显式报错。
fn parse_tag_filter(tag: &str) -> Result<Option<Regex>, String> {
    match tag.strip_prefix("latest@") {
        Some(p) => match Regex::new(p) {
            Ok(re) => Ok(Some(re)),
            Err(e) => Err(format!(
                "标签过滤正则无效 [{p}]: {e}（请修正「Release 标签追踪策略」或改回 latest）"
            )),
        },
        None => Ok(None),
    }
}

/// 完整的检查流程。返回结果对象（调用方还会将其写入
/// 状态文件）。
pub async fn check(client: &reqwest::Client) -> Value {
    let cfg = load_cfg();
    if cfg.repo.is_empty() {
        return check_error(
            &cfg,
            "未配置 GitHub 仓库,请先在「仓库配置」中填写(owner/repo)",
        );
    }
    // `latest` 追踪最新 Release；`latest@<正则>` 将候选限制为
    // 标签名匹配该正则的 Release（例如 `latest@^自用`）。
    let use_latest = cfg.tag.eq_ignore_ascii_case("latest") || cfg.tag.starts_with("latest@");
    let tag_filter = match parse_tag_filter(&cfg.tag) {
        Ok(f) => f,
        Err(e) => return check_error(&cfg, &e),
    };
    let api = if use_latest {
        format!(
            "https://api.github.com/repos/{}/releases?per_page=30",
            cfg.repo
        )
    } else {
        format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            cfg.repo, cfg.tag
        )
    };

    status::set("checking", 10, "正在获取 Release 信息...");
    let device = crate::compat::DeviceProfile::current();
    logline!(
        "[check] repo={} tag={} device={} board={} arch={}",
        cfg.repo,
        cfg.tag,
        device.model,
        device.board_name,
        device.arch
    );

    let body = match api_fetch(client, &api, &cfg).await {
        Ok((_, body)) => body,
        Err(e) => return check_error(&cfg, &e),
    };

    status::set("checking", 60, "正在解析固件列表...");
    let parsed = match parse(&body) {
        Ok(v) => v,
        Err(e) => return check_error(&cfg, &format!("JSON 解析失败: {e}")),
    };

    let min_size_mb: u64 = uci::get("min_size_mb")
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let min_bytes = min_size_mb.saturating_mul(1024 * 1024);
    let rule_re = if cfg.pattern.is_empty() {
        None
    } else {
        match Regex::new(&cfg.pattern) {
            Ok(r) => Some(r),
            Err(e) => return check_error(&cfg, &format!("firmware_pattern 正则无效: {e}")),
        }
    };

    let mut resolved_tag = String::new();
    let mut chosen: Option<(Value, crate::compat::CompatResult, Option<String>)> = None;
    let mut matched_rel: Option<&Value> = None;
    let mut ambiguous: Vec<String> = Vec::new();
    let mut candidate_names: Vec<String> = Vec::new();
    let mut arch_conflicts: Vec<String> = Vec::new();

    for (tag_name, release) in release_candidates(&parsed, use_latest, &cfg.tag, &tag_filter) {
        let assets = match release.get("assets").and_then(|a| a.as_arr()) {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };

        let pick: Option<(Value, crate::compat::CompatResult, Option<String>)> =
            if let Some(re) = &rule_re {
                // 显式规则：取第一个体积达标且声明架构
                // 不冲突的名称匹配项。
                let mut found = None;
                for a in assets {
                    if !name_matches(a, re, min_bytes) {
                        continue;
                    }
                    let name = asset_name(a);
                    let eval = crate::compat::evaluate_with_rule(&device, &name, &cfg.pattern);
                    if eval.level == crate::compat::CompatLevel::Incompatible {
                        arch_conflicts.push(name);
                        continue;
                    }
                    found = Some((a.clone(), eval, Some(cfg.pattern.clone())));
                    break;
                }
                found
            } else {
                // 自动模式：只接受机型与架构都得到确认的资产。
                // 不做“先打分、然后随便取一个”的兜底。
                let mut scored: Vec<(i32, String, &Value, crate::compat::CompatResult)> = assets
                    .iter()
                    .filter_map(|a| {
                        let name = asset_name(a);
                        if asset_size(a) < min_bytes {
                            return None;
                        }
                        if candidate_names.len() < 6 {
                            candidate_names.push(format!("{} [{}]", name, "候选"));
                        }
                        let eval = crate::compat::evaluate_asset(&device, &name);
                        if eval.level == crate::compat::CompatLevel::Compatible {
                            Some((eval.score, name.clone(), a, eval))
                        } else {
                            None
                        }
                    })
                    .collect();
                scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                match scored.len() {
                    0 => None,
                    1 => {
                        let (_, _, a, eval) = scored.remove(0);
                        Some((a.clone(), eval, None))
                    }
                    _ => {
                        let top = scored[0].0;
                        let tied: Vec<String> = scored
                            .iter()
                            .filter(|c| c.0 == top)
                            .map(|c| c.1.clone())
                            .collect();
                        if tied.len() > 1 {
                            ambiguous = tied;
                            None
                        } else {
                            let (_, _, a, eval) = scored.remove(0);
                            Some((a.clone(), eval, None))
                        }
                    }
                }
            };

        if let Some((asset, eval, rule)) = pick {
            resolved_tag = tag_name;
            chosen = Some((asset, eval, rule));
            matched_rel = Some(release);
            break;
        }
    }

    if chosen.is_none() {
        if !ambiguous.is_empty() {
            return check_error(
                &cfg,
                &format!(
                    "Release {} 中有多个同等匹配的固件，无法自动选择：{}。请配置 firmware_pattern 或在「固件列表」中手动选择",
                    resolved_tag,
                    ambiguous.join(", ")
                ),
            );
        }
        if !arch_conflicts.is_empty() {
            return check_error(
                &cfg,
                &format!(
                    "firmware_pattern 命中的文件与架构冲突，已拒绝：{}",
                    arch_conflicts.join(", ")
                ),
            );
        }
        candidate_names.retain(|_| true);
        return check_error(
            &cfg,
            &format!(
                "未找到可确认匹配的固件（本机 {}/{}/{}，大小≥{} MB）。候选文件：{}。请配置 firmware_pattern 或在「固件列表」中选择文件后生成规则",
                device.model,
                device.board_name,
                device.arch,
                min_size_mb,
                if candidate_names.is_empty() {
                    "无".to_string()
                } else {
                    candidate_names.join("; ")
                }
            ),
        );
    }

    let (asset, compat, chosen_rule) = chosen.unwrap();
    let file_name = asset_name(&asset);
    let size = asset_size(&asset);
    let updated = asset
        .get("updated_at")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    let url = asset
        .get("browser_download_url")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    // API 资产端点：会重定向到带签名的 release-assets CDN URL，
    // 因此 github.com Web 前端不可用时仍能提供文件。
    let api_url = asset
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();

    let (fw_version, version_source) = match crate::version::pick(&file_name) {
        Some(v) => (v.as_display(), "filename"),
        None => match crate::version::pick(&resolved_tag) {
            Some(v) => (v.as_display(), "tag"),
            None => (String::new(), "none"),
        },
    };
    let (cur_id, cur_release, cur_revision) = read_distrib();
    let target_ver = crate::version::pick(&fw_version);
    // 本机版本按可信度逐级探测（自用 SNAPSHOT 构建会退到构建时间）。
    let local = crate::localver::detect();
    let local_ver = local.comparable(target_ver.as_ref());
    let local_display = match &local_ver {
        Some(v) => v.as_display(),
        None if local.source == "none" => local.raw.clone(),
        None => String::new(),
    };

    let last_ts = uci::get("last_upgrade_ts");
    let last_version = uci::get("last_upgrade_version");

    let decision = decide(
        last_ts.as_deref(),
        last_version.as_deref(),
        &updated,
        target_ver.as_ref(),
        &local,
        local_ver.as_ref(),
    );

    let mut sha256_hash = uci::get("expected_sha256")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let mut hash_source = if sha256_hash.is_empty() {
        "none"
    } else {
        "config"
    };
    if sha256_hash.is_empty() {
        if let Some(rel) = matched_rel {
            if let Some(assets) = rel.get("assets").and_then(|a| a.as_arr()) {
                let lower_target = file_name.to_ascii_lowercase();
                let chk_asset = assets.iter().find(|a| {
                    let lower = asset_name(a).to_ascii_lowercase();
                    lower == "sha256sums"
                        || lower == "sha256sums.txt"
                        || lower == "sha256sum.txt"
                        || lower == "checksums.txt"
                        || lower == "sha256sums.sha256"
                        || lower == format!("{lower_target}.sha256")
                        || lower == format!("{lower_target}.sha256sum")
                });
                if let Some(ca) = chk_asset {
                    if let Some(dl) = ca.get("browser_download_url").and_then(|u| u.as_str()) {
                        if let Some(txt) = fetch_text(client, dl, &cfg).await {
                            if let Some(h) = crate::hash::find_sha256_in_text(&txt, &file_name)
                                .or_else(|| crate::hash::extract_single_sha256(&txt))
                            {
                                sha256_hash = h;
                                hash_source = "checksum_asset";
                            }
                        }
                    }
                }
            }
            if sha256_hash.is_empty() {
                if let Some(b) = rel.get("body").and_then(|b| b.as_str()) {
                    if let Some(h) = crate::hash::find_sha256_in_text(b, &file_name) {
                        sha256_hash = h;
                        hash_source = "release_body";
                    }
                }
            }
        }
    }
    if sha256_hash.is_empty() {
        hash_source = "none";
    }

    let latest = vec![
        ("name".into(), Value::str(&file_name)),
        ("size".into(), Value::num(size)),
        ("size_human".into(), Value::str(&human_size(size))),
        ("updated".into(), Value::str(&updated)),
        ("updated_local".into(), Value::str(&utc_to_local(&updated))),
        ("version".into(), Value::str(&fw_version)),
        ("version_source".into(), Value::str(version_source)),
        ("url".into(), Value::str(&url)),
        ("api_url".into(), Value::str(&api_url)),
        ("sha256".into(), Value::str(&sha256_hash)),
        ("sha256_source".into(), Value::str(hash_source)),
    ];

    logline!(
        "[check] 选定资产: {} | 版本 {} (来源 {}) | 识别: {} ({}) | {}",
        file_name,
        if fw_version.is_empty() {
            "<无法解析>"
        } else {
            &fw_version
        },
        version_source,
        compat.badge,
        compat.model_check,
        compat.reason
    );
    logline!(
        "[check] 本机版本: {} (来源 {} / {}, 判定状态 {})",
        if local_display.is_empty() {
            "<无法比较>"
        } else {
            &local_display
        },
        local.source,
        local.confidence.as_str(),
        decision.state
    );
    for line in &compat.basis {
        logline!("[check] 依据: {line}");
    }

    let mut current = Vec::new();
    current.push(("id".into(), Value::str(&cur_id)));
    current.push(("release".into(), Value::str(&cur_release)));
    current.push(("revision".into(), Value::str(&cur_revision)));
    current.push((
        "parsed".into(),
        match &local_ver {
            Some(v) => Value::str(&v.as_display()),
            None => Value::Null,
        },
    ));
    current.push(("version_source".into(), Value::str(&local.source)));
    current.push(("version_source_text".into(), Value::str(&local.source_text)));
    current.push((
        "version_confidence".into(),
        Value::str(local.confidence.as_str()),
    ));

    Value::Obj(vec![
        ("ok".into(), Value::Bool(true)),
        ("repo".into(), Value::str(&cfg.repo)),
        ("tag".into(), Value::str(&cfg.tag)),
        ("resolved_tag".into(), Value::str(&resolved_tag)),
        (
            "release_url".into(),
            Value::str(&format!(
                "https://github.com/{}/releases/tag/{}",
                cfg.repo, resolved_tag
            )),
        ),
        ("current".into(), Value::Obj(current)),
        ("latest".into(), Value::Obj(latest)),
        (
            "version".into(),
            Value::Obj(vec![
                ("current".into(), Value::str(&cur_release)),
                ("local".into(), Value::str(&local_display)),
                ("local_source".into(), Value::str(&local.source)),
                ("local_source_text".into(), Value::str(&local.source_text)),
                (
                    "local_confidence".into(),
                    Value::str(local.confidence.as_str()),
                ),
                ("target".into(), Value::str(&fw_version)),
                ("relation".into(), Value::str(decision.relation)),
                ("state".into(), Value::str(decision.state)),
                ("source".into(), Value::str(version_source)),
                (
                    "confirm_required".into(),
                    Value::Bool(decision.confirm_required),
                ),
            ]),
        ),
        ("has_update".into(), Value::Bool(decision.has_update)),
        ("reason".into(), Value::str(&decision.reason)),
        ("compatibility".into(), compat.to_json()),
        (
            "matched_rule".into(),
            match &chosen_rule {
                Some(r) => Value::str(r),
                None => Value::Null,
            },
        ),
        ("device".into(), device.to_json()),
    ])
}

/// `probe` 子命令：列出任意仓库最新 Release 的资产
///（供 Web 界面让用户交互式选择固件文件）。
/// repo/tag 从参数读取，不读取 UCI —— 完全不绑定来源。
pub async fn probe(client: &reqwest::Client, repo: &str, tag: &str) -> Value {
    let cur = load_cfg();
    let cfg = Cfg {
        repo: repo.to_string(),
        tag: if tag.is_empty() {
            "latest".into()
        } else {
            tag.to_string()
        },
        proxy: cur.proxy,
        pattern: String::new(),
        token: cur.token,
    };
    if cfg.repo.is_empty() || !cfg.repo.contains('/') {
        return Value::Obj(vec![
            ("ok".into(), Value::Bool(false)),
            (
                "error".into(),
                Value::str("仓库格式不正确(应为 owner/repo)"),
            ),
        ]);
    }
    let use_latest = cfg.tag.eq_ignore_ascii_case("latest") || cfg.tag.starts_with("latest@");
    let tag_filter = match parse_tag_filter(&cfg.tag) {
        Ok(f) => f,
        Err(e) => {
            return Value::Obj(vec![
                ("ok".into(), Value::Bool(false)),
                ("error".into(), Value::str(&e)),
            ])
        }
    };
    let api = if use_latest {
        format!(
            "https://api.github.com/repos/{}/releases?per_page=30",
            cfg.repo
        )
    } else {
        format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            cfg.repo, cfg.tag
        )
    };

    let body = match api_fetch(client, &api, &cfg).await {
        Ok((_, body)) => body,
        Err(e) => {
            return Value::Obj(vec![
                ("ok".into(), Value::Bool(false)),
                ("error".into(), Value::str(&e)),
            ])
        }
    };
    let parsed = match parse(&body) {
        Ok(v) => v,
        Err(e) => {
            return Value::Obj(vec![
                ("ok".into(), Value::Bool(false)),
                ("error".into(), Value::str(&format!("JSON 解析失败: {e}"))),
            ])
        }
    };

    let device = crate::compat::DeviceProfile::current();
    for (tag_name, release) in release_candidates(&parsed, use_latest, &cfg.tag, &tag_filter) {
        if let Some(assets) = release.get("assets").and_then(|a| a.as_arr()) {
            if assets.is_empty() {
                continue;
            }
            let mut list: Vec<(i32, Value)> = assets
                .iter()
                .take(80)
                .map(|a| {
                    let name = asset_name(a);
                    let size = asset_size(a);
                    let compat = crate::compat::evaluate_asset(&device, &name);
                    let score = compat.score;
                    let obj = Value::Obj(vec![
                        ("name".into(), Value::str(&name)),
                        ("size".into(), Value::num(size)),
                        ("size_human".into(), Value::str(&human_size(size))),
                        ("compat".into(), compat.to_json()),
                    ]);
                    (score, obj)
                })
                .collect();
            list.sort_by(|a, b| b.0.cmp(&a.0));
            let sorted_assets: Vec<Value> = list.into_iter().map(|(_, v)| v).collect();
            return Value::Obj(vec![
                ("ok".into(), Value::Bool(true)),
                ("resolved_tag".into(), Value::str(&tag_name)),
                ("device".into(), device.to_json()),
                ("assets".into(), Value::Arr(sorted_assets)),
            ]);
        }
    }
    Value::Obj(vec![
        ("ok".into(), Value::Bool(false)),
        (
            "error".into(),
            Value::str("该仓库(或该标签过滤条件)下没有带附件的 Release"),
        ),
    ])
}

fn check_error(cfg: &Cfg, msg: &str) -> Value {
    logline!("[check] error: {msg}");
    Value::Obj(vec![
        ("ok".into(), Value::Bool(false)),
        ("repo".into(), Value::str(&cfg.repo)),
        ("tag".into(), Value::str(&cfg.tag)),
        ("error".into(), Value::str(msg)),
    ])
}

fn name_matches(a: &Value, re: &Regex, min_bytes: u64) -> bool {
    asset_size(a) >= min_bytes
        && a.get("name")
            .and_then(|n| n.as_str())
            .map(|n| re.is_match(n))
            .unwrap_or(false)
}

pub fn read_distrib() -> (String, String, String) {
    let text = std::fs::read_to_string("/etc/openwrt_release").unwrap_or_default();
    let grab = |key: &str| -> String {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix(&format!("{key}='")) {
                return rest.trim_end_matches('\'').to_string();
            }
        }
        String::new()
    };
    (
        grab("DISTRIB_ID"),
        grab("DISTRIB_RELEASE"),
        grab("DISTRIB_REVISION"),
    )
}

// ---- 时间辅助函数（UTC+8，与原硬编码行为一致）----

/// "2026-11-07T09:00:00Z" → "2026-11-07 17:00:00"
pub fn utc_to_local(iso: &str) -> String {
    let b = iso.as_bytes();
    if b.len() < 19 {
        return iso.to_string();
    }
    let num = |a: usize, z: usize| -> i64 {
        iso.get(a..z)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
    };
    let (y, mo, d) = (num(0, 4), num(5, 7), num(8, 10));
    let (h, mi, s) = (num(11, 13), num(14, 16), num(17, 19));
    if y == 0 {
        return iso.to_string();
    }
    let days = days_from_civil(y, mo, d);
    let epoch = days * 86400 + h * 3600 + mi * 60 + s + 8 * 3600;
    let (y2, mo2, d2) = crate::version::civil_from_days(epoch.div_euclid(86400));
    let sec = epoch.rem_euclid(86400);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y2,
        mo2,
        d2,
        sec / 3600,
        (sec % 3600) / 60,
        sec % 60
    )
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn human_size(n: u64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", n as f64 / 1073741824.0)
    } else if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> crate::version::Version {
        crate::version::pick(s).unwrap()
    }

    #[test]
    fn version_extract() {
        assert_eq!(
            crate::version::pick("immortalwrt-25.12.0-x86-64-generic-ext4-combined-efi.img.gz")
                .map(|v| v.as_display())
                .as_deref(),
            Some("25.12.0")
        );
        assert_eq!(
            crate::version::pick(
                "immortalwrt-23.05.3-mediatek-filogic-hiveton_h5000m-squashfs-sysupgrade.bin"
            )
            .map(|v| v.as_display())
            .as_deref(),
            Some("23.05.3")
        );
        assert_eq!(
            crate::version::pick("vendor-2.1.0-distro-24.10.0-x86-64.img.gz")
                .map(|v| v.as_display())
                .as_deref(),
            Some("24.10.0")
        );
        assert!(crate::version::pick("no-version-here.img.gz").is_none());
    }

    #[test]
    fn tag_filter_rejects_invalid_regex() {
        // 不配置过滤时保持「不过滤」语义
        assert!(parse_tag_filter("latest").unwrap().is_none());
        assert!(parse_tag_filter("latest@^自用").unwrap().is_some());
        // 非法正则必须是错误，不能被吞成 None 后退化为「全通道最新」
        let err = parse_tag_filter("latest@^(自用").unwrap_err();
        assert!(err.contains("标签过滤正则无效"));
    }

    /// 构造一个「发行信息里直接读到版本号」的本机版本
    fn local_of(value: &str) -> crate::localver::LocalVersion {
        crate::localver::LocalVersion {
            version: crate::version::pick(value),
            raw: value.to_string(),
            source: "DISTRIB_RELEASE".into(),
            source_text: "发行信息 DISTRIB_RELEASE".into(),
            confidence: if crate::version::pick(value).is_some() {
                crate::localver::Confidence::Strong
            } else {
                crate::localver::Confidence::None
            },
            build_ymd: None,
        }
    }

    /// 构造一个「只能靠构建时间推断」的本机版本（自用 SNAPSHOT）
    fn local_by_build_date(y: i64, m: u32, d: u32) -> crate::localver::LocalVersion {
        crate::localver::LocalVersion {
            version: None,
            raw: "SNAPSHOT".into(),
            source: "build_time".into(),
            source_text: format!("固件构建时间 {y}-{m:02}-{d:02}（弱依据，仅能与日期型标签比较）"),
            confidence: crate::localver::Confidence::Weak,
            build_ymd: Some((y, m, d)),
        }
    }

    fn decide_with(
        target: Option<&crate::version::Version>,
        local: &crate::localver::LocalVersion,
        last_ts: Option<&str>,
        last_version: Option<&str>,
    ) -> Decision {
        let local_ver = local.comparable(target);
        decide(
            last_ts,
            last_version,
            "2026-01-01T00:00:00Z",
            target,
            local,
            local_ver.as_ref(),
        )
    }

    #[test]
    fn decide_newer() {
        let t = v("25.12.0");
        let d = decide_with(Some(&t), &local_of("24.10.0"), Some("t"), Some("24.10.0"));
        assert!(d.has_update);
        assert_eq!(d.relation, "newer");
        assert_eq!(d.state, "decidable");
        assert!(!d.confirm_required);
        assert!(d.reason.contains("新版本 v25.12.0"));
    }

    #[test]
    fn decide_downgrade_is_not_an_update() {
        let t = v("24.10.0");
        let d = decide_with(Some(&t), &local_of("25.12.0"), Some("t"), Some("25.12.0"));
        assert!(!d.has_update);
        assert_eq!(d.relation, "older");
        // 降级才需要二次确认
        assert!(d.confirm_required);
        assert!(d.reason.contains("不执行降级"));
    }

    #[test]
    fn decide_up_to_date() {
        let t = v("25.12.0");
        let d = decide_with(
            Some(&t),
            &local_of("25.12.0"),
            Some("2026-01-01T00:00:00Z"),
            Some("25.12.0"),
        );
        assert!(!d.has_update);
        assert_eq!(d.relation, "same");
        assert!(d.reason.contains("已是最新"));
        // 同版本不引导普通升级，重装只提「强制升级」
        assert!(d.reason.contains("同版本"));
        assert!(d.reason.contains("强制升级"));
        assert!(!d.reason.contains("新版本"));
    }

    #[test]
    fn decide_first_run_same_version_is_not_an_update() {
        // 默认的 last_upgrade_version '0' 不能凭空制造出“有更新”
        let t = v("25.12.0");
        let d = decide_with(Some(&t), &local_of("25.12.0"), Some("0"), Some("0"));
        assert!(!d.has_update);
        assert_eq!(d.relation, "same");
    }

    #[test]
    fn decide_recompiled_same_version_is_not_an_update() {
        // 同版本号但 Release 被重新构建：仍不是「有更新」，重装要走强制升级
        let t = v("25.12.0");
        let local = local_of("25.12.0");
        let local_ver = local.comparable(Some(&t));
        let d = decide(
            Some("2026-01-01T00:00:00Z"),
            Some("25.12.0"),
            "2026-02-02T00:00:00Z",
            Some(&t),
            &local,
            local_ver.as_ref(),
        );
        assert!(!d.has_update, "同版本重新构建不应被当成新版本");
        assert_eq!(d.relation, "same");
        assert!(d.reason.contains("重新构建"));
        assert!(d.reason.contains("强制升级"));
    }

    #[test]
    fn decide_snapshot_device_is_allowed_not_blocked() {
        // SNAPSHOT 固件确实取不到版本号：放行，但状态必须是「无法比较」
        let t = v("25.12.0");
        let d = decide_with(Some(&t), &local_of("SNAPSHOT"), Some("t"), Some("0"));
        assert!(d.has_update, "无法比较不应阻断升级");
        assert_eq!(d.relation, "unknown");
        assert_eq!(d.state, "incomparable");
        assert!(!d.confirm_required);
        assert!(d.reason.contains("没有可比较的版本号"));
        // 必须说明其余安全闸仍然生效，且不再是「请确认后升级」
        assert!(d.reason.contains("仍然生效"));
        assert!(!d.reason.contains("请确认后升级"));
    }

    #[test]
    fn decide_self_build_by_build_time() {
        // 自用 SNAPSHOT 构建：版本从构建时间推断，结论可用但要标明弱依据
        let t = v("26.11.08");
        let d = decide_with(
            Some(&t),
            &local_by_build_date(2026, 11, 7),
            Some("old"),
            Some("0"),
        );
        assert!(d.has_update);
        assert_eq!(d.relation, "newer");
        assert_eq!(d.state, "suspect");
        assert!(d.reason.contains("26.11.07"));
        assert!(d.reason.contains("依据"));
    }

    #[test]
    fn decide_build_time_vs_non_date_release_is_incomparable() {
        // 构建时间只能和日期型标签比较；遇到 24.10.0 这类普通发布号必须放弃比较，
        // 否则 26.11.07 > 24.10.0 会被当成当前版本更高而判定降级、挡住升级。
        let t = v("24.10.0");
        let d = decide_with(
            Some(&t),
            &local_by_build_date(2026, 11, 7),
            Some("old"),
            Some("0"),
        );
        assert!(d.has_update, "不可比时应放行升级");
        assert_eq!(d.relation, "unknown");
        assert_eq!(d.state, "incomparable");
        assert!(!d.confirm_required);
        assert!(d.reason.contains("没有可比较的版本号"));
    }

    #[test]
    fn decide_self_build_without_version() {
        let d = decide_with(None, &local_of("自用构建"), Some("old"), Some("0"));
        assert!(d.has_update);
        assert_eq!(d.relation, "unknown");
        assert!(d.reason.contains("发布时间"));
    }

    #[test]
    fn proxy_normalisation() {
        assert_eq!(
            normalize_proxy("https://gh.acg2.mom"),
            "https://gh.acg2.mom/"
        );
        assert_eq!(
            normalize_proxy(" https://gh.acg2.mom// "),
            "https://gh.acg2.mom/"
        );
        assert_eq!(normalize_proxy("https://"), "");
        assert_eq!(normalize_proxy("gh.acg2.mom"), "");
        assert_eq!(normalize_proxy(""), "");
        assert_eq!(
            prefixed("https://gh.acg2.mom", "https://github.com/a/b"),
            "https://gh.acg2.mom/https://github.com/a/b"
        );
    }

    #[test]
    fn mirror_list_is_usable() {
        assert_eq!(
            MIRRORS[0], DEFAULT_PROXY,
            "gh.acg2.mom must stay the default"
        );
        assert!(
            MIRRORS.len() >= 5,
            "keep several verified mirrors available"
        );
        let mut seen: Vec<String> = Vec::new();
        for m in MIRRORS {
            assert_eq!(
                normalize_proxy(m).as_str(),
                *m,
                "mirror must already be normalised: {m}"
            );
            assert!(!seen.contains(&m.to_string()), "duplicate mirror: {m}");
            seen.push(m.to_string());
        }
        assert!(
            !MIRRORS.iter().any(|m| m.contains("moeyy.xyz")),
            "dead mirror must be gone"
        );
    }

    #[test]
    fn token_only_for_api_host() {
        assert!(host_of("https://api.github.com/repos/a/b").eq_ignore_ascii_case("api.github.com"));
        assert!(!host_of("https://gh.acg2.mom/https://api.github.com/x")
            .eq_ignore_ascii_case("api.github.com"));
        assert!(
            !host_of("https://evil.example/api.github.com").eq_ignore_ascii_case("api.github.com")
        );
    }

    #[test]
    fn utc_local() {
        assert_eq!(utc_to_local("2026-11-07T09:00:00Z"), "2026-11-07 17:00:00");
    }

    #[test]
    fn asset_match() {
        let parsed = parse(
            r#"[{"name":"sha256sums","size":100},
                {"name":"immortalwrt-25.12.0-x86-64-generic-ext4-combined-efi.img.gz","size":80000000}]"#,
        ).unwrap();
        let assets = parsed.as_arr().unwrap();
        let re = Regex::new(r"combined-efi.*\.img\.gz").unwrap();
        assert!(name_matches(&assets[1], &re, 0));
        assert!(!name_matches(&assets[0], &re, 0));
        // 运行时构造的非法模式：clippy 的 `invalid_regex` 只检查
        // 字面量，而这里刻意做的是反向测试。
        let broken = String::from("[");
        assert!(Regex::new(&broken).is_err());
    }

    #[test]
    fn asset_size_floor() {
        let v = parse(
            r#"[{"name":"H5000M-Config.txt","size":345434},
                {"name":"H5000M-sysupgrade.bin","size":77189386},
                {"name":"manifest","size":12534}]"#,
        )
        .unwrap();
        let assets = v.as_arr().unwrap();
        let re = Regex::new(r"^H5000M-.*").unwrap();
        assert!(name_matches(&assets[1], &re, 15 * 1024 * 1024));
        assert!(!name_matches(&assets[0], &re, 15 * 1024 * 1024));
        assert!(name_matches(&assets[0], &re, 0));
        assert!(!name_matches(&assets[1], &re, 500 * 1024 * 1024));
    }

    #[test]
    fn latest_tag_filter() {
        let parsed = parse(
            r#"[{"tag_name":"自用-26.11.07","assets":[{"name":"H5000M-NETMONITOR-sysupgrade.bin"}]},
                {"tag_name":"MT5700-26.11.07","assets":[{"name":"H5000M-MT5700-sysupgrade.bin"}]},
                {"tag_name":"NetWiz-26.11.06","assets":[{"name":"X86-combined.img.gz"}]}]"#,
        )
        .unwrap();
        let f = Some(Regex::new("^自用").unwrap());
        let c = release_candidates(&parsed, true, "latest", &f);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "自用-26.11.07");

        let none = Some(Regex::new("^AP3000M").unwrap());
        assert!(release_candidates(&parsed, true, "latest", &none).is_empty());

        let all = release_candidates(&parsed, true, "latest", &None);
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn human() {
        assert_eq!(human_size(52_428_800), "50.0 MB");
        assert_eq!(human_size(512), "512 B");
    }
}
