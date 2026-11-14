//! Desktop-owned local notifications. The resident serial service remains the
//! only device owner; this listener only reads its cached topology over HTTP.
use crate::{desktop_state, serial, system_permissions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Mutex,
    time::Duration,
};
use tauri::{Emitter, Manager};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub serial_tasks: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self { serial_tasks: true }
    }
}
pub struct NotificationState(Mutex<Settings>);

fn save(directory: &Path, saved: &mut Settings, next: Settings) -> Result<(), String> {
    fs::write(
        directory.join("notifications.tmp"),
        serde_json::to_vec(&next).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("无法保存通知设置：{e}"))?;
    fs::rename(
        directory.join("notifications.tmp"),
        directory.join("notifications.json"),
    )
    .map_err(|e| format!("无法保存通知设置：{e}"))?;
    *saved = next;
    Ok(())
}

#[tauri::command]
pub fn notification_settings(
    state: tauri::State<'_, NotificationState>,
) -> Result<Settings, String> {
    state.0.lock().map(|s| *s).map_err(|e| e.to_string())
}
#[tauri::command]
pub fn notification_settings_set(
    app: tauri::AppHandle,
    state: tauri::State<'_, NotificationState>,
    serial_tasks: bool,
) -> Result<(), String> {
    let directory = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let mut saved = state.0.lock().map_err(|e| e.to_string())?;
    save(&directory, &mut saved, Settings { serial_tasks })
}

#[tauri::command]
pub async fn notification_test(app: tauri::AppHandle) -> Result<(), String> {
    let chinese = desktop_state::notification_chinese(&app);
    let (title, body) = if chinese {
        (
            "GizMoth 通知测试",
            "串口任务完成、失败或取消时，GizMoth 会在这里通知你。",
        )
    } else {
        (
            "GizMoth notification test",
            "GizMoth will notify you here when a serial task succeeds, fails or is cancelled.",
        )
    };
    system_permissions::send(&app, title, body).await
}

