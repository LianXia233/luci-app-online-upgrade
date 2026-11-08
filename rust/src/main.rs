// online-upgrade — OpenWrt/ImmortalWrt firmware online upgrade backend.
// Replaces the original shell implementation; talks JSON to the LuCI frontend.

mod compat;
mod downloader;
mod github;
mod hash;
mod http;
mod json;
mod lock;
mod localver;
mod status;
mod uci;
mod upgrade;
mod util;
mod version;

use json::Value;

async fn cmd_check() -> i32 {
    status::trim_log();
    let client = http::client();
    status::set("checking", 5, "正在检查固件更新...");
    let result = github::check(&client).await;
    if matches!(result.get("ok"), Some(Value::Bool(true))) {
        status::set_result("check_done", 100, "检测完成", result);
        0
    } else {
        let err = result
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("未知错误")
            .to_string();
        status::set_error("check_failed", 100, "检测失败", &err);
        status::log(&format!("[check] {err}"));
        1
    }
}

fn usage() -> i32 {
    eprintln!("usage: online-upgrade <check|upgrade [--force]|download|backup|restore <file>|status|clear-log>");
    2
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("check");

    // Mutating modes share one process lock: rpcd spawns each invocation
    // detached, so without it two clicks can write the same temp firmware and
    // the same status file. The loser exits without touching the status file,
    // otherwise it would overwrite the progress of the process that owns the
    // operation.
    let needs_lock = matches!(mode, "check" | "upgrade" | "download" | "backup" | "restore");
    let guard = if needs_lock {
        match lock::acquire(mode) {
            Ok(g) => Some(g),
            Err(e) => {
                status::log(&format!("[lock] {e}"));
                eprintln!("[lock] {e}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    let code = match mode {
        "check" => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(cmd_check()),
        "upgrade" => {
            let force = args.iter().any(|a| a == "--force");
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap()
                .block_on(upgrade::run(force))
        }
        "backup" => util::backup(),
        "download" => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
            .block_on(upgrade::download_only()),
        "probe" => {
            // probe --repo owner/repo [--tag latest|latest@regex|tag]
            let mut repo = String::new();
            let mut tag = String::from("latest");
            let mut it = args.iter().skip(2);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--repo" => repo = it.next().cloned().unwrap_or_default(),
                    "--tag" => tag = it.next().cloned().unwrap_or_else(|| "latest".into()),
                    _ => {}
                }
            }
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let client = http::client();
                    let res = github::probe(&client, &repo, &tag).await;
                    println!("{}", res.dump());
                });
            0
        }
        "restore" => match args.get(2) {
            Some(p) => util::restore(p),
            None => usage(),
        },
        "status" => {
            println!("{}", status::read().dump());
            0
        }
        "clear-log" => {
            status::clear_log();
            println!("{}", Value::Obj(vec![("ok".into(), Value::Bool(true))]).dump());
            0
        }
        _ => usage(),
    };
    // process::exit skips destructors, so release the lock explicitly: a
    // surviving lock file is only reclaimed through the stale-pid path, and a
    // recycled pid would then keep the lock "alive" forever.
    drop(guard);
    std::process::exit(code);
}
