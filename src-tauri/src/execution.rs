//! Versioned execution requests and durable action history. Results stay in memory;
//! only an explicitly selected output file receives the final response body.
pub mod cli;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::watch;

pub const VERSION: &str = "gizmoth.dev/v1";
pub type R<T> = Result<T, String>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Envelope {
    pub api_version: String,
    pub kind: String,
    #[serde(default)]
    pub metadata: Metadata,
    pub spec: Value,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Operation {
    Describe,
    SerialList,
    SerialPorts,
    SerialStatus {
        port: String,
    },
    #[cfg(unix)]
    Compile(Value),
    List,
    Get {
        id: String,
        wait: u64,
    },
    Cancel {
        id: String,
    },
    Logs {
        source: String,
        id: String,
        cursor: Option<u64>,
        limit: u64,
        wait: u64,
    },
    Device {
        port: String,
        command: String,
        params: Value,
    },
}
impl Operation {
    pub fn action(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Compile(_) => true,
            Self::Cancel { .. } | Self::Device { .. } => true,
            _ => false,
        }
    }
}

fn identifier(value: &Value, key: &str) -> R<String> {
    let text = value[key]
        .as_str()
        .ok_or_else(|| format!("{key} must be a string"))?;
    if text.is_empty()
        || text.len() > 128
        || !text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(format!("Invalid {key}"));
    }
    Ok(text.into())
}
fn integer(value: &Value, key: &str, default: u64, max: u64) -> R<u64> {
    match value.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|n| *n <= max)
            .ok_or_else(|| format!("{key} must be an integer from 0 to {max}")),
    }
}
fn fields(value: &Value, allowed: &[&str]) -> R<()> {
    for key in value.as_object().ok_or("spec must be an object")?.keys() {
        if key != "operation" && !allowed.contains(&key.as_str()) {
            return Err(format!("Unknown parameter {key}"));
        }
    }
    Ok(())
}
impl Envelope {
    pub fn parse(value: Value) -> R<(Self, Operation)> {
        let envelope: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if envelope.api_version != VERSION {
            return Err(format!("Unsupported apiVersion {}", envelope.api_version));
        }
        if envelope.kind != "Request" {
            return Err(format!("Unsupported kind {}", envelope.kind));
        }
        if let Some(name) = &envelope.metadata.name {
            if name.trim().is_empty()
                || name.chars().count() > 120
                || name.chars().any(char::is_control)
            {
                return Err(
                    "metadata.name must have 1–120 characters and no control characters".into(),
                );
            }
        }
        let spec = &envelope.spec;
        let operation = spec["operation"]
            .as_str()
            .ok_or("spec.operation is required")?;
        let op = match operation {
            "describe" => {
                fields(spec, &[])?;
                Operation::Describe
            }
            "compile" => {
                #[cfg(unix)]
                {
                    let mut build = spec.clone();
                    build
                        .as_object_mut()
                        .ok_or("spec must be an object")?
                        .remove("operation");
                    let request: crate::compile::jobs::BuildRequest =
                        serde_json::from_value(build.clone()).map_err(|e| e.to_string())?;
                    crate::compile::validate_request(&request)?;
                    Operation::Compile(build)
                }
                #[cfg(not(unix))]
                return Err("Compiling is not available on this platform".into());
            }
            "serial.list" => {
                fields(spec, &[])?;
                Operation::SerialList
            }
            "serial.ports" => {
                fields(spec, &[])?;
                Operation::SerialPorts
            }
            "serial.status" => {
                fields(spec, &["port"])?;
                Operation::SerialStatus {
                    port: identifier(spec, "port")?,
                }
            }
            "jobs.list" => {
                fields(spec, &[])?;
                Operation::List
            }
            "jobs.get" | "jobs.wait" => {
                fields(spec, &["id", "waitSeconds"])?;
                Operation::Get {
                    id: identifier(spec, "id")?,
                    wait: integer(
                        spec,
                        "waitSeconds",
                        if operation == "jobs.wait" { 20 } else { 0 },
                        120,
                    )?,
                }
            }
            "jobs.cancel" => {
                fields(spec, &["id"])?;
                Operation::Cancel {
                    id: identifier(spec, "id")?,
                }
            }
            "logs.read" => {
                fields(spec, &["source", "id", "cursor", "limit", "waitSeconds"])?;
                let source = spec["source"]
                    .as_str()
                    .filter(|s| matches!(*s, "compile" | "serial"))
                    .ok_or("source must be compile or serial")?
                    .to_owned();
                let cursor = spec
                    .get("cursor")
                    .map(|v| v.as_u64().ok_or("cursor must be a nonnegative integer"))
                    .transpose()?;
                Operation::Logs {
                    source,
                    id: identifier(spec, "id")?,
                    cursor,
                    limit: integer(spec, "limit", 500, 65536)?.max(1),
                    wait: integer(spec, "waitSeconds", 0, 120)?,
                }
            }
            "serial.send" | "serial.install" | "serial.refresh" | "serial.reboot" => {
                let mut params = spec.clone();
                let object = params.as_object_mut().ok_or("spec must be an object")?;
                object.remove("operation");
                let port = identifier(spec, "port")?;
                object.remove("port");
                let command = operation.trim_start_matches("serial.").to_owned();
                crate::serial::ws::parse(
                    &json!({"id":"exec", "method":command,"params":params}).to_string(),
                )
                .map_err(|(_, e)| e)?;
                Operation::Device {
                    port,
                    command,
                    params,
                }
            }
            _ => return Err(format!("Unsupported operation {operation}; use describe")),
        };
        Ok((envelope, op))
    }
    pub fn operation(&self) -> &str {
        self.spec["operation"].as_str().unwrap_or_default()
    }
    pub fn title(&self) -> String {
        self.metadata
            .name
            .clone()
            .unwrap_or_else(|| self.operation().into())
    }
}

