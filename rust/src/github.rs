// GitHub Release check: fetch API (with proxy fallback + rate-limit retry),
// match the firmware asset, compare versions.

use crate::json::{parse, Value};
use crate::status;
use crate::logline;
use crate::uci;
use regex::Regex;
use std::time::Duration;

// NOTE: no hardcoded update source here on purpose — repo / tag / pattern all
// come from UCI (`online-upgrade.settings.*`). Empty values produce explicit
// "please configure" errors instead of silently using a built-in repo.
pub const DEFAULT_TAG: &str = "latest"; // neutral mode default, not a source
pub const DEFAULT_PROXY: &str = "https://gh.acg2.mom/";
// only used in explicit-tag mode for backwards compatibility with v1 configs
const FALLBACK_PATTERN: &str = r"combined.*\.img\.gz$";

pub struct Cfg {
    pub repo: String,
    pub tag: String,
    pub proxy: String,
    pub pattern: String,
    pub token: Option<String>,
}

pub fn load_cfg() -> Cfg {
    Cfg {
        repo: uci::get_or("repo", ""),
        tag: uci::get_or("tag", DEFAULT_TAG),
        proxy: uci::get_or("proxy", DEFAULT_PROXY),
        pattern: uci::get_or("firmware_pattern", ""),
        token: uci::get("github_token"),
    }
}

async fn api_get(client: &reqwest::Client, url: &str, token: Option<&str>) -> Result<(u16, String), String> {
    let mut req = client.get(url).timeout(Duration::from_secs(30));
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let code = resp.status().as_u16();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    Ok((code, body))
}

