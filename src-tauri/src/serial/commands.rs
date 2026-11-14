//! Desktop commands: the window is a client of the resident daemon. Mutations
//! and explicit Add-port discovery start it on demand; other reads never do.
use super::{client, daemon::load_configs, state_dir};
use crate::blocking;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Manager};

type R<T> = Result<T, String>;

fn data_dir(app: &AppHandle) -> R<PathBuf> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

const SHORT: Duration = Duration::from_secs(10);
/// Probing a remote node covers a TCP connection, node handshake and topology read.
const REMOTE: Duration = Duration::from_secs(30);
static RESTORE_AFTER_UPDATE: Mutex<bool> = Mutex::new(false);

fn prepare_update_with(restore: &mut bool, prepare: impl FnOnce() -> R<bool>) -> R<()> {
    *restore |= prepare()?;
    Ok(())
}

fn cancel_update_with(restore: &mut bool, restart: impl FnOnce() -> R<()>) -> R<()> {
    if *restore {
        restart()?;
        *restore = false;
    }
    Ok(())
}

/// Called by the native updater, never exposed as a separate Tauri command.
/// Repeated preparation preserves whether a running service was retired.
pub async fn prepare_update(app: &AppHandle) -> R<()> {
    let dir = data_dir(app)?;
    blocking(move || {
        let mut restore = RESTORE_AFTER_UPDATE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        prepare_update_with(&mut restore, || client::prepare_update(&dir))
    })
    .await
}

/// Restores only the service this updater preparation actually retired.
pub async fn cancel_update(app: &AppHandle) -> R<()> {
    let dir = data_dir(app)?;
    blocking(move || {
        let mut restore = RESTORE_AFTER_UPDATE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cancel_update_with(&mut restore, || client::ensure(&dir).map(|_| ()))
    })
    .await
}

fn offline(dir: &std::path::Path) -> Value {
    let ports: Vec<Value> = load_configs(&state_dir(dir))
        .into_iter()
        .map(|config| {
            let mut value = serde_json::to_value(config).unwrap_or_default();
            if let Value::Object(map) = &mut value {
                map.insert("connection".into(), json!("off"));
                map.insert("activity".into(), json!("idle"));
                map.insert("present".into(), Value::Null);
                map.insert("logGeneration".into(), json!(0));
            }
            value
        })
        .collect();
    json!({ "running": false, "version": 0, "instanceId": null, "instanceGeneration": 0, "h2loaderCore": super::h2loader_available(), "ports": ports })
}

fn with_running(mut snapshot: Value, update_error: Option<String>) -> Value {
    snapshot["running"] = json!(true);
    if snapshot.get("instanceGeneration").is_none() {
        snapshot["instanceGeneration"] = json!(0);
    }
    snapshot["serviceUpdateError"] = json!(update_error);
    snapshot
}

// These desktop-only epochs order verified observations even when an older
// client replaces a modern service with a legacy daemon with no generation.
// Keep the connection identity private; it includes the authentication token.
#[derive(Default)]
struct Observation {
    owner: Option<Option<client::Conn>>,
    owner_epoch: u64,
    observation_epoch: u64,
    last_offline_observation: u64,
}

impl Observation {
    fn issue(&mut self, owner: Option<client::Conn>, mut snapshot: Value) -> R<Value> {
        let next = |epoch: u64| {
            epoch
                .checked_add(1)
                .filter(|epoch| *epoch <= 9_007_199_254_740_991)
                .ok_or_else(|| "串口服务观测序号已耗尽，请重启应用。".to_owned())
        };
        let observation_epoch = next(self.observation_epoch)?;
        let owner_epoch = if self.owner.as_ref() != Some(&owner) {
            next(self.owner_epoch)?
        } else {
            self.owner_epoch
        };
        let object = snapshot.as_object_mut().ok_or("串口服务响应无效")?;
        object.insert("ownerEpoch".into(), json!(owner_epoch));
        object.insert("observationEpoch".into(), json!(observation_epoch));
        self.owner = Some(owner);
        self.owner_epoch = owner_epoch;
        self.observation_epoch = observation_epoch;
        Ok(snapshot)
    }
}

