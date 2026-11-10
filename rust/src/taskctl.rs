// Download task lifecycle: cross-process control channel + persistent history.
//
// rpcd starts `online-upgrade download` detached, so pause/resume/cancel from
// a second process cannot use an in-process channel. A small JSON control file
// is the out-of-band channel; the downloader polls it from a Tokio task and
// publishes the resulting state on a `tokio::sync::watch` channel, which is
// what the transfer loops await. History lives in /root (flash) so it survives
// a reboot; /tmp artefacts are reclaimed only when no finished task still
// refers to them (a verified firmware is reused by `upgrade`).

use crate::json::{self, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Duration;

/// Out-of-band control file polled by the active download process.
pub const CONTROL_FILE: &str = "/tmp/online-upgrade-control.json";
/// Persistent task history (survives reboot, unlike the /tmp status file).
pub const TASKS_FILE: &str = "/root/.online-upgrade-tasks.json";
/// Firmware artefact owned by a download task.
pub const FIRMWARE_FILE: &str = "/tmp/firmware.img.gz";
/// Maximum number of history entries kept on flash.
pub const MAX_TASKS: usize = 30;

/// Task state: actively transferring.
pub const ST_RUNNING: &str = "running";
/// Task state: paused, resumable.
pub const ST_PAUSED: &str = "paused";
/// Task state: finished and verified.
pub const ST_DONE: &str = "done";
/// Task state: network / verification failure.
pub const ST_FAILED: &str = "failed";
/// Task state: stopped by the user or the slow-speed guard.
pub const ST_ABORTED: &str = "aborted";

/// Why a download was stopped. Kept typed so the UI and history can render
/// a slow-speed abort differently from a plain user cancel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The user clicked cancel / deleted the active task.
    UserCancel(String),
    /// The throughput guard fired; carries the measured evidence.
    Slow {
        /// Human-readable explanation including the route and thresholds.
        reason: String,
        /// Average throughput during the observation window (bytes/second).
        bps: u64,
        /// Configured floor (bytes/second).
        min_bps: u64,
        /// Length of the observation window in seconds.
        window_secs: u64,
    },
}

impl StopReason {
    /// The message shown in the status file, log and task history.
    pub fn text(&self) -> &str {
        match self {
            StopReason::UserCancel(s) => s,
            StopReason::Slow { reason, .. } => reason,
        }
    }
}

/// State of the active download, observed by every chunk worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Transferring normally.
    Running,
    /// No new chunks are claimed; the process stays alive waiting for resume.
    Paused,
    /// Terminal stop with a typed reason (user cancel or slow speed).
    Stopped(StopReason),
}

/// Command written by `online-upgrade pause|resume|cancel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Continue / start transfer.
    Resume,
    /// Suspend transfer at the next chunk boundary.
    Pause,
    /// Stop transfer immediately.
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

/// Slow-speed guard thresholds, all read from UCI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlowPolicy {
    /// Minimum acceptable throughput in bytes per second; 0 disables the guard.
    pub min_bps: u64,
    /// How long the throughput must stay below `min_bps` before stopping.
    pub window_secs: u64,
    /// Startup grace period (TLS handshake / TCP slow start) excluded from checks.
    pub grace_secs: u64,
}

impl SlowPolicy {
    /// Build a policy with clamped values. `min_kb` is in KiB/s.
    pub fn new(min_kb: u64, window_secs: u64, grace_secs: u64) -> SlowPolicy {
        SlowPolicy {
            min_bps: min_kb.saturating_mul(1024),
            window_secs: window_secs.clamp(3, 300),
            grace_secs: grace_secs.min(300),
        }
    }

    /// Load from `online-upgrade.settings.dl_*`, with sane defaults.
    pub fn load() -> SlowPolicy {
        let kb = uci_u32("dl_min_speed_kb", 32, 0, 10240);
        let win = uci_u32("dl_slow_window_sec", 15, 3, 300);
        let grace = uci_u32("dl_slow_grace_sec", 10, 0, 300);
        SlowPolicy::new(kb as u64, win as u64, grace as u64)
    }

    /// Whether the guard should run at all.
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

/// A persisted download task record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// Stable id (`<unix>-<pid>-<seq>`).
    pub id: String,
    /// Firmware asset name.
    pub name: String,
    /// Download URL (may contain a mirror prefix).
    pub url: String,
    /// One of the `ST_*` constants.
    pub state: String,
    /// Expected size in bytes (0 when unknown).
    pub total: u64,
    /// Bytes written so far.
    pub done: u64,
    /// Last measured throughput in bytes per second.
    pub speed_bps: u64,
    /// Failure / stop reason, empty on success.
    pub reason: String,
    /// Unix timestamp of creation.
    pub created: u64,
    /// Unix timestamp of the terminal state, 0 while active.
    pub finished: u64,
}

