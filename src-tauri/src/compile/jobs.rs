//! One build: queued by a WebSocket, run on a slot's VM, streamed back and
//! optionally collected into an artifact.
use super::{
    artifacts::Artifact,
    guest::{
        self,
        protocol::{self, GuestJob, GuestMessage, HostMessage},
    },
    sync, Service, SlotState,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::broadcast;

const LOG_LIMIT: usize = 8 << 20;
const MAX_ARTIFACT_FILES: u32 = 2000;
const MAX_ARTIFACT_BYTES: u64 = 16 << 30;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildRequest {
    pub workspace: PathBuf,
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub collect_on_failure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Booting,
    Syncing,
    Running,
    Collecting,
    Succeeded,
    Failed,
    Cancelled,
}

impl Status {
    pub fn finished(self) -> bool {
        matches!(self, Status::Succeeded | Status::Failed | Status::Cancelled)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobState {
    pub status: Status,
    pub slot: Option<String>,
    pub position: Option<usize>,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub exit: Option<ExitInfo>,
    pub error: Option<String>,
    pub artifact: Option<String>,
    pub synced: Option<Value>,
    pub missing: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Event {
    Frame(Value),
    Log(Bytes),
}

/// Bounded log keeping absolute offsets so readers can page through it.
#[derive(Default)]
pub struct LogRing {
    data: Vec<u8>,
    start: u64,
}

impl LogRing {
    pub fn push(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
        if self.data.len() > LOG_LIMIT {
            let drop = self.data.len() - LOG_LIMIT + (1 << 20);
            self.data.drain(..drop);
            self.start += drop as u64;
        }
    }

    pub fn end(&self) -> u64 {
        self.start + self.data.len() as u64
    }

    /// Returns (bytes, next offset, whether `after` was already discarded).
    pub fn read(&self, after: Option<u64>, limit: usize) -> (Vec<u8>, u64, bool) {
        let end = self.end();
        let from = match after {
            Some(offset) => offset.clamp(self.start, end),
            None => end.saturating_sub(limit as u64).max(self.start),
        };
        let truncated = after.is_some_and(|offset| offset < self.start);
        let begin = (from - self.start) as usize;
        let stop = (begin + limit).min(self.data.len());
        (
            self.data[begin..stop].to_vec(),
            self.start + stop as u64,
            truncated,
        )
    }
}

pub struct Job {
    pub id: String,
    pub order: u64,
    pub client: String,
    pub title: String,
    pub request: BuildRequest,
    pub key: String,
    pub state: Mutex<JobState>,
    pub log: Mutex<LogRing>,
    pub events: broadcast::Sender<Event>,
    cancel: AtomicBool,
}

impl Job {
    pub fn new(
        order: u64,
        client: String,
        title: String,
        request: BuildRequest,
        key: String,
    ) -> Job {
        Job {
            id: uuid::Uuid::new_v4().to_string(),
            order,
            client,
            title,
            request,
            key,
            state: Mutex::new(JobState {
                status: Status::Queued,
                slot: None,
                position: None,
                created_at: crate::serial::now_ms(),
                started_at: None,
                finished_at: None,
                exit: None,
                error: None,
                artifact: None,
                synced: None,
                missing: Vec::new(),
            }),
            log: Mutex::new(LogRing::default()),
            events: broadcast::channel(1024).0,
            cancel: AtomicBool::new(false),
        }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn status(&self) -> Status {
        self.state.lock().unwrap().status
    }

    pub fn emit(&self, frame: Value) {
        let _ = self.events.send(Event::Frame(frame));
    }

    pub fn append(&self, bytes: &[u8]) {
        self.log.lock().unwrap().push(bytes);
        let _ = self.events.send(Event::Log(Bytes::copy_from_slice(bytes)));
    }

    pub fn note(&self, text: &str) {
        self.append(format!("\x1b[2m[gizmoth] {text}\x1b[0m\n").as_bytes());
    }

    pub fn set_status(&self, status: Status) {
        let mut state = self.state.lock().unwrap();
        if state.status.finished() {
            return;
        }
        state.status = status;
        if status != Status::Queued && state.started_at.is_none() {
            state.started_at = Some(crate::serial::now_ms());
        }
        state.position = None;
        drop(state);
        self.emit(json!({ "type": "status", "jobId": self.id, "status": status }));
    }

    pub fn set_position(&self, position: usize) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.status != Status::Queued || state.position == Some(position) {
            return false;
        }
        state.position = Some(position);
        drop(state);
        self.emit(json!({ "type": "queued", "jobId": self.id, "position": position }));
        true
    }

    pub fn summary(&self) -> Value {
        let state = self.state.lock().unwrap().clone();
        let mut value = serde_json::to_value(&state).unwrap_or_default();
        value["id"] = self.id.clone().into();
        value["client"] = self.client.clone().into();
        value["title"] = self.title.clone().into();
        value["workspace"] = self.request.workspace.display().to_string().into();
        value["command"] = json!(self.request.command);
        value["cwd"] = self.request.cwd.clone().into();
        value["artifacts"] = json!(self.request.artifacts);
        value["logBytes"] = self.log.lock().unwrap().end().into();
        value
    }

    /// Marks the job finished exactly once and emits the final frame.
    pub fn finish(&self, status: Status, error: Option<String>, artifact: Option<Value>) {
        let mut state = self.state.lock().unwrap();
        if state.status.finished() {
            return;
        }
        state.status = status;
        state.position = None;
        state.finished_at = Some(crate::serial::now_ms());
        if error.is_some() {
            state.error = error;
        }
        drop(state);
        if let Some(message) = self.state.lock().unwrap().error.clone() {
            self.note(&format!("error: {message}"));
        }
        let mut frame = json!({ "type": "finished", "job": self.summary() });
        if let Some(artifact) = artifact {
            frame["artifact"] = artifact;
        }
        self.emit(frame);
    }
}

/// Reads DATA frames as a byte stream.
struct DataReader<'a> {
    stream: &'a mut UnixStream,
    pending: Vec<u8>,
    offset: usize,
}

impl Read for DataReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.offset == self.pending.len() {
            match protocol::read_frame(self.stream)? {
                Some((protocol::DATA, data)) => {
                    self.pending = data;
                    self.offset = 0;
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "expected artifact data",
                    ))
                }
            }
        }
        let n = buffer.len().min(self.pending.len() - self.offset);
        buffer[..n].copy_from_slice(&self.pending[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
}

fn next_message(stream: &mut UnixStream, job: &Job) -> Result<GuestMessage, String> {
    loop {
        match protocol::read_frame(stream) {
            Ok(Some((protocol::JSON, payload))) => {
                let message: GuestMessage = protocol::parse(&payload).map_err(|e| e.to_string())?;
                if let GuestMessage::Error { message } = message {
                    return Err(message);
                }
                return Ok(message);
            }
            Ok(Some((protocol::STDOUT | protocol::STDERR, bytes))) => job.append(&bytes),
            Ok(Some(_)) => return Err("unexpected data from the VM".into()),
            Ok(None) => return Err("The VM closed the connection".into()),
            Err(error) => return Err(format!("VM connection: {error}")),
        }
    }
}

/// Runs a job on its assigned slot. Called on a dedicated thread.
pub fn run(service: Arc<Service>, job: Arc<Job>, slot: String) {
    let outcome = execute(&service, &job, &slot);
    let alive = service.slot_alive(&slot);
    match outcome {
        Ok((exit, artifact)) => {
            let success = exit.code == Some(0) && !exit.timed_out && !exit.cancelled;
            let status = if exit.cancelled || job.cancelled() {
                Status::Cancelled
            } else if success {
                Status::Succeeded
            } else {
                Status::Failed
            };
            let error = if exit.timed_out {
                Some("The command timed out".to_string())
            } else if !success && !exit.cancelled {
                Some(match (exit.code, exit.signal) {
                    (Some(code), _) => format!("The command exited with code {code}"),
                    (_, Some(signal)) => format!("The command was killed by signal {signal}"),
                    _ => "The command failed".into(),
                })
            } else {
                None
            };
            let view = artifact.as_ref().map(|a| service.artifacts.view(a));
            job.state.lock().unwrap().artifact = artifact.map(|a| a.id);
            job.finish(status, error, view);
        }
        Err(error) => {
            let status = if job.cancelled() {
                Status::Cancelled
            } else {
                Status::Failed
            };
            job.finish(
                status,
                if status == Status::Cancelled {
                    None
                } else {
                    Some(error)
                },
                None,
            );
        }
    }
    service.release_slot(&slot, alive);
}

fn execute(
    service: &Arc<Service>,
    job: &Arc<Job>,
    slot: &str,
) -> Result<(ExitInfo, Option<Artifact>), String> {
    let socket = service.ensure_vm(slot, job)?;
    if job.cancelled() {
        return Err("cancelled".into());
    }
    job.set_status(Status::Syncing);
    let mut stream = guest::connect(&socket, protocol::CONTROL_PORT, Duration::from_secs(10))
        .map_err(|e| format!("VM agent: {e}"))?;
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(120)))
        .map_err(|e| e.to_string())?;
    let request = &job.request;
    let (settings_env, excludes) = service.job_settings();
    let mut env: Vec<(String, String)> = settings_env;
    env.extend(request.env.iter().map(|(k, v)| (k.clone(), v.clone())));
    let guest_job = GuestJob {
        id: job.id.clone(),
        workspace: job.key.clone(),
        argv: request.command.clone(),
        cwd: request.cwd.clone(),
        env,
        timeout_secs: request.timeout_seconds,
    };
    protocol::write_json(&mut stream, &HostMessage::Job { job: guest_job })
        .map_err(|e| e.to_string())?;
    let mut recorded = BTreeMap::new();
    loop {
        match next_message(&mut stream, job)? {
            GuestMessage::Manifest { entries, done } => {
                recorded.extend(entries.into_iter().map(|e| (e.path.clone(), e)));
                if done {
                    break;
                }
            }
            GuestMessage::Phase { .. } => {}
            other => return Err(format!("unexpected {other:?} before synchronization")),
        }
    }
    let mut patterns = excludes;
    patterns.extend(request.exclude.iter().cloned());
    let host = sync::scan(&request.workspace, &patterns)?;
    let (deletes, puts) = sync::diff(&host, &recorded);
    let changed_bytes: u64 = puts.iter().map(|e| e.size).sum();
    job.note(&format!(
        "Synchronizing {} ({} changed, {} removed, {})",
        request.workspace.display(),
        puts.len(),
        deletes.len(),
        human(changed_bytes)
    ));
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    sync::send(&mut writer, &request.workspace, &deletes, &puts, &|| {
        job.cancelled()
    })?;
    loop {
        match next_message(&mut stream, job)? {
            GuestMessage::Synced { files, bytes } => {
                let synced = json!({ "entries": host.len(), "files": files, "bytes": bytes, "removed": deletes.len() });
                job.state.lock().unwrap().synced = Some(synced.clone());
                job.emit(json!({ "type": "synced", "jobId": job.id, "synced": synced }));
                break;
            }
            GuestMessage::Phase { .. } => {}
            other => return Err(format!("unexpected {other:?} during synchronization")),
        }
    }
    job.set_status(Status::Running);
    let finished = Arc::new(AtomicBool::new(false));
    let canceller = {
        let (job, finished, writer) = (
            job.clone(),
            finished.clone(),
            Mutex::new(writer.try_clone().map_err(|e| e.to_string())?),
        );
        std::thread::spawn(move || {
            while !finished.load(Ordering::SeqCst) {
                if job.cancelled() {
                    let mut writer = writer.lock().unwrap();
                    let _ = protocol::write_json(&mut *writer, &HostMessage::Cancel);
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };
    let exit = loop {
        match next_message(&mut stream, job) {
            Ok(GuestMessage::Exit {
                code,
                signal,
                timed_out,
                cancelled,
            }) => {
                break ExitInfo {
                    code,
                    signal,
                    timed_out,
                    cancelled,
                }
            }
            Ok(_) => {}
            Err(error) => {
                finished.store(true, Ordering::SeqCst);
                return Err(error);
            }
        }
    };
    finished.store(true, Ordering::SeqCst);
    let _ = canceller.join();
    job.state.lock().unwrap().exit = Some(exit.clone());
    job.emit(json!({ "type": "exit", "jobId": job.id, "exit": exit }));
    let success = exit.code == Some(0) && !exit.timed_out && !exit.cancelled;
    let mut artifact = None;
    if !request.artifacts.is_empty() && (success || request.collect_on_failure) && !job.cancelled()
    {
        job.set_status(Status::Collecting);
        protocol::write_json(
            &mut writer,
            &HostMessage::Collect {
                patterns: request.artifacts.clone(),
                max_files: MAX_ARTIFACT_FILES,
                max_bytes: MAX_ARTIFACT_BYTES,
            },
        )
        .map_err(|e| e.to_string())?;
        let mut staging = service.artifacts.begin()?;
        loop {
            match next_message(&mut stream, job)? {
                GuestMessage::File { path, size, mode } => {
                    let mut reader = DataReader {
                        stream: &mut stream,
                        pending: Vec::new(),
                        offset: 0,
                    };
                    staging.write(&path, size, mode, &mut reader)?;
                    if reader.offset != reader.pending.len() {
                        return Err("artifact data overran its file".into());
                    }
                }
                GuestMessage::Collected {
                    files,
                    bytes,
                    missing,
                } => {
                    if !missing.is_empty() {
                        job.note(&format!("No files matched: {}", missing.join(", ")));
                    }
                    job.state.lock().unwrap().missing = missing;
                    if files > 0 {
                        job.note(&format!(
                            "Collected {files} artifact file(s), {}",
                            human(bytes)
                        ));
                    }
                    break;
                }
                GuestMessage::Phase { .. } => {}
                other => return Err(format!("unexpected {other:?} while collecting")),
            }
        }
        if !staging.is_empty() {
            let meta = Artifact {
                id: String::new(),
                job_id: job.id.clone(),
                client: job.client.clone(),
                title: job.title.clone(),
                workspace: request.workspace.display().to_string(),
                command: request.command.clone(),
                created_at: crate::serial::now_ms(),
                pinned: false,
                files: Vec::new(),
                bytes: 0,
            };
            let committed = service.artifacts.commit(staging, meta)?;
            job.emit(json!({ "type": "artifact", "jobId": job.id, "artifact": service.artifacts.view(&committed) }));
            service.apply_retention();
            artifact = Some(committed);
        }
    }
    let _ = protocol::write_json(&mut writer, &HostMessage::Finish);
    Ok((exit, artifact))
}

pub fn human(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

impl Service {
    pub(super) fn slot_alive(&self, slot: &str) -> bool {
        let vm = self
            .state
            .lock()
            .unwrap()
            .slots
            .iter()
            .find(|s| s.config.id == slot)
            .map(|s| s.vm.clone());
        vm.is_some_and(|vm| vm.lock().unwrap().as_mut().is_some_and(|vm| vm.alive()))
    }

    pub(super) fn release_slot(&self, slot: &str, alive: bool) {
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state.slots.iter_mut().find(|s| s.config.id == slot) {
            entry.job = None;
            entry.last_used = crate::serial::now_ms();
            if alive {
                entry.state = SlotState::Ready;
            } else {
                entry.state = SlotState::Stopped;
                entry.started_at = None;
                if let Ok(mut vm) = entry.vm.lock() {
                    *vm = None;
                }
            }
            entry.refresh_disk();
        }
        drop(state);
        self.changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_ring_pages_and_reports_truncation() {
        let mut ring = LogRing::default();
        ring.push(b"hello ");
        ring.push(b"world");
        assert_eq!(ring.read(Some(0), 5), (b"hello".to_vec(), 5, false));
        assert_eq!(ring.read(Some(5), 100), (b" world".to_vec(), 11, false));
        assert_eq!(ring.read(None, 5), (b"world".to_vec(), 11, false));
        ring.push(&vec![b'x'; LOG_LIMIT]);
        let (_, _, truncated) = ring.read(Some(0), 10);
        assert!(truncated);
        assert!(ring.end() > LOG_LIMIT as u64);
    }
}
