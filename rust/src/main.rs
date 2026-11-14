// online-upgrade —— OpenWrt/ImmortalWrt 固件在线升级后端。
// 取代原有的 shell 实现，通过 JSON 与 LuCI 前端通信。

mod actions;
mod artifacts;
mod cache;
mod compat;
mod compatstate;
mod device;
mod diagnostics;
mod downloader;
mod firmware;
mod ghapi;
mod github;
mod hash;
mod http;
mod json;
mod localver;
mod lock;
mod model;
mod network;
mod releases;
mod selfupdate;
mod states;
mod status;
mod taskctl;
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
    eprintln!(
        "usage: online-upgrade <check|upgrade [--force]|download|pause|resume|cancel|tasks|task-delete <id> [--force]|backup|restore <file>|status|clear-log|probe|plugin-info|plugin-check|plugin-update|states|refresh [--only <model>] [--force]|device|probe-repo <url>|netcheck|clear-cache>"
    );
    2
}

// ---- 新增只读/刷新子命令的公共参数解析 ----
fn arg_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == key {
            return it.next().map(|s| s.as_str());
        }
    }
    None
}

fn ok_json(pairs: Vec<(&str, Value)>) -> String {
    Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()).dump()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("check");

    // 会修改状态的模式共用同一把进程锁：rpcd 以 detached 方式
    // 启动每次调用，若没有该锁，两次点击可能同时写入同一份临时固件
    // 和同一个状态文件。未抢到锁的进程直接退出，不会写状态文件，
    // 否则会覆盖真正持有该操作锁的进程所上报的进度。
    // （check / upgrade / download / backup / restore）
    let needs_lock = matches!(
        mode,
        "check" | "upgrade" | "download" | "backup" | "restore" | "plugin-check" | "plugin-update"
    );
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
            // probe --repo owner/repo [--tag latest|latest@正则|tag]
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
        // 下载任务控制命令。这些命令**不能**获取操作锁：
        // 它们只写入一个小的控制文件，由 detached 下载进程轮询读取，
        // 因此必须在下载运行期间保持可调用。
        "pause" | "resume" | "cancel" => {
            let cmd = match mode {
                "pause" => taskctl::Command::Pause,
                "resume" => taskctl::Command::Resume,
                _ => taskctl::Command::Cancel,
            };
            match taskctl::send_command(cmd) {
                Ok(()) => {
                    println!(
                        "{}",
                        ok_json(vec![
                            ("ok", Value::Bool(true)),
                            ("command", Value::str(mode)),
                        ])
                    );
                    0
                }
                Err(e) => {
                    println!(
                        "{}",
                        ok_json(vec![("ok", Value::Bool(false)), ("error", Value::str(&e))])
                    );
                    1
                }
            }
        }
        "tasks" => {
            let store = taskctl::TaskStore::open();
            // 没有 running/paused 任务时复位残留阶段（例如进程被强杀后）。
            let active = store
                .tasks()
                .iter()
                .any(|t| matches!(t.state.as_str(), taskctl::ST_RUNNING | taskctl::ST_PAUSED));
            if !active {
                status::clear_task_stage();
            }
            println!("{}", store.to_json().dump());
            0
        }
        "task-delete" => match args.get(2) {
            Some(id) if taskctl::valid_task_id(id) => {
                let force = args.iter().any(|a| a == "--force");
                let mut store = taskctl::TaskStore::open();
                let res = if force {
                    store.remove_any(id)
                } else {
                    store.remove(id)
                };
                match res {
                    Ok(t) => {
                        // 只有删除固件任务才需要回收固件产物；删除插件
                        // 任务不能顺带清掉仍在续传/已校验的固件文件。
                        let removed = if t.kind == taskctl::KIND_FIRMWARE {
                            taskctl::purge_orphan_artifacts(&store)
                        } else {
                            0
                        };
                        // 删掉最后一个任务后，把状态文件从「下载中/已下载」复位。
                        let active = store
                            .tasks()
                            .iter()
                            .any(|x| matches!(x.state.as_str(), taskctl::ST_RUNNING | taskctl::ST_PAUSED));
                        if !active {
                            status::clear_task_stage();
                        }
                        println!(
                            "{}",
                            ok_json(vec![
                                ("ok", Value::Bool(true)),
                                ("deleted", Value::str(&t.id)),
                                ("files_removed", Value::num(removed as u64)),
                            ])
                        );
                        0
                    }
                    Err(e) => {
                        println!(
                            "{}",
                            ok_json(vec![("ok", Value::Bool(false)), ("error", Value::str(&e))])
                        );
                        1
                    }
                }
            }
            _ => {
                println!(
                    "{}",
                    ok_json(vec![
                        ("ok", Value::Bool(false)),
                        ("error", Value::str("任务 ID 不合法或缺失")),
                    ])
                );
                2
            }
        },
        "status" => {
            println!("{}", status::read().dump());
            0
        }
        "clear-log" => {
            status::clear_log();
            println!(
                "{}",
                Value::Obj(vec![("ok".into(), Value::Bool(true))]).dump()
            );
            0
        }
        // 插件自身版本检测与原地更新。两者都写独立的插件状态文件，
        // 不会覆盖固件任务的进度；plugin-update 仍然复用同一把操作锁，
        // 避免与固件升级同时改动 /usr/bin/online-upgrade。
        "plugin-info" => {
            println!("{}", selfupdate::info().dump());
            0
        }
        "plugin-check" => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(selfupdate::run_check()),
        "plugin-update" => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(selfupdate::run_update()),

        // ---- 四套独立状态模型 / 统一诊断层（新增，均为只读或独立刷新） ----
        // 这些命令不写 /tmp/online-upgrade-status.json，不占用升级流程的
        // 全局操作锁，因此不会干扰 check / download / upgrade 的进度上报。
        "states" => {
            println!("{}", states::read_only().dump());
            0
        }
        "device" => {
            println!("{}", states::device_json().dump());
            0
        }
        "refresh" => {
            let only = arg_value(&args, "--only");
            let force = args.iter().any(|a| a == "--force");
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let res = states::refresh_all(only, force).await;
                    println!("{}", res.dump());
                });
            0
        }
        "probe-repo" => match args.get(2) {
            Some(u) => {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        let res = states::probe_repo(u).await;
                        println!("{}", res.dump());
                    });
                0
            }
            None => usage(),
        },
        "netcheck" => {
            let force = args.iter().any(|a| a == "--force");
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let hub = ghapi::Hub::new(ghapi::ApiCfg::from_uci());
                    let r = network::probe(&hub, force).await;
                    println!("{}", r.to_json().dump());
                });
            0
        }
        "clear-cache" => {
            let n = cache::invalidate_all();
            println!(
                "{}",
                ok_json(vec![
                    ("ok", Value::Bool(true)),
                    ("cleared", Value::num(n as u64)),
                ])
            );
            0
        }
        _ => usage(),
    };
    // process::exit 不会执行析构函数，因此必须显式释放锁：
    // 残留的锁文件只能通过陈旧 pid 检测回收，若 pid 被系统复用，
    // 该锁就会被永远视为“存活”状态。
    drop(guard);
    std::process::exit(code);
}
