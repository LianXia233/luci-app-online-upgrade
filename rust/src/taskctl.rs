// 下载任务生命周期：跨进程控制通道与持久化历史。
//
// rpcd 以 detached 方式启动 `online-upgrade download`，因此来自另一个
// 进程的暂停/继续/取消无法使用进程内通道。这里改用一个小型 JSON
// 控制文件作为带外通道；下载器由一个 Tokio 任务轮询该文件，
// 并把结果状态发布到 `tokio::sync::watch` 通道，
// 由各传输循环等待该状态。历史记录存放在 /root（闪存）中，以便重启后
// 仍然保留；/tmp 中的产物仅在没有已完成任务仍引用它们时才回收
// （已校验的固件会被 `upgrade` 复用）。

use crate::json::{self, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Duration;

/// 由活动下载进程轮询的带外控制文件。
pub const CONTROL_FILE: &str = "/tmp/online-upgrade-control.json";
/// 持久化任务历史（与 /tmp 状态文件不同，重启后仍保留）。
pub const TASKS_FILE: &str = "/root/.online-upgrade-tasks.json";
/// 归属下载任务的固件产物。
pub const FIRMWARE_FILE: &str = "/tmp/firmware.img.gz";
/// 闪存中保留的历史记录条数上限。
pub const MAX_TASKS: usize = 30;

/// 任务状态：正在传输。
pub const ST_RUNNING: &str = "running";
/// 任务状态：已暂停，可续传。
pub const ST_PAUSED: &str = "paused";
/// 任务状态：已完成并通过校验。
pub const ST_DONE: &str = "done";
/// 任务状态：网络或校验失败。
pub const ST_FAILED: &str = "failed";
/// 任务状态：被用户或慢速保护中止。
pub const ST_ABORTED: &str = "aborted";

/// 下载被停止的原因。保留类型信息，使界面与历史记录能够把
/// 慢速中止与普通用户取消区分展示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// 用户点击取消，或删除了正在执行的任务。
    UserCancel(String),
    /// 吞吐量保护触发；携带实测证据。
    Slow {
        /// 面向用户的说明，包含线路与阈值。
        reason: String,
        /// 观察窗口内的平均吞吐量（字节/秒）。
        bps: u64,
        /// 配置的速度下限（字节/秒）。
        min_bps: u64,
        /// 观察窗口长度（秒）。
        window_secs: u64,
    },
}

impl StopReason {
    /// 显示在状态文件、日志和任务历史中的消息。
    pub fn text(&self) -> &str {
        match self {
            StopReason::UserCancel(s) => s,
            StopReason::Slow { reason, .. } => reason,
        }
    }
}

/// 活动下载的运行状态，每个分块 worker 都会观察该状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    /// 正常传输中。
    Running,
    /// 不再领取新分块；进程保持存活并等待继续指令。
    Paused,
    /// 带原因类型的终止（用户取消或慢速保护）。
    Stopped(StopReason),
}

/// 由 `online-upgrade pause|resume|cancel` 写入的命令。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// 继续或开始传输。
    Resume,
    /// 在下一个分块边界暂停传输。
    Pause,
    /// 立即停止传输。
    Cancel,
}

impl Command {
    fn as_str(self) -> &'static str {
        match self {
            Command::Resume => "running",
            Command::Pause => "paused",
            Command::Cancel => "stopped",
        }
    }

    fn parse(s: &str) -> Option<Command> {
        match s {
            "running" => Some(Command::Resume),
            "paused" => Some(Command::Pause),
            "stopped" => Some(Command::Cancel),
            _ => None,
        }
    }
}

/// 慢速保护阈值，全部从 UCI 读取。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlowPolicy {
    /// 可接受的最低吞吐量（字节/秒）；0 表示关闭保护。
    pub min_bps: u64,
    /// 吞吐量需持续低于 `min_bps` 多久才触发停止。
    pub window_secs: u64,
    /// 启动宽限期（TLS 握手 / TCP 慢启动），该时段不参与判定。
    pub grace_secs: u64,
}

impl SlowPolicy {
    /// 构造带范围钳制的策略。`min_kb` 单位为 KiB/s。
    pub fn new(min_kb: u64, window_secs: u64, grace_secs: u64) -> SlowPolicy {
        SlowPolicy {
            min_bps: min_kb.saturating_mul(1024),
            window_secs: window_secs.clamp(3, 300),
            grace_secs: grace_secs.min(300),
        }
    }