struct ObservationRequest {
    dir: PathBuf,
    observation: Arc<Mutex<Observation>>,
    started_after_observation: u64,
}

/// Capture before discovery or HTTP reads. An offline publication invalidates
/// running requests already in flight, even when their owner record is unchanged.
fn begin_observation(dir: &Path) -> ObservationRequest {
    static OBSERVATIONS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<Observation>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let observation = OBSERVATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .entry(dir.to_path_buf())
        .or_default()
        .clone();
    let started_after_observation = observation
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .observation_epoch;
    ObservationRequest {
        dir: dir.to_path_buf(),
        observation,
        started_after_observation,
    }
}

fn observed_snapshot(
    request: ObservationRequest,
    expected: Option<&client::Conn>,
    snapshot: Value,
) -> R<Value> {
    let mut observation = request
        .observation
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if expected.is_some()
        && request.started_after_observation < observation.last_offline_observation
    {
        return Err("串口服务已在请求期间离线，请重新读取状态。".into());
    }
    // Owner verification, offline fencing and epoch issuance share one mutex.
    let owner = client::observation_owner(&request.dir, expected)?;
    let value = observation.issue(owner, snapshot)?;
    if expected.is_none() {
        observation.last_offline_observation = observation.observation_epoch;
    }
    Ok(value)
}

/// Call a daemon endpoint, starting the daemon first.
async fn mutate(
    app: &AppHandle,
    method: &'static str,
    path: String,
    body: Option<Value>,
) -> R<Value> {
    mutate_within(app, method, path, body, SHORT).await
}

async fn mutate_within(
    app: &AppHandle,
    method: &'static str,
    path: String,
    body: Option<Value>,
    timeout: Duration,
) -> R<Value> {
    let dir = data_dir(app)?;
    blocking(move || client::ensure(&dir)?.request(method, &path, body.as_ref(), timeout)).await
}

/// Recovery actions must remain available while an older, busy service is
/// deferring migration. They operate only on that existing service.
async fn recover(app: &AppHandle, path: String) -> R<Value> {
    let dir = data_dir(app)?;
    blocking(move || {
        client::running(&dir)
            .ok_or_else(|| "串口服务未运行".to_owned())?
            .request("POST", &path, None, SHORT)
    })
    .await
}

fn segment(id: &str) -> R<&str> {
    // Imported ports are `<node id>--<port id>`, two UUIDs long.
    if !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        Ok(id)
    } else {
        Err("无效的标识。".into())
    }
}

/// Starts the daemon at launch when a port was left switched on.
pub fn autostart(app: &tauri::App) {
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let state = state_dir(&dir);
    // The Gateway is on by default, so GizMoth's service starts with the app.
    let gateway = super::gateway::read(&state).is_none_or(|config| config.enabled);
    if gateway || load_configs(&state).iter().any(|p| p.enabled) {
        std::thread::spawn(move || {
            let _ = client::ensure(&dir);
        });
    }
}

/// The Compiling page: requests to the service's compile API only.
#[tauri::command]
pub async fn compile_request(
    app: AppHandle,
    method: String,
    path: String,
    body: Option<Value>,
) -> R<Value> {
    let method: &'static str = match method.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        _ => return Err("Invalid method".into()),
    };
    let route = path.split('?').next().unwrap_or_default();
    if !(route == "/v1/compile" || route.starts_with("/v1/compile/"))
        || route.contains("..")
        || route.ends_with("/ws")
    {
        return Err("Invalid compile path".into());
    }
    mutate_within(&app, method, path, body, Duration::from_secs(30)).await
}

/// Long-poll for compile changes past `since`.
#[tauri::command]
pub async fn compile_watch(app: AppHandle, since: u64) -> R<Value> {
    mutate_within(
        &app,
        "GET",
        format!("/v1/compile/watch?since={since}&timeoutMs=20000"),
        None,
        Duration::from_secs(30),
    )
    .await
}

