// Standalone backup / restore subcommands (pure sync — run outside the tokio runtime).

use crate::json::Value;
use crate::status;
use crate::logline;
use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn print_result(ok: bool, key: &str, msg: &str) {
    println!(
        "{}",
        Value::Obj(vec![
            ("ok".into(), Value::Bool(ok)),
            (key.into(), Value::str(msg)),
        ])
        .dump()
    );
}

/// `online-upgrade backup` — create a config backup in /tmp and copy to /root.
pub fn backup() -> i32 {
    let stamp = run("date", &["+%Y%m%d-%H%M%S"]).unwrap_or_else(|| "unknown".into());
    let tmp = format!("/tmp/pre-upgrade-backup-{stamp}.tar.gz");
    let root = format!("/root/pre-upgrade-backup-{stamp}.tar.gz");
    status::set("backing_up", 50, "正在创建配置备份...");
    logline!("[backup] {root}");

    let rc = Command::new("sysupgrade").args(["-b", &tmp]).status();
    let ok = rc.map(|s| s.success()).unwrap_or(false)
        && std::fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false);
    if !ok {
        status::set_error("failed", 100, "备份失败", "sysupgrade -b 执行失败");
        print_result(false, "error", "sysupgrade -b 执行失败");
        return 1;
    }
    // The /root copy is what survives a /tmp cleanup and what README promises
    // as the restore source: a failed copy must not be reported as success.
    if let Err(e) = std::fs::copy(&tmp, &root) {
        let msg = format!("备份已生成于 {tmp},但复制到 {root} 失败: {e}");
        status::set_error("failed", 100, "备份保存失败", &msg);
        logline!("[backup] {msg}");
        print_result(false, "error", &msg);
        return 1;
    }
    let files = run("sh", &["-c", &format!("tar tzf {tmp} 2>/dev/null | wc -l")]).unwrap_or_default();
    let size = std::fs::metadata(&root).map(|m| m.len()).unwrap_or(0);
    logline!("[backup] 完成 {root} ({files} 个文件)");
    status::set_result(
        "backup_done",
        100,
        "备份完成",
        Value::Obj(vec![
            ("path".into(), Value::str(&root)),
            ("size".into(), Value::num(size)),
            ("size_human".into(), Value::str(&crate::github::human_size(size))),
            ("files".into(), Value::str(&files)),
        ]),
    );
    println!(
        "{}",
        Value::Obj(vec![
            ("ok".into(), Value::Bool(true)),
            ("path".into(), Value::str(&root)),
            ("size".into(), Value::num(size)),
            ("size_human".into(), Value::str(&crate::github::human_size(size))),
            ("files".into(), Value::str(&files)),
        ])
        .dump()
    );
    0
}

/// `online-upgrade restore <file>` — verify gzip magic then extract to /.
pub fn restore(path: &str) -> i32 {
    if !path.starts_with("/root/pre-upgrade-backup-") && !path.starts_with("/tmp/") {
        print_result(false, "error", "不允许的路径");
        return 1;
    }
    logline!("[restore] {path}");

    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.len() < 2 || bytes[0] != 0x1f || bytes[1] != 0x8b {
                status::set_error("failed", 100, "恢复失败", "不是有效的 gzip 备份文件");
                print_result(false, "error", "不是有效的 gzip 备份文件");
                return 1;
            }
        }
        Err(e) => {
            let msg = format!("无法读取备份文件: {e}");
            status::set_error("failed", 100, "恢复失败", &msg);
            print_result(false, "error", &msg);
            return 1;
        }
    }

    let ok = Command::new("tar")
        .args(["-xzf", path, "-C", "/"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        status::set("restore_done", 100, "配置恢复完成,建议重启路由器");
        print_result(true, "message", "配置已恢复,建议重启路由器使配置生效");
        0
    } else {
        status::set_error("failed", 100, "恢复失败", "tar 解压失败,备份文件可能已损坏");
        print_result(false, "error", "tar 解压失败,备份文件可能已损坏");
        1
    }
}