    /// 从 `online-upgrade.settings.dl_*` 加载，并使用合理默认值。
    pub fn load() -> SlowPolicy {
        let kb = uci_u32("dl_min_speed_kb", 32, 0, 10240);
        let win = uci_u32("dl_slow_window_sec", 15, 3, 300);
        let grace = uci_u32("dl_slow_grace_sec", 10, 0, 300);
        SlowPolicy::new(kb as u64, win as u64, grace as u64)
    }

    /// 是否应启用慢速保护。
    pub fn enabled(&self) -> bool {
        self.min_bps > 0
    }
}

fn uci_u32(key: &str, def: u32, lo: u32, hi: u32) -> u32 {
    crate::uci::get(key)
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.clamp(lo, hi))
        .unwrap_or(def)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 持久化的下载任务记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// 稳定 ID（`<unix>-<pid>-<seq>`）。
    pub id: String,
    /// 固件资产名称。
    pub name: String,
    /// 下载 URL（可能带镜像前缀）。
    pub url: String,
    /// 取值见 `ST_*` 常量。
    pub state: String,
    /// 预期大小（字节，未知时为 0）。
    pub total: u64,
    /// 已写入字节数。
    pub done: u64,
    /// 最近一次测得的吞吐量（字节/秒）。
    pub speed_bps: u64,
    /// 失败或停止原因，成功时为空。
    pub reason: String,
    /// 创建时间（Unix 时间戳）。
    pub created: u64,
    /// 进入终态的时间（Unix 时间戳），活动期间为 0。
    pub finished: u64,
}

static TASK_SEQ: AtomicU64 = AtomicU64::new(0);

/// 生成在进程内与跨进程都唯一的任务 ID。
pub fn new_task_id() -> String {
    let n = TASK_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}-{:x}", now_secs(), std::process::id(), n)
}

/// 在任何文件操作前校验来自 rpcd 的任务 ID。
pub fn valid_task_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-')
}

impl Task {
    /// 创建一个新的运行中任务。
    pub fn new(name: &str, url: &str, total: u64) -> Task {
        Task {
            id: new_task_id(),
            name: name.to_string(),
            url: url.to_string(),
            state: ST_RUNNING.to_string(),
            total,
            done: 0,
            speed_bps: 0,
            reason: String::new(),
            created: now_secs(),
            finished: 0,
        }
    }

    /// 将任务标记为终态，并记录结果与原因。
    pub fn finish(&mut self, state: &str, reason: &str) {
        self.state = state.to_string();
        self.reason = reason.to_string();
        self.finished = now_secs();
    }

    fn to_value(&self) -> Value {
        Value::Obj(vec![
            ("id".into(), Value::str(&self.id)),
            ("name".into(), Value::str(&self.name)),
            ("url".into(), Value::str(&self.url)),
            ("state".into(), Value::str(&self.state)),
            ("total".into(), Value::num(self.total)),
            ("done".into(), Value::num(self.done)),
            ("speed_bps".into(), Value::num(self.speed_bps)),
            ("reason".into(), Value::str(&self.reason)),
            ("created".into(), Value::num(self.created)),
            ("finished".into(), Value::num(self.finished)),
        ])
    }

    fn from_value(v: &Value) -> Option<Task> {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let id = s("id");
        if id.is_empty() {
            return None;
        }
        Some(Task {
            id,
            name: s("name"),
            url: s("url"),
            state: s("state"),
            total: n("total"),
            done: n("done"),
            speed_bps: n("speed_bps"),
            reason: s("reason"),
            created: n("created"),
            finished: n("finished"),
        })
    }
}

/// 持久化且有上限的任务历史。所有写入均为原子操作（临时文件 + rename）。
pub struct TaskStore {
    path: PathBuf,
    tasks: Vec<Task>,
}

impl TaskStore {
    /// 打开默认历史文件并加载其内容。
    pub fn open() -> TaskStore {
        let mut s = TaskStore::with_path(TASKS_FILE);
        s.load();
        s
    }

    /// 打开指定路径的历史文件（供测试与命令行覆盖使用）。
    pub fn with_path(path: impl Into<PathBuf>) -> TaskStore {
        TaskStore {
            path: path.into(),
            tasks: Vec::new(),
        }
    }