/// Fetch a GitHub API URL with the full resilience chain:
/// direct → proxy-prefix fallback (connection failure) → 403 rate-limit retry.
async fn api_fetch(client: &reqwest::Client, url: &str, cfg: &Cfg) -> Result<(u16, String), String> {
    let (mut code, mut body) = match api_get(client, url, cfg.token.as_deref()).await {
        Ok(x) => x,
        Err(e) => {
            // TLS/DNS/连接层失败 → 走代理重试
            logline!("[check] direct failed: {e}, trying proxy");
            let proxied = format!("{}{}", cfg.proxy, url);
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

/// Candidate releases: `latest` mode yields the most recent releases (already
/// sorted by GitHub, newest first) — optionally filtered by a tag-name regex
/// given as `latest@<regex>`; explicit tag mode yields a single entry.
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

/// Full check flow. Returns the result object (also written into the status file
/// by the caller).
pub async fn check(client: &reqwest::Client) -> Value {
    let cfg = load_cfg();
    if cfg.repo.is_empty() {
        return check_error(&cfg, "未配置 GitHub 仓库,请先在「仓库配置」中填写(owner/repo)");
    }
    if cfg.pattern.is_empty() {
        return check_error(&cfg, "未配置固件匹配模式,请先在「仓库配置」中填写(用于匹配固件文件名的正则)");
    }
    // `latest` tracks the newest release; `latest@<regex>` restricts candidates
    // to releases whose tag name matches the regex (e.g. `latest@^自用`).
    let use_latest = cfg.tag.eq_ignore_ascii_case("latest") || cfg.tag.starts_with("latest@");
    let tag_filter = cfg
        .tag
        .strip_prefix("latest@")
        .map(|p| Regex::new(p))
        .transpose()
        .ok()
        .flatten();
    let api = if use_latest {
        format!("https://api.github.com/repos/{}/releases?per_page=30", cfg.repo)
    } else {
        format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            cfg.repo, cfg.tag
        )
    };

    status::set("checking", 10, "正在获取 Release 信息...");
    logline!("[check] repo={} tag={}", cfg.repo, cfg.tag);

    let body = match api_fetch(client, &api, &cfg).await {
        Ok((_, body)) => body,
        Err(e) => return check_error(&cfg, &e),
    };

    status::set("checking", 60, "正在解析固件列表...");
    let parsed = match parse(&body) {
        Ok(v) => v,
        Err(e) => return check_error(&cfg, &format!("JSON 解析失败: {e}")),
    };

    // Pick the newest release (in `latest` mode: newest first) whose assets
    // contain a file matching the firmware pattern AND the minimum size floor
    // (online-upgrade.settings.min_size_mb, default 15 MB, 0 = no floor).
    let min_size_mb: u64 = uci::get("min_size_mb")
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let min_bytes = min_size_mb.saturating_mul(1024 * 1024);
    let re = Regex::new(&cfg.pattern).map_err(|e| e.to_string()).ok();
    let fallback_re = if use_latest {
        None // x86 fallback makes no sense in latest mode
    } else {
        Regex::new(FALLBACK_PATTERN).ok()
    };

    let mut resolved_tag: Option<String> = None;
    let mut file_name: Option<String> = None;
    let mut asset_ref: Option<&Value> = None;
    for (tag_name, release) in release_candidates(&parsed, use_latest, &cfg.tag, &tag_filter) {
        let assets = match release.get("assets").and_then(|a| a.as_arr()) {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };
        let hit = re
            .as_ref()
            .and_then(|r| pick_asset(assets, r, min_bytes))
            .or_else(|| {
                fallback_re
                    .as_ref()
                    .and_then(|r| pick_asset(assets, r, min_bytes))
            });
        if let Some(asset) = hit {
            resolved_tag = Some(tag_name);
            file_name = asset
                .get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string());
            asset_ref = Some(asset);
            break;
        }
    }
    let (Some(file_name), Some(resolved_tag), Some(asset)) =
        (file_name, resolved_tag, asset_ref)
    else {
        return check_error(
            &cfg,
            &format!(
                "未找到匹配的固件文件(pattern: {}, 大小≥{} MB{})",
                cfg.pattern,
                min_size_mb,
                if use_latest { ", 最近 30 个 Release" } else { "" }
            ),
        );
    };
    let _ = use_latest;
    let size = asset.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
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

    let fw_version = extract_fw_version(&file_name);
    let (cur_id, cur_release, cur_revision) = read_distrib();

    let last_ts = uci::get("last_upgrade_ts");
    let last_version = uci::get("last_upgrade_version");

    let (has_update, reason) = decide(
        last_ts.as_deref(),
        last_version.as_deref(),
        &updated,
        &fw_version,
        &cur_release,
    );

    let mut current = Vec::new();
    current.push(("id".into(), Value::str(&cur_id)));
    current.push(("release".into(), Value::str(&cur_release)));
    current.push(("revision".into(), Value::str(&cur_revision)));

    let mut latest = Vec::new();
    latest.push(("name".into(), Value::str(&file_name)));
    latest.push(("size".into(), Value::num(size)));
    latest.push(("size_human".into(), Value::str(&human_size(size))));
    latest.push(("updated".into(), Value::str(&updated)));
    latest.push(("updated_local".into(), Value::str(&utc_to_local(&updated))));
    latest.push(("version".into(), Value::str(&fw_version)));
    latest.push(("url".into(), Value::str(&url)));

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
        ("has_update".into(), Value::Bool(has_update)),
        ("reason".into(), Value::str(&reason)),
    ])
}