/// Shows an artifact, slot disk or cache folder, or the working directory, in
/// the file manager.
#[tauri::command]
pub async fn compile_reveal(app: AppHandle, path: String) -> R<()> {
    use tauri_plugin_opener::OpenerExt;
    let config = data_dir(&app)?.join("compile");
    #[cfg(unix)]
    let working = crate::compile::storage::configured_root(&config);
    #[cfg(not(unix))]
    let working = config.clone();
    let roots: Vec<PathBuf> = [config, working]
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    let target = PathBuf::from(&path)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !roots.iter().any(|root| target.starts_with(root)) {
        return Err("Only compile folders can be revealed".into());
    }
    app.opener()
        .reveal_item_in_dir(target)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn serial_overview(app: AppHandle) -> R<Value> {
    let dir = data_dir(&app)?;
    blocking(move || {
        let request = begin_observation(&dir);
        match client::discover(&dir)? {
            Some(discovery) => {
                let snapshot = with_running(
                    discovery.conn.request("GET", "/v1/ports", None, SHORT)?,
                    discovery.update_error,
                );
                observed_snapshot(request, Some(&discovery.conn), snapshot)
            }
            None => observed_snapshot(request, None, offline(&dir)),
        }
    })
    .await
}

/// Long-poll: returns when the daemon's version passes `since` (or ~20 s).
#[tauri::command]
pub async fn serial_watch(app: AppHandle, since: u64, instance_id: Option<String>) -> R<Value> {
    let dir = data_dir(&app)?;
    let path = watch_path(since, instance_id.as_deref());
    blocking(move || {
        let request = begin_observation(&dir);
        match client::discover(&dir)? {
            Some(discovery) => {
                let snapshot = with_running(
                    discovery
                        .conn
                        .request("GET", &path, None, Duration::from_secs(30))?,
                    discovery.update_error,
                );
                observed_snapshot(request, Some(&discovery.conn), snapshot)
            }
            None => {
                std::thread::sleep(Duration::from_millis(1500));
                observed_snapshot(request, None, offline(&dir))
            }
        }
    })
    .await
}

fn watch_path(since: u64, instance_id: Option<&str>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("since", &since.to_string());
    if let Some(instance) = instance_id {
        query.append_pair("instance", instance);
    }
    format!("/v1/watch?{}", query.finish())
}

#[tauri::command]
pub async fn serial_system_ports(app: AppHandle) -> R<Value> {
    let value = mutate(&app, "GET", "/v1/system-ports".into(), None).await?;
    system_ports_response(value)
}

fn system_ports_response(value: Value) -> R<Value> {
    if value.is_array() {
        Ok(value)
    } else {
        Err("串口服务响应无效".into())
    }
}

#[tauri::command]
pub async fn serial_add(app: AppHandle, port: Value) -> R<Value> {
    mutate(&app, "POST", "/v1/ports".into(), Some(port)).await
}

#[tauri::command]
pub async fn serial_update(app: AppHandle, id: String, patch: Value) -> R<Value> {
    let path = format!("/v1/ports/{}", segment(&id)?);
    mutate(&app, "PATCH", path, Some(patch)).await
}

#[tauri::command]
pub async fn serial_remove(app: AppHandle, id: String) -> R<Value> {
    let path = format!("/v1/ports/{}", segment(&id)?);
    mutate(&app, "DELETE", path, None).await
}

#[tauri::command]
pub async fn serial_logs(
    app: AppHandle,
    id: String,
    after: Option<u64>,
    limit: Option<u32>,
) -> R<Value> {
    let dir = data_dir(&app)?;
    let limit = limit.unwrap_or(500).clamp(1, 2000);
    let path = match after {
        Some(after) => format!(
            "/v1/ports/{}/logs?after={after}&limit={limit}",
            segment(&id)?
        ),
        None => format!("/v1/ports/{}/logs?limit={limit}", segment(&id)?),
    };
    blocking(move || match client::running(&dir) {
        Some(conn) => conn.request("GET", &path, None, SHORT),
        None => Ok(json!({ "entries": [], "next": after.unwrap_or(0), "truncated": false, "generation": 0, "instanceId": null })),
    })
    .await
}

#[tauri::command]
pub async fn serial_clear_logs(app: AppHandle, id: String) -> R<Value> {
    let path = format!("/v1/ports/{}/logs", segment(&id)?);
    mutate(&app, "DELETE", path, None).await
}

#[tauri::command]
pub async fn serial_send(
    app: AppHandle,
    id: String,
    text: String,
    line_ending: String,
) -> R<Value> {
    let path = format!("/v1/ports/{}/send", segment(&id)?);
    mutate(
        &app,
        "POST",
        path,
        Some(json!({ "text": text, "lineEnding": line_ending })),
    )
    .await
}

/// Connects to another GizMoth without saving it; reports its node information.
#[tauri::command]
pub async fn serial_remote_probe(app: AppHandle, url: String, api_key: String) -> R<Value> {
    let body = json!({ "url": url, "apiKey": api_key });
    mutate_within(&app, "POST", "/v1/remotes/probe".into(), Some(body), REMOTE).await
}

#[tauri::command]
pub async fn serial_remote_add(app: AppHandle, url: String, api_key: String) -> R<Value> {
    let body = json!({ "url": url, "apiKey": api_key });
    mutate_within(&app, "POST", "/v1/remotes".into(), Some(body), REMOTE).await
}

#[tauri::command]
pub async fn serial_remote_remove(app: AppHandle, id: String) -> R<Value> {
    let path = format!("/v1/remotes/{}", segment(&id)?);
    mutate(&app, "DELETE", path, None).await
}

#[tauri::command]
pub async fn serial_choose_package() -> R<Option<String>> {
    blocking(|| {
        Ok(rfd::FileDialog::new()
            .add_filter("H2Loader 固件包", &["zlib", "tar"])
            .pick_file()
            .map(|p| p.to_string_lossy().into_owned()))
    })
    .await
}

/// Folder picker for the working directory (Settings → Storage).
#[tauri::command]
pub async fn compile_choose_directory(current: Option<String>) -> R<Option<String>> {
    blocking(move || {
        let mut dialog = rfd::FileDialog::new().set_title("选择 GizMoth 工作目录");
        if let Some(parent) = current.as_deref().map(Path::new).and_then(Path::parent) {
            dialog = dialog.set_directory(parent);
        }
        Ok(dialog
            .pick_folder()
            .map(|p| p.to_string_lossy().into_owned()))
    })
    .await
}

#[tauri::command]
pub async fn serial_inspect(app: AppHandle, path: String) -> R<Value> {
    mutate(
        &app,
        "POST",
        "/v1/packages/inspect".into(),
        Some(json!({ "path": path })),
    )
    .await
}

#[tauri::command]
pub async fn serial_job(
    app: AppHandle,
    id: String,
    kind: String,
    path: Option<String>,
) -> R<Value> {
    let endpoint = format!("/v1/ports/{}/jobs", segment(&id)?);
    let job_id = uuid::Uuid::new_v4().to_string();
    // A job on an imported port first uploads its package to that node.
    let timeout = if super::remote::split(&id).is_some() {
        super::remote::JOB_TIMEOUT
    } else {
        SHORT
    };
    mutate_within(
        &app,
        "POST",
        endpoint,
        Some(json!({ "kind": kind, "path": path, "source": "app", "jobId": job_id })),
        timeout,
    )
    .await
    .map_err(|error| format!("{error}（任务 ID：{job_id}）"))
}

#[tauri::command]
pub async fn serial_cancel_job(app: AppHandle, job_id: String) -> R<Value> {
    let path = format!("/v1/jobs/{}/cancel", segment(&job_id)?);
    recover(&app, path).await
}

/// Accept or reject a queued pipe request.
#[tauri::command]
pub async fn serial_answer_request(
    app: AppHandle,
    id: String,
    request: u64,
    accept: bool,
) -> R<Value> {
    let action = if accept { "accept" } else { "reject" };
    let path = format!("/v1/ports/{}/requests/{request}/{action}", segment(&id)?);
    if accept {
        mutate(&app, "POST", path, None).await
    } else {
        recover(&app, path).await
    }
}

/// Disconnect exactly the selected WS session; never a replacement holder.
#[tauri::command]
pub async fn serial_release(
    app: AppHandle,
    id: String,
    connection: u64,
    pause_automatic: Option<bool>,
) -> R<Value> {
    let dir = data_dir(&app)?;
    let path = format!(
        "/v1/ports/{}/connections/{connection}/disconnect",
        segment(&id)?
    );
    let body = json!({ "pauseAutomatic": pause_automatic.unwrap_or(false) });
    blocking(move || {
        let conn = client::running(&dir).ok_or("Serial service is not running")?;
        conn.request("POST", &path, Some(&body), SHORT)
    })
    .await
}

#[tauri::command]
pub async fn serial_stop_service(app: AppHandle) -> R<()> {
    let dir = data_dir(&app)?;
    blocking(move || client::stop_service(&dir)).await
}

const MAX_EXPORT_ENTRIES: usize = 5000;
const MAX_EXPORT_BYTES: usize = 40 * 1024 * 1024;

/// Export only the history that existed at the first read. A live producer can
/// keep every later page nonempty; its new entries must not extend the export.
fn collect_export_logs(mut read_page: impl FnMut(Option<u64>, u32) -> R<Value>) -> R<String> {
    let tail = read_page(None, 1)?;
    let cutoff = tail["next"].as_u64().ok_or("串口服务响应无效")?;
    let generation = tail["generation"].as_u64();
    let instance = tail["instanceId"].as_str().map(str::to_owned);
    let mut output = String::new();
    let mut after = 0;
    let mut count = 0;
    'pages: while after < cutoff && count < MAX_EXPORT_ENTRIES {
        let limit = (MAX_EXPORT_ENTRIES - count).min(2000) as u32;
        let page = read_page(Some(after), limit)?;
        if page["generation"].as_u64() != generation
            || page["instanceId"].as_str() != instance.as_deref()
        {
            break;
        }
        let entries = page["entries"].as_array().ok_or("串口服务响应无效")?;
        if entries.is_empty() {
            break;
        }
        let before_count = count;
        for entry in entries {
            let seq = entry["seq"].as_u64().ok_or("串口服务响应无效")?;
            if seq > cutoff {
                break 'pages;
            }
            if seq <= after {
                continue;
            }
            let line = format!(
                "{} [{}] {}\n",
                entry["at"].as_u64().unwrap_or(0),
                entry["source"].as_str().unwrap_or(""),
                entry["text"].as_str().unwrap_or("")
            );
            if line.len() > MAX_EXPORT_BYTES.saturating_sub(output.len()) {
                break 'pages;
            }
            output.push_str(&line);
            count += 1;
            if count == MAX_EXPORT_ENTRIES {
                break 'pages;
            }
        }
        let next = page["next"].as_u64().unwrap_or(after);
        if next <= after || count == before_count {
            break;
        }
        after = next;
    }
    if output.is_empty() {
        output.push('\n');
    }
    Ok(output)
}