    /// 从磁盘重新加载；文件损坏或缺失时视为空历史。
    pub fn load(&mut self) {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(_) => {
                self.tasks.clear();
                return;
            }
        };
        self.tasks = match json::parse(&text) {
            Ok(v) => v
                .get("tasks")
                .and_then(|x| x.as_arr())
                .map(|arr| arr.iter().filter_map(Task::from_value).collect())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
    }

    /// 原子地持久化历史。
    pub fn save(&self) -> Result<(), String> {
        let arr: Vec<Value> = self.tasks.iter().map(Task::to_value).collect();
        let body = Value::Obj(vec![("tasks".into(), Value::Arr(arr))]).dump();
        atomic_write(&self.path, &body)
    }

    /// 插入或更新任务，仅保留最新的 `MAX_TASKS` 条记录。
    pub fn upsert(&mut self, task: Task) {
        if let Some(existing) = self.tasks.iter_mut().find(|t| t.id == task.id) {
            *existing = task;
        } else {
            self.tasks.push(task);
        }
        self.tasks.sort_by(|a, b| b.created.cmp(&a.created));
        self.tasks.truncate(MAX_TASKS);
    }

    /// 按 ID 获取任务的可变引用。
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    /// 删除任务。运行中或已暂停的任务必须先停止。
    pub fn remove(&mut self, id: &str) -> Result<Task, String> {
        let active = self
            .tasks
            .iter()
            .find(|t| t.id == id)
            .map(|t| matches!(t.state.as_str(), ST_RUNNING | ST_PAUSED))
            .ok_or_else(|| "任务不存在或已被删除".to_string())?;
        if active {
            return Err("任务仍在进行中，请先暂停或取消后再删除".to_string());
        }
        self.remove_any(id)
    }

    /// 无视记录状态直接删除任务。仅当确认所属进程
    /// 已退出时使用（例如崩溃后残留的 `running` 记录）。
    pub fn remove_any(&mut self, id: &str) -> Result<Task, String> {
        let pos = self
            .tasks
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| "任务不存在或已被删除".to_string())?;
        let task = self.tasks.remove(pos);
        self.save()?;
        Ok(task)
    }

    /// 历史记录的只读视图。
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// `tasks` 子命令返回的 JSON 表示。
    pub fn to_json(&self) -> Value {
        Value::Obj(vec![
            ("ok".into(), json::Value::Bool(true)),
            (
                "tasks".into(),
                Value::Arr(self.tasks.iter().map(Task::to_value).collect()),
            ),
        ])
    }
}

/// 共享固件产物是否可以安全回收。
///
/// 已完成并通过校验的下载会被 `upgrade` 复用（见
/// `reuse_download`），因此只要还存在一个 `done` 任务，就保留该文件。
pub fn should_purge(tasks: &[Task]) -> bool {
    !tasks.iter().any(|t| t.state == ST_DONE)
}