static TASK_SEQ: AtomicU64 = AtomicU64::new(0);

/// Generate a task id that is unique within a process and across processes.
pub fn new_task_id() -> String {
    let n = TASK_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}-{:x}", now_secs(), std::process::id(), n)
}

/// Validate a task id coming from rpcd before using it in any file operation.
pub fn valid_task_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-')
}

impl Task {
    /// Start a fresh running task.
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

    /// Mark the task as terminal, recording the outcome and reason.
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

/// Persistent, bounded task history. All writes are atomic (tmp + rename).
pub struct TaskStore {
    path: PathBuf,
    tasks: Vec<Task>,
}

impl TaskStore {
    /// Open the default history file and load its contents.
    pub fn open() -> TaskStore {
        let mut s = TaskStore::with_path(TASKS_FILE);
        s.load();
        s
    }

    /// Open a history file at a custom path (used by tests and CLI overrides).
    pub fn with_path(path: impl Into<PathBuf>) -> TaskStore {
        TaskStore {
            path: path.into(),
            tasks: Vec::new(),
        }
    }

    /// Reload from disk; a corrupt or missing file is treated as empty history.
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

    /// Persist the history atomically.
    pub fn save(&self) -> Result<(), String> {
        let arr: Vec<Value> = self.tasks.iter().map(Task::to_value).collect();
        let body = Value::Obj(vec![("tasks".into(), Value::Arr(arr))]).dump();
        atomic_write(&self.path, &body)
    }

    /// Insert or update a task, keeping the newest `MAX_TASKS` entries.
    pub fn upsert(&mut self, task: Task) {
        if let Some(existing) = self.tasks.iter_mut().find(|t| t.id == task.id) {
            *existing = task;
        } else {
            self.tasks.push(task);
        }
        self.tasks.sort_by(|a, b| b.created.cmp(&a.created));
        self.tasks.truncate(MAX_TASKS);
    }

    /// Mutable access to a task by id.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    /// Remove a task. Active (running/paused) tasks must be stopped first.
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

    /// Remove a task regardless of its recorded state. Use only when the owner
    /// process is known to be gone (stale `running` entry after a crash).
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

    /// Read-only view of the history.
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// JSON representation returned by the `tasks` subcommand.
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

/// Whether the shared firmware artefacts are safe to reclaim.
///
/// A finished, verified download is reused by `upgrade` (see
/// `reuse_download`), so as long as one `done` task remains we keep the file.
pub fn should_purge(tasks: &[Task]) -> bool {
    !tasks.iter().any(|t| t.state == ST_DONE)
}

/// Delete the fixed set of firmware artefacts, but only when no `done` task
/// still refers to them. Returns the number of files removed.
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

/// Write a control command. `seq` is monotonic and stays inside the exact
/// integer range of the JSON number type (f64), so no change is ever lost to
/// floating-point rounding.
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

/// Poll the control file and publish state changes on a Tokio watch channel.
///
/// A terminal `Stopped` state (slow-speed abort) is never overwritten by a
/// stale resume command, so the original reason reaches the UI.
pub fn spawn_watcher(tx: watch::Sender<RunState>, path: PathBuf) -> JoinHandle<()> {
    // Read the baseline synchronously *before* spawning. Doing it inside the
    // async block races with a command written right after spawn: the watcher
    // could then adopt the new command as its baseline and never publish it.
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

/// Owns the control channel for one download process.
pub struct DownloadControl {
    tx: watch::Sender<RunState>,
    rx: watch::Receiver<RunState>,
    speed: Arc<AtomicU64>,
    watcher: Option<JoinHandle<()>>,
}

impl DownloadControl {
    /// Reset the control file to `running` and start the watcher task.
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

    /// A sender for the slow-speed monitor to publish a terminal stop.
    pub fn tx(&self) -> watch::Sender<RunState> {
        self.tx.clone()
    }

    /// An independent receiver for one chunk worker.
    pub fn rx(&self) -> watch::Receiver<RunState> {
        self.rx.clone()
    }

    /// Shared counter holding the last measured throughput (bytes/second).
    pub fn speed(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.speed)
    }

    /// Last measured throughput in bytes per second.
    pub fn current_speed(&self) -> u64 {
        self.speed.load(Ordering::Relaxed)
    }

    /// Current run state.
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
        // newest first
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
        // poll interval is 400ms
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(*rx.borrow_and_update(), RunState::Paused);
        send_command_at(&p, Command::Resume).unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(*rx.borrow_and_update(), RunState::Running);
        h.abort();
        let _ = fs::remove_file(&p);
    }
}