#[tauri::command]
pub async fn serial_export_logs(app: AppHandle, id: String, name: String) -> R<Option<String>> {
    let dir = data_dir(&app)?;
    let path = format!("/v1/ports/{}/logs", segment(&id)?);
    blocking(move || {
        let conn = client::running(&dir).ok_or("串口服务未运行")?;
        let output = collect_export_logs(|after, limit| {
            let query = match after {
                Some(after) => format!("{path}?limit={limit}&after={after}"),
                None => format!("{path}?limit={limit}"),
            };
            conn.request("GET", &query, None, SHORT)
        })?;
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let Some(target) = rfd::FileDialog::new()
            .set_file_name(format!("{safe}.log"))
            .save_file()
        else {
            return Ok(None);
        };
        std::fs::write(&target, output).map_err(|e| e.to_string())?;
        Ok(Some(target.to_string_lossy().into_owned()))
    })
    .await
}

/// Gateway settings, addresses and API key; starts the service if needed.
#[tauri::command]
pub async fn gateway_status(app: AppHandle) -> R<Value> {
    mutate(&app, "GET", "/v1/gateway".into(), None).await
}

#[tauri::command]
pub async fn gateway_update(app: AppHandle, patch: Value) -> R<Value> {
    mutate(&app, "PATCH", "/v1/gateway".into(), Some(patch)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(first: u64, count: u64, generation: u64, instance: &str) -> Value {
        let entries: Vec<_> = (first..first + count)
            .map(|seq| json!({ "seq": seq, "at": 123, "source": "rx", "text": format!("line-{seq}") }))
            .collect();
        json!({
            "entries": entries,
            "next": first + count - 1,
            "generation": generation,
            "instanceId": instance,
        })
    }

    #[test]
    fn export_stops_at_initial_cutoff_even_while_tail_keeps_growing() {
        let mut requests = 0;
        let output = collect_export_logs(|after, limit| {
            requests += 1;
            assert!(requests <= 3, "export chased the moving tail");
            Ok(match after {
                None => page(3001, 1, 2, "instance-a"),
                Some(after) => page(after + 1, u64::from(limit), 2, "instance-a"),
            })
        })
        .unwrap();
        assert_eq!(requests, 3);
        assert_eq!(output.lines().count(), 3001);
        assert!(output.ends_with("123 [rx] line-3001\n"));
        assert!(!output.contains("line-3002"));
    }

    #[test]
    fn export_does_not_mix_clear_generations_or_service_instances() {
        for change_instance in [false, true] {
            let mut requests = 0;
            let output = collect_export_logs(|after, _| {
                requests += 1;
                Ok(match after {
                    None => page(3000, 1, 2, "instance-a"),
                    Some(0) => page(1, 2000, 2, "instance-a"),
                    Some(_) if change_instance => page(2001, 1000, 2, "instance-b"),
                    Some(_) => page(2001, 1000, 3, "instance-a"),
                })
            })
            .unwrap();
            assert_eq!(requests, 3);
            assert_eq!(output.lines().count(), 2000);
            assert!(!output.contains("line-2001"));
        }
    }

    #[test]
    fn export_stops_when_only_duplicate_entries_make_cursor_progress() {
        let mut requests = 0;
        let output = collect_export_logs(|after, _| {
            requests += 1;
            assert!(requests <= 2);
            Ok(match after {
                None => page(100, 1, 0, "instance-a"),
                Some(_) => {
                    let mut value = page(0, 1, 0, "instance-a");
                    value["next"] = json!(1);
                    value
                }
            })
        })
        .unwrap();
        assert_eq!(requests, 2);
        assert_eq!(output, "\n");
    }

    #[test]
    fn export_stops_on_no_cursor_progress() {
        let mut requests = 0;
        let output = collect_export_logs(|after, _| {
            requests += 1;
            assert!(requests <= 2);
            Ok(match after {
                None => page(100, 1, 0, "instance-a"),
                Some(_) => {
                    let mut value = page(1, 1, 0, "instance-a");
                    value["next"] = json!(0);
                    value
                }
            })
        })
        .unwrap();
        assert_eq!(requests, 2);
        assert_eq!(output, "123 [rx] line-1\n");
    }

    #[test]
    fn export_caps_entries_for_unbounded_legacy_pages() {
        let mut requests = 0;
        let output = collect_export_logs(|after, limit| {
            requests += 1;
            assert!(requests <= 4);
            Ok(match after {
                None => json!({ "next": u64::MAX }),
                Some(after) => {
                    let mut value = page(after + 1, u64::from(limit), 0, "instance-a");
                    value.as_object_mut().unwrap().remove("generation");
                    value.as_object_mut().unwrap().remove("instanceId");
                    value
                }
            })
        })
        .unwrap();
        assert_eq!(requests, 4);
        assert_eq!(output.lines().count(), MAX_EXPORT_ENTRIES);
        assert!(output.len() <= MAX_EXPORT_BYTES);
    }

    #[test]
    fn snapshot_generation_keeps_modern_identity_and_supports_legacy_and_offline_reads() {
        let modern = with_running(
            json!({"instanceId": "new", "instanceGeneration": 8, "version": 2}),
            None,
        );
        assert_eq!(modern["instanceGeneration"], 8);
        let legacy = with_running(
            json!({"instanceId": "old", "version": 100}),
            Some("busy".into()),
        );
        assert_eq!(legacy["instanceGeneration"], 0);
        assert_eq!(legacy["serviceUpdateError"], "busy");
        let dir = tempfile::tempdir().unwrap();
        let stopped = offline(dir.path());
        assert_eq!(stopped["running"], false);
        assert_eq!(stopped["instanceGeneration"], 0);
    }

    fn record_observation_owner(dir: &Path, token: &str) -> client::Conn {
        let state = state_dir(dir);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            super::super::daemon::info_path(&state),
            serde_json::to_vec(&json!({
                "pid": 1, "port": 0, "token": token,
                "apiVersion": super::super::API_VERSION, "startedAt": 1,
            }))
            .unwrap(),
        )
        .unwrap();
        client::Conn::read(&state).unwrap()
    }

    #[test]
    fn verified_observations_accept_legacy_replacement_and_reject_late_modern_owner() {
        let dir = tempfile::tempdir().unwrap();
        let modern_owner = record_observation_owner(dir.path(), "modern-secret");
        let modern = observed_snapshot(
            begin_observation(dir.path()),
            Some(&modern_owner),
            with_running(
                json!({"instanceId": "modern", "instanceGeneration": 8, "version": 100}),
                None,
            ),
        )
        .unwrap();
        assert_eq!(modern["ownerEpoch"], 1);
        assert_eq!(modern["observationEpoch"], 1);
        let legacy_owner = record_observation_owner(dir.path(), "legacy-secret");
        let legacy = observed_snapshot(begin_observation(dir.path()), Some(&legacy_owner), with_running(
            json!({"instanceId": "legacy", "version": 1, "ports": [{"job": {"state": "running"}}]}),
            Some("busy".into()),
        )).unwrap();
        assert_eq!(legacy["instanceGeneration"], 0);
        assert_eq!(legacy["ownerEpoch"], 2);
        assert_eq!(legacy["observationEpoch"], 2);
        assert_eq!(legacy["serviceUpdateError"], "busy");
        assert_eq!(legacy["ports"][0]["job"]["state"], "running");
        assert!(!legacy.to_string().contains("legacy-secret"));
        assert!(
            observed_snapshot(begin_observation(dir.path()), Some(&modern_owner), modern).is_err()
        );
        let next =
            observed_snapshot(begin_observation(dir.path()), Some(&legacy_owner), legacy).unwrap();
        assert_eq!(next["ownerEpoch"], 2);
        assert_eq!(next["observationEpoch"], 3);
    }

    #[test]
    fn offline_observations_keep_owner_identity_until_discovery_changes() {
        let dir = tempfile::tempdir().unwrap();
        let owner = record_observation_owner(dir.path(), "same-owner");
        let running = with_running(json!({"instanceId": "same", "version": 9}), None);
        let first = observed_snapshot(begin_observation(dir.path()), Some(&owner), running.clone())
            .unwrap();
        let stopped =
            observed_snapshot(begin_observation(dir.path()), None, offline(dir.path())).unwrap();
        assert_eq!(stopped["ownerEpoch"], first["ownerEpoch"]);
        assert_eq!(stopped["observationEpoch"], 2);
        let resumed =
            observed_snapshot(begin_observation(dir.path()), Some(&owner), running).unwrap();
        assert_eq!(resumed["ownerEpoch"], first["ownerEpoch"]);
        assert_eq!(resumed["observationEpoch"], 3);
        std::fs::remove_file(super::super::daemon::info_path(&state_dir(dir.path()))).unwrap();
        let gone =
            observed_snapshot(begin_observation(dir.path()), None, offline(dir.path())).unwrap();
        assert_eq!(gone["ownerEpoch"], 2);
        assert_eq!(gone["observationEpoch"], 4);
        let other_dir = tempfile::tempdir().unwrap();
        let independent = observed_snapshot(
            begin_observation(other_dir.path()),
            None,
            offline(other_dir.path()),
        )
        .unwrap();
        assert_eq!(independent["ownerEpoch"], 1);
        assert_eq!(independent["observationEpoch"], 1);
    }

    #[test]
    fn offline_publication_fences_in_flight_same_owner_responses_and_allows_fresh_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let owner = record_observation_owner(dir.path(), "same-owner");
        let running =
            |version| with_running(json!({"instanceId": "same", "version": version}), None);
        let first =
            observed_snapshot(begin_observation(dir.path()), Some(&owner), running(9)).unwrap();

        // The response was obtained while online, but its publication is delayed
        // until another request has verified and published an offline snapshot.
        let in_flight = begin_observation(dir.path());
        let response = running(10);
        let response_owner = owner.clone();
        let (release, waiting) = std::sync::mpsc::channel();
        let delayed = std::thread::spawn(move || {
            waiting.recv_timeout(Duration::from_secs(2)).unwrap();
            observed_snapshot(in_flight, Some(&response_owner), response)
        });
        let stopped =
            observed_snapshot(begin_observation(dir.path()), None, offline(dir.path())).unwrap();
        assert_eq!(stopped["ownerEpoch"], first["ownerEpoch"]);
        assert_eq!(stopped["observationEpoch"], 2);
        release.send(()).unwrap();
        assert!(delayed.join().unwrap().unwrap_err().contains("离线"));

        let resumed =
            observed_snapshot(begin_observation(dir.path()), Some(&owner), running(11)).unwrap();
        assert_eq!(resumed["ownerEpoch"], first["ownerEpoch"]);
        assert_eq!(
            resumed["observationEpoch"], 3,
            "a fenced response must not consume an observation epoch"
        );
        assert_eq!(resumed["version"], 11);
    }

    #[test]
    fn system_ports_response_accepts_the_daemon_array() {
        let ports = json!([{ "path": "/dev/tty.example" }]);
        assert_eq!(system_ports_response(ports.clone()).unwrap(), ports);
        assert_eq!(system_ports_response(json!([])).unwrap(), json!([]));
        assert!(system_ports_response(json!({ "ports": [] })).is_err());
    }

    #[test]
    fn cancelled_update_restores_a_retired_service_once_after_repeated_preparation() {
        let mut restore = false;
        prepare_update_with(&mut restore, || Ok(true)).unwrap();
        prepare_update_with(&mut restore, || Ok(false)).unwrap();
        let mut restarts = 0;
        cancel_update_with(&mut restore, || {
            restarts += 1;
            Ok(())
        })
        .unwrap();
        cancel_update_with(&mut restore, || panic!("must not restart twice")).unwrap();
        assert_eq!(restarts, 1);
        assert!(!restore);
    }

    #[test]
    fn cancelled_update_does_not_start_a_service_that_was_already_stopped() {
        let mut restore = false;
        prepare_update_with(&mut restore, || Ok(false)).unwrap();
        cancel_update_with(&mut restore, || panic!("must preserve stopped service")).unwrap();
        assert!(!restore);
        assert!(prepare_update_with(&mut restore, || Err("busy".into())).is_err());
        cancel_update_with(&mut restore, || {
            panic!("busy refusal did not retire anything")
        })
        .unwrap();
    }

    #[test]
    fn cancelled_update_keeps_restore_intent_when_restart_fails() {
        let mut restore = false;
        prepare_update_with(&mut restore, || Ok(true)).unwrap();
        assert!(cancel_update_with(&mut restore, || Err("restart failed".into())).is_err());
        assert!(restore);
        cancel_update_with(&mut restore, || Ok(())).unwrap();
        assert!(!restore);
    }

    #[test]
    fn watch_encodes_instance_as_one_query_value() {
        let instance = "a b/&?=中";
        let path = watch_path(42, Some(instance));
        let pairs: Vec<_> = url::form_urlencoded::parse(path.split_once('?').unwrap().1.as_bytes())
            .into_owned()
            .collect();
        assert_eq!(
            pairs,
            [
                ("since".into(), "42".into()),
                ("instance".into(), instance.into())
            ]
        );
        assert_eq!(watch_path(0, None), "/v1/watch?since=0");
    }
}