/// 删除固定的固件产物集合，但仅当没有 `done` 任务
/// 仍引用它们时执行。返回删除的文件数。
pub fn purge_orphan_artifacts(store: &TaskStore) -> usize {
    if !should_purge(store.tasks()) {
        return 0;
    }
    let paths = [
        FIRMWARE_FILE.to_string(),
        format!("{FIRMWARE_FILE}.ts"),
        format!("{FIRMWARE_FILE}.parts.json"),
    ];
    let mut removed = 0usize;
    for p in &paths {
        if fs::remove_file(p).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn atomic_write(path: &Path, body: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, body).map_err(|e| format!("写入 {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("重命名到 {}: {e}", path.display()))
}

fn read_command(path: &Path) -> Option<(Command, u64)> {
    let text = fs::read_to_string(path).ok()?;
    let v = json::parse(&text).ok()?;
    let cmd = Command::parse(v.get("cmd").and_then(|x| x.as_str())?)?;
    let seq = v.get("seq").and_then(|x| x.as_u64()).unwrap_or(0);
    Some((cmd, seq))
}

fn read_seq(path: &Path) -> u64 {
    read_command(path).map(|(_, s)| s).unwrap_or(0)
}

/// 写入控制命令。`seq` 单调递增，且始终落在 JSON 数字类型（f64）
/// 可精确表示的整数范围内，
/// 因此不会因浮点舍入而丢失变更。
pub fn send_command(cmd: Command) -> Result<(), String> {
    send_command_at(Path::new(CONTROL_FILE), cmd)
}

fn send_command_at(path: &Path, cmd: Command) -> Result<(), String> {
    let seq = read_seq(path).saturating_add(1).max(now_secs());
    let body = Value::Obj(vec![
        ("cmd".into(), Value::str(cmd.as_str())),
        ("seq".into(), Value::num(seq)),
        ("ts".into(), Value::num(now_secs())),
    ])
    .dump();
    atomic_write(path, &body)
}

/// 轮询控制文件，并在 Tokio watch 通道上发布状态变化。
///
/// 终态的 `Stopped`（慢速中止）不会被过期的 resume 命令
/// 覆盖，确保原始原因能传递到界面。
pub fn spawn_watcher(tx: watch::Sender<RunState>, path: PathBuf) -> JoinHandle<()> {
    // 必须在 spawn 之前同步读取基线。若放到异步块内部，
    // 会与 spawn 后立即写入的命令竞争：
    // watcher 可能把新命令当作基线，从而永远不发布它。
    let mut last = read_seq(&path);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(400));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let Some((cmd, seq)) = read_command(&path) else {
                continue;
            };
            if seq <= last {
                continue;
            }
            last = seq;
            let next = match cmd {
                Command::Resume => RunState::Running,
                Command::Pause => RunState::Paused,
                Command::Cancel => {
                    RunState::Stopped(StopReason::UserCancel("用户取消下载".to_string()))
                }
            };
            if matches!(&*tx.borrow(), RunState::Stopped(_)) {
                break;
            }
            if tx.send(next).is_err() {
                break;
            }
        }
    })
}

/// 持有单个下载进程的控制通道。
pub struct DownloadControl {
    tx: watch::Sender<RunState>,
    rx: watch::Receiver<RunState>,
    speed: Arc<AtomicU64>,
    watcher: Option<JoinHandle<()>>,
}

impl DownloadControl {
    /// 将控制文件重置为 `running` 并启动 watcher 任务。
    pub fn start() -> Result<DownloadControl, String> {
        DownloadControl::start_at(Path::new(CONTROL_FILE))
    }

    fn start_at(path: &Path) -> Result<DownloadControl, String> {
        send_command_at(path, Command::Resume)?;
        let (tx, rx) = watch::channel(RunState::Running);
        let watcher = spawn_watcher(tx.clone(), path.to_path_buf());
        Ok(DownloadControl {
            tx,
            rx,
            speed: Arc::new(AtomicU64::new(0)),
            watcher: Some(watcher),
        })
    }

    /// 供慢速监控发布终止状态的发送端。
    pub fn tx(&self) -> watch::Sender<RunState> {
        self.tx.clone()
    }

    /// 为单个分块 worker 提供独立接收端。
    pub fn rx(&self) -> watch::Receiver<RunState> {
        self.rx.clone()
    }

    /// 保存最近一次测得吞吐量的共享计数器（字节/秒）。
    pub fn speed(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.speed)
    }

    /// 最近一次测得的吞吐量（字节/秒）。
    pub fn current_speed(&self) -> u64 {
        self.speed.load(Ordering::Relaxed)
    }

    /// 当前运行状态。
    pub fn state(&self) -> RunState {
        self.rx.borrow().clone()
    }
}