/// `probe` subcommand: list the assets of the newest release of an arbitrary
/// repo (used by the web UI to let users pick a firmware file interactively).
/// Reads repo/tag from arguments, NOT from UCI — fully source-agnostic.
pub async fn probe(client: &reqwest::Client, repo: &str, tag: &str) -> Value {
    let cur = load_cfg();
    let cfg = Cfg {
        repo: repo.to_string(),
        tag: if tag.is_empty() { "latest".into() } else { tag.to_string() },
        proxy: cur.proxy,
        pattern: String::new(),
        token: cur.token,
    };
    if cfg.repo.is_empty() || !cfg.repo.contains('/') {
        return Value::Obj(vec![
            ("ok".into(), Value::Bool(false)),
            ("error".into(), Value::str("仓库格式不正确(应为 owner/repo)")),
        ]);
    }
    let use_latest = cfg.tag.eq_ignore_ascii_case("latest") || cfg.tag.starts_with("latest@");
    let tag_filter = cfg
        .tag
        .strip_prefix("latest@")
        .map(|p| Regex::new(p))
        .transpose()
        .ok()
        .flatten();
    let api = if use_latest {
        format!("https://api.github.com/repos/{}/releases?per_page=30", cfg.repo)
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

    for (tag_name, release) in release_candidates(&parsed, use_latest, &cfg.tag, &tag_filter) {
        if let Some(assets) = release.get("assets").and_then(|a| a.as_arr()) {
            if assets.is_empty() {
                continue;
            }
            let list: Vec<Value> = assets
                .iter()
                .take(80)
                .map(|a| {
                    let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let size = a.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
                    Value::Obj(vec![
                        ("name".into(), Value::str(&name)),
                        ("size".into(), Value::num(size)),
                        ("size_human".into(), Value::str(&human_size(size))),
                    ])
                })
                .collect();
            return Value::Obj(vec![
                ("ok".into(), Value::Bool(true)),
                ("resolved_tag".into(), Value::str(&tag_name)),
                ("assets".into(), Value::Arr(list)),
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

/// First asset matching `re` whose size is at least `min_bytes`
/// (min_bytes == 0 disables the size floor).
fn pick_asset<'a>(assets: &'a [Value], re: &Regex, min_bytes: u64) -> Option<&'a Value> {
    assets.iter().find(|a| {
        let name_ok = a
            .get("name")
            .and_then(|n| n.as_str())
            .map(|n| re.is_match(n))
            .unwrap_or(false);
        if !name_ok {
            return false;
        }
        let size = a.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
        size >= min_bytes
    })
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

/// Extract "X.Y.Z" from a firmware filename, e.g. immortalwrt-25.12.0-x86... → "25.12.0"
pub fn extract_fw_version(filename: &str) -> String {
    let re = Regex::new(r"\d+\.\d+\.\d+").unwrap();
    re.find(filename).map(|m| m.as_str().to_string()).unwrap_or_default()
}

/// "25.12.0" → (25,12,0); non-numeric (e.g. "SNAPSHOT") → (0,0,0)
fn ver_tuple(s: &str) -> (u64, u64, u64) {
    let parts: Vec<u64> = s
        .split('.')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0))
        .collect();
    (
        *parts.first().unwrap_or(&0),
        *parts.get(1).unwrap_or(&0),
        *parts.get(2).unwrap_or(&0),
    )
}

/// Version/update decision — port of the shell script's logic.
fn decide(
    last_ts: Option<&str>,
    last_version: Option<&str>,
    asset_updated: &str,
    fw_version: &str,
    current_release: &str,
) -> (bool, String) {
    if last_ts.is_none() && last_version.is_none() {
        return (true, "首次检测".into());
    }
    if !fw_version.is_empty() && current_release != fw_version {
        let cur = ver_tuple(current_release);
        let new = ver_tuple(fw_version);
        if cur < new {
            return (
                true,
                format!("新版固件 v{fw_version}(当前 v{current_release})"),
            );
        }
        if let Some(lv) = last_version {
            if lv != fw_version {
                return (true, format!("固件重新编译(v{fw_version})"));
            }
        }
        return (false, format!("已是最新(v{current_release})"));
    }
    if last_ts.map(|t| t != asset_updated).unwrap_or(true) && !asset_updated.is_empty() {
        return (true, format!("固件重新编译({})", utc_to_local(asset_updated)));
    }
    (false, "已是最新".into())
}

// ---- time helpers (UTC+8, matching the previous hardcoded behaviour) ----

/// "2026-11-07T09:00:00Z" → "2026-11-07 17:00:00"
pub fn utc_to_local(iso: &str) -> String {
    let b = iso.as_bytes();
    if b.len() < 19 {
        return iso.to_string();
    }
    let num = |a: usize, z: usize| -> i64 {
        iso.get(a..z).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0)
    };
    let (y, mo, d) = (num(0, 4), num(5, 7), num(8, 10));
    let (h, mi, s) = (num(11, 13), num(14, 16), num(17, 19));
    if y == 0 {
        return iso.to_string();
    }
    let days = days_from_civil(y, mo, d);
    let epoch = days * 86400 + h * 3600 + mi * 60 + s + 8 * 3600;
    let (y2, mo2, d2) = civil_from_days(epoch.div_euclid(86400));
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

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
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

    #[test]
    fn version_extract() {
        assert_eq!(extract_fw_version("immortalwrt-25.12.0-x86-64-generic-ext4-combined-efi.img.gz"), "25.12.0");
        assert_eq!(extract_fw_version("immortalwrt-23.05.3-mediatek-filogic-hiveton_h5000m-squashfs-sysupgrade.bin"), "23.05.3");
        assert_eq!(extract_fw_version("no-version-here.img.gz"), "");
    }

    #[test]
    fn ver_tuple_basic() {
        assert_eq!(ver_tuple("25.12.0"), (25, 12, 0));
        assert_eq!(ver_tuple("SNAPSHOT"), (0, 0, 0));
        assert_eq!(ver_tuple("23.05.3"), (23, 5, 3));
    }

    #[test]
    fn decide_first_run() {
        let (up, why) = decide(None, None, "2026-01-01T00:00:00Z", "25.12.0", "24.10.0");
        assert!(up);
        assert_eq!(why, "首次检测");
    }

    #[test]
    fn decide_newer() {
        let (up, why) = decide(Some("t"), Some("24.10.0"), "2026-01-01T00:00:00Z", "25.12.0", "24.10.0");
        assert!(up);
        assert!(why.contains("新版固件 v25.12.0"));
    }

    #[test]
    fn decide_up_to_date() {
        let (up, why) = decide(Some("2026-01-01T00:00:00Z"), Some("25.12.0"), "2026-01-01T00:00:00Z", "25.12.0", "25.12.0");
        assert!(!up);
        assert!(why.contains("已是最新"));
    }

    #[test]
    fn decide_recompiled_same_version() {
        // 版本号相同但时间戳不同 → 重新编译
        let (up, why) = decide(Some("2026-01-01T00:00:00Z"), Some("25.12.0"), "2026-02-02T00:00:00Z", "25.12.0", "25.12.0");
        assert!(up);
        assert!(why.contains("重新编译"));
    }

    #[test]
    fn decide_snapshot_device() {
        // 当前 SNAPSHOT(0,0,0),远端 25.12.0 → 视为新版
        let (up, _) = decide(Some("t"), Some("0"), "2026-01-01T00:00:00Z", "25.12.0", "SNAPSHOT");
        assert!(up);
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
        let hit = pick_asset(assets, &re, 0).unwrap();
        assert_eq!(
            hit.get("name").unwrap().as_str(),
            Some("immortalwrt-25.12.0-x86-64-generic-ext4-combined-efi.img.gz")
        );
        // invalid regex → no match possible (check() reports a config error)
        assert!(Regex::new("[").is_err());
    }

    #[test]
    #[test]
    fn pick_asset_size_floor() {
        let v = parse(
            r#"[{"name":"H5000M-Config.txt","size":345434},
                {"name":"H5000M-sysupgrade.bin","size":77189386},
                {"name":"manifest","size":12534}]"#,
        )
        .unwrap();
        let assets = v.as_arr().unwrap();
        let re = Regex::new(r"^H5000M-.*").unwrap();

        // 15MB floor: skips the small H5000M-named config, picks the firmware
        let hit = pick_asset(assets, &re, 15 * 1024 * 1024).unwrap();
        assert_eq!(hit.get("name").unwrap().as_str(), Some("H5000M-sysupgrade.bin"));

        // floor disabled: first name match wins (the small config)
        let hit = pick_asset(assets, &re, 0).unwrap();
        assert_eq!(hit.get("name").unwrap().as_str(), Some("H5000M-Config.txt"));

        // impossible floor: nothing passes
        assert!(pick_asset(assets, &re, 500 * 1024 * 1024).is_none());
    }

    #[test]
    fn latest_tag_filter() {
        let parsed = parse(
            r#"[{"tag_name":"自用-26.11.07","assets":[{"name":"H5000M-NETMONITOR-sysupgrade.bin"}]},
                {"tag_name":"MT5700-26.11.07","assets":[{"name":"H5000M-MT5700-sysupgrade.bin"}]},
                {"tag_name":"NetWiz-26.11.06","assets":[{"name":"X86-combined.img.gz"}]}]"#,
        ).unwrap();
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