#[derive(Debug, PartialEq, Eq)]
struct Completion {
    port: String,
    node: Option<String>,
    kind: String,
    state: String,
}
impl Completion {
    fn copy(&self, chinese: bool) -> (String, String) {
        let task = match (self.kind.as_str(), chinese) {
            ("stage", true) => "固件传输",
            ("stage", false) => "Firmware transfer",
            ("install", true) => "固件安装",
            ("install", false) => "Firmware installation",
            (_, true) => "设备重启",
            (_, false) => "Device reboot",
        };
        let outcome = match (self.state.as_str(), chinese) {
            ("succeeded", true) => "已完成",
            ("succeeded", false) => "succeeded",
            ("cancelled", true) => "已取消",
            ("cancelled", false) => "cancelled",
            (_, true) => "失败",
            (_, false) => "failed",
        };
        let title = if chinese {
            format!("{task}{outcome}")
        } else {
            format!("{task} {outcome}")
        };
        // Only display device/node labels. Job errors and package paths may
        // contain secrets and are deliberately kept out of lock-screen copy.
        let body = match &self.node {
            Some(node) => format!("{} · {}", clean(node), clean(&self.port)),
            None => clean(&self.port),
        };
        (title, body)
    }
}
fn clean(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

#[derive(Default)]
struct Source {
    initialized: bool,
    ports: HashSet<String>,
    // A job ID is never reused during an owning daemon's lifetime. Retain the
    // terminal set across disconnects, history eviction, and local restarts for
    // remote sources. It is cleared when that source's boot ID changes.
    completed: HashSet<(String, String)>,
}
#[derive(Default)]
struct Tracker {
    local_instance: Option<String>,
    version: u64,
    sources: HashMap<String, (String, Source)>,
}
impl Tracker {
    fn observe(&mut self, snapshot: &Value) -> Vec<Completion> {
        let Some(instance) = snapshot["instanceId"].as_str().filter(|id| !id.is_empty()) else {
            return Vec::new();
        };
        let version = snapshot["version"].as_u64().unwrap_or(0);
        if self.local_instance.as_deref() == Some(instance) && version <= self.version {
            return Vec::new();
        }
        self.local_instance = Some(instance.to_owned());
        self.version = version;
        let mut result = Vec::new();
        self.source("local", instance, None, &snapshot["ports"], &mut result);
        if let Some(nodes) = snapshot["nodes"].as_array() {
            for node in nodes {
                // Offline snapshots deliberately retain cached ports. Do not
                // consume those terminals until a fresh topology is received.
                if node["status"] != "online" {
                    continue;
                }
                if let (Some(id), Some(boot)) = (node["id"].as_str(), node["bootId"].as_str()) {
                    // Online is published after the handshake, before retained
                    // ports are replaced. Never baseline them under a new boot.
                    if node["topologyBootId"].as_str() != Some(boot) {
                        continue;
                    }
                    self.source(
                        &format!("remote:{id}"),
                        boot,
                        node["name"].as_str(),
                        &node["ports"],
                        &mut result,
                    );
                }
            }
        }
        result
    }
    fn source(
        &mut self,
        key: &str,
        boot: &str,
        node: Option<&str>,
        ports: &Value,
        result: &mut Vec<Completion>,
    ) {
        let entry = self
            .sources
            .entry(key.to_owned())
            .or_insert_with(|| (boot.to_owned(), Source::default()));
        if entry.0 != boot {
            *entry = (boot.to_owned(), Source::default());
        }
        let source = &mut entry.1;
        let baseline = !source.initialized;
        source.initialized = true;
        for port in ports.as_array().into_iter().flatten() {
            let Some(id) = port["id"].as_str() else {
                continue;
            };
            // The first local snapshot baselines the entire daemon. A port
            // added later can finish work before its first observation. Remote
            // ports, however, can import arbitrary pre-existing history.
            let first_port = source.ports.insert(id.to_owned());
            let initial = baseline || (key != "local" && first_port);
            for job in port["history"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(std::iter::once(&port["job"]))
            {
                let Some(job_id) = job["id"].as_str() else {
                    continue;
                };
                let Some(state) = job["state"]
                    .as_str()
                    .filter(|state| matches!(*state, "succeeded" | "failed" | "cancelled"))
                else {
                    continue;
                };
                let Some(kind) = job["kind"].as_str().filter(|kind| {
                    matches!(
                        *kind,
                        "stage" | "install" | "rebootApp" | "rebootLoader" | "rebootUpgrade"
                    )
                }) else {
                    continue;
                };
                if !source.completed.insert((id.to_owned(), job_id.to_owned())) || initial {
                    continue;
                }
                result.push(Completion {
                    port: port["name"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Serial port")
                        .to_owned(),
                    node: node.map(str::to_owned),
                    kind: kind.to_owned(),
                    state: state.to_owned(),
                });
            }
        }
    }
}

pub fn setup(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let directory = app.path().app_data_dir()?;
    fs::create_dir_all(&directory)?;
    let settings = match fs::read(directory.join("notifications.json")) {
        Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|error| {
            eprintln!("Cannot read notification settings: {error}");
            // A corrupt preference must not unexpectedly re-enable delivery.
            Settings {
                serial_tasks: false,
            }
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(error) => {
            eprintln!("Cannot read notification settings: {error}");
            Settings {
                serial_tasks: false,
            }
        }
    };
    app.manage(NotificationState(Mutex::new(settings)));
    // Native test builds must never probe a user's service or show OS prompts.
    #[cfg(not(feature = "native-e2e"))]
    {
        let app = app.handle().clone();
        let (delivery, mut pending) = tokio::sync::mpsc::channel::<Completion>(DELIVERY_CAPACITY);
        let delivery_app = app.clone();
        // Exactly one bounded delivery worker. Waiting for OS permission/status
        // or submission callbacks cannot block the snapshot observer.
        tauri::async_runtime::spawn(async move {
            while let Some(completion) = pending.recv().await {
                if !enabled(&delivery_app) {
                    continue;
                }
                let (title, body) =
                    completion.copy(desktop_state::notification_chinese(&delivery_app));
                if let Err(error) = system_permissions::send(&delivery_app, &title, &body).await {
                    if !error.starts_with("通知权限") {
                        eprintln!("Serial notification failed: {error}");
                        let _ = delivery_app.emit_to(
                            "main",
                            "notification-error",
                            "系统无法发送串口任务通知。",
                        );
                    }
                }
            }
        });
        tauri::async_runtime::spawn(async move {
            let mut tracker = Tracker::default();
            let mut owner = None;
            let mut since = 0;
            let mut instance = None::<String>;
            loop {
                let path = watch_path(since, instance.as_deref());
                let dir = directory.clone();
                let observed = crate::blocking(move || {
                    // running(), unlike ensure()/discover(), never starts,
                    // upgrades, or retires a stopped or existing service.
                    let conn = serial::client::running(&dir).ok_or("Serial service stopped")?;
                    let snapshot = conn.request("GET", &path, None, Duration::from_secs(25))?;
                    serial::client::observation_owner(&dir, Some(&conn))?;
                    Ok((conn, snapshot))
                })
                .await;
                match observed {
                    Ok((conn, snapshot)) => {
                        if owner.as_ref().is_some_and(|old| old != &conn) {
                            // A response from another owner establishes a new
                            // baseline even for legacy reused instance IDs.
                            tracker.local_instance = None;
                            tracker.sources.remove("local");
                        }
                        owner = Some(conn);
                        since = snapshot["version"].as_u64().unwrap_or(0);
                        instance = snapshot["instanceId"].as_str().map(str::to_owned);
                        let completions = tracker.observe(&snapshot);
                        // Consume even while disabled. Queuing is nonblocking;
                        // overload must not stall observation or silently drop.
                        if enabled(&app) {
                            let dropped = enqueue(&delivery, completions);
                            if dropped > 0 {
                                eprintln!("Serial notification queue overflow: {dropped} tasks");
                                let _ = app.emit_to(
                                    "main",
                                    "notification-error",
                                    format!("系统通知队列已满，已跳过 {dropped} 条任务通知。"),
                                );
                            }
                        }
                    }
                    Err(_) => {
                        since = 0;
                        instance = None;
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                    }
                }
            }
        });
    }
    Ok(())
}
const DELIVERY_CAPACITY: usize = 128;
fn enabled(app: &tauri::AppHandle) -> bool {
    app.state::<NotificationState>()
        .0
        .lock()
        .map(|s| s.serial_tasks)
        .unwrap_or(false)
}
fn enqueue(sender: &tokio::sync::mpsc::Sender<Completion>, completions: Vec<Completion>) -> usize {
    let mut dropped = 0;
    for completion in completions {
        if sender.try_send(completion).is_err() {
            dropped += 1;
        }
    }
    dropped
}
fn watch_path(since: u64, instance: Option<&str>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("since", &since.to_string());
    query.append_pair("timeoutMs", "20000");
    if let Some(instance) = instance {
        query.append_pair("instance", instance);
    }
    format!("/v1/watch?{}", query.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn job(id: &str, kind: &str, state: &str) -> Value {
        json!({"id":id,"kind":kind,"state":state,"source":"ws"})
    }
    fn snapshot(version: u64, current: Value, history: Vec<Value>) -> Value {
        json!({"instanceId":"daemon-a", "version":version, "ports":[{"id":"port-a","name":"Board", "job":current,"history":history}],"nodes":[]})
    }
    #[test]
    fn baselines_old_history_but_follows_initial_active_job() {
        let mut tracker = Tracker::default();
        let old = job("old", "install", "succeeded");
        assert!(tracker
            .observe(&snapshot(
                1,
                job("active", "install", "running"),
                vec![old.clone()]
            ))
            .is_empty());
        assert_eq!(
            tracker
                .observe(&snapshot(
                    2,
                    job("active", "install", "succeeded"),
                    vec![old]
                ))
                .len(),
            1
        );
        assert!(tracker
            .observe(&snapshot(
                3,
                Value::Null,
                vec![job("active", "install", "succeeded")]
            ))
            .is_empty());
    }
    #[test]
    fn catches_fast_jobs_in_history_and_all_terminal_outcomes_without_refresh_noise() {
        let mut tracker = Tracker::default();
        tracker.observe(&snapshot(1, Value::Null, vec![]));
        let history = vec![
            job("one", "stage", "succeeded"),
            job("two", "rebootApp", "failed"),
            job("three", "install", "cancelled"),
            job("noise", "refresh", "failed"),
        ];
        assert_eq!(
            tracker
                .observe(&snapshot(2, history[0].clone(), history.clone()))
                .len(),
            3
        );
        assert!(tracker
            .observe(&snapshot(3, Value::Null, history))
            .is_empty());
        assert!(tracker
            .observe(&snapshot(2, job("stale", "install", "succeeded"), vec![]))
            .is_empty());
    }
    #[test]
    fn source_is_not_filtered_by_initiator() {
        let mut tracker = Tracker::default();
        tracker.observe(&snapshot(1, Value::Null, vec![]));
        let jobs = ["app", "mcp", "ws", "local"]
            .into_iter()
            .map(|source| {
                let mut task = job(source, "install", "succeeded");
                task["source"] = json!(source);
                task
            })
            .collect();
        assert_eq!(tracker.observe(&snapshot(2, Value::Null, jobs)).len(), 4);
    }
    #[test]
    fn local_restart_baselines_and_allows_reused_job_ids_in_new_lifetime() {
        let mut tracker = Tracker::default();
        tracker.observe(&snapshot(1, Value::Null, vec![]));
        assert_eq!(
            tracker
                .observe(&snapshot(2, job("same", "install", "succeeded"), vec![]))
                .len(),
            1
        );
        let mut restarted = snapshot(1, job("historic", "install", "succeeded"), vec![]);
        restarted["instanceId"] = json!("daemon-b");
        assert!(tracker.observe(&restarted).is_empty());
        restarted["version"] = json!(2);
        restarted["ports"][0]["job"] = job("same", "install", "cancelled");
        assert_eq!(tracker.observe(&restarted).len(), 1);
    }
    #[test]
    fn remote_boot_and_disconnect_are_independent_from_local_daemon_restart() {
        let mut tracker = Tracker::default();
        let mut value = snapshot(1, Value::Null, vec![]);
        value["nodes"] = json!([{"id":"remote","bootId":"boot-a","topologyBootId":"boot-a","name":"Lab","status":"online","ports":[{"id":"remote--p","name":"Remote board","history":[job("old", "install", "succeeded")],"job":job("active", "install", "running")}]}]);
        assert!(tracker.observe(&value).is_empty());
        value["version"] = json!(2);
        value["nodes"][0]["status"] = json!("offline");
        value["nodes"][0]["ports"][0]["job"] = job("active", "install", "failed");
        assert!(tracker.observe(&value).is_empty());
        value["version"] = json!(3);
        value["nodes"][0]["status"] = json!("online");
        let completions = tracker.observe(&value);
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].node.as_deref(), Some("Lab"));
        value["instanceId"] = json!("replacement-local");
        value["version"] = json!(1);
        assert!(tracker.observe(&value).is_empty());
        value["nodes"][0]["bootId"] = json!("boot-b");
        value["nodes"][0]["topologyBootId"] = json!("boot-b");
        value["version"] = json!(2);
        assert!(tracker.observe(&value).is_empty());
        value["version"] = json!(3);
        value["nodes"][0]["ports"][0]["job"] = job("new", "rebootLoader", "succeeded");
        assert_eq!(tracker.observe(&value).len(), 1);
    }
    #[test]
    fn newly_added_local_port_notifies_fast_job_and_readded_port_dedupes() {
        let mut tracker = Tracker::default();
        tracker.observe(&snapshot(1, Value::Null, vec![]));
        let mut value = snapshot(2, Value::Null, vec![]);
        value["ports"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"another", "job":job("old", "install", "succeeded")}));
        assert_eq!(tracker.observe(&value).len(), 1);
        tracker.observe(&snapshot(3, Value::Null, vec![]));
        value["version"] = json!(4);
        assert!(tracker.observe(&value).is_empty());
    }
    #[test]
    fn remote_handshake_retained_ports_cannot_establish_new_boot_baseline() {
        let mut tracker = Tracker::default();
        let mut value = snapshot(1, Value::Null, vec![]);
        value["nodes"] = json!([{"id":"remote","bootId":"boot-a","topologyBootId":"boot-a","status":"online","ports":[{"id":"remote--p","job":job("prior-boot", "install", "succeeded")}]}]);
        assert!(tracker.observe(&value).is_empty());
        // Reconnect advertises a new boot before replacing retained ports.
        value["version"] = json!(2);
        value["nodes"][0]["bootId"] = json!("boot-b");
        value["nodes"][0]["topologyBootId"] = Value::Null;
        assert!(tracker.observe(&value).is_empty());
        // Repeated handshake/control snapshots must not establish a baseline.
        value["version"] = json!(3);
        assert!(tracker.observe(&value).is_empty());
        // Actual first topology contains work that pre-dates this connection.
        value["version"] = json!(4);
        value["nodes"][0]["topologyBootId"] = json!("boot-b");
        value["nodes"][0]["ports"][0]["history"] =
            json!([job("existing-before-connect", "install", "succeeded")]);
        value["nodes"][0]["ports"][0]["job"] = job("active", "install", "running");
        assert!(tracker.observe(&value).is_empty());
        value["version"] = json!(5);
        value["nodes"][0]["ports"][0]["job"] = job("active", "install", "succeeded");
        assert_eq!(tracker.observe(&value).len(), 1);
        // A newly imported port also baselines its historical terminal jobs.
        value["version"] = json!(6);
        value["nodes"][0]["ports"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"remote--new","job":job("historic", "install", "failed")}));
        assert!(tracker.observe(&value).is_empty());
    }
    #[test]
    fn slow_delivery_does_not_block_observation_or_lose_evicted_history() {
        let mut tracker = Tracker::default();
        tracker.observe(&snapshot(1, Value::Null, vec![]));
        let (sender, mut receiver) = tokio::sync::mpsc::channel(DELIVERY_CAPACITY);
        // No worker receives anything while the observer consumes three full
        // daemon history windows. Each previous window has already disappeared.
        for batch in 0..3 {
            let history = (0..20)
                .map(|index| job(&format!("{batch}-{index}"), "install", "succeeded"))
                .collect();
            let completions = tracker.observe(&snapshot(batch + 2, Value::Null, history));
            assert_eq!(completions.len(), 20);
            assert_eq!(enqueue(&sender, completions), 0);
        }
        assert_eq!(receiver.len(), 60);
        for _ in 0..60 {
            assert!(receiver.try_recv().is_ok());
        }
        assert!(receiver.try_recv().is_err());
    }
    #[test]
    fn delivery_queue_is_bounded_and_reports_dropped_count() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let task = || Completion {
            port: "Board".into(),
            node: None,
            kind: "install".into(),
            state: "succeeded".into(),
        };
        assert_eq!(enqueue(&sender, vec![task(), task(), task(), task()]), 2);
        assert_eq!(receiver.len(), 2);
        receiver.try_recv().unwrap();
        assert_eq!(enqueue(&sender, vec![task()]), 0);
    }
    #[test]
    fn disabled_setting_persists_and_failed_save_preserves_memory() {
        let dir = tempfile::tempdir().unwrap();
        let mut saved = Settings::default();
        save(
            dir.path(),
            &mut saved,
            Settings {
                serial_tasks: false,
            },
        )
        .unwrap();
        let loaded: Settings =
            serde_json::from_slice(&fs::read(dir.path().join("notifications.json")).unwrap())
                .unwrap();
        assert!(!loaded.serial_tasks);
        assert!(save(&dir.path().join("missing"), &mut saved, Settings::default()).is_err());
        assert!(!saved.serial_tasks);
    }
}