impl Drop for DownloadControl {
    fn drop(&mut self) {
        if let Some(h) = self.watcher.take() {
            h.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ou-taskctl-{}-{}.json", std::process::id(), name));
        let _ = fs::remove_file(&p);
        let mut t = p.clone();
        t.set_extension("tmp");
        let _ = fs::remove_file(&t);
        p
    }

    #[test]
    fn command_roundtrip_and_monotonic_seq() {
        let p = tmp("cmd");
        send_command_at(&p, Command::Resume).unwrap();
        assert_eq!(read_command(&p).map(|x| x.0), Some(Command::Resume));
        let first = read_seq(&p);
        send_command_at(&p, Command::Pause).unwrap();
        let second = read_seq(&p);
        assert!(second > first, "seq must advance on every command");
        assert_eq!(read_command(&p).map(|x| x.0), Some(Command::Pause));
        send_command_at(&p, Command::Cancel).unwrap();
        assert_eq!(read_command(&p).map(|x| x.0), Some(Command::Cancel));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn task_id_is_valid_and_unique() {
        let a = new_task_id();
        let b = new_task_id();
        assert_ne!(a, b);
        assert!(valid_task_id(&a));
        assert!(!valid_task_id("../etc/passwd"));
        assert!(!valid_task_id(""));
        assert!(!valid_task_id("ABC"));
    }

    #[test]
    fn store_upsert_persist_and_cap() {
        let p = tmp("store");
        let mut s = TaskStore::with_path(&p);
        let t = Task::new("fw.bin", "https://example.test/fw.bin", 1000);
        let id = t.id.clone();
        s.upsert(t);
        s.save().unwrap();

        let mut reloaded = TaskStore::with_path(&p);
        reloaded.load();
        assert_eq!(reloaded.tasks().len(), 1);
        assert_eq!(reloaded.tasks()[0].id, id);

        for i in 0..(MAX_TASKS + 5) {
            let mut t = Task::new(&format!("fw{i}"), "https://example.test/x", 1);
            t.created = 1000 + i as u64;
            reloaded.upsert(t);
        }
        assert_eq!(reloaded.tasks().len(), MAX_TASKS);
        // 最新的在最前
        assert!(reloaded.tasks()[0].created > reloaded.tasks()[1].created);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn remove_rejects_active_task() {
        let p = tmp("remove");
        let mut s = TaskStore::with_path(&p);
        let running = Task::new("fw.bin", "https://example.test/x", 10);
        let rid = running.id.clone();
        s.upsert(running);
        let err = s.remove(&rid).unwrap_err();
        assert!(err.contains("仍在进行中"));

        let mut paused = Task::new("fw2.bin", "https://example.test/y", 10);
        paused.state = ST_PAUSED.to_string();
        let pid = paused.id.clone();
        s.upsert(paused);
        assert!(s.remove(&pid).is_err());

        let mut done = Task::new("fw3.bin", "https://example.test/z", 10);
        done.finish(ST_DONE, "");
        let did = done.id.clone();
        s.upsert(done);
        assert!(s.remove(&did).is_ok());
        assert!(s.remove("missing").is_err());
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn purge_only_when_no_finished_task() {
        let mut s = TaskStore::with_path(tmp("purge"));
        let mut failed = Task::new("fw.bin", "https://example.test/x", 10);
        failed.finish(ST_FAILED, "network");
        s.upsert(failed);
        assert!(should_purge(s.tasks()));

        let mut done = Task::new("fw2.bin", "https://example.test/y", 10);
        done.finish(ST_DONE, "");
        s.upsert(done);
        assert!(
            !should_purge(s.tasks()),
            "a verified download must survive history cleanup"
        );
    }

    #[test]
    fn slow_policy_clamps_and_disables() {
        let p = SlowPolicy::new(32, 15, 10);
        assert_eq!(p.min_bps, 32 * 1024);
        assert_eq!(p.window_secs, 15);
        assert!(p.enabled());
        assert!(!SlowPolicy::new(0, 1, 0).enabled());
        assert_eq!(SlowPolicy::new(1, 1, 999).window_secs, 3);
        assert_eq!(SlowPolicy::new(1, 999, 999).grace_secs, 300);
    }

    #[test]
    fn corrupt_history_is_tolerated() {
        let p = tmp("corrupt");
        fs::write(&p, "{not json").unwrap();
        let mut s = TaskStore::with_path(&p);
        s.load();
        assert!(s.tasks().is_empty());
        let _ = fs::remove_file(&p);
    }

    #[tokio::test]
    async fn watcher_publishes_commands() {
        let p = tmp("watch");
        send_command_at(&p, Command::Resume).unwrap();
        let (tx, mut rx) = watch::channel(RunState::Running);
        let h = spawn_watcher(tx.clone(), p.clone());
        send_command_at(&p, Command::Pause).unwrap();
        // 轮询间隔为 400 毫秒
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(*rx.borrow_and_update(), RunState::Paused);
        send_command_at(&p, Command::Resume).unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(*rx.borrow_and_update(), RunState::Running);
        h.abort();
        let _ = fs::remove_file(&p);
    }
}