pub fn describe() -> Value {
    let string =
        json!({"type":"string", "minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9-]+$"});
    let wait = json!({"type":"integer","minimum":0,"maximum":120,"default":0});
    let schema = |operation: &str, mut properties: Value, required: Vec<&str>| {
        properties["operation"] = json!({"const":operation});
        let mut required = required;
        required.push("operation");
        json!({"operation":operation,"inputSchema":{"type":"object","additionalProperties":false,"properties":properties,"required":required}})
    };
    let mut operations = vec![
        schema("describe", json!({}), vec![]),
        schema("serial.list", json!({}), vec![]),
        schema("serial.ports", json!({}), vec![]),
        schema("serial.status", json!({"port":string}), vec!["port"]),
        schema("jobs.list", json!({}), vec![]),
        schema(
            "jobs.get",
            json!({"id":string,"waitSeconds":wait}),
            vec!["id"],
        ),
        schema(
            "jobs.wait",
            json!({"id":string,"waitSeconds":{"type":"integer","minimum":0,"maximum":120,"default":20}}),
            vec!["id"],
        ),
        schema("jobs.cancel", json!({"id":string}), vec!["id"]),
        schema(
            "logs.read",
            json!({"source":{"enum":["compile","serial"]},"id":string,"cursor":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536,"default":500},"waitSeconds":wait}),
            vec!["source", "id"],
        ),
    ];
    #[cfg(unix)]
    {
        let commands = crate::compile::ws::commands();
        let build = &commands[0]["inputSchema"]["oneOf"][1];
        operations.push(schema(
            "compile",
            build["properties"].clone(),
            vec!["workspace", "command"],
        ));
    }
    for command in crate::serial::ws::commands()
        .into_iter()
        .filter(|c| c["name"] != "cancel")
    {
        let operation = format!("serial.{}", command["name"].as_str().unwrap());
        let mut properties = command["inputSchema"]["properties"].clone();
        properties["port"] = string.clone();
        properties["port"]["description"] = json!(
            "Local configured serial port ID; existing grant and lifecycle enforcement applies"
        );
        let mut required: Vec<_> = command["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        required.push("port");
        operations.push(schema(&operation, properties, required));
    }
    json!({"apiVersion":VERSION,"kind":"Discovery","requestSchema":{"type":"object","additionalProperties":false,"required":["apiVersion","kind","spec"],"properties":{"apiVersion":{"const":VERSION},"kind":{"const":"Request"},"metadata":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string","minLength":1,"maxLength":120}}},"spec":{"oneOf":operations.iter().map(|v|v["inputSchema"].clone()).collect::<Vec<_>>()}}},"operations":operations,"execution":{"waitSecondsMax":120,"detachedOwnedByService":true,"output":"Absolute new local file; existing files are never replaced","history":"Action requests only; no logs or result bodies persisted"}})
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecJob {
    pub id: String,
    pub name: String,
    pub operation: String,
    pub request: Envelope,
    pub status: String,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub error: Option<String>,
    pub output: Option<String>,
    pub source_job_id: Option<String>,
}
impl ExecJob {
    pub fn finished(&self) -> bool {
        matches!(
            self.status.as_str(),
            "succeeded" | "failed" | "cancelled" | "interrupted"
        )
    }
}
pub struct Entry {
    pub job: Mutex<ExecJob>,
    pub cancel: AtomicBool,
    changed: watch::Sender<u64>,
    result: Mutex<Option<Value>>,
}
impl Entry {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    pub fn receipt(&self) -> Value {
        let mut value = json!({"job": self.job.lock().unwrap().clone()});
        if let Some(result) = self.result.lock().unwrap().clone() {
            value["result"] = result;
        }
        value
    }
    pub async fn wait(&self, seconds: u64) -> Value {
        let mut receiver = self.changed.subscribe();
        let wait = async {
            loop {
                if self.job.lock().unwrap().finished() {
                    break;
                }
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(seconds.min(120)), wait).await;
        self.receipt()
    }
}
pub struct Service {
    root: PathBuf,
    entries: Mutex<HashMap<String, Arc<Entry>>>,
}
impl Service {
    pub fn new(data: &Path) -> R<Arc<Self>> {
        let root = data.join("execution");
        match fs::symlink_metadata(&root) {
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                return Err("Execution history must be a real directory".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let service = Arc::new(Self {
            root,
            entries: Mutex::new(HashMap::new()),
        });
        for file in fs::read_dir(&service.root).map_err(|e| e.to_string())? {
            let file = file.map_err(|e| e.to_string())?;
            if file.path().extension().is_none_or(|e| e != "json") {
                continue;
            }
            if !file.file_type().map_err(|e| e.to_string())?.is_file() {
                return Err("Invalid execution history entry".into());
            }
            let mut job: ExecJob =
                serde_json::from_slice(&fs::read(file.path()).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("Invalid execution history: {e}"))?;
            if uuid::Uuid::parse_str(&job.id).is_err()
                || file.file_name().to_string_lossy() != format!("{}.json", job.id)
            {
                return Err("Invalid execution history ID".into());
            }
            if !job.finished() {
                job.status = "interrupted".into();
                job.finished_at = Some(crate::serial::now_ms());
                job.error = Some("Resident service restarted before completion".into());
                service.persist(&job)?;
            }
            service
                .entries
                .lock()
                .unwrap()
                .insert(job.id.clone(), Self::entry(job));
        }
        Ok(service)
    }
    fn entry(job: ExecJob) -> Arc<Entry> {
        Arc::new(Entry {
            job: Mutex::new(job),
            cancel: AtomicBool::new(false),
            changed: watch::channel(0).0,
            result: Mutex::new(None),
        })
    }
    fn persist(&self, job: &ExecJob) -> R<()> {
        let temp = self.root.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(job)?)?;
            file.sync_all()?;
            fs::rename(&temp, self.root.join(format!("{}.json", job.id)))?;
            Ok::<_, std::io::Error>(())
        })();
        let _ = fs::remove_file(temp);
        result.map_err(|e| e.to_string())
    }
    pub fn accept(&self, request: Envelope, output: Option<&Output>) -> R<Arc<Entry>> {
        // Control requests must remain available to drain a saturated queue.
        if request.operation() != "jobs.cancel"
            && self
                .entries
                .lock()
                .unwrap()
                .values()
                .filter(|e| !e.job.lock().unwrap().finished())
                .count()
                >= 256
        {
            return Err("Execution queue is full".into());
        }
        let job = ExecJob {
            id: uuid::Uuid::new_v4().to_string(),
            name: request.title(),
            operation: request.operation().into(),
            request,
            status: "queued".into(),
            created_at: crate::serial::now_ms(),
            started_at: None,
            finished_at: None,
            error: None,
            output: output.map(|o| o.path.to_string_lossy().into_owned()),
            source_job_id: None,
        };
        self.persist(&job)?;
        let entry = Self::entry(job);
        self.entries
            .lock()
            .unwrap()
            .insert(entry.job.lock().unwrap().id.clone(), entry.clone());
        Ok(entry)
    }
    pub fn get(&self, id: &str) -> R<Arc<Entry>> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| "Execution job not found".into())
    }
    pub fn list(&self) -> Value {
        let mut jobs: Vec<_> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .map(|e| e.job.lock().unwrap().clone())
            .collect();
        jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        json!(jobs)
    }
    pub fn busy(&self) -> bool {
        self.entries
            .lock()
            .unwrap()
            .values()
            .any(|e| !e.job.lock().unwrap().finished())
    }
    pub fn update(&self, entry: &Entry, update: impl FnOnce(&mut ExecJob)) -> R<()> {
        let mut job = entry.job.lock().unwrap();
        let mut next = job.clone();
        update(&mut next);
        self.persist(&next)?;
        *job = next;
        drop(job);
        entry.changed.send_modify(|v| *v += 1);
        Ok(())
    }
    pub fn finish(&self, entry: &Entry, outcome: R<Value>, output: Option<Output>) {
        let (mut status, mut error, mut result) = match outcome {
            Ok(result) => ("succeeded", None, Some(result)),
            Err(error) => (
                if entry.cancelled() {
                    "cancelled"
                } else {
                    "failed"
                },
                Some(error),
                None,
            ),
        };
        // Final output has the same structured shape as the CLI's final response.
        let mut job = entry.job.lock().unwrap().clone();
        let finished_at = Some(crate::serial::now_ms());
        job.status = status.into();
        job.finished_at = finished_at;
        job.error = error.clone();
        let mut receipt = json!({"job":job});
        if let Some(result) = &result {
            receipt["result"] = result.clone();
        }
        if let Some(output) = output {
            if let Err(message) = output.write(&receipt) {
                status = "failed";
                error = Some(format!("Output file: {message}"));
                result = None;
            }
        }
        *entry.result.lock().unwrap() = result;
        if let Err(message) = self.update(entry, |job| {
            job.status = status.into();
            job.finished_at = finished_at;
            job.error = error;
        }) {
            let mut job = entry.job.lock().unwrap();
            job.status = "failed".into();
            job.error = Some(format!("Execution history write failed: {message}"));
            job.finished_at = Some(crate::serial::now_ms());
            drop(job);
            entry.changed.send_modify(|v| *v += 1);
        }
    }
}

/// Pin the directory at admission. Publish with a hard link, which cannot
/// overwrite an existing target, even if one appears while execution runs.
pub struct Output {
    dir: cap_std::fs::Dir,
    name: std::ffi::OsString,
    pub path: PathBuf,
}
impl Output {
    pub fn prepare(path: &str) -> R<Self> {
        let path = Path::new(path);
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("output must be an absolute local path without ..".into());
        }
        let name = path
            .file_name()
            .ok_or("output must name a file")?
            .to_owned();
        let parent = path
            .parent()
            .ok_or("output requires a directory")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let dir = cap_std::fs::Dir::open_ambient_dir(&parent, cap_std::ambient_authority())
            .map_err(|e| e.to_string())?;
        match dir.symlink_metadata(&name) {
            Ok(_) => {
                return Err("output already exists; choose a new file to preserve your data".into())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        Ok(Self {
            path: parent.join(&name),
            dir,
            name,
        })
    }
    pub fn write(self, value: &Value) -> R<()> {
        let temp = format!(".gizmoth-{}.tmp", uuid::Uuid::new_v4());
        let result = (|| {
            let mut options = cap_std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = self.dir.open_with(&temp, &options)?;
            serde_json::to_writer_pretty(&mut file, value)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            self.dir.hard_link(&temp, &self.dir, &self.name)?;
            Ok::<_, std::io::Error>(())
        })();
        let _ = self.dir.remove_file(temp);
        result.map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(operation: &str) -> Value {
        json!({"apiVersion":VERSION,"kind":"Request","metadata":{},"spec":{"operation":operation}})
    }
    #[test]
    fn invalid_requests_are_rejected_before_acceptance() {
        for value in [
            json!(null),
            request("shell"),
            json!({"apiVersion":"v0","kind":"Request","metadata":{},"spec":{"operation":"describe"}}),
            json!({"apiVersion":VERSION,"kind":"Wrong","metadata":{},"spec":{"operation":"describe"}}),
            json!({"apiVersion":VERSION,"kind":"Request","metadata":{},"spec":{"operation":"jobs.list","extra":true}}),
        ] {
            assert!(Envelope::parse(value).is_err());
        }
        assert!(Envelope::parse(request("describe")).is_ok());
    }
    #[test]
    fn serial_queries_are_strict_observations_and_discoverable() {
        let schemas = describe();
        for name in ["serial.list", "serial.ports", "serial.status"] {
            let mut value = request(name);
            if name == "serial.status" {
                assert!(Envelope::parse(value.clone()).is_err());
                value["spec"]["port"] = json!("port-id");
            }
            let (_, operation) = Envelope::parse(value.clone()).unwrap();
            assert!(!operation.action());
            assert!(schemas["operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|schema| schema["operation"] == name));
            value["spec"]["unexpected"] = json!(true);
            assert!(Envelope::parse(value).is_err());
        }
        for port in [json!(null), json!(""), json!(123), json!("/dev/cu.device")] {
            let mut value = request("serial.status");
            value["spec"]["port"] = port;
            assert!(Envelope::parse(value).is_err());
        }
    }

    #[tokio::test]
    async fn history_survives_restart_without_results_or_logs() {
        let temp = tempfile::tempdir().unwrap();
        let service = Service::new(temp.path()).unwrap();
        let (request, _) = Envelope::parse(request("describe")).unwrap();
        let entry = service.accept(request.clone(), None).unwrap();
        let id = entry.job.lock().unwrap().id.clone();
        service.finish(&entry, Ok(json!({"secretResult":"not-on-disk"})), None);
        assert_eq!(entry.wait(0).await["result"]["secretResult"], "not-on-disk");
        let pending = service.accept(request, None).unwrap();
        let pending_id = pending.job.lock().unwrap().id.clone();
        let loaded = Service::new(temp.path()).unwrap();
        assert_eq!(
            loaded.get(&id).unwrap().receipt()["job"]["status"],
            "succeeded"
        );
        assert!(loaded.get(&id).unwrap().receipt().get("result").is_none());
        assert_eq!(
            loaded.get(&pending_id).unwrap().receipt()["job"]["status"],
            "interrupted"
        );
        assert!(
            !fs::read_to_string(temp.path().join(format!("execution/{id}.json")))
                .unwrap()
                .contains("secretResult")
        );
    }
    #[test]
    fn output_never_overwrites_files_or_symlinks_and_publication_is_atomic() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("result.json");
        let output = Output::prepare(path.to_str().unwrap()).unwrap();
        fs::write(&path, "user data").unwrap();
        assert!(output.write(&json!({"answer":1})).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "user data");
        assert!(Output::prepare(path.to_str().unwrap()).is_err());
        let path = temp.path().join("new.json");
        Output::prepare(path.to_str().unwrap())
            .unwrap()
            .write(&json!({"answer":2}))
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()["answer"],
            2
        );
        assert!(Output::prepare("relative.json").is_err());
        #[cfg(unix)]
        {
            let link = temp.path().join("link.json");
            std::os::unix::fs::symlink(temp.path().join("missing"), &link).unwrap();
            assert!(Output::prepare(link.to_str().unwrap()).is_err());
        }
    }
}
