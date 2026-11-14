//! The resident serial daemon: the single owner of every configured port.
//!
//! - One worker thread per enabled, present port holds the only open handle.
//! - The daemon alone enumerates ports; clients read its snapshot.
//! - A loopback HTTP API, authenticated by a per-launch token stored in a
//!   private file, serves the desktop window and the MCP entry point.
mod node_service;

use super::{
    gateway, grpc,
    h2loader::{self, Events, Reboot, Session},
    logs::{Ring, Source},
    now_ms, remote, state_dir, system_ports, ws, Mode, PortConfig, Protocol, SystemPort,
    UsbIdentity, API_VERSION, BUILD_ID,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{service::service_fn, Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::{HashSet, VecDeque},
    convert::Infallible,
    fs,
    io::{Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::{broadcast, watch, OwnedSemaphorePermit, Semaphore};

const IDLE_EXIT_MS: u64 = 10 * 60 * 1000;
const WATCH_TIMEOUT: Duration = Duration::from_secs(20);
const BODY_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const SOCKET_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const HISTORY: usize = 20;
const MAX_BODY: usize = 64 * 1024;
const MAX_FRAME: usize = 64 * 1024;
const QUEUE_MESSAGES: usize = 64;
const QUEUE_BYTES: usize = 256 * 1024;
const MAX_CONNECTIONS: usize = 64;
const MAX_PIPES: usize = 64;
const WRITE_BATCH: usize = 8;

/// Shared with workers so auth narrowing is checked even before a
/// reconfiguration reaches that port's seat mutex. App sessions are independent.
struct Authority {
    generation: AtomicU64,
    enabled: AtomicBool,
    lan: AtomicBool,
}

impl Authority {
    fn new(config: &gateway::Config) -> Self {
        Self {
            generation: AtomicU64::new(1),
            enabled: AtomicBool::new(config.enabled),
            lan: AtomicBool::new(config.scope == gateway::Scope::Lan),
        }
    }
    fn allows(&self, caller: Caller) -> bool {
        match caller {
            Caller::App => true,
            Caller::Gateway { local, generation } => {
                self.enabled.load(Ordering::SeqCst)
                    && (local || self.lan.load(Ordering::SeqCst))
                    && generation == self.generation.load(Ordering::SeqCst)
            }
        }
    }
}

/// Endpoint reservations survive config path changes, port removal and worker
/// retirement until the actual thread (and therefore hardware handle) exits.
struct EndpointLease {
    owners: Arc<Mutex<HashSet<String>>>,
    path: String,
}

impl EndpointLease {
    fn acquire(owners: &Arc<Mutex<HashSet<String>>>, path: &str) -> Option<Self> {
        owners
            .lock()
            .unwrap()
            .insert(path.to_string())
            .then(|| Self {
                owners: owners.clone(),
                path: path.into(),
            })
    }
}
impl Drop for EndpointLease {
    fn drop(&mut self) {
        self.owners.lock().unwrap().remove(&self.path);
    }
}

/// A byte permit lives until the consumer finishes with the payload, including
/// an in-flight socket/device write. Device-write channels also bound messages.
struct Chunk {
    bytes: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

impl Chunk {
    fn new(bytes: Vec<u8>, budget: &Arc<Semaphore>) -> Result<Self, ()> {
        if bytes.len() > MAX_FRAME {
            return Err(());
        }
        let permit = budget
            .clone()
            .try_acquire_many_owned(bytes.len() as u32)
            .map_err(|_| ())?;
        Ok(Self {
            bytes,
            _permit: permit,
        })
    }
}

/// H2Loader can deliver raw logs one byte per callback. Bound outgoing data by
/// bytes and coalesce adjacent callbacks, rather than treating callback count
/// as evidence of a stalled client. Chunks retain their permits during sends.
struct DeviceOutput {
    pending: Mutex<VecDeque<Chunk>>,
    budget: Arc<Semaphore>,
    ready: tokio::sync::Notify,
}

impl DeviceOutput {
    fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            budget: Arc::new(Semaphore::new(QUEUE_BYTES)),
            ready: tokio::sync::Notify::new(),
        }
    }

    fn push(&self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() > MAX_FRAME {
            return Err(());
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let permit = self
            .budget
            .clone()
            .try_acquire_many_owned(bytes.len() as u32)
            .map_err(|_| ())?;
        let mut pending = self.pending.lock().unwrap();
        if let Some(tail) = pending
            .back_mut()
            .filter(|tail| tail.bytes.len() + bytes.len() <= MAX_FRAME)
        {
            tail.bytes.extend_from_slice(bytes);
            tail._permit.merge(permit);
        } else {
            pending.push_back(Chunk {
                bytes: bytes.to_vec(),
                _permit: permit,
            });
        }
        drop(pending);
        self.ready.notify_one();
        Ok(())
    }

    async fn recv(&self) -> Chunk {
        loop {
            let ready = self.ready.notified();
            let chunk = self.pending.lock().unwrap().pop_front();
            if let Some(chunk) = chunk {
                return chunk;
            }
            ready.await;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteOwner {
    App,
    Pipe { id: u64, caller: Caller },
}

struct WritePacket {
    chunk: Chunk,
    text: String,
    worker: u64,
    owner: WriteOwner,
}

#[derive(Clone)]
struct Writer {
    worker: u64,
    tx: mpsc::SyncSender<Command>,
    budget: Arc<Semaphore>,
}

impl Writer {
    fn send(
        &self,
        bytes: Vec<u8>,
        text: String,
        owner: WriteOwner,
    ) -> Result<(), (StatusCode, String)> {
        let chunk = Chunk::new(bytes, &self.budget)
            .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "串口发送队列已满".into()))?;
        self.tx
            .try_send(Command::Send(WritePacket {
                chunk,
                text,
                worker: self.worker,
                owner,
            }))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    (StatusCode::SERVICE_UNAVAILABLE, "串口发送队列已满".into())
                }
                mpsc::TrySendError::Disconnected(_) => {
                    (StatusCode::CONFLICT, "串口连接已关闭".into())
                }
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Connection {
    Off,
    /// Enabled, but the device is not plugged in.
    Waiting,
    Connecting,
    Connected,
    /// Another program holds the port.
    Occupied,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Activity {
    Idle,
    Handshaking,
    Sending,
    Transferring,
    Rebooting,
    Verifying,
    Reconnecting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    Refresh,
    Stage,
    Install,
    RebootApp,
    RebootLoader,
    RebootUpgrade,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    pub state: JobState,
    pub phase: String,
    pub acknowledged: u64,
    pub total: u64,
    /// Which client asked: "app" or "mcp".
    pub source: String,
    pub created_at: u64,
    pub finished_at: Option<u64>,
    pub error: Option<String>,
    pub package: Option<Value>,
    #[serde(skip)]
    cancel: Arc<AtomicBool>,
    #[serde(skip)]
    snapshot: Option<PathBuf>,
    #[serde(skip)]
    worker: u64,
    #[serde(skip)]
    lease: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Live {
    connection: Connection,
    detected: Option<Protocol>,
    activity: Activity,
    error: Option<String>,
    rx_bytes: u64,
    tx_bytes: u64,
    last_rx_at: Option<u64>,
    last_tx_at: Option<u64>,
    connected_at: Option<u64>,
    h2loader: Option<Value>,
    h2loader_at: Option<u64>,
    job: Option<Job>,
    history: VecDeque<Job>,
    /// Cleared under this mutex before retiring a worker. Its late results
    /// cannot publish Connected/Error over a newer Off/Connecting state.
    #[serde(skip)]
    worker: Option<u64>,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            connection: Connection::Off,
            detected: None,
            activity: Activity::Idle,
            error: None,
            rx_bytes: 0,
            tx_bytes: 0,
            last_rx_at: None,
            last_tx_at: None,
            connected_at: None,
            h2loader: None,
            h2loader_at: None,
            job: None,
            history: VecDeque::new(),
            worker: None,
        }
    }
}

/// The one external WebSocket holder of a piping port.
struct Pipe {
    id: u64,
    client: String,
    /// Work performed by this connection, shown with the client's name.
    title: String,
    /// Optional additional context, shown to the user.
    reason: String,
    /// When it asked (queued) or was granted (holder).
    since: u64,
    /// Actual authenticated identity; the displayed client name is not trusted.
    caller: Caller,
    /// Device bytes for the holder, coalesced into byte-bounded frames.
    out: Arc<DeviceOutput>,
    /// Latest queue/grant/end state. Positions may coalesce; terminal notices
    /// cannot be stuck behind an unbounded queue of stale positions.
    control: watch::Sender<Control>,
    /// Cancels the session-owned job when this socket loses its grant.
    job_cancel: Option<Arc<AtomicBool>>,
    /// Whether the holder receives raw device bytes. Control-only sessions
    /// (for example another GizMoth reading logs separately) opt out.
    relay: bool,
}

/// Text-frame notices to a pipe client. Device bytes are binary frames.
#[derive(Clone)]
enum Control {
    Queued(usize, Option<String>),
    Granted,
    /// Ends the session: rejected while queued, or revoked while holding.
    End(&'static str, String),
}

impl Control {
    fn frame(&self) -> String {
        match self {
            Control::Queued(position, holder) => {
                json!({ "type": "queued", "state": "pending", "position": position, "holder": holder }).to_string()
            }
            Control::Granted => json!({ "type": "granted", "protocol": ws::PROTOCOL }).to_string(),
            Control::End(kind, reason) => json!({ "type": kind, "reason": reason }).to_string(),
        }
    }
}

/// Ownership, transport readiness and physical writes share this mutex.
/// A revocation waits for an in-flight write; queued packets are lease tagged.
struct Seat {
    holder: Option<Pipe>,
    queue: VecDeque<Pipe>,
    approved: Option<u64>,
    mode: Mode,
    auto_grant: bool,
    ready: bool,
    writer: Option<Writer>,
    /// A disconnected holder must finish/cancel its device work before handoff.
    active_job: Option<String>,
}

impl Default for Seat {
    fn default() -> Self {
        Self {
            holder: None,
            queue: VecDeque::new(),
            approved: None,
            mode: Mode::Takeover,
            auto_grant: true,
            ready: false,
            writer: None,
            active_job: None,
        }
    }
}

impl Seat {
    fn notify_positions(&self) {
        let holder = self.holder.as_ref().map(|p| p.client.clone());
        for (index, pipe) in self.queue.iter().enumerate() {
            pipe.control
                .send_replace(Control::Queued(index + 1, holder.clone()));
        }
    }
    fn entry(pipe: &Pipe) -> Value {
        json!({ "id": pipe.id, "client": pipe.client, "title": pipe.title, "reason": pipe.reason, "since": pipe.since })
    }
    fn revoke(&mut self, reason: &str) -> Option<String> {
        let pipe = self.holder.take()?;
        if let Some(cancel) = &pipe.job_cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        pipe.control
            .send_replace(Control::End("revoked", reason.into()));
        self.notify_positions();
        Some(pipe.client)
    }
    fn end(&mut self, reason: &str) {
        self.revoke(reason);
        for pipe in self.queue.drain(..) {
            pipe.control
                .send_replace(Control::End("rejected", reason.into()));
        }
        self.approved = None;
    }
    fn promote(&mut self, authority: &Authority) -> Option<String> {
        if self.mode != Mode::Piping
            || !self.ready
            || self.writer.is_none()
            || self.holder.is_some()
            || self.active_job.is_some()
        {
            return None;
        }
        // Reconfiguration also removes these under this mutex. The epoch check
        // fences a promotion racing with a key change before that removal.
        self.queue.retain(|pipe| {
            if !authority.allows(pipe.caller) {
                pipe.control
                    .send_replace(Control::End("rejected", "Gateway 授权已更改".into()));
                false
            } else {
                true
            }
        });
        let index = match self.approved {
            Some(id) => match self.queue.iter().position(|p| p.id == id) {
                Some(index) => index,
                None => {
                    self.approved = None;
                    if self.auto_grant {
                        0
                    } else {
                        return None;
                    }
                }
            },
            None if self.auto_grant => 0,
            None => return None,
        };
        let mut next = self.queue.remove(index)?;
        self.approved = None;
        next.since = now_ms();
        next.control.send_replace(Control::Granted);
        let name = next.client.clone();
        self.holder = Some(next);
        self.notify_positions();
        Some(name)
    }
    fn revoke_unauthorized(&mut self, authority: &Authority) {
        if self
            .holder
            .as_ref()
            .is_some_and(|pipe| !authority.allows(pipe.caller))
        {
            self.revoke("Gateway 授权已更改");
        }
        self.queue.retain(|pipe| {
            if authority.allows(pipe.caller) {
                true
            } else {
                pipe.control
                    .send_replace(Control::End("rejected", "Gateway 授权已更改".into()));
                false
            }
        });
        if self
            .approved
            .is_some_and(|id| !self.queue.iter().any(|p| p.id == id))
        {
            self.approved = None;
        }
        self.notify_positions();
    }
    fn owns(&self, id: u64, caller: Caller, authority: &Authority) -> bool {
        self.mode == Mode::Piping
            && authority.allows(caller)
            && self
                .holder
                .as_ref()
                .is_some_and(|pipe| pipe.id == id && pipe.caller == caller)
    }
    fn authorize_command(
        &self,
        lease: Option<u64>,
        caller: Caller,
        authority: &Authority,
    ) -> Result<(), (StatusCode, String)> {
        match lease {
            Some(id) if self.owns(id, caller, authority) => Ok(()),
            Some(_) => Err((
                StatusCode::CONFLICT,
                "尚未获得独占通信权，请等待 granted 事件".into(),
            )),
            None if caller == Caller::App
                && self.mode == Mode::Takeover
                && self.holder.is_none() =>
            {
                Ok(())
            }
            None if caller != Caller::App => Err(ws_required()),
            None => Err((
                StatusCode::CONFLICT,
                "WS 独占期间界面及其他发送入口暂停".into(),
            )),
        }
    }
    fn authorize_device_command(
        &self,
        lease: Option<u64>,
        caller: Caller,
        authority: &Authority,
    ) -> Result<(), (StatusCode, String)> {
        self.authorize_command(lease, caller, authority)?;
        if !self.ready {
            return Err((StatusCode::SERVICE_UNAVAILABLE,
                "Device unavailable; the WebSocket lease is retained. Wait for a device event with ready:true, or disconnect to release".into()));
        }
        Ok(())
    }
    fn permits(&self, packet: &WritePacket, authority: &Authority) -> bool {
        if !self.ready
            || self
                .writer
                .as_ref()
                .is_none_or(|w| w.worker != packet.worker)
        {
            return false;
        }
        match packet.owner {
            WriteOwner::App => self.mode == Mode::Takeover,
            WriteOwner::Pipe { id, caller } => {
                self.mode == Mode::Piping
                    && authority.allows(caller)
                    && self
                        .holder
                        .as_ref()
                        .is_some_and(|p| p.id == id && p.caller == caller)
            }
        }
    }
}

/// Physical device availability is independent of a WebSocket's ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum DeviceState {
    Disconnected,
    Connecting,
    Reconnecting,
    Connected,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceEvent {
    #[serde(rename = "type")]
    kind: &'static str,
    sequence: u64,
    at: u64,
    state: DeviceState,
    connection: Connection,
    ready: bool,
    reason: Option<String>,
}

impl Default for DeviceEvent {
    fn default() -> Self {
        Self {
            kind: "device",
            sequence: 0,
            at: now_ms(),
            state: DeviceState::Disconnected,
            connection: Connection::Off,
            ready: false,
            reason: Some("串口尚未连接".into()),
        }
    }
}

pub struct Shared {
    live: Mutex<Live>,
    logs: Mutex<Ring>,
    dirty: Arc<AtomicBool>,
    seat: Mutex<Seat>,
    device_state: Mutex<DeviceEvent>,
    device_events: broadcast::Sender<DeviceEvent>,
    next_worker: AtomicU64,
    authority: Arc<Authority>,
    /// Persist the fact that an Auto port has previously answered as H2Loader.
    h2loader_seen: AtomicBool,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut Live)) {
        f(&mut self.live.lock().unwrap());
        self.dirty.store(true, Ordering::SeqCst);
    }
    fn worker_update(&self, worker: u64, f: impl FnOnce(&mut Live)) -> bool {
        let mut live = self.live.lock().unwrap();
        if live.worker != Some(worker) {
            return false;
        }
        f(&mut live);
        self.dirty.store(true, Ordering::SeqCst);
        true
    }
    fn system(&self, text: impl Into<String>) {
        self.logs
            .lock()
            .unwrap()
            .push(Source::System, now_ms(), text);
        self.dirty.store(true, Ordering::SeqCst);
    }
    fn rx(&self, bytes: &[u8]) {
        let now = now_ms();
        self.logs.lock().unwrap().push_rx(bytes, now);
        let mut live = self.live.lock().unwrap();
        live.rx_bytes += bytes.len() as u64;
        live.last_rx_at = Some(now);
        self.dirty.store(true, Ordering::SeqCst);
    }
    fn tx(&self, bytes: &[u8], text: &str) {
        let now = now_ms();
        self.logs.lock().unwrap().push(Source::Tx, now, text);
        let mut live = self.live.lock().unwrap();
        live.tx_bytes += bytes.len() as u64;
        live.last_tx_at = Some(now);
        self.dirty.store(true, Ordering::SeqCst);
    }
    /// A slow client loses its lease explicitly; raw bytes are never dropped
    /// while keeping a supposedly healthy protocol session alive.
    fn relay(&self, bytes: &[u8]) {
        let mut seat = self.seat.lock().unwrap();
        let full = seat
            .holder
            .as_ref()
            .is_some_and(|pipe| pipe.relay && pipe.out.push(bytes).is_err());
        if full {
            seat.revoke("客户端接收过慢，WS 独占会话已结束");
            seat.promote(&self.authority);
            self.dirty.store(true, Ordering::SeqCst);
        }
    }
    /// Test helper for verifying physical-write/revocation serialization.
    #[cfg(test)]
    fn revoke(&self, reason: &str) {
        let client = self.seat.lock().unwrap().revoke(reason);
        if let Some(client) = client {
            self.system(format!("{client} 的占据已结束：{reason}"));
        }
    }
    fn publish_device(&self, state: DeviceState, connection: Connection, reason: Option<String>) {
        let ready = state == DeviceState::Connected;
        let mut current = self.device_state.lock().unwrap();
        if current.state == state
            && current.connection == connection
            && current.ready == ready
            && current.reason == reason
        {
            return;
        }
        *current = DeviceEvent {
            kind: "device",
            sequence: current.sequence.saturating_add(1),
            at: now_ms(),
            state,
            connection,
            ready,
            reason,
        };
        // Retain individual transitions even if unplug/reconnect happens between
        // daemon snapshot ticks. Slow receivers resynchronize without losing a lease.
        let _ = self.device_events.send(current.clone());
        self.dirty.store(true, Ordering::SeqCst);
    }
    fn transport_change(
        &self,
        worker: u64,
        state: DeviceState,
        connection: Connection,
        reason: &str,
    ) {
        let mut seat = self.seat.lock().unwrap();
        if seat.writer.as_ref().is_none_or(|w| w.worker != worker) {
            return;
        }
        seat.ready = false;
        self.worker_update(worker, |live| {
            live.connection = connection;
            live.connected_at = None;
            if matches!(connection, Connection::Error | Connection::Occupied) {
                live.error = Some(reason.into());
            }
        });
        self.publish_device(state, connection, Some(reason.into()));
    }
    fn transport_down(&self, worker: u64, connection: Connection, reason: &str) {
        self.transport_change(worker, DeviceState::Disconnected, connection, reason);
    }
    fn transport_ready(&self, worker: u64) -> bool {
        let mut seat = self.seat.lock().unwrap();
        if seat.writer.as_ref().is_none_or(|w| w.worker != worker) {
            return false;
        }
        seat.ready = true;
        self.worker_update(worker, |live| {
            if live.connection != Connection::Connected || live.connected_at.is_none() {
                live.connected_at = Some(now_ms());
            }
            live.connection = Connection::Connected;
            live.error = None;
        });
        if let Some(client) = seat.promote(&self.authority) {
            self.system(format!("授予 {client} 串口使用权"));
        }
        self.publish_device(DeviceState::Connected, Connection::Connected, None);
        true
    }
    fn flush_partial(&self) {
        if self.logs.lock().unwrap().flush_partial(now_ms()) {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }
    fn holder(&self, running: bool) -> Value {
        let seat = self.seat.lock().unwrap();
        if let Some(pipe) = seat.holder.as_ref() {
            let mut entry = Seat::entry(pipe);
            entry["kind"] = json!("client");
            return entry;
        }
        match seat.mode {
            Mode::Takeover if running => json!({ "kind": "gizmoth" }),
            _ => Value::Null,
        }
    }
    fn queue(&self) -> Value {
        json!(self
            .seat
            .lock()
            .unwrap()
            .queue
            .iter()
            .map(Seat::entry)
            .collect::<Vec<_>>())
    }
    fn flush_stale(&self) {
        if self.logs.lock().unwrap().flush_stale(now_ms()) {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }
    fn job_events(
        &self,
        lease: u64,
        completed: &mut HashSet<String>,
        current: &mut Value,
    ) -> Vec<Value> {
        let seat = self.seat.lock().unwrap();
        if !seat.holder.as_ref().is_some_and(|pipe| pipe.id == lease) {
            return Vec::new();
        }
        let live = self.live.lock().unwrap();
        let mut events = Vec::new();
        // Progress may coalesce, but report each retained completion before a
        // newer job so that starting another command cannot hide a terminal state.
        for job in live
            .history
            .iter()
            .rev()
            .filter(|job| job.lease == Some(lease))
        {
            if completed.insert(job.id.clone()) {
                events.push(json!({"type":"job", "job":job}));
            }
        }
        if let Some(job) = live.job.as_ref().filter(|job| job.lease == Some(lease)) {
            let event = json!({"type":"job", "job":job});
            if *current != event {
                *current = event.clone();
                events.push(event);
            }
        }
        events
    }

    fn release_job(&self, id: &str) {
        let mut seat = self.seat.lock().unwrap();
        if seat.active_job.as_deref() == Some(id) {
            seat.active_job = None;
            seat.promote(&self.authority);
            self.dirty.store(true, Ordering::SeqCst);
        }
    }
    fn finish_job(&self, worker: u64, result: Result<(), String>) {
        let mut finished = None;
        self.update(|live| {
            if live.job.as_ref().is_none_or(|job| job.worker != worker) {
                return;
            }
            if let Some(mut job) = live.job.take() {
                job.finished_at = Some(now_ms());
                match result {
                    Ok(()) => {
                        job.state = JobState::Succeeded;
                        job.phase = "done".into();
                    }
                    Err(e) if e == "cancelled" || job.cancel.load(Ordering::SeqCst) => {
                        job.state = JobState::Cancelled;
                        job.error = Some(h2loader::describe("cancelled"));
                    }
                    Err(e) => {
                        job.state = JobState::Failed;
                        job.error = Some(h2loader::describe(&e));
                    }
                }
                if let Some(path) = job.snapshot.take() {
                    let _ = fs::remove_file(path);
                }
                finished = Some(job.clone());
                live.history.push_front(job);
                live.history.truncate(HISTORY);
            }
            if live.worker == Some(worker) {
                live.activity = Activity::Idle;
            }
        });
        if let Some(job) = finished {
            self.release_job(&job.id);
            let text = match job.state {
                JobState::Succeeded => format!("任务完成：{}", job_label(job.kind)),
                JobState::Cancelled => format!("任务已取消：{}", job_label(job.kind)),
                _ => format!(
                    "任务失败：{}（{}）",
                    job_label(job.kind),
                    job.error.unwrap_or_default()
                ),
            };
            self.system(text);
        }
    }
}

fn job_label(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Refresh => "读取状态",
        JobKind::Stage => "发送固件",
        JobKind::Install => "安装固件",
        JobKind::RebootApp => "重启到 App",
        JobKind::RebootLoader => "重启到 Loader",
        JobKind::RebootUpgrade => "重启升级",
    }
}

/// Device commands a job needs, as named in `h2loader::COMMANDS`.
fn job_commands(kind: JobKind) -> &'static [&'static str] {
    match kind {
        JobKind::Refresh => &["status"],
        JobKind::Stage => &["stagePayload"],
        JobKind::Install => &["stagePayload", "rebootUpgrade"],
        JobKind::RebootApp => &["rebootApp"],
        JobKind::RebootLoader => &["rebootLoader"],
        JobKind::RebootUpgrade => &["rebootUpgrade"],
    }
}

fn command_label(command: &str) -> &'static str {
    match command {
        "status" => "读取状态",
        "stagePayload" => "接收固件",
        "rebootUpgrade" => "重启升级",
        "rebootApp" => "重启到 App",
        "rebootLoader" => "重启到 Loader",
        _ => "未知命令",
    }
}

enum Command {
    Job,
    Send(WritePacket),
}

struct Worker {
    id: u64,
    tx: mpsc::SyncSender<Command>,
    stop: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct Port {
    config: PortConfig,
    present: bool,
    shared: Arc<Shared>,
    worker: Option<Worker>,
    /// A stopped worker still releasing the device; no new worker starts first.
    stopping: Option<JoinHandle<()>>,
    /// Reservation established before copying/inspecting a firmware package.
    preparing: Option<String>,
}

struct PendingListeners {
    listeners: Arc<gateway::Listeners>,
    network_refresh: bool,
}

pub struct Daemon {
    dir: PathBuf,
    /// Where the single HTTP/WebSocket server currently listens.
    bound: Mutex<SocketAddr>,
    gateway: Mutex<gateway::Config>,
    /// Why the configured Gateway address could not be bound, if it failed.
    gateway_error: Mutex<Option<String>>,
    /// Bound sockets retained for transactional reconfiguration.
    listeners: Mutex<Option<Arc<gateway::Listeners>>>,
    /// A newly prepared listener set waiting for the accept loop to adopt it.
    pending: Mutex<Option<PendingListeners>>,
    rebind: watch::Sender<u64>,
    next_pipe: AtomicU64,
    token: String,
    instance_id: String,
    instance_generation: u64,
    started_at: u64,
    /// Lock order: reconfiguration -> admission -> ports -> seat -> live.
    /// Gateway reads never retain their mutex while acquiring ports.
    reconfiguration: Mutex<()>,
    admission: Mutex<bool>,
    authority: Arc<Authority>,
    pipe_slots: Arc<Semaphore>,
    ports: Mutex<Vec<Port>>,
    /// IDs are retained for this daemon lifetime, including after port removal.
    job_ids: Mutex<HashSet<String>>,
    system: Mutex<Vec<SystemPort>>,
    /// Removed ports still have to finish releasing their hardware handles.
    retired_workers: Mutex<Vec<JoinHandle<()>>>,
    endpoints: Arc<Mutex<HashSet<String>>>,
    dirty: Arc<AtomicBool>,
    version: watch::Sender<u64>,
    last_request: AtomicU64,
    shutdown: watch::Sender<bool>,
    /// This service's persistent identity as a node of a cascade.
    node: grpc::NodeIdentity,
    /// Firmware packages uploaded through gRPC, installed by package ID.
    uploads: Mutex<Vec<node_service::Upload>>,
    /// Other GizMoth services whose ports this one imports.
    remotes: Arc<remote::Remotes>,
    /// Linux build VMs, shared caches and build artifacts.
    #[cfg(unix)]
    compile: Arc<crate::compile::Service>,
}

// ---------------------------------------------------------------- workers

#[cfg_attr(not(gizmoth_serial_core), allow(dead_code))]
struct H2Events<'a> {
    shared: &'a Shared,
    worker: u64,
    stop: &'a AtomicBool,
    interrupt: &'a AtomicBool,
    /// Shared ownership keeps the active job's cancellation flag alive.
    job_cancel: RefCell<Option<Arc<AtomicBool>>>,
    /// Only the passive monitor needs idle yielding, not connection/status I/O.
    monitoring: Cell<bool>,
    /// Device output arrived since the previous cancellation check.
    active: Cell<bool>,
}

/// Host Core's passive monitor polls without blocking when the line is quiet,
/// and checks cancellation once per round. Yield there so an idle port does
/// not spin a core; rounds that carried output continue immediately.
#[cfg_attr(not(gizmoth_serial_core), allow(dead_code))]
const IDLE_YIELD: Duration = Duration::from_millis(8);

impl Events for H2Events<'_> {
    fn log(&self, bytes: &[u8]) {
        self.active.set(true);
        self.shared.relay(bytes);
        self.shared.rx(bytes);
    }
    fn cancelled(&self) -> bool {
        self.shared.flush_stale();
        if self.stop.load(Ordering::SeqCst) {
            return true;
        }
        match self.job_cancel.borrow().as_ref() {
            Some(flag) => flag.load(Ordering::SeqCst),
            None => {
                if self.monitoring.get()
                    && !self.active.replace(false)
                    && !self.interrupt.load(Ordering::SeqCst)
                {
                    std::thread::sleep(IDLE_YIELD);
                }
                self.interrupt.load(Ordering::SeqCst)
            }
        }
    }
    fn progress(&self, acknowledged: u64, total: u64) {
        self.shared.worker_update(self.worker, |live| {
            let Some(job) = live.job.as_mut() else { return };
            let delta = acknowledged.saturating_sub(job.acknowledged);
            job.acknowledged = acknowledged;
            job.total = total;
            // Firmware bytes count as traffic sent to the device.
            live.tx_bytes += delta;
            live.last_tx_at = Some(now_ms());
        });
    }
}

enum Outcome {
    Stopped,
    /// Auto mode: the port answered, but not as H2Loader.
    NotH2loader(String),
    Failed(Connection, String),
}

fn sleep_unless(stop: &AtomicBool, ms: u64) {
    let mut left = ms;
    while left > 0 && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(left.min(100)));
        left = left.saturating_sub(100);
    }
}

fn fail_pending(rx: &mpsc::Receiver<Command>, shared: &Shared, worker: u64, reason: &str) {
    while let Ok(command) = rx.try_recv() {
        if let Command::Job = command {
            shared.finish_job(worker, Err(reason.to_string()));
        }
    }
}

fn run_worker(
    config: PortConfig,
    shared: Arc<Shared>,
    worker: u64,
    rx: mpsc::Receiver<Command>,
    stop: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
) {
    let mut mode = config.protocol;
    let mut attempt = 0u32;
    while !stop.load(Ordering::SeqCst) {
        if !shared.worker_update(worker, |live| {
            live.connection = Connection::Connecting;
            live.activity = if mode == Protocol::Serial {
                Activity::Idle
            } else {
                Activity::Handshaking
            };
        }) {
            break;
        }
        shared.transport_change(
            worker,
            DeviceState::Connecting,
            Connection::Connecting,
            "正在连接串口",
        );
        let outcome = match mode {
            Protocol::Serial => run_raw(
                &config,
                config.baud,
                &shared,
                worker,
                &rx,
                &stop,
                mode != config.protocol,
            ),
            _ => run_h2loader(
                &config,
                &shared,
                worker,
                &rx,
                &stop,
                &interrupt,
                mode == Protocol::Auto,
            ),
        };
        // Every attempt/session is a boundary, including errors, Auto fallback
        // and stop. A short final RX line must not join the next session.
        shared.flush_partial();
        match &outcome {
            Outcome::Failed(connection, reason) => {
                shared.transport_down(worker, *connection, reason)
            }
            Outcome::NotH2loader(reason) => shared.transport_change(
                worker,
                DeviceState::Connecting,
                Connection::Connecting,
                &h2loader::describe(reason),
            ),
            Outcome::Stopped => {
                shared.transport_down(worker, Connection::Off, "串口工作进程已停止")
            }
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match outcome {
            Outcome::Stopped => break,
            Outcome::NotH2loader(reason) => {
                shared.system(format!(
                    "未识别到 H2Loader（{}），按普通串口监听",
                    h2loader::describe(&reason)
                ));
                mode = Protocol::Serial;
            }
            Outcome::Failed(state, reason) => {
                attempt += 1;
                if attempt == 1 || attempt.is_multiple_of(10) {
                    shared.system(format!("连接失败：{reason}，稍后重试"));
                }
                if !shared.worker_update(worker, |live| {
                    live.connection = state;
                    live.activity = Activity::Idle;
                    live.error = Some(reason.clone());
                    live.connected_at = None;
                }) {
                    break;
                }
                fail_pending(&rx, &shared, worker, "closed");
                sleep_unless(
                    &stop,
                    if state == Connection::Occupied {
                        3000
                    } else {
                        2000
                    },
                );
            }
        }
    }
    shared.flush_partial();
    fail_pending(&rx, &shared, worker, "cancelled");
    shared.finish_job(worker, Err("cancelled".into()));
}

fn occupied(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("busy") || text.contains("access is denied") || text.contains("in use")
}

/// The seat is retained through the actual OS writes. Taking a new
/// lease/revoking the old one therefore waits for its in-flight bytes, and no
/// packet from that old lease can start afterwards.
fn write_packet(
    device: &mut impl Write,
    shared: &Shared,
    packet: WritePacket,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let seat = shared.seat.lock().unwrap();
    if stop.load(Ordering::SeqCst) || !seat.permits(&packet, &shared.authority) {
        return Ok(()); // deliberately invalidate the revoked lease's backlog
    }
    shared.worker_update(packet.worker, |live| live.activity = Activity::Sending);
    let result = (|| {
        let deadline = std::time::Instant::now() + SOCKET_SEND_TIMEOUT;
        let mut remaining = packet.chunk.bytes.as_slice();
        while !remaining.is_empty() {
            if stop.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "cancelled",
                ));
            }
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "serial write timed out",
                ));
            }
            match device.write(&remaining[..remaining.len().min(4096)]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "serial write returned zero",
                    ))
                }
                Ok(written) => remaining = &remaining[written..],
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    sleep_unless(stop, 8);
                }
                Err(error) => return Err(error),
            }
        }
        // Serial writes already enter the ordered OS transmit queue. A per-frame
        // flush would call unbounded tcdrain/FlushFileBuffers and defeat stop,
        // revocation and the deadline. Handover preserves that queue's order.
        Ok(())
    })();
    shared.worker_update(packet.worker, |live| live.activity = Activity::Idle);
    if result.is_ok() {
        shared.tx(&packet.chunk.bytes, &packet.text);
    }
    drop(seat);
    result
}

fn run_raw(
    config: &PortConfig,
    baud: u32,
    shared: &Shared,
    worker: u64,
    rx: &mpsc::Receiver<Command>,
    stop: &AtomicBool,
    fallback: bool,
) -> Outcome {
    // Ask the driver to preserve DTR rather than actively clearing it. Unix
    // drivers may still assert DTR during open (notably Linux); Windows does
    // not assert it by default. This is not a reset-free guarantee.
    let builder = serialport::new(&config.path, baud)
        .timeout(Duration::from_millis(100))
        .preserve_dtr_on_open();
    #[cfg(unix)]
    let port = builder.open_native().and_then(|port| {
        use std::os::fd::AsRawFd;
        let fd = port.as_raw_fd();
        // serialport polls with a timeout but clears O_NONBLOCK after open.
        // A large blocking write after poll can still stall on a full driver
        // buffer. Keep the FD nonblocking and retry with our bounded deadline.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Box::new(port) as Box<dyn serialport::SerialPort>)
    });
    #[cfg(not(unix))]
    let port = builder.open();
    let mut port = match port {
        Ok(port) => port,
        Err(error) => {
            let text = error.to_string();
            return if occupied(&text) {
                Outcome::Failed(Connection::Occupied, "串口被其他程序占用".into())
            } else {
                Outcome::Failed(Connection::Error, text)
            };
        }
    };
    if !shared.worker_update(worker, |live| {
        live.connection = Connection::Connected;
        live.detected = Some(Protocol::Serial);
        live.error = None;
        live.activity = Activity::Idle;
        live.connected_at = Some(now_ms());
        live.h2loader = None;
    }) || stop.load(Ordering::SeqCst)
        || !shared.transport_ready(worker)
    {
        return Outcome::Stopped;
    }
    if fallback {
        shared.system(format!("已连接（{baud} baud）"));
    } else {
        shared.system(format!("已连接（普通串口，{baud} baud）"));
    }
    let mut buffer = [0u8; 4096];
    loop {
        if stop.load(Ordering::SeqCst) {
            return Outcome::Stopped;
        }
        // Bound work per round so device reads and stop checks cannot starve.
        for _ in 0..WRITE_BATCH {
            if stop.load(Ordering::SeqCst) {
                return Outcome::Stopped;
            }
            let Ok(command) = rx.try_recv() else { break };
            match command {
                Command::Send(packet) => {
                    if let Err(e) = write_packet(&mut port, shared, packet, stop) {
                        return Outcome::Failed(Connection::Error, e.to_string());
                    }
                }
                Command::Job => shared.finish_job(worker, Err("unsupported".into())),
            }
        }
        match port.read(&mut buffer) {
            Ok(0) => {}
            Ok(n) => {
                shared.relay(&buffer[..n]);
                shared.rx(&buffer[..n]);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => return Outcome::Failed(Connection::Error, format!("串口连接中断：{e}")),
        }
        shared.flush_stale();
    }
}

fn h2loader_open_failure(name: String, auto: bool, known_h2loader: bool) -> Outcome {
    if name == "busy" {
        return Outcome::Failed(Connection::Occupied, h2loader::describe(&name));
    }
    // Only a failed protocol probe may select plain serial. Retrying an open
    // rejected by the PAL would bypass its ownership and access checks.
    // A known H2Loader can be silent while rebooting. Preserve its protocol
    // and retry instead of declaring raw serial ready at the configured baud.
    if auto
        && !known_h2loader
        && matches!(
            name.as_str(),
            "timeout" | "format" | "truncated" | "core_unavailable"
        )
    {
        return Outcome::NotH2loader(name);
    }
    Outcome::Failed(Connection::Error, h2loader::describe(&name))
}

fn run_h2loader(
    config: &PortConfig,
    shared: &Shared,
    worker: u64,
    rx: &mpsc::Receiver<Command>,
    stop: &AtomicBool,
    interrupt: &AtomicBool,
    auto: bool,
) -> Outcome {
    let events = H2Events {
        shared,
        worker,
        stop,
        interrupt,
        job_cancel: RefCell::new(None),
        monitoring: Cell::new(false),
        active: Cell::new(false),
    };
    let mut session = match Session::open(&config.path, if auto { 4000 } else { 8000 }, &events) {
        Ok(session) => session,
        Err((_, name)) => {
            shared.flush_stale();
            if stop.load(Ordering::SeqCst) {
                return Outcome::Stopped;
            }
            return h2loader_open_failure(
                name,
                auto,
                config.h2loader_seen || shared.h2loader_seen.load(Ordering::SeqCst),
            );
        }
    };
    if !shared.worker_update(worker, |live| {
        live.connection = Connection::Connected;
        live.detected = Some(Protocol::H2loader);
        shared.h2loader_seen.store(true, Ordering::SeqCst);
        live.error = None;
        live.activity = Activity::Idle;
        live.connected_at = Some(now_ms());
    }) || stop.load(Ordering::SeqCst)
    {
        return Outcome::Stopped;
    }
    match session.status().map(|mut status| {
        h2loader::annotate(&mut status);
        status
    }) {
        Ok(status) => {
            shared.system(format!(
                "H2Loader 已连接：{} {} · {} {}",
                status["board"].as_str().unwrap_or("?"),
                status["target"].as_str().unwrap_or(""),
                status["active"]["role"].as_str().unwrap_or("unknown"),
                status["active"]["version"].as_str().unwrap_or(""),
            ));
            shared.worker_update(worker, |live| {
                live.h2loader = Some(status);
                live.h2loader_at = Some(now_ms());
            });
        }
        Err(error) => return Outcome::Failed(Connection::Error, h2loader::describe(&error)),
    }
    if !shared.transport_ready(worker) {
        return Outcome::Stopped;
    }
    loop {
        if stop.load(Ordering::SeqCst) {
            return Outcome::Stopped;
        }
        interrupt.store(false, Ordering::SeqCst);
        match rx.try_recv() {
            Ok(Command::Job) => {
                if let Some(reason) = run_h2loader_job(&mut session, &events, shared) {
                    return Outcome::Failed(Connection::Error, reason);
                }
                continue;
            }
            Ok(Command::Send(..)) => {
                shared.system("H2Loader 会话不接受原始数据；如需直接收发，请切换为普通串口协议");
                continue;
            }
            Err(mpsc::TryRecvError::Disconnected) => return Outcome::Stopped,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        *events.job_cancel.borrow_mut() = None;
        events.monitoring.set(true);
        let rc = session.monitor();
        events.monitoring.set(false);
        if rc != 0 && rc != h2loader::CANCELLED {
            let name = h2loader::name(rc);
            shared.system(format!("H2Loader 连接中断：{}", h2loader::describe(&name)));
            shared.worker_update(worker, |live| live.activity = Activity::Reconnecting);
            return Outcome::Failed(Connection::Error, h2loader::describe(&name));
        }
    }
}

fn run_h2loader_job(session: &mut Session, events: &H2Events, shared: &Shared) -> Option<String> {
    let worker = events.worker;
    let (kind, cancel, snapshot) = shared
        .live
        .lock()
        .unwrap()
        .job
        .as_ref()
        .map(|job| (job.kind, job.cancel.clone(), job.snapshot.clone()))?;
    if cancel.load(Ordering::SeqCst) {
        shared.finish_job(worker, Err("cancelled".into()));
        return None;
    }
    *events.job_cancel.borrow_mut() = Some(cancel);
    let phase = |activity: Activity, phase: &str| {
        shared.worker_update(worker, |live| {
            live.activity = activity;
            if let Some(job) = live.job.as_mut() {
                job.state = JobState::Running;
                job.phase = phase.into();
            }
        });
        if activity == Activity::Rebooting {
            shared.transport_change(
                worker,
                DeviceState::Reconnecting,
                Connection::Connecting,
                "设备正在重启，等待重新连接",
            );
        }
    };
    shared.system(format!("开始任务：{}", job_label(kind)));
    let path = snapshot.as_ref().map(|p| p.to_string_lossy().into_owned());
    let result = (|| -> Result<(), String> {
        match kind {
            JobKind::Refresh => phase(Activity::Verifying, "status"),
            JobKind::Stage | JobKind::Install => {
                let path = path.as_deref().ok_or("invalid_argument")?;
                phase(Activity::Transferring, "stage");
                session.stage(path)?;
                if kind == JobKind::Install {
                    phase(Activity::Rebooting, "rebootUpgrade");
                    session.reboot(Reboot::Upgrade)?;
                }
            }
            JobKind::RebootApp => {
                phase(Activity::Rebooting, "rebootApp");
                session.reboot(Reboot::App)?;
            }
            JobKind::RebootLoader => {
                phase(Activity::Rebooting, "rebootLoader");
                session.reboot(Reboot::Loader)?;
            }
            JobKind::RebootUpgrade => {
                phase(Activity::Rebooting, "rebootUpgrade");
                session.reboot(Reboot::Upgrade)?;
            }
        }
        phase(Activity::Verifying, "verify");
        let mut status = session.status()?;
        h2loader::annotate(&mut status);
        shared.worker_update(worker, |live| {
            live.h2loader = Some(status);
            live.h2loader_at = Some(now_ms());
        });
        Ok(())
    })();
    *events.job_cancel.borrow_mut() = None;
    let reconnect = result.as_ref().err().map(|error| h2loader::describe(error));
    if let Some(reason) = &reconnect {
        // Invalidate the old transport before completing the job so a released
        // socket's successor cannot be granted an unusable native session.
        shared.transport_down(worker, Connection::Error, reason);
    } else {
        shared.transport_ready(worker);
    }
    shared.finish_job(worker, result);
    reconnect
}

// ---------------------------------------------------------------- daemon

fn private_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::credentials::atomic_write(path, bytes).map_err(std::io::Error::other)
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    ports: Vec<PortConfig>,
}

pub fn load_configs(dir: &Path) -> Vec<PortConfig> {
    fs::read(dir.join("ports.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        .map(|saved| saved.ports)
        .unwrap_or_default()
}

/// Daemon start-up only: an unreadable ports.json is moved aside rather than
/// silently replaced by the next save.
fn adopt_configs(dir: &Path) -> Vec<PortConfig> {
    let path = dir.join("ports.json");
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Saved>(&bytes) {
            Ok(saved) => saved.ports,
            Err(_) => {
                let _ = fs::rename(
                    &path,
                    dir.join(format!("ports.unreadable-{}.json", now_ms())),
                );
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Prefer exact configured paths. Following a USB identity requires uniqueness
/// in both the saved configurations and the live enumeration. Reservations are
/// updated after each adoption, so even malformed/legacy duplicate records can
/// never create two owners of one endpoint.
fn same_usb(a: &UsbIdentity, b: &UsbIdentity) -> bool {
    a.serial.as_ref().is_some_and(|serial| !serial.is_empty())
        && a.vid == b.vid
        && a.pid == b.pid
        && a.serial == b.serial
}

fn compatible_usb(saved: &UsbIdentity, found: &UsbIdentity) -> bool {
    // Product/manufacturer strings may change between App and Loader firmware.
    // VID/PID and a known serial identify the device; descriptions are display.
    saved.vid == found.vid
        && saved.pid == found.pid
        && saved
            .serial
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_none_or(|serial| found.serial.as_deref() == Some(serial))
}

fn resolve_paths(configs: &[PortConfig], system: &[SystemPort]) -> Vec<Option<String>> {
    use std::collections::HashSet;
    let mut reserved: HashSet<String> = configs.iter().map(|p| p.path.clone()).collect();
    let mut assigned = HashSet::new();
    configs
        .iter()
        .map(|config| {
            if let Some(exact) = system.iter().find(|s| {
                s.path == config.path
                    && config.usb.as_ref().is_none_or(|usb| {
                        s.usb
                            .as_ref()
                            .is_some_and(|found| compatible_usb(usb, found))
                    })
            }) {
                if assigned.insert(exact.path.clone()) {
                    return Some(exact.path.clone());
                }
                return None;
            }
            let usb = config.usb.as_ref()?;
            if configs
                .iter()
                .filter(|p| p.usb.as_ref().is_some_and(|u| same_usb(u, usb)))
                .count()
                != 1
            {
                return None;
            }
            let mut matches = system
                .iter()
                .filter(|s| s.usb.as_ref().is_some_and(|u| same_usb(u, usb)));
            let only = matches.next()?;
            if matches.next().is_some() || !reserved.insert(only.path.clone()) {
                return None;
            }
            assigned
                .insert(only.path.clone())
                .then(|| only.path.clone())
        })
        .collect()
}

impl Daemon {
    fn persist_configs(&self, ports: Vec<PortConfig>) -> std::io::Result<()> {
        let saved = Saved { version: 1, ports };
        private_write(
            &self.dir.join("ports.json"),
            &serde_json::to_vec_pretty(&saved)?,
        )
    }

    fn save(&self, ports: &[Port]) {
        let _ = self.persist_configs(ports.iter().map(|p| p.config.clone()).collect());
    }

    fn new_shared(&self) -> Arc<Shared> {
        Arc::new(Shared {
            live: Mutex::new(Live::default()),
            logs: Mutex::new(Ring::default()),
            dirty: self.dirty.clone(),
            seat: Mutex::new(Seat::default()),
            device_state: Mutex::new(DeviceEvent::default()),
            device_events: broadcast::channel(64).0,
            next_worker: AtomicU64::new(1),
            authority: self.authority.clone(),
            h2loader_seen: AtomicBool::new(false),
        })
    }

    fn stop_worker(port: &mut Port, next: Connection) {
        let shared = port.shared.clone();
        let mut seat = shared.seat.lock().unwrap();
        Self::stop_worker_locked(port, next, &mut seat);
    }

    /// Also used by acceptance while its target request is reserved under seat.
    fn stop_worker_locked(port: &mut Port, next: Connection, seat: &mut Seat) {
        // Retire the publication epoch while holding live, serialized against
        // late open/handshake results. The seat also waits out physical writes.
        seat.ready = false;
        seat.writer = None;
        let (state, reason) = match next {
            Connection::Connecting => (DeviceState::Connecting, "串口正在重新连接"),
            Connection::Waiting => (DeviceState::Disconnected, "设备已断开，等待重新接入"),
            _ => (DeviceState::Disconnected, "串口已停用"),
        };
        port.shared.publish_device(state, next, Some(reason.into()));
        let mut live = port.shared.live.lock().unwrap();
        live.worker = None;
        if let Some(worker) = port.worker.take() {
            worker.stop.store(true, Ordering::SeqCst);
            worker.interrupt.store(true, Ordering::SeqCst);
            if let Some(job) = live.job.as_ref() {
                job.cancel.store(true, Ordering::SeqCst);
            }
            debug_assert!(port.stopping.is_none());
            port.stopping = Some(worker.handle);
        }
        live.connection = next;
        live.activity = Activity::Idle;
        live.connected_at = None;
        if next == Connection::Off {
            live.error = None;
        }
        port.shared.dirty.store(true, Ordering::SeqCst);
    }

    fn start_worker(&self, port: &mut Port) {
        let Some(endpoint) = EndpointLease::acquire(&self.endpoints, &port.config.path) else {
            return;
        };
        let (tx, rx) = mpsc::sync_channel(QUEUE_MESSAGES);
        let stop = Arc::new(AtomicBool::new(false));
        let interrupt = Arc::new(AtomicBool::new(false));
        let id = port.shared.next_worker.fetch_add(1, Ordering::SeqCst);
        let config = port.config.clone();
        let shared = port.shared.clone();
        {
            let mut seat = shared.seat.lock().unwrap();
            seat.auto_grant = config.auto_grant;
            seat.ready = false;
            seat.writer = Some(Writer {
                worker: id,
                tx: tx.clone(),
                budget: Arc::new(Semaphore::new(QUEUE_BYTES)),
            });
            shared.update(|live| {
                live.worker = Some(id);
                live.connection = Connection::Connecting;
                live.detected = None;
                live.error = None;
            });
            shared.publish_device(
                DeviceState::Connecting,
                Connection::Connecting,
                Some("正在连接串口".into()),
            );
        }
        let (s, i) = (stop.clone(), interrupt.clone());
        let handle = std::thread::Builder::new()
            .name(format!("serial-{}", port.config.id))
            .spawn(move || {
                let _endpoint = endpoint;
                run_worker(config, shared, id, rx, s, i);
            })
            .expect("spawn serial worker");
        port.worker = Some(Worker {
            id,
            tx,
            stop,
            interrupt,
            handle,
        });
    }

    /// Single enumeration point: clients read this cached enumeration.
    fn reconcile(&self) {
        self.reconcile_system(system_ports());
    }

    fn reconcile_system(&self, system: Vec<SystemPort>) {
        let admission = self.admission.lock().unwrap();
        if !*admission {
            return;
        }
        *self.system.lock().unwrap() = system.clone();
        let mut ports = self.ports.lock().unwrap();
        let configs: Vec<_> = ports.iter().map(|p| p.config.clone()).collect();
        let paths = resolve_paths(&configs, &system);
        let mut changed = false;
        for (port, found) in ports.iter_mut().zip(paths) {
            if let Some(path) = found.as_ref().filter(|path| **path != port.config.path) {
                // A live worker on the old path must retire too; merely marking
                // the device present strands that worker on its stale endpoint.
                Self::stop_worker(port, Connection::Connecting);
                port.shared.system(format!("设备路径变为 {path}"));
                port.config.path = path.clone();
                changed = true;
            }
            if !port.config.h2loader_seen && port.shared.h2loader_seen.load(Ordering::SeqCst) {
                port.config.h2loader_seen = true;
                changed = true;
            }
            if port.present != found.is_some() {
                port.present = found.is_some();
                port.shared.dirty.store(true, Ordering::SeqCst);
            }
            if port.stopping.as_ref().is_some_and(|h| h.is_finished()) {
                if let Some(handle) = port.stopping.take() {
                    let _ = handle.join();
                }
            }
            let wanted = port.config.enabled && port.present;
            if port.worker.is_some() && !wanted {
                Self::stop_worker(
                    port,
                    if port.config.enabled {
                        Connection::Waiting
                    } else {
                        Connection::Off
                    },
                );
            }
            Self::grant_next(port);
            if port.worker.is_none() && wanted && port.stopping.is_none() {
                self.start_worker(port);
            } else if port.worker.is_none() && port.config.enabled && !port.present {
                port.shared
                    .update(|live| live.connection = Connection::Waiting);
                port.shared.publish_device(
                    DeviceState::Disconnected,
                    Connection::Waiting,
                    Some("设备未接入，等待重新连接".into()),
                );
            }
            if port.config.enabled {
                Self::grant_next(port);
            }
        }
        self.retired_workers
            .lock()
            .unwrap()
            .retain(|h| !h.is_finished());
        if changed {
            self.save(&ports);
        }
    }

    fn view(&self, port: &Port) -> Value {
        let live = {
            let live = port.shared.live.lock().unwrap();
            serde_json::to_value(&*live)
        };
        let (seq, generation) = {
            let logs = port.shared.logs.lock().unwrap();
            (logs.last_seq(), logs.generation())
        };
        let mut value = serde_json::to_value(&port.config).unwrap_or_default();
        if let (Value::Object(map), Ok(Value::Object(extra))) = (&mut value, live) {
            map.extend(extra);
            map.insert("instanceId".into(), json!(self.instance_id));
            map.insert("instanceGeneration".into(), json!(self.instance_generation));
            map.insert("present".into(), json!(port.present));
            map.insert("holder".into(), port.shared.holder(port.worker.is_some()));
            map.insert("mode".into(), json!(port.shared.seat.lock().unwrap().mode));
            map.insert("queue".into(), port.shared.queue());
            map.insert("logSeq".into(), json!(seq));
            map.insert("logGeneration".into(), json!(generation));
            map.insert("preparing".into(), json!(port.preparing.is_some()));
        }
        value
    }

    fn snapshot(&self) -> Value {
        let ports = self.ports.lock().unwrap();
        let gateway = {
            let config = self.gateway.lock().unwrap();
            let urls = if config.enabled {
                self.listeners
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|l| l.urls())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            json!({
                "enabled": config.enabled,
                "scope": config.scope,
                "port": self.bound.lock().unwrap().port(),
                "listening": config.enabled,
                "urls": urls,
                "error": self.gateway_error.lock().unwrap().clone(),
            })
        };
        json!({
            "instanceId": self.instance_id,
            "instanceGeneration": self.instance_generation,
            "node": { "id": self.node.id, "name": self.node.name },
            "nodes": self.remotes.nodes_json(),
            "remoteBusy": self.remotes.has_work(),
            "gateway": gateway,
            "pipeBase": format!("ws://127.0.0.1:{}/v1/ports", self.bound.lock().unwrap().port()),
            "deviceWebSocketProtocol": ws::PROTOCOL,
            "version": *self.version.borrow(),
            "h2loaderCore": h2loader::AVAILABLE,
            "ports": ports.iter().map(|port| self.view(port)).collect::<Vec<_>>(),
        })
    }
}

// ---------------------------------------------------------------- HTTP API

type Reply = Result<(StatusCode, Value), (StatusCode, String)>;

/// Who is calling. The desktop window may do everything; Gateway clients
/// (API key) may not reconfigure the service or the Gateway, and may name
/// local package files only from this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Caller {
    App,
    Gateway { local: bool, generation: u64 },
}

impl Caller {
    fn local(self) -> bool {
        matches!(self, Self::App | Self::Gateway { local: true, .. })
    }
}

async fn collect_body<B>(body: B) -> Result<Bytes, (StatusCode, String)>
where
    B: hyper::body::Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    tokio::time::timeout(BODY_TIMEOUT, Limited::new(body, MAX_BODY).collect())
        .await
        .map_err(|_| (StatusCode::REQUEST_TIMEOUT, "请求正文读取超时".into()))?
        .map_err(|_| bad("请求过大"))
        .map(|body| body.to_bytes())
}

fn bearer<B>(request: &Request<B>) -> Option<String> {
    request
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
}

fn pipe_token<B>(request: &Request<B>, peer: SocketAddr, lan: bool) -> Option<String> {
    let query = query_str(request.uri(), "token");
    if lan || !peer.ip().is_loopback() {
        // Reject credentials in URLs even if a valid header is also present.
        return query.is_none().then(|| bearer(request)).flatten();
    }
    bearer(request).or(query)
}

fn forbidden() -> (StatusCode, String) {
    (
        StatusCode::FORBIDDEN,
        "Gateway API Key 无权执行此操作".into(),
    )
}

fn bad(message: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, message.into())
}
fn not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "串口不存在".into())
}
fn ws_required() -> (StatusCode, String) {
    (StatusCode::FORBIDDEN, "Device commands require a granted WebSocket session; discover it with serial_pipe, connect and wait for granted".into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddPort {
    path: String,
    name: Option<String>,
    protocol: Option<Protocol>,
    baud: Option<u32>,
    enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchPort {
    mode: Option<Mode>,
    auto_grant: Option<bool>,
    name: Option<String>,
    protocol: Option<Protocol>,
    baud: Option<u32>,
    enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GatewayPatch {
    enabled: Option<bool>,
    scope: Option<gateway::Scope>,
    port: Option<u16>,
    regenerate_key: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartJob {
    kind: JobKind,
    path: Option<String>,
    source: Option<String>,
    job_id: Option<String>,
    /// Set only by the authenticated WS dispatcher, never by HTTP JSON.
    #[serde(skip)]
    lease: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DisconnectSession {
    #[serde(default)]
    pause_automatic: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendText {
    text: String,
    line_ending: Option<String>,
}

fn query(uri: &hyper::Uri, key: &str) -> Option<u64> {
    query_str(uri, key)?.parse().ok()
}

/// Form-urlencoded: clients such as `URLSearchParams` encode spaces as `+`.
fn query_str(uri: &hyper::Uri, key: &str) -> Option<String> {
    url::form_urlencoded::parse(uri.query()?.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn valid_baud(baud: u32) -> Result<u32, (StatusCode, String)> {
    if (300..=4_000_000).contains(&baud) {
        Ok(baud)
    } else {
        Err(bad("波特率无效"))
    }
}

impl PatchPort {
    fn candidate(&self, current: &PortConfig) -> Result<PortConfig, (StatusCode, String)> {
        let mut next = current.clone();
        if let Some(name) = self.name.as_ref() {
            let name: String = name.trim().chars().take(80).collect();
            if name.is_empty() || name.chars().any(char::is_control) {
                return Err(bad("名称不能为空或包含控制字符"));
            }
            next.name = name;
        }
        if let Some(protocol) = self.protocol {
            next.protocol = protocol;
        }
        if let Some(baud) = self.baud {
            next.baud = valid_baud(baud)?;
        }
        if let Some(mode) = self.mode {
            next.mode = mode;
            // Explicitly reclaiming control also pauses automatic grants so
            // an existing waiter cannot immediately take it back.
            if mode == Mode::Takeover {
                next.auto_grant = false;
            }
        }
        if let Some(auto) = self.auto_grant {
            next.auto_grant = auto;
        }
        if let Some(enabled) = self.enabled {
            next.enabled = enabled;
        }
        Ok(next)
    }
}

impl Daemon {
    fn admitted(
        &self,
        caller: Caller,
    ) -> Result<std::sync::MutexGuard<'_, bool>, (StatusCode, String)> {
        let admission = self.admission.lock().unwrap();
        if !*admission {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "串口服务正在退出".into()));
        }
        if !self.authority.allows(caller) {
            return Err((StatusCode::UNAUTHORIZED, "Gateway 授权已更改".into()));
        }
        Ok(admission)
    }

    fn forwarded_work(
        &self,
        caller: Caller,
    ) -> Result<remote::ForwardedWork, (StatusCode, String)> {
        // Reserve before releasing admission: retirement either sees this work,
        // or closes admission first and rejects the forwarded operation.
        let _admission = self.admitted(caller)?;
        Ok(self.remotes.reserve_work())
    }

    fn with_port<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut Port) -> Result<T, (StatusCode, String)>,
    ) -> Result<T, (StatusCode, String)> {
        let mut ports = self.ports.lock().unwrap();
        let port = ports
            .iter_mut()
            .find(|p| p.config.id == id)
            .ok_or_else(not_found)?;
        f(port)
    }

    fn add(&self, body: AddPort, caller: Caller) -> Reply {
        let admission = self.admitted(caller)?;
        let path = body.path.trim().to_string();
        if path.is_empty() || path.len() > 512 || path.chars().any(char::is_control) {
            return Err(bad("串口路径无效"));
        }
        let baud = valid_baud(body.baud.unwrap_or(115_200))?;
        let usb = self
            .system
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.path == path)
            .and_then(|p| p.usb.clone());
        let mut ports = self.ports.lock().unwrap();
        if ports.iter().any(|p| p.config.path == path) {
            return Err((StatusCode::CONFLICT, "此串口已在列表中".into()));
        }
        let name = body
            .name
            .map(|n| n.trim().chars().take(80).collect::<String>())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string());
        let config = PortConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            path,
            protocol: body.protocol.unwrap_or(Protocol::Auto),
            baud,
            enabled: body.enabled.unwrap_or(false),
            mode: Mode::Takeover,
            h2loader_seen: false,
            auto_grant: true,
            usb,
            created_at: now_ms(),
        };
        let port = Port {
            config,
            present: false,
            shared: self.new_shared(),
            worker: None,
            stopping: None,
            preparing: None,
        };
        let mut saved: Vec<_> = ports.iter().map(|p| p.config.clone()).collect();
        saved.push(port.config.clone());
        self.persist_configs(saved)
            .map_err(|e| bad(e.to_string()))?;
        port.shared.system("已添加到串口列表");
        ports.push(port);
        let view = self.view(ports.last().unwrap());
        drop(ports);
        drop(admission);
        self.reconcile();
        Ok((StatusCode::CREATED, view))
    }

    fn patch(&self, id: &str, body: PatchPort, caller: Caller) -> Reply {
        // These fields approve/revoke other clients and are application policy.
        // Check before touching even an otherwise valid name/enabled field.
        if caller != Caller::App && (body.mode.is_some() || body.auto_grant.is_some()) {
            return Err(forbidden());
        }
        if body.mode == Some(Mode::Piping) {
            return Err(bad(
                "Opening a WebSocket requests exclusive access; no mode-switch API is needed",
            ));
        }
        let view = {
            let _admission = self.admitted(caller)?;
            let mut ports = self.ports.lock().unwrap();
            let index = ports
                .iter()
                .position(|p| p.config.id == id)
                .ok_or_else(not_found)?;
            let port = &ports[index];
            let next = body.candidate(&port.config)?;
            let mode_changed = body
                .mode
                .is_some_and(|mode| mode != port.shared.seat.lock().unwrap().mode);
            let restart = next.protocol != port.config.protocol || next.baud != port.config.baud;
            if (restart || mode_changed)
                && (port.preparing.is_some() || port.shared.live.lock().unwrap().job.is_some())
            {
                return Err((
                    StatusCode::CONFLICT,
                    "请等待当前任务结束后再修改串口设置".into(),
                ));
            }
            // Persist a fully validated candidate before any config/lease or
            // worker side effect, including in-memory changes visible to watch.
            let mut saved: Vec<_> = ports.iter().map(|p| p.config.clone()).collect();
            saved[index] = next.clone();
            self.persist_configs(saved)
                .map_err(|e| bad(e.to_string()))?;
            let port = &mut ports[index];
            port.config = next;
            {
                let mut seat = port.shared.seat.lock().unwrap();
                if body.mode.is_some() {
                    seat.mode = port.config.mode;
                }
                if body.mode == Some(Mode::Takeover) {
                    seat.approved = None;
                }
                seat.auto_grant = port.config.auto_grant;
                if mode_changed {
                    seat.revoke("GizMoth 已更改串口模式");
                }
            }
            if !port.config.enabled {
                Self::stop_worker(port, Connection::Off);
            } else if restart && port.worker.is_some() {
                Self::stop_worker(port, Connection::Connecting);
                port.shared.system("设置已更改，重新连接");
            }
            if mode_changed {
                port.shared.system(match port.config.mode {
                    Mode::Takeover => "切换为接管模式：GizMoth 控制串口",
                    Mode::Piping => "切换为 WS 独占：仅获批连接可下发设备命令",
                });
            }
            Self::grant_next(port);
            port.shared.dirty.store(true, Ordering::SeqCst);
            self.view(port)
        };
        self.reconcile();
        Ok((StatusCode::OK, view))
    }

    fn remove(&self, id: &str, caller: Caller) -> Reply {
        let _admission = self.admitted(caller)?;
        let mut ports = self.ports.lock().unwrap();
        let index = ports
            .iter()
            .position(|p| p.config.id == id)
            .ok_or_else(not_found)?;
        let saved = ports
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, port)| port.config.clone())
            .collect();
        self.persist_configs(saved)
            .map_err(|e| bad(e.to_string()))?;
        let mut port = ports.remove(index);
        Self::stop_worker(&mut port, Connection::Off);
        port.shared.seat.lock().unwrap().end("串口已从列表移除");
        if let Some(handle) = port.stopping.take() {
            self.retired_workers.lock().unwrap().push(handle);
        }
        self.dirty.store(true, Ordering::SeqCst);
        Ok((StatusCode::OK, json!({ "removed": id })))
    }

    fn send(&self, id: &str, body: SendText, caller: Caller) -> Reply {
        self.send_owned(id, body, caller, None)
    }

    fn send_owned(&self, id: &str, body: SendText, caller: Caller, lease: Option<u64>) -> Reply {
        let _admission = self.admitted(caller)?;
        if body.text.len() > 4096 {
            return Err(bad("单次发送不超过 4096 字节"));
        }
        let ending = match body.line_ending.as_deref() {
            None | Some("lf") => "\n",
            Some("crlf") => "\r\n",
            Some("cr") => "\r",
            Some("none") => "",
            _ => return Err(bad("换行设置无效")),
        };
        self.with_port(id, |port| {
            port.shared.seat.lock().unwrap().authorize_device_command(
                lease,
                caller,
                &port.shared.authority,
            )?;
            let worker = port
                .worker
                .as_ref()
                .ok_or_else(|| (StatusCode::CONFLICT, "串口未连接".to_string()))?;
            let live = port.shared.live.lock().unwrap();
            if live.connection != Connection::Connected || live.detected != Some(Protocol::Serial) {
                return Err((
                    StatusCode::CONFLICT,
                    "仅已连接的普通串口可以直接发送数据".into(),
                ));
            }
            drop(live);
            let bytes = format!("{}{}", body.text, ending).into_bytes();
            let seat = port.shared.seat.lock().unwrap();
            seat.authorize_device_command(lease, caller, &port.shared.authority)?;
            let writer = seat
                .writer
                .as_ref()
                .filter(|w| w.worker == worker.id && seat.ready)
                .ok_or_else(|| (StatusCode::CONFLICT, "串口连接已关闭".into()))?;
            let owner = match lease {
                Some(id) => WriteOwner::Pipe { id, caller },
                None => WriteOwner::App,
            };
            writer.send(bytes, body.text, owner)?;
            Ok((StatusCode::ACCEPTED, json!({ "queued": true })))
        })
    }

    /// Cheap eligibility checks run both before package I/O and immediately
    /// before enqueue. The reservation excludes another preparation meanwhile.
    fn job_eligible(
        port: &Port,
        kind: JobKind,
        reservation: Option<&str>,
        caller: Caller,
        lease: Option<u64>,
    ) -> Result<u64, (StatusCode, String)> {
        port.shared.seat.lock().unwrap().authorize_device_command(
            lease,
            caller,
            &port.shared.authority,
        )?;
        let worker = port
            .worker
            .as_ref()
            .filter(|_| port.config.enabled && port.stopping.is_none())
            .ok_or_else(|| (StatusCode::CONFLICT, "串口未连接".to_string()))?;
        let live = port.shared.live.lock().unwrap();
        if live.worker != Some(worker.id)
            || live.connection != Connection::Connected
            || live.detected != Some(Protocol::H2loader)
        {
            return Err((
                StatusCode::CONFLICT,
                "仅已连接的 H2Loader 设备支持此任务".into(),
            ));
        }
        if live
            .job
            .as_ref()
            .is_some_and(|job| Some(job.id.as_str()) != reservation)
            || port
                .preparing
                .as_deref()
                .is_some_and(|id| Some(id) != reservation)
        {
            return Err((StatusCode::CONFLICT, "此串口正在执行或准备任务".into()));
        }
        if let Some(status) = &live.h2loader {
            let missing: Vec<_> = job_commands(kind)
                .iter()
                .filter(|c| !h2loader::allows(status, c))
                .map(|c| command_label(c))
                .collect();
            if !missing.is_empty() {
                return Err((
                    StatusCode::CONFLICT,
                    format!("设备当前不允许：{}", missing.join("、")),
                ));
            }
        }
        Ok(worker.id)
    }

    fn reserve_job(
        &self,
        id: &str,
        kind: JobKind,
        caller: Caller,
        job_id: &str,
        lease: Option<u64>,
    ) -> Result<u64, (StatusCode, String)> {
        let _admission = self.admitted(caller)?;
        self.with_port(id, |port| {
            let mut ids = self.job_ids.lock().unwrap();
            if ids.contains(job_id) {
                return Err((
                    StatusCode::CONFLICT,
                    "任务 ID 已使用，请查看串口状态".into(),
                ));
            }
            if ids.len() >= 100_000 {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "任务 ID 容量已满，请重启串口服务".into(),
                ));
            }
            let worker = Self::job_eligible(port, kind, None, caller, lease)?;
            if lease.is_some() {
                let mut seat = port.shared.seat.lock().unwrap();
                seat.authorize_command(lease, caller, &port.shared.authority)?;
                // Reserve handoff before publishing the job: disconnect can
                // race this small interval between reservation and registration.
                seat.active_job = Some(job_id.into());
            }
            ids.insert(job_id.into());
            port.preparing = Some(job_id.into());
            port.shared.dirty.store(true, Ordering::SeqCst);
            Ok(worker)
        })
    }

    /// Publish a job before any potentially slow package I/O. Clients can find
    /// and cancel it via the existing port snapshot even if the reply is lost.
    fn start_job(self: &Arc<Self>, id: &str, body: StartJob, caller: Caller) -> Reply {
        self.start_job_with(id, body, caller, || {})
    }

    fn start_job_with(
        self: &Arc<Self>,
        id: &str,
        body: StartJob,
        caller: Caller,
        before_prepare: impl FnOnce() + Send + 'static,
    ) -> Reply {
        let job_id = match body.job_id.as_deref() {
            Some(id) => uuid::Uuid::parse_str(id)
                .map_err(|_| bad("无效的任务 ID"))?
                .to_string(),
            None => uuid::Uuid::new_v4().to_string(),
        };
        let reserved_worker = self.reserve_job(id, body.kind, caller, &job_id, body.lease)?;
        let registered = self.with_port(id, |port| {
            if port.preparing.as_deref() != Some(&job_id) {
                return Err((StatusCode::CONFLICT, "任务准备已取消".into()));
            }
            if Self::job_eligible(port, body.kind, Some(&job_id), caller, body.lease)?
                != reserved_worker
            {
                return Err((StatusCode::CONFLICT, "串口已重新连接，请重试".into()));
            }
            let mut seat = port.shared.seat.lock().unwrap();
            seat.authorize_command(body.lease, caller, &port.shared.authority)?;
            let job = Job {
                id: job_id.clone(),
                kind: body.kind,
                state: JobState::Queued,
                phase: "preparing".into(),
                acknowledged: 0,
                total: 0,
                source: if body.lease.is_some() {
                    "ws".into()
                } else {
                    match body.source.as_deref() {
                        Some("mcp") => "mcp".into(),
                        _ => "app".into(),
                    }
                },
                created_at: now_ms(),
                finished_at: None,
                error: None,
                package: None,
                cancel: Arc::new(AtomicBool::new(false)),
                snapshot: None,
                worker: reserved_worker,
                lease: body.lease,
            };
            if body.lease.is_some() {
                seat.active_job = Some(job.id.clone());
                seat.holder.as_mut().unwrap().job_cancel = Some(job.cancel.clone());
            }
            port.shared.live.lock().unwrap().job = Some(job.clone());
            port.shared.dirty.store(true, Ordering::SeqCst);
            Ok((job, port.shared.clone()))
        });
        let (job, shared) = match registered {
            Ok(value) => value,
            Err(error) => {
                let _ = self.with_port(id, |port| {
                    if port.preparing.as_deref() == Some(&job_id) {
                        port.preparing = None;
                    }
                    port.shared.release_job(&job_id);
                    Ok(())
                });
                return Err(error);
            }
        };
        let (daemon, port_id, task_job, task_shared) =
            (self.clone(), id.to_owned(), job.clone(), shared.clone());
        if let Err(error) = std::thread::Builder::new()
            .name("firmware-prepare".into())
            .spawn(move || {
                before_prepare();
                daemon.prepare_job(&port_id, body, caller, task_job, task_shared);
            })
        {
            let _ = self.with_port(id, |port| {
                if port.preparing.as_deref() == Some(&job_id) {
                    port.preparing = None;
                }
                Ok(())
            });
            shared.finish_job(reserved_worker, Err(error.to_string()));
            return Err((StatusCode::SERVICE_UNAVAILABLE, "无法启动任务准备".into()));
        }
        Ok((
            StatusCode::ACCEPTED,
            serde_json::to_value(job).unwrap_or_default(),
        ))
    }

    fn prepare_job(&self, id: &str, body: StartJob, caller: Caller, job: Job, shared: Arc<Shared>) {
        let mut snapshot = None;
        let result: Result<(), (StatusCode, String)> = (|| {
            let cancelled = || {
                if job.cancel.load(Ordering::SeqCst) {
                    Err((StatusCode::CONFLICT, "任务准备已取消".into()))
                } else {
                    Ok(())
                }
            };
            cancelled()?;
            let package = if matches!(body.kind, JobKind::Stage | JobKind::Install) {
                let source = Path::new(body.path.as_deref().ok_or_else(|| bad("请先选择固件包"))?);
                if !source.is_absolute() || !source.is_file() {
                    return Err(bad("请选择本机固件包文件"));
                }
                let size = fs::metadata(source).map_err(|e| bad(e.to_string()))?.len();
                if size == 0 || size > 256 * 1024 * 1024 {
                    return Err(bad("固件包大小无效"));
                }
                let directory = self.dir.join("packages");
                fs::create_dir_all(&directory).map_err(|e| bad(e.to_string()))?;
                let path = directory.join(format!("{}.pkg", job.id));
                snapshot = Some(path.clone());
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut copy = options.open(&path).map_err(|e| bad(e.to_string()))?;
                let mut source_file = fs::File::open(source).map_err(|e| bad(e.to_string()))?;
                let mut buffer = [0u8; 64 * 1024];
                let mut copied = 0u64;
                loop {
                    cancelled()?;
                    let count = source_file
                        .read(&mut buffer)
                        .map_err(|e| bad(e.to_string()))?;
                    if count == 0 {
                        break;
                    }
                    copied += count as u64;
                    if copied > 256 * 1024 * 1024 {
                        return Err(bad("固件包大小无效"));
                    }
                    copy.write_all(&buffer[..count])
                        .map_err(|e| bad(e.to_string()))?;
                }
                if copied == 0 {
                    return Err(bad("固件包大小无效"));
                }
                cancelled()?;
                copy.sync_all().map_err(|e| bad(e.to_string()))?;
                cancelled()?;
                let mut meta = h2loader::inspect(&path.to_string_lossy())
                    .map_err(|e| bad(format!("固件包无效：{}", h2loader::describe(&e))))?;
                cancelled()?;
                meta["name"] = json!(source.file_name().map(|n| n.to_string_lossy().into_owned()));
                Some(meta)
            } else {
                None
            };
            let _admission = self.admitted(caller)?;
            self.with_port(id, |port| {
                if port.preparing.as_deref() != Some(&job.id) {
                    return Err((StatusCode::CONFLICT, "任务准备已取消".into()));
                }
                let worker_id =
                    Self::job_eligible(port, body.kind, Some(&job.id), caller, body.lease)?;
                if worker_id != job.worker {
                    return Err((StatusCode::CONFLICT, "串口已重新连接，请重试".into()));
                }
                let worker = port.worker.as_ref().unwrap();
                let seat = port.shared.seat.lock().unwrap();
                seat.authorize_device_command(body.lease, caller, &port.shared.authority)?;
                let mut live = port.shared.live.lock().unwrap();
                cancelled()?;
                if live.job.as_ref().is_none_or(|current| current.id != job.id) {
                    return Err((StatusCode::CONFLICT, "任务准备已结束".into()));
                }
                if let (Some(meta), Some(status)) = (&package, &live.h2loader) {
                    if meta["board"] != status["board"] || meta["target"] != status["target"] {
                        return Err(bad(format!(
                            "固件包适用于 {} {}，设备是 {} {}",
                            meta["board"].as_str().unwrap_or("?"),
                            meta["target"].as_str().unwrap_or(""),
                            status["board"].as_str().unwrap_or("?"),
                            status["target"].as_str().unwrap_or("")
                        )));
                    }
                }
                let current = live.job.as_mut().unwrap();
                current.phase = "queued".into();
                current.total = package
                    .as_ref()
                    .and_then(|p| p["bytes"].as_u64())
                    .unwrap_or(0);
                current.package = package;
                current.snapshot = snapshot.clone();
                if let Err(error) = worker.tx.try_send(Command::Job) {
                    current.snapshot = None;
                    return Err(match error {
                        mpsc::TrySendError::Full(_) => {
                            (StatusCode::SERVICE_UNAVAILABLE, "串口命令队列已满".into())
                        }
                        mpsc::TrySendError::Disconnected(_) => {
                            (StatusCode::CONFLICT, "串口连接已关闭".into())
                        }
                    });
                }
                port.preparing = None;
                worker.interrupt.store(true, Ordering::SeqCst);
                port.shared.dirty.store(true, Ordering::SeqCst);
                Ok(())
            })
        })();
        let _ = self.with_port(id, |port| {
            if port.preparing.as_deref() == Some(&job.id) {
                port.preparing = None;
                port.shared.dirty.store(true, Ordering::SeqCst);
            }
            Ok(())
        });
        if let Err((_, error)) = result {
            if let Some(path) = snapshot {
                let _ = fs::remove_file(path);
            }
            // Do not finish a later job if the original worker already retired.
            let mut live = shared.live.lock().unwrap();
            if live
                .job
                .as_ref()
                .is_some_and(|current| current.id == job.id)
            {
                let mut finished = live.job.take().unwrap();
                finished.finished_at = Some(now_ms());
                if finished.cancel.load(Ordering::SeqCst) {
                    finished.state = JobState::Cancelled;
                    finished.error = Some(h2loader::describe("cancelled"));
                } else {
                    finished.state = JobState::Failed;
                    finished.error = Some(error);
                }
                live.history.push_front(finished);
                live.history.truncate(HISTORY);
                shared.dirty.store(true, Ordering::SeqCst);
            }
            drop(live);
            shared.release_job(&job.id);
        }
    }

    /// Both FIFO grants and explicit approvals wait for a usable protocol worker.
    /// Connected sockets alone determine communication ownership; a legacy saved
    /// mode cannot reserve a device. The native worker keeps the serial protocol.
    fn grant_next(port: &mut Port) {
        let shared = port.shared.clone();
        let mut seat = shared.seat.lock().unwrap();
        Self::grant_next_locked(port, &mut seat);
    }

    fn grant_next_locked(port: &mut Port, seat: &mut Seat) {
        if !port.config.enabled {
            if seat.holder.is_none() && seat.active_job.is_none() {
                seat.mode = Mode::Takeover;
            }
            return;
        }
        let shared = port.shared.clone();
        seat.auto_grant = port.config.auto_grant;
        seat.revoke_unauthorized(&shared.authority);
        let requested = !seat.queue.is_empty() && (seat.auto_grant || seat.approved.is_some());
        let mode = if seat.holder.is_some() || seat.active_job.is_some() || requested {
            Mode::Piping
        } else {
            Mode::Takeover
        };
        if seat.mode != mode {
            let live = shared.live.lock().unwrap();
            // Do not interrupt an accepted job, package preparation or initial
            // protocol detection. Polling retries after that work completes.
            if port.preparing.is_some() || live.job.is_some() {
                return;
            }
            if mode == Mode::Piping && live.connection != Connection::Connected {
                return;
            }
            drop(live);
            // Ownership controls admission only. Keep GizMoth's existing
            // serial/H2Loader worker and protocol session for all WS commands.
            seat.mode = mode;
            shared.system(match mode {
                Mode::Piping => "WS 独占通信：界面和其他发送入口已暂停",
                Mode::Takeover => "WS 独占通信已结束：恢复 GizMoth 控制",
            });
        }
        if let Some(client) = seat.promote(&shared.authority) {
            shared.system(format!("授予 {client} WS 独占通信权"));
        }
    }

    fn accept(&self, id: &str, request: u64) -> Reply {
        let accepted = self.accept_with(id, request, |configs| self.persist_configs(configs))?;
        self.reconcile();
        Ok(accepted)
    }

    fn accept_with(
        &self,
        id: &str,
        request: u64,
        persist: impl FnOnce(Vec<PortConfig>) -> std::io::Result<()>,
    ) -> Reply {
        let _admission = self.admitted(Caller::App)?;
        let mut ports = self.ports.lock().unwrap();
        let index = ports
            .iter()
            .position(|p| p.config.id == id)
            .ok_or_else(not_found)?;
        let port = &ports[index];
        if !port.config.enabled {
            return Err((StatusCode::CONFLICT, "串口未连接".into()));
        }
        if port.preparing.is_some() || port.shared.live.lock().unwrap().job.is_some() {
            return Err((StatusCode::CONFLICT, "请等待当前任务结束".into()));
        }
        let shared = port.shared.clone();
        let mut seat = shared.seat.lock().unwrap();
        if !seat.queue.iter().any(|p| p.id == request) {
            return Err((StatusCode::NOT_FOUND, "请求不存在或已结束".into()));
        }
        // Reserve membership through persistence and the complete transition.
        // A concurrent leave either wins before validation (no effects), or
        // removes the accepted request after commit (a successful acceptance).
        // Approval reserves this session only; preserve the configured mode so
        // control also resumes after manually approved sessions disconnect.
        let saved: Vec<_> = ports.iter().map(|p| p.config.clone()).collect();
        persist(saved).map_err(|e| bad(e.to_string()))?;
        let port = &mut ports[index];
        seat.revoke("GizMoth 已授予其他 WS 连接通信权");
        seat.approved = Some(request);
        seat.notify_positions();
        Self::grant_next_locked(port, &mut seat);
        drop(seat);
        port.shared.dirty.store(true, Ordering::SeqCst);
        Ok((StatusCode::OK, self.view(port)))
    }

    fn reject(&self, id: &str, request: u64) -> Reply {
        self.with_port(id, |port| {
            let mut seat = port.shared.seat.lock().unwrap();
            let index = seat
                .queue
                .iter()
                .position(|p| p.id == request)
                .ok_or((StatusCode::NOT_FOUND, "请求不存在或已结束".to_string()))?;
            let pipe = seat.queue.remove(index).unwrap();
            let _ = pipe
                .control
                .send_replace(Control::End("rejected", "GizMoth 拒绝了请求".into()));
            seat.notify_positions();
            drop(seat);
            port.shared.system(format!("已拒绝 {} 的请求", pipe.client));
            port.shared.dirty.store(true, Ordering::SeqCst);
            Ok((StatusCode::OK, self.view(port)))
        })
    }

    /// Administrative disconnect is ID-specific and independent of device
    /// readiness. Validate membership through persistence and revocation so a
    /// stale UI action can never terminate a replacement holder.
    fn disconnect_session(&self, id: &str, connection: u64, pause_automatic: bool) -> Reply {
        let _admission = self.admitted(Caller::App)?;
        let mut ports = self.ports.lock().unwrap();
        let index = ports
            .iter()
            .position(|port| port.config.id == id)
            .ok_or_else(not_found)?;
        let shared = ports[index].shared.clone();
        let mut seat = shared.seat.lock().unwrap();
        let held = seat
            .holder
            .as_ref()
            .is_some_and(|pipe| pipe.id == connection);
        let queued = seat.queue.iter().position(|pipe| pipe.id == connection);
        if !held && queued.is_none() {
            return Err((
                StatusCode::NOT_FOUND,
                "选中的 WS 连接已结束，请刷新连接列表".into(),
            ));
        }
        if pause_automatic {
            let mut saved: Vec<_> = ports.iter().map(|port| port.config.clone()).collect();
            saved[index].auto_grant = false;
            self.persist_configs(saved)
                .map_err(|error| bad(error.to_string()))?;
        }
        let port = &mut ports[index];
        if pause_automatic {
            port.config.auto_grant = false;
            seat.auto_grant = false;
            seat.approved = None;
        }
        if held {
            seat.revoke("管理员断开了此 WS 连接");
        } else {
            let pipe = seat.queue.remove(queued.unwrap()).unwrap();
            pipe.control
                .send_replace(Control::End("rejected", "管理员断开了此 WS 连接".into()));
            if seat.approved == Some(connection) {
                seat.approved = None;
            }
            seat.notify_positions();
        }
        drop(seat);
        Self::grant_next(port);
        shared.system(format!("已断开选定的 WS 连接 {connection}"));
        Ok((StatusCode::OK, self.view(port)))
    }

    /// The socket identity is supplied by the transport, never by command JSON.
    fn ws_command(
        self: &Arc<Self>,
        id: &str,
        lease: u64,
        caller: Caller,
        request: &ws::Request,
    ) -> Reply {
        self.with_port(id, |port| {
            port.shared.seat.lock().unwrap().authorize_command(
                Some(lease),
                caller,
                &port.shared.authority,
            )
        })?;
        let args = &request.params;
        match request.method.as_str() {
            "send" => self.send_owned(
                id,
                SendText {
                    text: args["text"].as_str().unwrap().into(),
                    line_ending: args["lineEnding"].as_str().map(str::to_owned),
                },
                caller,
                Some(lease),
            ),
            "install" | "reboot" | "refresh" => {
                let kind = match request.method.as_str() {
                    "install" if args["stageOnly"].as_bool().unwrap_or(false) => JobKind::Stage,
                    "install" => JobKind::Install,
                    "reboot" if args["target"] == "app" => JobKind::RebootApp,
                    "reboot" => JobKind::RebootLoader,
                    _ => JobKind::Refresh,
                };
                let path = match (
                    request.method.as_str(),
                    args["package"].as_str(),
                    args["artifact"].as_str(),
                ) {
                    ("install", Some(_), _) if !caller.local() => {
                        return Err((StatusCode::FORBIDDEN, "局域网客户端不能指定本机文件".into()));
                    }
                    ("install", Some(package), None) => Some(package.to_owned()),
                    // Build artifacts live on this computer; any caller may name one.
                    ("install", None, Some(artifact)) => {
                        Some(self.compile_artifact(artifact, args["file"].as_str())?)
                    }
                    ("install", _, _) => {
                        return Err(bad("install needs exactly one of package or artifact"));
                    }
                    _ => None,
                };
                self.start_job(
                    id,
                    StartJob {
                        kind,
                        path,
                        source: Some("ws".into()),
                        job_id: args["jobId"].as_str().map(str::to_owned),
                        lease: Some(lease),
                    },
                    caller,
                )
            }
            "cancel" => {
                let job_id = uuid::Uuid::parse_str(args["jobId"].as_str().unwrap())
                    .unwrap()
                    .to_string();
                let _admission = self.admitted(caller)?;
                self.with_port(id, |port| {
                    let seat = port.shared.seat.lock().unwrap();
                    seat.authorize_command(Some(lease), caller, &port.shared.authority)?;
                    let live = port.shared.live.lock().unwrap();
                    let job = live
                        .job
                        .as_ref()
                        .filter(|job| job.lease == Some(lease) && job.id == job_id)
                        .ok_or((StatusCode::CONFLICT, "此连接没有对应的运行中任务".into()))?;
                    job.cancel.store(true, Ordering::SeqCst);
                    Ok((StatusCode::ACCEPTED, json!({"cancelling":job.id})))
                })
            }
            _ => Err(bad("未知的设备 WS 命令")),
        }
    }

    fn cancel_job(&self, job_id: &str) -> Reply {
        let ports = self.ports.lock().unwrap();
        for port in ports.iter() {
            let live = port.shared.live.lock().unwrap();
            if let Some(job) = live.job.as_ref().filter(|j| j.id == job_id) {
                job.cancel.store(true, Ordering::SeqCst);
                drop(live);
                port.shared.system("正在取消任务…");
                return Ok((StatusCode::ACCEPTED, json!({ "cancelling": job_id })));
            }
        }
        Err((StatusCode::NOT_FOUND, "任务不存在或已结束".into()))
    }

    fn logs(&self, id: &str, uri: &hyper::Uri) -> Reply {
        let limit = query(uri, "limit").unwrap_or(500);
        self.log_page(id, query(uri, "after"), limit)
    }

    fn log_page(&self, id: &str, after: Option<u64>, limit: u64) -> Reply {
        let limit = limit.clamp(1, 2000) as usize;
        self.with_port(id, |port| {
            let ring = port.shared.logs.lock().unwrap();
            let page = match after {
                Some(after) => ring.page(after, limit),
                None => ring.tail(limit),
            };
            Ok((StatusCode::OK, {
                let mut value = serde_json::to_value(page).unwrap_or_default();
                value["instanceId"] = json!(self.instance_id);
                value
            }))
        })
    }

    fn authorize(&self, token: &str, peer: SocketAddr, lan: bool) -> Option<Caller> {
        // LAN sockets never receive desktop privileges, including local callers
        // connecting to a LAN address. Proxy headers cannot change this identity.
        let local = !lan && peer.ip().is_loopback();
        if local && constant_eq(token.as_bytes(), self.token.as_bytes()) {
            return Some(Caller::App);
        }
        let config = self.gateway.lock().unwrap();
        let reachable = local || config.scope == gateway::Scope::Lan;
        (config.enabled && reachable && constant_eq(token.as_bytes(), config.api_key.as_bytes()))
            .then(|| Caller::Gateway {
                local,
                generation: self.authority.generation.load(Ordering::SeqCst),
            })
    }

    fn gateway_view(&self) -> Value {
        let config = self.gateway.lock().unwrap().clone();
        let bound = *self.bound.lock().unwrap();
        let urls = self
            .listeners
            .lock()
            .unwrap()
            .as_ref()
            .map(|l| l.urls())
            .unwrap_or_else(|| vec![format!("http://127.0.0.1:{}", bound.port())]);
        let error = self.gateway_error.lock().unwrap().clone();
        json!({
            "enabled": config.enabled,
            "scope": config.scope,
            "port": config.port,
            "apiKey": config.api_key,
            "node": { "id": self.node.id, "name": self.node.name },
            "listening": config.enabled,
            "boundPort": bound.port(),
            "urls": if config.enabled { urls } else { Vec::new() },
            "error": error,
        })
    }

    fn set_gateway_error(&self, error: Option<String>) {
        let mut current = self.gateway_error.lock().unwrap();
        if *current != error {
            *current = error;
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    fn refresh_gateway_network(&self) {
        self.refresh_gateway_network_with(gateway::try_lan_ips);
    }

    fn refresh_gateway_network_with(
        &self,
        enumerate: impl FnOnce() -> std::io::Result<Vec<std::net::Ipv4Addr>>,
    ) {
        // Never compete with a user transaction or replace an unadopted update.
        let Ok(_transaction) = self.reconfiguration.try_lock() else {
            return;
        };
        if !*self.admission.lock().unwrap() {
            return;
        }
        if self.pending.lock().unwrap().is_some() {
            // Failed adoption retains its candidate in every scope. The poll
            // interval bounds retries without spinning.
            self.rebind.send_modify(|v| *v += 1);
            return;
        }
        let config = self.gateway.lock().unwrap().clone();
        if !config.enabled {
            return;
        }
        let Some(current) = self.listeners.lock().unwrap().clone().filter(|l| {
            l.needs_port_retry() || (config.scope == gateway::Scope::Lan && l.lan_enabled())
        }) else {
            return;
        };
        let mut ips = match if config.scope == gateway::Scope::Lan {
            enumerate()
        } else {
            Ok(Vec::new())
        } {
            Ok(ips) => ips,
            Err(error) => {
                self.set_gateway_error(Some(format!(
                    "LAN interface query failed: {error}; loopback service remains available"
                )));
                return;
            }
        };
        ips.sort_unstable();
        ips.dedup();
        let expected_error = (ips.is_empty() && config.scope == gateway::Scope::Lan)
            .then(|| "LAN is offline; loopback service remains available".into());
        if !current.needs_port_retry()
            && ips == current.lan_addresses()
            && current.network_error() == expected_error
            && *self.gateway_error.lock().unwrap() == expected_error
        {
            return;
        }
        // Matching std socket addresses do not prove that the runtime adopted
        // them. Clear recovery errors only after a successful adopt below.
        match current.refresh_lan(&ips) {
            Ok(next) => {
                // Publish URLs only after the runtime successfully adopts this
                // candidate. A later manual transaction replaces pending here.
                *self.pending.lock().unwrap() = Some(PendingListeners {
                    listeners: Arc::new(next),
                    network_refresh: true,
                });
                self.rebind.send_modify(|v| *v += 1);
            }
            Err(error) => self.set_gateway_error(Some(format!(
                "LAN refresh failed: {error}; loopback service remains available"
            ))),
        }
    }

    fn adopt_pending(&self, active: &mut gateway::AsyncListeners) {
        self.adopt_pending_with(active, gateway::Listeners::adopt);
    }

    fn adopt_pending_with(
        &self,
        active: &mut gateway::AsyncListeners,
        adopt: impl FnOnce(&gateway::Listeners) -> std::io::Result<gateway::AsyncListeners>,
    ) {
        let _transaction = self.reconfiguration.lock().unwrap();
        let Some(next) = self.pending.lock().unwrap().take() else {
            return;
        };
        let adopted = adopt(&next.listeners).and_then(|adopted| {
            let address = next.listeners.local_addr()?;
            if address != *self.bound.lock().unwrap() {
                // Publish discovery only for a listener the runtime can adopt.
                self.write_info_at(address)?;
            }
            Ok((adopted, address))
        });
        match adopted {
            Ok((adopted, address)) => {
                *active = adopted;
                *self.bound.lock().unwrap() = address;
                self.set_gateway_error(next.listeners.network_error());
                *self.listeners.lock().unwrap() = Some(next.listeners);
                self.dirty.store(true, Ordering::SeqCst);
            }
            Err(error) => {
                self.set_gateway_error(Some(format!("Service listener adoption failed: {error}; previous listeners remain available")));
                *self.pending.lock().unwrap() = Some(next);
            }
        }
    }

    /// Serialize bind/persist/commit as one transaction. Authentication changes
    /// invalidate real callers; moving only the listener preserves their leases.
    fn reconfigure(&self, body: GatewayPatch) -> Reply {
        let _transaction = self.reconfiguration.lock().unwrap();
        let _admission = self.admitted(Caller::App)?;
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.as_ref().is_some_and(|next| next.network_refresh) {
                // Release a speculative new-IP bind before a user transaction
                // binds that same address with the requested configuration.
                pending.take();
            }
        }
        let current = self.gateway.lock().unwrap().clone();
        let mut next = current.clone();
        if let Some(enabled) = body.enabled {
            next.enabled = enabled;
        }
        if let Some(scope) = body.scope {
            next.scope = scope;
        }
        if let Some(port) = body.port {
            if port < 1024 {
                return Err(bad("端口需在 1024–65535 之间"));
            }
            next.port = port;
        }
        if body.regenerate_key == Some(true) {
            next.api_key = gateway::new_key();
        }
        let moved = next.enabled != current.enabled
            || next.scope != current.scope
            || next.port != current.port
            || self.gateway_error.lock().unwrap().is_some();
        let listener = if moved {
            let active = self.listeners.lock().unwrap().clone();
            let pending = self
                .pending
                .lock()
                .unwrap()
                .as_ref()
                .map(|next| next.listeners.clone());
            let desired = gateway::address(&gateway::Config {
                scope: gateway::Scope::Local,
                ..next.clone()
            });
            let old = pending
                .as_ref()
                .filter(|l| l.local_addr().ok() == Some(desired))
                .cloned()
                .or(active)
                .or(pending);
            Some(Arc::new(
                gateway::Listeners::prepare(&next, old.as_deref()).map_err(|e| {
                    (
                        StatusCode::CONFLICT,
                        if e.kind() == std::io::ErrorKind::AddrInUse {
                            format!("端口 {} 已被占用", next.port)
                        } else {
                            format!("无法监听 {}：{e}", gateway::address(&next))
                        },
                    )
                })?,
            ))
        } else {
            None
        };
        if next.enabled {
            if let Some(listener) = &listener {
                next.port = listener
                    .local_addr()
                    .map_err(|e| bad(e.to_string()))?
                    .port();
            }
        }
        let config_path = gateway::path(&self.dir);
        private_write(
            &config_path,
            &serde_json::to_vec_pretty(&next).unwrap_or_default(),
        )
        .map_err(|e| bad(e.to_string()))?;
        let ports = self.ports.lock().unwrap();
        let mut config = self.gateway.lock().unwrap();
        if current.api_key != next.api_key || (current.enabled && !next.enabled) {
            self.authority.generation.fetch_add(1, Ordering::SeqCst);
        }
        self.authority.enabled.store(next.enabled, Ordering::SeqCst);
        self.authority
            .lan
            .store(next.scope == gateway::Scope::Lan, Ordering::SeqCst);
        *config = next.clone();
        drop(config);
        for port in ports.iter() {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.revoke_unauthorized(&self.authority);
            seat.promote(&self.authority);
        }
        if let Some(listener) = listener {
            // Desired settings can commit now; discovery and advertised sockets
            // remain on the actual active listener until adoption succeeds.
            *self.pending.lock().unwrap() = Some(PendingListeners {
                listeners: listener,
                network_refresh: false,
            });
            self.rebind.send_modify(|v| *v += 1);
        }
        self.dirty.store(true, Ordering::SeqCst);
        drop(ports);
        Ok((StatusCode::OK, self.gateway_view()))
    }

    fn write_info_at(&self, address: SocketAddr) -> std::io::Result<()> {
        let info = Info {
            pid: std::process::id(),
            port: address.port(),
            token: self.token.clone(),
            api_version: API_VERSION,
            started_at: self.started_at,
        };
        private_write(&info_path(&self.dir), &serde_json::to_vec(&info)?)
    }

    fn write_info(&self) -> std::io::Result<()> {
        self.write_info_at(*self.bound.lock().unwrap())
    }

    /// Updater/migration retirement never interrupts jobs or external leases.
    /// New preparations, joins and approvals take admission before ports too.
    fn retire(&self) -> Reply {
        let mut admission = self.admission.lock().unwrap();
        if *admission {
            if self.remotes.has_work() {
                return Err((
                    StatusCode::CONFLICT,
                    "远端串口正在准备任务或持有会话，暂不能重启服务".into(),
                ));
            }
            let ports = self.ports.lock().unwrap();
            for port in ports.iter() {
                if port.preparing.is_some() || port.shared.live.lock().unwrap().job.is_some() {
                    return Err((
                        StatusCode::CONFLICT,
                        "串口正在执行或准备任务，暂不能重启服务".into(),
                    ));
                }
                let seat = port.shared.seat.lock().unwrap();
                if seat.holder.is_some() || !seat.queue.is_empty() {
                    return Err((
                        StatusCode::CONFLICT,
                        "外部客户端正在占据或等待串口，暂不能重启服务".into(),
                    ));
                }
            }
            *admission = false;
        }
        self.shutdown.send_replace(true);
        Ok((
            StatusCode::OK,
            json!({ "stopping": true, "instanceId": self.instance_id }),
        ))
    }

    async fn watch_snapshot(&self, uri: &hyper::Uri) -> Value {
        let since = query(uri, "since").unwrap_or(0);
        let mut receiver = self.version.subscribe();
        let timeout = Duration::from_millis(
            query(uri, "timeoutMs")
                .unwrap_or(WATCH_TIMEOUT.as_millis() as u64)
                .min(20_000),
        );
        let instance_matches = query_str(uri, "instance").is_none_or(|id| id == self.instance_id);
        if instance_matches && !timeout.is_zero() && *receiver.borrow() <= since {
            let _ = tokio::time::timeout(timeout, receiver.wait_for(|v| *v > since)).await;
        }
        self.snapshot()
    }

    /// Requests for an imported port. Reads are open to every caller; device
    /// commands are the desktop window's, carried by this node's session there.
    /// Configuration, approvals and log clearing stay with the owning node.
    async fn remote_port(
        &self,
        method: &Method,
        id: &str,
        rest: &[&str],
        uri: &hyper::Uri,
        body: &Bytes,
        caller: Caller,
    ) -> Reply {
        drop(self.admitted(caller)?);
        let parse = || -> Result<Value, (StatusCode, String)> {
            if body.is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_slice(body).map_err(|e| bad(e.to_string()))
            }
        };
        let app = caller == Caller::App;
        match (method, rest) {
            (&Method::GET, []) => self
                .remotes
                .port_view(id)
                .map(|view| (StatusCode::OK, view))
                .ok_or_else(not_found),
            (&Method::GET, ["logs"]) => {
                self.remotes
                    .logs(id, query(uri, "after"), query(uri, "limit").unwrap_or(500))
                    .await
            }
            (&Method::POST, ["send"]) if app => {
                let _work = self.forwarded_work(caller)?;
                let body: SendText =
                    serde_json::from_value(parse()?).map_err(|e| bad(e.to_string()))?;
                self.remotes
                    .send(id, body.text, body.line_ending.as_deref())
                    .await
            }
            (&Method::POST, ["jobs"]) if app => {
                let _work = self.forwarded_work(caller)?;
                let body: StartJob =
                    serde_json::from_value(parse()?).map_err(|e| bad(e.to_string()))?;
                self.remotes
                    .job(id, body.kind, body.path, body.job_id)
                    .await
            }
            (&Method::POST, ["connections", connection, "disconnect"]) if app => {
                let connection = connection.parse().map_err(|_| bad("连接标识无效"))?;
                self.remotes.release(id, connection)
            }
            (_, _) if !app => Err(forbidden()),
            _ => Err((
                StatusCode::CONFLICT,
                "远端串口的配置、审批和日志由它所在的 GizMoth 管理".into(),
            )),
        }
    }

    async fn remote_admin(&self, method: &Method, rest: &[&str], body: Value) -> Reply {
        drop(self.admitted(Caller::App)?);
        let text = |key: &str| body[key].as_str().map(str::to_owned);
        match (method, rest) {
            (&Method::POST, ["probe"]) => {
                self.remotes
                    .probe(
                        &text("url").unwrap_or_default(),
                        &text("apiKey").unwrap_or_default(),
                    )
                    .await
            }
            (&Method::POST, []) => {
                self.remotes
                    .add(
                        &text("url").unwrap_or_default(),
                        &text("apiKey").unwrap_or_default(),
                    )
                    .await
            }
            (&Method::DELETE, [id]) => self.remotes.remove(id),
            _ => Err((StatusCode::NOT_FOUND, "not found".into())),
        }
    }

    async fn handle(
        self: Arc<Self>,
        request: Request<hyper::body::Incoming>,
        peer: SocketAddr,
        lan: bool,
    ) -> Reply {
        let Some(caller) = bearer(&request).and_then(|token| self.authorize(&token, peer, lan))
        else {
            return Err((StatusCode::UNAUTHORIZED, "unauthorized".into()));
        };
        let app = caller == Caller::App;
        // External APIs are queries only. Ownership and all device commands
        // are carried by the authenticated WebSocket itself.
        if !app && request.method() != Method::GET {
            return Err(ws_required());
        }
        let local = caller.local();
        self.last_request.store(now_ms(), Ordering::SeqCst);
        let method = request.method().clone();
        let uri = request.uri().clone();
        let segments: Vec<String> = uri
            .path()
            .trim_matches('/')
            .split('/')
            .map(str::to_string)
            .collect();
        let body = collect_body(request.into_body()).await?;
        let parse = |bytes: &Bytes| -> Result<Value, (StatusCode, String)> {
            if bytes.is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_slice(bytes).map_err(|e| bad(e.to_string()))
            }
        };
        fn typed<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, (StatusCode, String)> {
            serde_json::from_value(value).map_err(|e| bad(e.to_string()))
        }
        let s: Vec<&str> = segments.iter().map(String::as_str).collect();
        // Imported ports belong to another node; their requests are forwarded.
        if let ["v1", "ports", id, rest @ ..] = s.as_slice() {
            if remote::split(id).is_some() {
                return self
                    .remote_port(&method, id, rest, &uri, &body, caller)
                    .await;
            }
        }
        if let ["v1", "remotes", rest @ ..] = s.as_slice() {
            if !app {
                return Err(forbidden());
            }
            return self.remote_admin(&method, rest, parse(&body)?).await;
        }
        if let ["v1", "compile", rest @ ..] = s.as_slice() {
            return self
                .compile_request(&method, rest, &uri, parse(&body)?, app)
                .await;
        }
        match (&method, s.as_slice()) {
            (&Method::GET, ["v1", "health"]) => Ok((
                StatusCode::OK,
                json!({
                    "apiVersion": API_VERSION,
                    "deviceWebSocketProtocol": ws::PROTOCOL,
                    "buildId": BUILD_ID,
                    "instanceId": self.instance_id,
                    "instanceGeneration": self.instance_generation,
                    "pid": std::process::id(),
                    "startedAt": self.started_at,
                    "h2loaderCore": h2loader::AVAILABLE,
                    "compileProtocol": compile_protocol(),
                    "caller": if app { "app" } else { "gateway" },
                }),
            )),
            (&Method::GET, ["v1", "gateway"]) if app => Ok((StatusCode::OK, self.gateway_view())),
            (&Method::PATCH, ["v1", "gateway"]) if app => {
                let (this, body) = (self.clone(), typed::<GatewayPatch>(parse(&body)?)?);
                tokio::task::spawn_blocking(move || this.reconfigure(body))
                    .await
                    .map_err(|_| bad("内部错误"))?
            }
            (_, ["v1", "gateway"]) | (_, ["v1", "shutdown"]) | (_, ["v1", "retire"]) if !app => {
                Err(forbidden())
            }
            (&Method::POST, ["v1", "packages", "inspect"]) if !local => Err(forbidden()),
            (&Method::GET, ["v1", "ports"]) => Ok((StatusCode::OK, self.snapshot())),
            (&Method::GET, ["v1", "watch"]) => {
                Ok((StatusCode::OK, self.watch_snapshot(&uri).await))
            }
            (&Method::GET, ["v1", "system-ports"]) => {
                Ok((StatusCode::OK, json!(*self.system.lock().unwrap())))
            }
            (&Method::POST, ["v1", "ports"]) => {
                let this = self.clone();
                let body = typed::<AddPort>(parse(&body)?)?;
                tokio::task::spawn_blocking(move || this.add(body, caller))
                    .await
                    .map_err(|_| bad("内部错误"))?
            }
            (&Method::PATCH, ["v1", "ports", id]) => {
                let (this, id) = (self.clone(), id.to_string());
                let body = typed::<PatchPort>(parse(&body)?)?;
                tokio::task::spawn_blocking(move || this.patch(&id, body, caller))
                    .await
                    .map_err(|_| bad("内部错误"))?
            }
            (&Method::DELETE, ["v1", "ports", id]) => self.remove(id, caller),
            (&Method::GET, ["v1", "ports", id]) => {
                let ports = self.ports.lock().unwrap();
                let port = ports
                    .iter()
                    .find(|p| p.config.id == *id)
                    .ok_or_else(not_found)?;
                Ok((StatusCode::OK, self.view(port)))
            }
            (&Method::GET, ["v1", "ports", id, "logs"]) => self.logs(id, &uri),
            (&Method::DELETE, ["v1", "ports", id, "logs"]) => {
                let _admission = self.admitted(caller)?;
                self.with_port(id, |port| {
                    port.shared.logs.lock().unwrap().clear();
                    port.shared.dirty.store(true, Ordering::SeqCst);
                    Ok((StatusCode::OK, json!({ "cleared": true })))
                })
            }
            (&Method::POST, ["v1", "ports", id, "send"]) => {
                self.send(id, typed(parse(&body)?)?, caller)
            }
            (&Method::POST, ["v1", "ports", id, "jobs"]) => {
                let (this, id) = (self.clone(), id.to_string());
                let mut body = typed::<StartJob>(parse(&body)?)?;
                if !local && body.path.is_some() {
                    return Err((StatusCode::FORBIDDEN, "局域网客户端不能指定本机文件".into()));
                }
                if !app && body.source.is_none() {
                    body.source = Some("gateway".into());
                }
                this.start_job(&id, body, caller)
            }
            (&Method::POST, ["v1", "jobs", id, "cancel"]) => {
                let local = {
                    let _admission = self.admitted(caller)?;
                    self.cancel_job(id)
                };
                match local {
                    Err((StatusCode::NOT_FOUND, _)) if app => {
                        let _work = self.forwarded_work(caller)?;
                        self.remotes.cancel_job(id).await
                    }
                    other => other,
                }
            }
            (&Method::POST, ["v1", "ports", id, "requests", request, action]) if app => {
                let request: u64 = request.parse().map_err(|_| not_found())?;
                match *action {
                    "accept" => self.accept(id, request),
                    "reject" => self.reject(id, request),
                    _ => Err(not_found()),
                }
            }
            (&Method::POST, ["v1", "ports", id, "connections", connection, "disconnect"])
                if app =>
            {
                let connection = connection
                    .parse::<u64>()
                    .map_err(|_| bad("Invalid WS connection ID"))?;
                let action = typed::<DisconnectSession>(parse(&body)?)?;
                self.disconnect_session(id, connection, action.pause_automatic)
            }
            (_, ["v1", "ports", _, "requests", ..])
            | (_, ["v1", "ports", _, "connections", ..])
                if !app =>
            {
                Err(forbidden())
            }
            (&Method::POST, ["v1", "packages", "inspect"]) => {
                let value = parse(&body)?;
                let path = value["path"]
                    .as_str()
                    .ok_or_else(|| bad("缺少路径"))?
                    .to_string();
                let result = tokio::task::spawn_blocking(move || h2loader::inspect(&path))
                    .await
                    .map_err(|_| bad("内部错误"))?;
                result
                    .map(|meta| (StatusCode::OK, meta))
                    .map_err(|e| bad(format!("固件包无效：{}", h2loader::describe(&e))))
            }
            (&Method::POST, ["v1", "retire"]) => self.retire(),
            (&Method::POST, ["v1", "shutdown"]) => {
                *self.admission.lock().unwrap() = false;
                let _ = self.shutdown.send(true);
                Ok((StatusCode::OK, json!({ "stopping": true })))
            }
            _ => Err((StatusCode::NOT_FOUND, "not found".into())),
        }
    }
}

// ---------------------------------------------------------------- piping

fn is_pipe_request(request: &Request<hyper::body::Incoming>) -> bool {
    request.method() == Method::GET && request.uri().path().ends_with("/pipe")
}

/// Sanitize the display identity of a WebSocket take request. The actual
/// authorization and lease identity come from the authenticated connection.
fn pipe_text(uri: &hyper::Uri, key: &str, limit: usize) -> Result<String, (StatusCode, String)> {
    let text = query_str(uri, key)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string();
    if text.is_empty() {
        Err(bad(format!("缺少 {key}：请提供客户端名称和任务标题")))
    } else {
        Ok(text)
    }
}

async fn websocket_send<S>(sink: &mut S, message: tokio_tungstenite::tungstenite::Message) -> bool
where
    S: futures_util::Sink<tokio_tungstenite::tungstenite::Message> + Unpin,
{
    use futures_util::SinkExt;
    matches!(
        tokio::time::timeout(SOCKET_SEND_TIMEOUT, sink.send(message)).await,
        Ok(Ok(()))
    )
}

/// A queued exclusive session, shared by WebSocket pipes and gRPC sessions.
struct Joined {
    pipe_id: u64,
    shared: Arc<Shared>,
    device: Arc<DeviceOutput>,
    notices: watch::Receiver<Control>,
    permit: OwnedSemaphorePermit,
}

impl Daemon {
    fn join_pipe(
        &self,
        id: &str,
        client: String,
        title: String,
        reason: String,
        caller: Caller,
        relay: bool,
    ) -> Result<Joined, (StatusCode, String)> {
        let Ok(permit) = self.pipe_slots.clone().try_acquire_owned() else {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "透传连接已满".into()));
        };
        let device = Arc::new(DeviceOutput::new());
        let (control, notices) = watch::channel(Control::Queued(1, None));
        let pipe_id = self.next_pipe.fetch_add(1, Ordering::SeqCst);
        // Recheck the authenticated generation under the same admission lock
        // as key changes and retirement; a pre-authenticated join cannot slip in.
        let _admission = self.admitted(caller)?;
        let shared = self.with_port(id, |port| {
            // Requests may wait on a configured device even while it is offline.
            let mut seat = port.shared.seat.lock().unwrap();
            if seat.queue.len() >= MAX_PIPES {
                return Err((StatusCode::SERVICE_UNAVAILABLE, "串口请求队列已满".into()));
            }
            seat.auto_grant = port.config.auto_grant;
            seat.queue.push_back(Pipe {
                id: pipe_id,
                client: client.clone(),
                title: title.clone(),
                reason: reason.clone(),
                since: now_ms(),
                caller,
                out: device.clone(),
                control,
                job_cancel: None,
                relay,
            });
            seat.notify_positions();
            drop(seat);
            Daemon::grant_next(port);
            port.shared
                .system(format!("{client} 请求独占通信：{title}"));
            Ok(port.shared.clone())
        })?;
        self.last_request.store(now_ms(), Ordering::SeqCst);
        Ok(Joined {
            pipe_id,
            shared,
            device,
            notices,
            permit,
        })
    }

    fn leave_pipe(&self, id: &str, shared: &Shared, pipe_id: u64, reason: &str) {
        let mut seat = shared.seat.lock().unwrap();
        let held = seat.holder.as_ref().is_some_and(|p| p.id == pipe_id);
        if held {
            seat.revoke(reason);
        }
        seat.queue.retain(|p| p.id != pipe_id);
        if seat.approved == Some(pipe_id) {
            seat.approved = None;
        }
        seat.notify_positions();
        drop(seat);
        let _ = self.with_port(id, |port| {
            Daemon::grant_next(port);
            Ok(())
        });
        shared.dirty.store(true, Ordering::SeqCst);
    }
}

async fn pipe(
    daemon: Arc<Daemon>,
    mut request: Request<hyper::body::Incoming>,
    peer: SocketAddr,
    lan: bool,
) -> Response<Full<Bytes>> {
    use tokio_tungstenite::tungstenite::{
        handshake::derive_accept_key,
        protocol::{Role, WebSocketConfig},
    };
    let uri = request.uri().clone();
    let token = pipe_token(&request, peer, lan).unwrap_or_default();
    let Some(caller) = daemon.authorize(&token, peer, lan) else {
        return respond(Err((StatusCode::UNAUTHORIZED, "unauthorized".into())));
    };
    let parts: Vec<_> = uri.path().trim_matches('/').split('/').collect();
    let id = match parts.as_slice() {
        ["v1", "ports", id, "pipe"] => id.to_string(),
        _ => return respond(Err(not_found())),
    };
    // Reject missing/empty metadata before allocating channels or a seat.
    let (client, title) = match (
        pipe_text(&uri, "client", 64),
        pipe_text(&uri, "title", ws::MAX_TASK_TITLE_CHARS),
    ) {
        (Ok(client), Ok(title)) => (client, title),
        (Err(error), _) | (_, Err(error)) => return respond(Err(error)),
    };
    let reason = query_str(&uri, "reason")
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect::<String>()
        .trim()
        .to_owned();
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
    };
    let key = header("sec-websocket-key").to_string();
    if header("sec-websocket-version") != "13"
        || !header("upgrade").eq_ignore_ascii_case("websocket")
        || !header("connection")
            .split(',')
            .any(|s| s.trim().eq_ignore_ascii_case("upgrade"))
        || key.len() != 24
    {
        return respond(Err(bad("需要有效的 WebSocket 升级请求")));
    }
    let Joined {
        pipe_id,
        shared,
        device,
        mut notices,
        permit: pipe_permit,
    } = match daemon.join_pipe(&id, client, title, reason, caller, true) {
        Ok(joined) => joined,
        Err(error) => return respond(Err(error)),
    };
    let mut transport_events = shared.device_events.subscribe();
    let initial_device = shared.device_state.lock().unwrap().clone();
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        let _pipe_permit = pipe_permit;
        let leave =
            |daemon: &Arc<Daemon>| daemon.leave_pipe(&id, &shared, pipe_id, "WS 连接已断开");
        let upgraded = match tokio::time::timeout(BODY_TIMEOUT, upgrade).await {
            Ok(Ok(upgraded)) => upgraded,
            _ => {
                leave(&daemon);
                return;
            }
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME))
            .max_frame_size(Some(MAX_FRAME))
            .write_buffer_size(4096)
            .max_write_buffer_size(2 * MAX_FRAME);
        let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
            TokioIo::new(upgraded),
            Role::Server,
            Some(config),
        )
        .await;
        let (mut sink, mut stream) = socket.split();
        let mut updates = daemon.version.subscribe();
        let mut requests = HashSet::new();
        let mut completed_jobs = HashSet::new();
        let mut current_job = Value::Null;
        let initial = notices.borrow_and_update().clone();
        if !websocket_send(&mut sink, Message::Text(initial.frame().into())).await
            || matches!(initial, Control::End(..))
        {
            leave(&daemon);
            return;
        }
        let mut device_sequence = initial_device.sequence;
        if !websocket_send(
            &mut sink,
            Message::Text(serde_json::to_string(&initial_device).unwrap().into()),
        )
        .await
        {
            leave(&daemon);
            return;
        }
        loop {
            tokio::select! {
                biased;
                changed = notices.changed() => {
                    if changed.is_err() { break; }
                    let notice = notices.borrow_and_update().clone();
                    let end = matches!(notice, Control::End(..));
                    if !websocket_send(&mut sink, Message::Text(notice.frame().into())).await || end { break; }
                }
                event = transport_events.recv() => {
                    let (event, missed) = match event {
                        Ok(event) if event.sequence <= device_sequence => continue,
                        Ok(event) => (event, 0),
                        Err(broadcast::error::RecvError::Lagged(missed)) => (shared.device_state.lock().unwrap().clone(), missed),
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    device_sequence = event.sequence;
                    let mut frame = serde_json::to_value(event).unwrap();
                    if missed > 0 { frame["missedEvents"] = json!(missed); }
                    if !websocket_send(&mut sink, Message::Text(frame.to_string().into())).await { break; }
                }
                changed = updates.changed() => {
                    if changed.is_err() { break; }
                    let events = shared.job_events(pipe_id, &mut completed_jobs, &mut current_job);
                    let mut failed = false;
                    for event in events {
                        if !websocket_send(&mut sink, Message::Text(event.to_string().into())).await { failed = true; break; }
                    }
                    if failed { break; }
                }
                message = stream.next() => {
                    let text = match message {
                        Some(Ok(Message::Text(text))) => Ok(text),
                        Some(Ok(Message::Binary(_))) => Err("Device commands must be JSON text frames"),
                        Some(Ok(Message::Ping(data))) => {
                            if !websocket_send(&mut sink, Message::Pong(data)).await { break; }
                            continue;
                        }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => continue,
                    };
                    let pending = {
                        let seat = shared.seat.lock().unwrap();
                        shared.authority.allows(caller)
                            && seat.queue.iter().any(|p| p.id == pipe_id && p.caller == caller)
                    };
                    let mut request_id = Value::Null;
                    let outcome = (|| -> Reply {
                        let text = text.map_err(bad)?;
                        let request = match ws::parse(&text) {
                            Ok(request) => request,
                            Err((id, message)) => { request_id = id; return Err(bad(message)); }
                        };
                        request_id = request.id.clone();
                        if pending {
                            return Err((StatusCode::CONFLICT, "尚未获得独占通信权，请等待 granted 事件".into()));
                        }
                        if requests.len() >= ws::MAX_COMMANDS {
                            return Err((StatusCode::TOO_MANY_REQUESTS, "WS 会话命令数量已达上限，请完成任务后重新连接".into()));
                        }
                        if !requests.insert(request.id.to_string()) {
                            return Err((StatusCode::CONFLICT, "重复的 WS 命令 ID；请查询结果，不要重复下发".into()));
                        }
                        daemon.ws_command(&id, pipe_id, caller, &request)
                    })();
                    let frame = ws::result(request_id,
                        outcome.map(|(_, value)| value).map_err(|(code, message)| (code.as_u16(), message)), pending);
                    // Errors refuse this command only. Pending sockets remain
                    // subscribed and receive granted on this same connection.
                    if !websocket_send(&mut sink, Message::Text(frame.to_string().into())).await { break; }
                }
                Chunk { bytes, _permit: permit } = device.recv() => {
                    let sent = websocket_send(&mut sink, Message::Binary(bytes.into())).await;
                    // Keep the in-flight bytes charged until the send completes.
                    drop(permit);
                    if !sent { break; }
                },
            }
        }
        let _ = websocket_send(&mut sink, Message::Close(None)).await;
        leave(&daemon);
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(hyper::header::CONNECTION, "Upgrade")
        .header(hyper::header::UPGRADE, "websocket")
        .header("sec-websocket-accept", derive_accept_key(key.as_bytes()))
        .body(Full::new(Bytes::new()))
        .unwrap()
}

// ---------------------------------------------------------------- compiling

fn compile_protocol() -> Value {
    #[cfg(unix)]
    return Value::from(crate::compile::PROTOCOL);
    #[cfg(not(unix))]
    return Value::Null;
}

fn is_compile_socket(request: &Request<hyper::body::Incoming>) -> bool {
    request.method() == Method::GET && request.uri().path() == "/v1/compile/ws"
}

impl Daemon {
    fn compile_busy(&self) -> bool {
        #[cfg(unix)]
        return self.compile.busy();
        #[cfg(not(unix))]
        return false;
    }

    /// Absolute path of a build artifact's firmware package.
    fn compile_artifact(
        &self,
        id: &str,
        file: Option<&str>,
    ) -> Result<String, (StatusCode, String)> {
        #[cfg(unix)]
        return self
            .compile
            .artifact_file(id, file)
            .map(|path| path.display().to_string())
            .map_err(|error| (StatusCode::NOT_FOUND, error));
        #[cfg(not(unix))]
        {
            let _ = (id, file);
            Err((
                StatusCode::NOT_IMPLEMENTED,
                "Compiling is not available on this platform".into(),
            ))
        }
    }

    async fn compile_request(
        self: &Arc<Self>,
        method: &Method,
        rest: &[&str],
        uri: &hyper::Uri,
        body: Value,
        app: bool,
    ) -> Reply {
        #[cfg(unix)]
        {
            let compile = self.compile.clone();
            if method == Method::GET && rest == ["watch"] {
                let since = query(uri, "since").unwrap_or(0);
                let timeout =
                    Duration::from_millis(query(uri, "timeoutMs").unwrap_or(20_000).min(25_000));
                let mut receiver = compile.subscribe();
                if *receiver.borrow() <= since {
                    let _ = tokio::time::timeout(timeout, receiver.wait_for(|v| *v > since)).await;
                }
                return tokio::task::spawn_blocking(move || compile.snapshot())
                    .await
                    .map(|value| (StatusCode::OK, value))
                    .map_err(|_| bad("内部错误"));
            }
            let (method, rest, query) = (
                method.as_str().to_string(),
                rest.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                uri.query().unwrap_or_default().to_string(),
            );
            let outcome = tokio::task::spawn_blocking(move || {
                let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
                compile.handle(&method, &rest, &query, body, app)
            })
            .await
            .map_err(|_| bad("内部错误"))?;
            outcome
                .map(|(status, value)| {
                    (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
                        value,
                    )
                })
                .map_err(|(status, message)| {
                    (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
                        message,
                    )
                })
        }
        #[cfg(not(unix))]
        {
            let _ = (method, rest, uri, body, app);
            Err((
                StatusCode::NOT_IMPLEMENTED,
                "Compiling is not available on this platform".into(),
            ))
        }
    }
}

/// `gizmoth.compile.v1`: one build per authenticated WebSocket.
async fn compile_socket(
    daemon: Arc<Daemon>,
    mut request: Request<hyper::body::Incoming>,
    peer: SocketAddr,
    lan: bool,
) -> Response<Full<Bytes>> {
    use tokio_tungstenite::tungstenite::{
        handshake::derive_accept_key,
        protocol::{Role, WebSocketConfig},
    };
    let uri = request.uri().clone();
    let token = pipe_token(&request, peer, lan).unwrap_or_default();
    let Some(caller) = daemon.authorize(&token, peer, lan) else {
        return respond(Err((StatusCode::UNAUTHORIZED, "unauthorized".into())));
    };
    let (client, title) = match (
        pipe_text(&uri, "client", 64),
        pipe_text(&uri, "title", ws::MAX_TASK_TITLE_CHARS),
    ) {
        (Ok(client), Ok(title)) => (client, title),
        (Err(error), _) | (_, Err(error)) => return respond(Err(error)),
    };
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let key = header("sec-websocket-key");
    if header("sec-websocket-version") != "13"
        || !header("upgrade").eq_ignore_ascii_case("websocket")
        || !header("connection")
            .split(',')
            .any(|s| s.trim().eq_ignore_ascii_case("upgrade"))
        || key.len() != 24
    {
        return respond(Err(bad("需要有效的 WebSocket 升级请求")));
    }
    #[cfg(not(unix))]
    {
        let _ = (caller, client, title, &mut request);
        return respond(Err((
            StatusCode::NOT_IMPLEMENTED,
            "Compiling is not available on this platform".into(),
        )));
    }
    #[cfg(unix)]
    {
        if let Err(error) = daemon.admitted(caller).map(drop) {
            return respond(Err(error));
        }
        let Ok(permit) = daemon.pipe_slots.clone().try_acquire_owned() else {
            return respond(Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "WebSocket connections are full".into(),
            )));
        };
        daemon.last_request.store(now_ms(), Ordering::SeqCst);
        let compile = daemon.compile.clone();
        let upgrade = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            let _permit = permit;
            let Ok(Ok(upgraded)) = tokio::time::timeout(BODY_TIMEOUT, upgrade).await else {
                return;
            };
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_FRAME))
                .max_frame_size(Some(MAX_FRAME))
                .write_buffer_size(64 * 1024)
                .max_write_buffer_size(4 * MAX_FRAME);
            let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                TokioIo::new(upgraded),
                Role::Server,
                Some(config),
            )
            .await;
            crate::compile::ws::serve(compile, socket, client, title, caller.local()).await;
        });
        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(hyper::header::CONNECTION, "Upgrade")
            .header(hyper::header::UPGRADE, "websocket")
            .header("sec-websocket-accept", derive_accept_key(key.as_bytes()))
            .body(Full::new(Bytes::new()))
            .unwrap()
    }
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn respond(reply: Reply) -> Response<Full<Bytes>> {
    let (status, body) = match reply {
        Ok((status, value)) => (status, value),
        Err((status, message)) => (status, json!({ "error": message })),
    };
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .header(hyper::header::CACHE_CONTROL, "no-store")
        .body(Full::new(Bytes::from(
            serde_json::to_vec(&body).unwrap_or_default(),
        )))
        .unwrap()
}

// ---------------------------------------------------------------- lifecycle

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub pid: u32,
    pub port: u16,
    pub token: String,
    pub api_version: u32,
    pub started_at: u64,
}

pub fn info_path(dir: &Path) -> PathBuf {
    dir.join("daemon.json")
}

fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// The lock file is a permanent inode. A discovery request can fail or stall
/// without transferring ownership, and crashes release the OS lock themselves.
fn acquire_daemon_lock(dir: &Path) -> std::io::Result<Option<fs::File>> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(dir.join("daemon.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

fn remove_owned_info(dir: &Path, token: &str) {
    let owned = fs::read(info_path(dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Info>(&bytes).ok())
        .is_some_and(|info| info.pid == std::process::id() && info.token == token);
    if owned {
        let _ = fs::remove_file(info_path(dir));
    }
}

async fn serve_connection(
    daemon: Arc<Daemon>,
    accepted: gateway::Accepted,
    _permit: OwnedSemaphorePermit,
) {
    let (peer, lan) = (accepted.peer, accepted.lan);
    let mut stopped = daemon.shutdown.subscribe();
    let stream = accepted.stream;
    let (activity, mut activity_rx) = watch::channel(0u64);
    // In-flight gRPC handlers and response streams are active connections.
    let streams = Arc::new(AtomicU64::new(0));
    let grpc = node_service::server(daemon.clone());
    let open = streams.clone();
    let service = service_fn(move |mut request: Request<hyper::body::Incoming>| {
        activity.send_modify(|sequence| *sequence += 1);
        let daemon = daemon.clone();
        let grpc = grpc.clone();
        let open = open.clone();
        async move {
            if node_service::is_grpc(&request) {
                // Streams and uploads have per-message deadlines instead of one
                // response deadline; identity comes from this connection only.
                request
                    .extensions_mut()
                    .insert(node_service::PeerInfo { peer, lan });
                return Ok::<_, Infallible>(node_service::call(grpc, request, &open).await);
            }
            // Forwarded remote jobs may upload a firmware package first.
            let deadline = if remote::is_job_request(request.uri().path()) {
                remote::JOB_TIMEOUT
            } else {
                RESPONSE_TIMEOUT
            };
            let response = tokio::time::timeout(deadline, async move {
                if is_pipe_request(&request) {
                    return pipe(daemon, request, peer, lan).await;
                }
                if is_compile_socket(&request) {
                    return compile_socket(daemon, request, peer, lan).await;
                }
                respond(daemon.handle(request, peer, lan).await)
            })
            .await
            .unwrap_or_else(|_| respond(Err((StatusCode::REQUEST_TIMEOUT, "请求处理超时".into()))));
            Ok::<_, Infallible>(response.map(tonic::body::Body::new))
        }
    });
    // HTTP/1.1 serves the API and WebSockets; HTTP/2 prior knowledge (h2c)
    // serves gRPC on the same plaintext port on loopback, LAN and Tailscale.
    let mut http =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    // Reset the idle budget for every request. WebSockets have their own pipe
    // permit and send deadlines after Hyper hands the connection over.
    http.http1()
        .keep_alive(true)
        .timer(TokioTimer::new())
        .header_read_timeout(BODY_TIMEOUT);
    http.http2()
        .timer(TokioTimer::new())
        .max_concurrent_streams(MAX_PIPES as u32)
        .keep_alive_interval(Some(Duration::from_secs(20)))
        .keep_alive_timeout(Duration::from_secs(20));
    let connection = http.serve_connection_with_upgrades(TokioIo::new(stream), service);
    tokio::pin!(connection);
    let idle = tokio::time::sleep(RESPONSE_TIMEOUT + BODY_TIMEOUT);
    tokio::pin!(idle);
    loop {
        tokio::select! {
            _ = &mut connection => break,
            _ = async { let _ = stopped.wait_for(|stop| *stop).await; } => {
                connection.as_mut().graceful_shutdown();
                let _ = tokio::time::timeout(BODY_TIMEOUT, &mut connection).await;
                break;
            }
            changed = activity_rx.changed() => {
                if changed.is_err() { break; }
                idle.as_mut().reset(tokio::time::Instant::now() + RESPONSE_TIMEOUT + BODY_TIMEOUT);
            }
            _ = &mut idle => {
                if streams.load(Ordering::SeqCst) == 0 { break; }
                idle.as_mut().reset(tokio::time::Instant::now() + RESPONSE_TIMEOUT + BODY_TIMEOUT);
            }
        }
    }
}

/// Called only while holding the daemon lifetime lock. Persist before publishing
/// an instance so overlapping clients can order replies across process restarts.
fn advance_instance_generation(dir: &Path) -> std::io::Result<u64> {
    let path = dir.join("instance-generation.json");
    let previous = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<u64>(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    // Snapshots are consumed by JavaScript: retain exact integer ordering.
    let next = previous
        .checked_add(1)
        .filter(|next| *next <= 9_007_199_254_740_991)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "daemon generation exhausted",
            )
        })?;
    private_write(&path, next.to_string().as_bytes())?;
    Ok(next)
}

/// Runs until shutdown. Returns the process exit code.
pub fn run(data_dir: &Path) -> i32 {
    let dir = state_dir(data_dir);
    if fs::create_dir_all(&dir).is_err() {
        return 2;
    }
    let _ownership = match acquire_daemon_lock(&dir) {
        Ok(Some(file)) => file,
        Ok(None) => return 0,
        Err(_) => return 2,
    };
    let instance_generation = match advance_instance_generation(&dir) {
        Ok(generation) => generation,
        Err(_) => return 2,
    };
    let _ = fs::remove_dir_all(dir.join("packages"));
    let mut gateway_config = gateway::load_or_create(&dir, private_write);
    // Occupied ports advance to an available service port. Other bind failures
    // retain private loopback control and report the error.
    let (listener, gateway_error) = match gateway::Listeners::start(&gateway_config) {
        Ok(listener) => {
            let error = listener.network_error();
            (listener, error)
        }
        Err(error) => {
            let message = if error.kind() == std::io::ErrorKind::AddrInUse {
                format!("端口 {} 已被占用", gateway_config.port)
            } else {
                format!("无法监听 {}：{error}", gateway::address(&gateway_config))
            };
            let fallback = gateway::Config {
                enabled: false,
                ..gateway_config.clone()
            };
            match gateway::Listeners::prepare(&fallback, None) {
                Ok(listener) => (listener, Some(message)),
                Err(_) => {
                    return 2;
                }
            }
        }
    };
    let Ok(bound) = listener.local_addr() else {
        return 2;
    };
    if gateway_config.enabled && !listener.needs_port_retry() && gateway_config.port != bound.port()
    {
        gateway_config.port = bound.port();
        if private_write(
            &gateway::path(&dir),
            &serde_json::to_vec_pretty(&gateway_config).unwrap_or_default(),
        )
        .is_err()
        {
            return 2;
        }
    }
    let (rebind, mut rebind_rx) = watch::channel(0u64);
    let (version, _) = watch::channel(0u64);
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let authority = Arc::new(Authority::new(&gateway_config));
    let listener = Arc::new(listener);
    let node = grpc::load_node(&dir, private_write);
    let dirty = Arc::new(AtomicBool::new(true));
    let remotes = remote::Remotes::load(&dir, node.clone(), dirty.clone(), private_write);
    let daemon = Arc::new(Daemon {
        node,
        uploads: Mutex::new(Vec::new()),
        remotes,
        bound: Mutex::new(bound),
        gateway: Mutex::new(gateway_config),
        gateway_error: Mutex::new(gateway_error),
        listeners: Mutex::new(Some(listener.clone())),
        pending: Mutex::new(None),
        rebind,
        next_pipe: AtomicU64::new(1),
        token: random_token(),
        instance_id: uuid::Uuid::new_v4().to_string(),
        instance_generation,
        reconfiguration: Mutex::new(()),
        admission: Mutex::new(true),
        authority,
        pipe_slots: Arc::new(Semaphore::new(MAX_PIPES)),
        system: Mutex::new(Vec::new()),
        retired_workers: Mutex::new(Vec::new()),
        endpoints: Arc::new(Mutex::new(HashSet::new())),
        started_at: now_ms(),
        ports: Mutex::new(Vec::new()),
        job_ids: Mutex::new(HashSet::new()),
        dirty,
        version,
        last_request: AtomicU64::new(now_ms()),
        shutdown,
        dir: dir.clone(),
        #[cfg(unix)]
        compile: crate::compile::Service::new(data_dir),
    });
    {
        let mut ports = daemon.ports.lock().unwrap();
        for config in adopt_configs(&dir) {
            let shared = daemon.new_shared();
            ports.push(Port {
                config,
                present: false,
                shared,
                worker: None,
                stopping: None,
                preparing: None,
            });
        }
    }
    if daemon.write_info().is_err() {
        return 2;
    }
    daemon.reconcile();
    let poller = {
        let daemon = daemon.clone();
        let mut stopped = shutdown_rx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(1500));
            if *stopped.borrow_and_update() {
                break;
            }
            daemon.reconcile();
            daemon.refresh_gateway_network();
        })
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    if let Ok(runtime) = runtime {
        let daemon = daemon.clone();
        runtime.block_on(async move {
            let adopted = listener.adopt();
            // The daemon retains the current socket set. Do not keep an extra
            // startup copy alive after a later scope change closes LAN sockets.
            drop(listener);
            let Ok(mut listener) = adopted else {
                return;
            };
            daemon.remotes.start();
            let ticker = {
                let daemon = daemon.clone();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_millis(120));
                    loop {
                        interval.tick().await;
                        if daemon.dirty.swap(false, Ordering::SeqCst) {
                            daemon.version.send_modify(|v| *v += 1);
                        }
                        let idle = now_ms().saturating_sub(daemon.last_request.load(Ordering::SeqCst));
                        let any_enabled = daemon.ports.lock().unwrap().iter().any(|p| p.config.enabled);
                        // An enabled Gateway keeps the service available to agents.
                        // So do imported nodes, which keep their sessions here.
                        let serving = daemon.gateway.lock().unwrap().enabled
                            || daemon.remotes.has_nodes()
                            || daemon.compile_busy();
                        if !any_enabled && !serving && idle > IDLE_EXIT_MS {
                            let _ = daemon.shutdown.send(true);
                        }
                    }
                })
            };
            // Remote clients cannot consume the local control connection budget.
            let local_capacity = Arc::new(Semaphore::new(MAX_CONNECTIONS));
            let remote_capacity = Arc::new(Semaphore::new(MAX_CONNECTIONS));
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = shutdown_rx.wait_for(|stop| *stop) => break,
                    _ = connections.join_next(), if !connections.is_empty() => {},
                    _ = rebind_rx.changed() => daemon.adopt_pending(&mut listener),
                    accepted = listener.accept() => {
                        let Ok(accepted) = accepted else { continue };
                        let (peer, lan) = (accepted.peer, accepted.lan);
                        let capacity = if !lan && peer.ip().is_loopback() { &local_capacity } else { &remote_capacity };
                        let Ok(permit) = capacity.clone().try_acquire_owned() else { continue };
                        connections.spawn(serve_connection(daemon.clone(), accepted, permit));
                    }
                }
            }
            // /retire signals shutdown while returning a response. Let Hyper
            // flush that response before dropping the runtime/listeners.
            let _ = tokio::time::timeout(BODY_TIMEOUT, async {
                while connections.join_next().await.is_some() {}
            }).await;
            connections.abort_all();
            ticker.abort();
        });
    }
    *daemon.admission.lock().unwrap() = false;
    daemon.shutdown.send_replace(true);
    // Stop the enumerator first, so it cannot create a worker during teardown.
    let _ = poller.join();
    let mut handles: Vec<JoinHandle<()>> = {
        let mut ports = daemon.ports.lock().unwrap();
        ports
            .iter_mut()
            .filter_map(|port| {
                Daemon::stop_worker(port, Connection::Off);
                port.shared.seat.lock().unwrap().end("串口服务已停止");
                port.stopping.take()
            })
            .collect()
    };
    handles.extend(daemon.retired_workers.lock().unwrap().drain(..));
    // Keep the lifetime OS lock through real worker exit, even if a driver is
    // slow to release a handle. Never advertise a replacement owner early.
    for handle in handles {
        let _ = handle.join();
    }
    #[cfg(unix)]
    daemon.compile.shutdown();
    remove_owned_info(&dir, &daemon.token);
    let _ = fs::remove_dir_all(dir.join("packages"));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::server::conn::http1;

    include!("session_tests.rs");

    #[test]
    fn h2loader_cancellation_keeps_owned_flags_and_only_yields_while_monitoring() {
        let (_directory, daemon) = fixture();
        let shared = daemon.new_shared();
        let stop = AtomicBool::new(false);
        let interrupt = AtomicBool::new(false);
        let events = H2Events {
            shared: &shared,
            worker: 0,
            stop: &stop,
            interrupt: &interrupt,
            job_cancel: RefCell::new(None),
            monitoring: Cell::new(false),
            active: Cell::new(false),
        };
        events.log(b"startup\n");
        assert!(!events.cancelled());
        assert!(
            events.active.get(),
            "startup must not consume the monitor's idle signal"
        );
        events.monitoring.set(true);
        assert!(!events.cancelled());
        assert!(!events.active.get());
        interrupt.store(true, Ordering::SeqCst);
        assert!(events.cancelled());

        let cancel = Arc::new(AtomicBool::new(false));
        *events.job_cancel.borrow_mut() = Some(cancel.clone());
        assert_eq!(Arc::strong_count(&cancel), 2);
        assert!(
            !events.cancelled(),
            "an active job uses its own cancellation flag"
        );
        cancel.store(true, Ordering::SeqCst);
        assert!(events.cancelled());
        *events.job_cancel.borrow_mut() = None;
        assert_eq!(Arc::strong_count(&cancel), 1);
        stop.store(true, Ordering::SeqCst);
        assert!(events.cancelled());
    }

    fn fixture() -> (tempfile::TempDir, Arc<Daemon>) {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path().join("serial");
        fs::create_dir(&dir).unwrap();
        let config = gateway::Config::default();
        let node = grpc::NodeIdentity {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Fixture".into(),
        };
        let remotes = remote::Remotes::load(
            &dir,
            node.clone(),
            Arc::new(AtomicBool::new(false)),
            private_write,
        );
        let daemon = Arc::new(Daemon {
            dir,
            bound: Mutex::new("127.0.0.1:7420".parse().unwrap()),
            authority: Arc::new(Authority::new(&config)),
            gateway: Mutex::new(config),
            gateway_error: Mutex::new(None),
            listeners: Mutex::new(None),
            pending: Mutex::new(None),
            rebind: watch::channel(0).0,
            next_pipe: AtomicU64::new(1),
            token: "app-token".into(),
            instance_id: uuid::Uuid::new_v4().to_string(),
            instance_generation: 1,
            started_at: 1,
            reconfiguration: Mutex::new(()),
            admission: Mutex::new(true),
            pipe_slots: Arc::new(Semaphore::new(MAX_PIPES)),
            ports: Mutex::new(Vec::new()),
            job_ids: Mutex::new(HashSet::new()),
            system: Mutex::new(Vec::new()),
            retired_workers: Mutex::new(Vec::new()),
            endpoints: Arc::new(Mutex::new(HashSet::new())),
            dirty: Arc::new(AtomicBool::new(false)),
            version: watch::channel(0).0,
            last_request: AtomicU64::new(1),
            shutdown: watch::channel(false).0,
            node,
            uploads: Mutex::new(Vec::new()),
            remotes,
            #[cfg(unix)]
            compile: crate::compile::Service::new(directory.path()),
        });
        (directory, daemon)
    }

    fn offline_network_fixture() -> (tempfile::TempDir, Arc<Daemon>) {
        let (directory, daemon) = fixture();
        let mut config = gateway::Config {
            scope: gateway::Scope::Lan,
            port: 0,
            ..gateway::Config::default()
        };
        let listeners = Arc::new(gateway::Listeners::prepare_for_ips(&config, None, &[]).unwrap());
        let address = listeners.local_addr().unwrap();
        config.port = address.port();
        private_write(
            &gateway::path(&daemon.dir),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        *daemon.gateway.lock().unwrap() = config;
        *daemon.bound.lock().unwrap() = address;
        *daemon.gateway_error.lock().unwrap() = listeners.network_error();
        *daemon.listeners.lock().unwrap() = Some(listeners);
        daemon.authority.lan.store(true, Ordering::SeqCst);
        daemon.write_info().unwrap();
        (directory, daemon)
    }

    fn config() -> PortConfig {
        PortConfig {
            id: "a".into(),
            name: "Board".into(),
            path: "/synthetic/old".into(),
            protocol: Protocol::H2loader,
            baud: 115_200,
            enabled: false,
            mode: Mode::Takeover,
            h2loader_seen: false,
            auto_grant: true,
            usb: None,
            created_at: 1,
        }
    }

    fn port(daemon: &Daemon) -> Port {
        Port {
            config: config(),
            present: false,
            shared: daemon.new_shared(),
            worker: None,
            stopping: None,
            preparing: None,
        }
    }

    fn mock_worker(port: &mut Port, detected: Protocol) -> mpsc::Receiver<Command> {
        mock_worker_epoch(port, detected, 1)
    }

    fn mock_worker_epoch(
        port: &mut Port,
        detected: Protocol,
        worker_id: u64,
    ) -> mpsc::Receiver<Command> {
        let (tx, rx) = mpsc::sync_channel(QUEUE_MESSAGES);
        port.config.enabled = true;
        port.shared.update(|live| {
            live.worker = Some(worker_id);
            live.connection = Connection::Connected;
            live.detected = Some(detected);
        });
        {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.writer = Some(Writer {
                worker: worker_id,
                tx: tx.clone(),
                budget: Arc::new(Semaphore::new(QUEUE_BYTES)),
            });
            seat.ready = true;
        }
        port.worker = Some(Worker {
            id: worker_id,
            tx,
            stop: Arc::new(AtomicBool::new(false)),
            interrupt: Arc::new(AtomicBool::new(false)),
            handle: std::thread::spawn(|| {}),
        });
        port.shared
            .publish_device(DeviceState::Connected, Connection::Connected, None);
        rx
    }

    fn test_pipe(id: u64, caller: Caller) -> (Pipe, watch::Receiver<Control>, Arc<DeviceOutput>) {
        let (control, notices) = watch::channel(Control::Queued(1, None));
        let device = Arc::new(DeviceOutput::new());
        (
            Pipe {
                id,
                client: format!("client-{id}"),
                title: format!("Task {id}"),
                reason: "test".into(),
                since: 1,
                caller,
                out: device.clone(),
                control,
                job_cancel: None,
                relay: true,
            },
            notices,
            device,
        )
    }

    fn test_job() -> Job {
        Job {
            id: "job".into(),
            kind: JobKind::Install,
            state: JobState::Running,
            phase: "stage".into(),
            acknowledged: 0,
            total: 1,
            source: "app".into(),
            created_at: 1,
            finished_at: None,
            error: None,
            package: None,
            cancel: Arc::new(AtomicBool::new(false)),
            snapshot: None,
            worker: 1,
            lease: None,
        }
    }

    fn patch(value: Value) -> PatchPort {
        serde_json::from_value(value).unwrap()
    }
    fn gateway_patch(value: Value) -> GatewayPatch {
        serde_json::from_value(value).unwrap()
    }
    fn start(kind: JobKind, path: Option<String>) -> StartJob {
        StartJob {
            kind,
            path,
            source: None,
            job_id: None,
            lease: None,
        }
    }

    #[test]
    fn permanent_lock_inode_outlives_owner_and_ignores_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire_daemon_lock(dir.path()).unwrap().unwrap();
        fs::write(
            info_path(dir.path()),
            b"unhealthy discovery is not ownership",
        )
        .unwrap();
        assert!(acquire_daemon_lock(dir.path()).unwrap().is_none());
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            first.metadata().unwrap().ino()
        };
        drop(first);
        assert!(dir.path().join("daemon.lock").is_file());
        let _second = acquire_daemon_lock(dir.path()).unwrap().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(inode, _second.metadata().unwrap().ino());
        }
    }

    #[test]
    fn info_cleanup_requires_the_launch_token() {
        let (_directory, daemon) = fixture();
        daemon.write_info().unwrap();
        remove_owned_info(&daemon.dir, "another launch");
        assert!(info_path(&daemon.dir).is_file());
        remove_owned_info(&daemon.dir, &daemon.token);
        assert!(!info_path(&daemon.dir).exists());
    }

    #[test]
    fn manual_and_fifo_grants_keep_the_holder_through_transport_loss() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::Serial);
        let (first, first_notice, _first_bytes) = test_pipe(1, Caller::App);
        let (second, second_notice, _second_bytes) = test_pipe(2, Caller::App);
        {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.queue.extend([first, second]);
            seat.mode = Mode::Piping;
            seat.auto_grant = false;
            seat.approved = Some(2);
            seat.ready = false;
            assert!(seat.promote(&daemon.authority).is_none());
        }
        assert!(matches!(*second_notice.borrow(), Control::Queued(..)));
        assert!(port.shared.transport_ready(1));
        assert!(matches!(*second_notice.borrow(), Control::Granted));
        assert!(matches!(*first_notice.borrow(), Control::Queued(..)));
        port.shared
            .transport_down(1, Connection::Error, "reconnect");
        assert!(matches!(*second_notice.borrow(), Control::Granted));
        {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.auto_grant = true;
            assert!(seat.promote(&daemon.authority).is_none());
        }
        assert!(port.shared.transport_ready(1));
        assert_eq!(
            port.shared.seat.lock().unwrap().holder.as_ref().unwrap().id,
            2
        );
        port.shared.seat.lock().unwrap().revoke("explicit release");
        assert!(port
            .shared
            .seat
            .lock()
            .unwrap()
            .promote(&daemon.authority)
            .is_some());
        assert_eq!(
            port.shared.seat.lock().unwrap().holder.as_ref().unwrap().id,
            1
        );
        assert!(!port.shared.transport_ready(99));
    }

    #[test]
    fn accept_reserves_request_through_commit_against_a_concurrent_leave() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::H2loader);
        let stop = port.worker.as_ref().unwrap().stop.clone();
        let shared = port.shared.clone();
        let (target, _notices, _bytes) = test_pipe(2, Caller::App);
        shared.seat.lock().unwrap().queue.push_back(target);
        daemon.ports.lock().unwrap().push(port);
        let mut leaving = None;
        let (status, view) = daemon
            .accept_with("a", 2, |configs| {
                assert!(
                    shared.seat.try_lock().is_err(),
                    "request membership must remain reserved during persistence"
                );
                let shared = shared.clone();
                let (started, wait) = mpsc::channel();
                leaving = Some(std::thread::spawn(move || {
                    started.send(()).unwrap();
                    let mut seat = shared.seat.lock().unwrap();
                    // A WebSocket leave races persistence but can only remove the
                    // selected request after its acceptance has committed.
                    seat.queue.retain(|p| p.id != 2);
                    if seat.holder.as_ref().is_some_and(|p| p.id == 2) {
                        seat.revoke("left");
                    }
                    if seat.approved == Some(2) {
                        seat.approved = None;
                    }
                    seat.notify_positions();
                }));
                wait.recv_timeout(Duration::from_secs(2)).unwrap();
                daemon.persist_configs(configs)
            })
            .unwrap();
        leaving.unwrap().join().unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["mode"], "piping");
        assert!(
            !stop.load(Ordering::SeqCst),
            "ownership must preserve the protocol worker"
        );
        assert_eq!(load_configs(&daemon.dir)[0].mode, Mode::Takeover);
        let ports = daemon.ports.lock().unwrap();
        assert_eq!(ports[0].config.mode, Mode::Takeover);
        assert!(ports[0].worker.is_some());
        // A granted socket may win the leave race, exactly as the real leave
        // path removes either holder or waiter under this same seat mutex.
        assert!(shared.seat.lock().unwrap().queue.is_empty());
    }

    #[test]
    fn failed_or_missing_accept_target_does_not_transition_or_revoke_another_holder() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::Serial);
        let stop = port.worker.as_ref().unwrap().stop.clone();
        let shared = port.shared.clone();
        shared
            .seat
            .lock()
            .unwrap()
            .queue
            .push_back(test_pipe(2, Caller::App).0);
        daemon.ports.lock().unwrap().push(port);
        assert!(daemon
            .accept_with("a", 2, |_| Err(std::io::Error::other(
                "synthetic persistence failure"
            )))
            .is_err());
        assert_eq!(daemon.ports.lock().unwrap()[0].config.mode, Mode::Takeover);
        assert!(!stop.load(Ordering::SeqCst));
        assert_eq!(shared.seat.lock().unwrap().queue[0].id, 2);
        assert!(!daemon.dir.join("ports.json").exists());
        let (holder, notice, _bytes) = test_pipe(1, Caller::App);
        holder.control.send_replace(Control::Granted);
        daemon
            .with_port("a", |port| {
                port.config.mode = Mode::Piping;
                let mut seat = port.shared.seat.lock().unwrap();
                seat.mode = Mode::Piping;
                seat.holder = Some(holder);
                seat.queue.clear(); // the selected request already disconnected
                Ok(())
            })
            .unwrap();
        assert_eq!(
            daemon
                .accept_with("a", 2, |_| panic!(
                    "missing requests must not persist configuration"
                ))
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(shared.seat.lock().unwrap().holder.as_ref().unwrap().id, 1);
        assert!(matches!(*notice.borrow(), Control::Granted));
        assert!(!stop.load(Ordering::SeqCst));
    }

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    async fn test_server(daemon: Arc<Daemon>) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            http1::Builder::new()
                .serve_connection(
                    TokioIo::new(stream),
                    service_fn(move |request| {
                        let daemon = daemon.clone();
                        async move {
                            let response = if is_pipe_request(&request) {
                                pipe(daemon, request, peer, false).await
                            } else {
                                respond(daemon.handle(request, peer, false).await)
                            };
                            Ok::<_, Infallible>(response)
                        }
                    }),
                )
                .with_upgrades()
                .await
                .unwrap();
        });
        address
    }

    async fn connect(daemon: Arc<Daemon>) -> Socket {
        let key = daemon.gateway.lock().unwrap().api_key.clone();
        let address = test_server(daemon).await;
        // Every connection deliberately has the same name and API key.
        let mut request = format!("ws://{address}/v1/ports/a/pipe?client=same-agent&title=Device%20diagnostics&reason=test")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {key}").parse().unwrap());
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            tokio_tungstenite::client_async(request, stream),
        )
        .await
        .unwrap()
        .unwrap()
        .0
    }

    async fn notice(socket: &mut Socket, kind: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                    panic!("expected a control notice")
                };
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["type"] == kind {
                    return value;
                }
                assert!(
                    value["type"] == "queued"
                        || value["type"] == "job"
                        || value["type"] == "device",
                    "{value}"
                );
            }
        })
        .await
        .unwrap()
    }

    async fn ws_call(socket: &mut Socket, id: &str, method: &str, params: Value) -> Value {
        socket
            .send(Message::Text(
                json!({"id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let result = notice(socket, "result").await;
        assert_eq!(result["id"], id);
        result
    }

    #[tokio::test]
    async fn websocket_exclusivity_auto_grants_fences_other_senders_and_restores_control() {
        let (_directory, daemon) = fixture();
        daemon.authority.enabled.store(true, Ordering::SeqCst);
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::Serial);
        port.config.protocol = Protocol::Serial;
        let shared = port.shared.clone();
        daemon.ports.lock().unwrap().push(port);
        assert!(daemon.view(&daemon.ports.lock().unwrap()[0])["autoGrant"]
            .as_bool()
            .unwrap());
        daemon
            .send(
                "a",
                SendText {
                    text: "stale UI".into(),
                    line_ending: None,
                },
                Caller::App,
            )
            .unwrap();

        let mut first = connect(daemon.clone()).await;
        notice(&mut first, "granted").await;
        let worker = daemon.ports.lock().unwrap()[0].worker.as_ref().unwrap().id;
        assert_eq!(
            worker, 1,
            "plain serial keeps the service's existing open handle"
        );
        assert_eq!(daemon.ports.lock().unwrap()[0].config.mode, Mode::Takeover);
        assert_eq!(
            daemon.view(&daemon.ports.lock().unwrap()[0])["mode"],
            "piping"
        );
        let Command::Send(stale) = rx.try_recv().unwrap() else {
            panic!("send")
        };
        let mut output = Vec::new();
        write_packet(&mut output, &shared, stale, &AtomicBool::new(false)).unwrap();
        assert!(
            output.is_empty(),
            "a queued UI write cannot cross the grant boundary"
        );

        for caller in [
            Caller::App,
            Caller::Gateway {
                local: true,
                generation: 1,
            },
        ] {
            assert_eq!(
                daemon
                    .send(
                        "a",
                        SendText {
                            text: "blocked".into(),
                            line_ending: None
                        },
                        caller
                    )
                    .unwrap_err()
                    .0,
                if caller == Caller::App {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::FORBIDDEN
                }
            );
            assert_eq!(
                daemon
                    .start_job("a", start(JobKind::Refresh, None), caller)
                    .unwrap_err()
                    .0,
                if caller == Caller::App {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::FORBIDDEN
                }
            );
        }
        let mut second = connect(daemon.clone()).await;
        assert_eq!(notice(&mut second, "queued").await["state"], "pending");
        let refused = ws_call(
            &mut second,
            "second-send",
            "send",
            json!({"text":"forbidden","lineEnding":"none"}),
        )
        .await;
        assert_eq!(refused["state"], "pending");
        assert_eq!(refused["ok"], false);
        assert!(
            rx.try_recv().is_err(),
            "ungranted WS bytes must not reach the worker"
        );
        // This same socket must stay open and receive granted after first leaves.
        first
            .send(Message::Binary(b"not a command".to_vec().into()))
            .await
            .unwrap();
        assert_eq!(notice(&mut first, "result").await["ok"], false);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            ws_call(
                &mut first,
                "first-send",
                "send",
                json!({"text":"first","lineEnding":"none"})
            )
            .await["ok"],
            true
        );
        let Command::Send(packet) = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(packet) = rx.try_recv() {
                    break packet;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap() else {
            panic!("send")
        };
        write_packet(&mut output, &shared, packet, &AtomicBool::new(false)).unwrap();
        assert_eq!(output, b"first");
        shared.relay(b"device reply");
        assert_eq!(
            first.next().await.unwrap().unwrap(),
            Message::Binary(b"device reply".to_vec().into())
        );

        first.close(None).await.unwrap();
        notice(&mut second, "granted").await;
        assert_eq!(
            ws_call(
                &mut second,
                "second-send",
                "send",
                json!({"text":"second","lineEnding":"none"})
            )
            .await["ok"],
            true
        );
        let Command::Send(packet) = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(packet) = rx.try_recv() {
                    break packet;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap() else {
            panic!("send")
        };
        write_packet(&mut output, &shared, packet, &AtomicBool::new(false)).unwrap();
        assert_eq!(output, b"firstsecond");
        second.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if shared.seat.lock().unwrap().mode == Mode::Takeover {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        daemon
            .send(
                "a",
                SendText {
                    text: "UI restored".into(),
                    line_ending: None,
                },
                Caller::App,
            )
            .unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Command::Send(_)));
        assert!(daemon.ports.lock().unwrap()[0].config.auto_grant);
    }

    async fn worker_command(rx: &mpsc::Receiver<Command>) -> Command {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(command) = rx.try_recv() {
                    break command;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn websocket_h2loader_commands_publish_jobs_and_release_only_after_cancellation() {
        let (directory, daemon) = fixture();
        daemon.authority.enabled.store(true, Ordering::SeqCst);
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::H2loader);
        let shared = port.shared.clone();
        daemon.ports.lock().unwrap().push(port);
        let mut first = connect(daemon.clone()).await;
        notice(&mut first, "granted").await;
        assert_eq!(
            shared.live.lock().unwrap().detected,
            Some(Protocol::H2loader)
        );
        assert_eq!(
            daemon.ports.lock().unwrap()[0].worker.as_ref().unwrap().id,
            1
        );

        // Installation dispatches a native preparation job, not raw JSON or
        // protocol bytes. This intentionally invalid fixture cannot be flashed.
        let package = directory.path().join("invalid.update.tar.zlib");
        fs::write(&package, b"invalid test package").unwrap();
        let install = ws_call(
            &mut first,
            "install-1",
            "install",
            json!({"package":package,"stageOnly":true}),
        )
        .await;
        assert_eq!(install["ok"], true);
        assert_eq!(install["result"]["kind"], "stage");
        let install_id = install["result"]["id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let finished = {
                    shared
                        .live
                        .lock()
                        .unwrap()
                        .history
                        .iter()
                        .any(|job| job.id == install_id)
                };
                if finished {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        daemon.version.send_modify(|version| *version += 1);
        let event = notice(&mut first, "job").await;
        assert_eq!(event["job"]["id"], install_id);
        assert_eq!(event["job"]["state"], "failed");
        assert!(rx.try_recv().is_err());

        let refresh = ws_call(&mut first, "refresh-1", "refresh", json!({})).await;
        assert_eq!(refresh["ok"], true);
        assert!(matches!(worker_command(&rx).await, Command::Job));
        assert_eq!(
            shared.live.lock().unwrap().job.as_ref().unwrap().source,
            "ws"
        );
        assert_eq!(
            ws_call(&mut first, "refresh-1", "refresh", json!({})).await["ok"],
            false
        );
        assert!(
            rx.try_recv().is_err(),
            "a repeated request ID cannot enqueue the job twice"
        );
        shared.update(|live| {
            let job = live.job.as_mut().unwrap();
            job.state = JobState::Running;
            job.acknowledged = 3;
            job.total = 10;
        });
        daemon.version.send_modify(|version| *version += 1);
        assert_eq!(notice(&mut first, "job").await["job"]["acknowledged"], 3);
        shared.finish_job(1, Ok(()));
        daemon.version.send_modify(|version| *version += 1);
        assert_eq!(notice(&mut first, "job").await["job"]["state"], "succeeded");

        let reboot = ws_call(&mut first, "reboot-1", "reboot", json!({"target":"app"})).await;
        assert_eq!(reboot["ok"], true);
        assert_eq!(reboot["result"]["kind"], "rebootApp");
        assert!(matches!(worker_command(&rx).await, Command::Job));
        let cancel = shared
            .live
            .lock()
            .unwrap()
            .job
            .as_ref()
            .unwrap()
            .cancel
            .clone();
        let wrong_id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            ws_call(
                &mut first,
                "cancel-wrong",
                "cancel",
                json!({"jobId":wrong_id})
            )
            .await["ok"],
            false
        );
        assert!(!cancel.load(Ordering::SeqCst));
        assert_eq!(
            ws_call(
                &mut first,
                "cancel-1",
                "cancel",
                json!({"jobId":reboot["result"]["id"]})
            )
            .await["ok"],
            true
        );
        assert!(cancel.load(Ordering::SeqCst));
        // Clear only the fake worker's observed flag to separately prove that
        // disconnect itself cancels work before the next socket can be granted.
        cancel.store(false, Ordering::SeqCst);
        let mut second = connect(daemon.clone()).await;
        notice(&mut second, "queued").await;
        first.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !cancel.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        {
            let seat = shared.seat.lock().unwrap();
            assert!(seat.holder.is_none());
            assert!(seat.active_job.is_some());
            assert_eq!(seat.queue.len(), 1);
        }
        shared.finish_job(1, Err("cancelled".into()));
        notice(&mut second, "granted").await;
        assert_eq!(
            shared.live.lock().unwrap().history[0].state,
            JobState::Cancelled
        );
        assert_eq!(
            daemon.ports.lock().unwrap()[0].worker.as_ref().unwrap().id,
            1
        );
        second.close(None).await.unwrap();
    }

    #[tokio::test]
    async fn external_http_is_query_only_even_when_no_socket_holds_the_device() {
        let (_directory, daemon) = fixture();
        daemon.authority.enabled.store(true, Ordering::SeqCst);
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::Serial);
        port.config.protocol = Protocol::Serial;
        daemon.ports.lock().unwrap().push(port);
        let key = daemon.gateway.lock().unwrap().api_key.clone();
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        for (method, path, body) in [
            (
                reqwest::Method::POST,
                "/v1/ports/a/send",
                json!({"text":"forbidden"}),
            ),
            (
                reqwest::Method::POST,
                "/v1/ports/a/jobs",
                json!({"kind":"rebootApp"}),
            ),
            (reqwest::Method::POST, "/v1/jobs/any/cancel", json!({})),
            (
                reqwest::Method::PATCH,
                "/v1/ports/a",
                json!({"enabled":false}),
            ),
        ] {
            let address = test_server(daemon.clone()).await;
            let response = http
                .request(method, format!("http://{address}{path}"))
                .bearer_auth(&key)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        }
        assert!(rx.try_recv().is_err());
        let address = test_server(daemon.clone()).await;
        let response = http
            .get(format!("http://{address}/v1/ports/a"))
            .bearer_auth(key)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["mode"], "takeover");
    }

    #[test]
    fn remote_ws_install_cannot_read_a_service_local_package() {
        let (_directory, daemon) = fixture();
        daemon.authority.enabled.store(true, Ordering::SeqCst);
        daemon.authority.lan.store(true, Ordering::SeqCst);
        let caller = Caller::Gateway {
            local: false,
            generation: 1,
        };
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::H2loader);
        {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.mode = Mode::Piping;
            seat.holder = Some(test_pipe(1, caller).0);
        }
        daemon.ports.lock().unwrap().push(port);
        let package = daemon.dir.join("private.update.tar.zlib");
        let request = ws::parse(
            &json!({"id":"install","method":"install","params":{"package":package}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            daemon.ws_command("a", 1, caller, &request).unwrap_err().0,
            StatusCode::FORBIDDEN
        );
        assert!(rx.try_recv().is_err());
        assert!(!daemon.dir.join("packages").exists());
        assert!(daemon.ports.lock().unwrap()[0]
            .shared
            .live
            .lock()
            .unwrap()
            .job
            .is_none());
    }

    #[test]
    fn automatic_ws_waits_for_jobs_and_preserves_h2loader_protocol() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::H2loader);
        let (request, notices, _bytes) = test_pipe(1, Caller::App);
        port.shared.seat.lock().unwrap().queue.push_back(request);
        port.shared.update(|live| live.job = Some(test_job()));
        Daemon::grant_next(&mut port);
        assert_eq!(port.shared.seat.lock().unwrap().mode, Mode::Takeover);
        assert!(matches!(*notices.borrow(), Control::Queued(..)));
        port.shared.update(|live| live.job = None);
        port.preparing = Some("preparing".into());
        Daemon::grant_next(&mut port);
        assert!(matches!(*notices.borrow(), Control::Queued(..)));
        port.preparing = None;
        Daemon::grant_next(&mut port);
        assert!(matches!(*notices.borrow(), Control::Granted));
        assert!(port.worker.is_some());
        assert!(port.stopping.is_none());
        assert_eq!(
            port.shared.live.lock().unwrap().detected,
            Some(Protocol::H2loader)
        );
        port.shared.seat.lock().unwrap().revoke("client left");
        Daemon::grant_next(&mut port);
        assert_eq!(port.shared.seat.lock().unwrap().mode, Mode::Takeover);
        assert!(port.worker.is_some());
        assert_eq!(port.config.mode, Mode::Takeover);
    }

    #[test]
    fn handoff_waits_for_a_reserved_job_even_before_it_is_published() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::H2loader);
        let shared = port.shared.clone();
        {
            let mut seat = shared.seat.lock().unwrap();
            seat.mode = Mode::Piping;
            seat.holder = Some(test_pipe(1, Caller::App).0);
            seat.queue.push_back(test_pipe(2, Caller::App).0);
        }
        daemon.ports.lock().unwrap().push(port);
        let job_id = uuid::Uuid::new_v4().to_string();
        daemon
            .reserve_job("a", JobKind::Refresh, Caller::App, &job_id, Some(1))
            .unwrap();
        {
            let mut seat = shared.seat.lock().unwrap();
            seat.revoke("disconnected during reservation");
            assert!(seat.promote(&daemon.authority).is_none());
            assert_eq!(seat.active_job.as_deref(), Some(job_id.as_str()));
        }
        assert!(shared.live.lock().unwrap().job.is_none());
        daemon
            .with_port("a", |port| {
                port.preparing = None;
                port.shared.release_job(&job_id);
                Ok(())
            })
            .unwrap();
        assert_eq!(shared.seat.lock().unwrap().holder.as_ref().unwrap().id, 2);
    }

    #[test]
    fn saved_piping_mode_never_reserves_a_device_without_a_socket() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::H2loader);
        port.config.mode = Mode::Piping; // A legacy saved setting is not a lease.
        Daemon::grant_next(&mut port);
        assert_eq!(port.shared.seat.lock().unwrap().mode, Mode::Takeover);
        let (request, _notices, _bytes) = test_pipe(1, Caller::App);
        port.shared.seat.lock().unwrap().queue.push_back(request);
        Daemon::grant_next(&mut port);
        assert!(port.shared.seat.lock().unwrap().holder.is_some());
        port.shared.seat.lock().unwrap().revoke("socket closed");
        Daemon::grant_next(&mut port);
        assert_eq!(port.shared.seat.lock().unwrap().mode, Mode::Takeover);
        assert!(port.worker.is_some());
    }

    #[test]
    fn manual_approval_keeps_default_control_until_accepted() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::Serial);
        port.config.auto_grant = false;
        let (request, notices, _bytes) = test_pipe(1, Caller::App);
        port.shared.seat.lock().unwrap().queue.push_back(request);
        Daemon::grant_next(&mut port);
        assert_eq!(port.shared.seat.lock().unwrap().mode, Mode::Takeover);
        assert!(matches!(*notices.borrow(), Control::Queued(..)));
        assert!(port.worker.is_some());
    }

    #[test]
    fn manual_accept_is_temporary_and_restores_control_after_release() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::Serial);
        port.config.protocol = Protocol::Serial;
        port.config.auto_grant = false;
        let (request, notices, _bytes) = test_pipe(1, Caller::App);
        port.shared.seat.lock().unwrap().queue.push_back(request);
        daemon.ports.lock().unwrap().push(port);
        let (_, view) = daemon
            .accept_with("a", 1, |configs| daemon.persist_configs(configs))
            .unwrap();
        assert_eq!(view["mode"], "piping");
        assert!(matches!(*notices.borrow(), Control::Granted));
        assert_eq!(load_configs(&daemon.dir)[0].mode, Mode::Takeover);
        daemon
            .with_port("a", |port| {
                port.shared.seat.lock().unwrap().holder = None;
                Daemon::grant_next(port);
                assert_eq!(daemon.view(port)["mode"], "takeover");
                assert!(!port.config.auto_grant);
                Ok(())
            })
            .unwrap();
        daemon
            .send(
                "a",
                SendText {
                    text: "UI restored".into(),
                    line_ending: None,
                },
                Caller::App,
            )
            .unwrap();
    }

    #[test]
    fn reclaim_before_transport_ready_cancels_approval_but_preserves_waiter() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::H2loader);
        port.shared.seat.lock().unwrap().ready = false;
        port.config.auto_grant = false;
        let (request, notices, _bytes) = test_pipe(1, Caller::App);
        let shared = port.shared.clone();
        shared.seat.lock().unwrap().queue.push_back(request);
        daemon.ports.lock().unwrap().push(port);
        daemon
            .accept_with("a", 1, |configs| daemon.persist_configs(configs))
            .unwrap();
        assert_eq!(shared.seat.lock().unwrap().approved, Some(1));
        daemon
            .with_port("a", |port| {
                port.worker.take().unwrap().handle.join().unwrap();
                Ok(())
            })
            .unwrap();
        daemon
            .patch("a", patch(json!({"mode": "takeover"})), Caller::App)
            .unwrap();
        {
            let seat = shared.seat.lock().unwrap();
            assert_eq!(seat.approved, None);
            assert_eq!(seat.queue.len(), 1);
        }
        daemon
            .with_port("a", |port| {
                if let Some(stopping) = port.stopping.take() {
                    stopping.join().unwrap();
                }
                let _rx = mock_worker(port, Protocol::H2loader);
                Daemon::grant_next(port);
                assert!(port.worker.is_some());
                assert_eq!(daemon.view(port)["mode"], "takeover");
                Ok(())
            })
            .unwrap();
        assert!(matches!(*notices.borrow(), Control::Queued(..)));
    }

    #[test]
    fn revoked_or_stale_epoch_packets_never_reach_the_writer() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::Serial);
        let caller = Caller::Gateway {
            local: true,
            generation: 1,
        };
        let (first, _notice, _bytes) = test_pipe(1, caller);
        let writer = {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.mode = Mode::Piping;
            seat.holder = Some(first);
            seat.writer.as_ref().unwrap().clone()
        };
        writer
            .send(
                b"old".to_vec(),
                "old".into(),
                WriteOwner::Pipe { id: 1, caller },
            )
            .unwrap();
        {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.revoke("handover");
            seat.holder = Some(test_pipe(2, caller).0);
        }
        let Command::Send(packet) = rx.try_recv().unwrap() else {
            panic!("send")
        };
        let mut output = Vec::new();
        write_packet(&mut output, &port.shared, packet, &AtomicBool::new(false)).unwrap();
        assert!(output.is_empty());
        writer
            .send(
                b"new".to_vec(),
                "new".into(),
                WriteOwner::Pipe { id: 2, caller },
            )
            .unwrap();
        let Command::Send(packet) = rx.try_recv().unwrap() else {
            panic!("send")
        };
        write_packet(&mut output, &port.shared, packet, &AtomicBool::new(false)).unwrap();
        assert_eq!(output, b"new");
        writer
            .send(
                b"stale".to_vec(),
                "stale".into(),
                WriteOwner::Pipe { id: 2, caller },
            )
            .unwrap();
        daemon.authority.generation.store(2, Ordering::SeqCst);
        let Command::Send(packet) = rx.try_recv().unwrap() else {
            panic!("send")
        };
        write_packet(&mut output, &port.shared, packet, &AtomicBool::new(false)).unwrap();
        assert_eq!(output, b"new");
    }

    #[test]
    fn gateway_writes_need_a_granted_socket_even_without_another_holder() {
        let (_directory, daemon) = fixture();
        daemon.authority.enabled.store(true, Ordering::SeqCst);
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::Serial);
        daemon.ports.lock().unwrap().push(port);
        let caller = Caller::Gateway {
            local: true,
            generation: 1,
        };
        assert_eq!(
            daemon
                .send(
                    "a",
                    SendText {
                        text: "forbidden".into(),
                        line_ending: None
                    },
                    caller
                )
                .unwrap_err()
                .0,
            StatusCode::FORBIDDEN
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn revocation_serializes_with_an_in_flight_physical_write() {
        struct BlockingWriter {
            entered: Option<mpsc::Sender<()>>,
            release: mpsc::Receiver<()>,
        }
        impl Write for BlockingWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::Serial);
        let writer = {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.mode = Mode::Piping;
            seat.holder = Some(test_pipe(1, Caller::App).0);
            seat.writer.as_ref().unwrap().clone()
        };
        writer
            .send(
                b"frame".to_vec(),
                "frame".into(),
                WriteOwner::Pipe {
                    id: 1,
                    caller: Caller::App,
                },
            )
            .unwrap();
        let Command::Send(packet) = rx.recv().unwrap() else {
            panic!("send")
        };
        let (entered, wait) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let shared = port.shared.clone();
        let writing = std::thread::spawn(move || {
            write_packet(
                &mut BlockingWriter {
                    entered: Some(entered),
                    release: blocked,
                },
                &shared,
                packet,
                &AtomicBool::new(false),
            )
            .unwrap();
        });
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            port.shared.seat.try_lock().is_err(),
            "ownership must remain locked until physical write completes"
        );
        let shared = port.shared.clone();
        let revoking = std::thread::spawn(move || shared.revoke("handover"));
        release.send(()).unwrap();
        writing.join().unwrap();
        revoking.join().unwrap();
        assert!(port.shared.seat.lock().unwrap().holder.is_none());
    }

    #[test]
    fn partial_raw_writes_check_stop_and_never_call_unbounded_flush() {
        struct PartialWriter<'a> {
            stop: &'a AtomicBool,
            bytes: Vec<u8>,
        }
        impl Write for PartialWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.push(bytes[0]);
                self.stop.store(true, Ordering::SeqCst);
                Ok(1)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                panic!("raw frames must not invoke unbounded driver drains")
            }
        }
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::Serial);
        let writer = port
            .shared
            .seat
            .lock()
            .unwrap()
            .writer
            .as_ref()
            .unwrap()
            .clone();
        writer
            .send(b"abc".to_vec(), "abc".into(), WriteOwner::App)
            .unwrap();
        let Command::Send(packet) = rx.recv().unwrap() else {
            panic!("send")
        };
        let stop = AtomicBool::new(false);
        let mut device = PartialWriter {
            stop: &stop,
            bytes: Vec::new(),
        };
        assert_eq!(
            write_packet(&mut device, &port.shared, packet, &stop)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(device.bytes, b"a");
        writer
            .send(b"x".to_vec(), "x".into(), WriteOwner::App)
            .unwrap();
        let Command::Send(packet) = rx.recv().unwrap() else {
            panic!("send")
        };
        stop.store(false, Ordering::SeqCst);
        write_packet(&mut device, &port.shared, packet, &stop).unwrap();
        assert_eq!(device.bytes, b"ax");
    }

    #[test]
    fn narrowing_scope_blocks_remote_promotions_and_writes_immediately() {
        let (_directory, daemon) = fixture();
        daemon.authority.lan.store(true, Ordering::SeqCst);
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::Serial);
        let remote = Caller::Gateway {
            local: false,
            generation: 1,
        };
        let (pipe, notices, _bytes) = test_pipe(1, remote);
        let writer = {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.mode = Mode::Piping;
            seat.holder = Some(pipe);
            seat.writer.as_ref().unwrap().clone()
        };
        writer
            .send(
                b"remote".to_vec(),
                "remote".into(),
                WriteOwner::Pipe {
                    id: 1,
                    caller: remote,
                },
            )
            .unwrap();
        daemon.authority.lan.store(false, Ordering::SeqCst);
        let Command::Send(packet) = rx.recv().unwrap() else {
            panic!("send")
        };
        let mut bytes = Vec::new();
        write_packet(&mut bytes, &port.shared, packet, &AtomicBool::new(false)).unwrap();
        assert!(bytes.is_empty());
        let mut seat = port.shared.seat.lock().unwrap();
        seat.revoke_unauthorized(&daemon.authority);
        assert!(matches!(*notices.borrow(), Control::End("revoked", _)));
        seat.queue.push_back(test_pipe(2, remote).0);
        assert!(seat.promote(&daemon.authority).is_none());
        assert!(seat.queue.is_empty());
    }

    #[test]
    fn key_rotation_revokes_real_gateway_callers_and_preserves_app_sessions() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::Serial);
        port.config.mode = Mode::Piping;
        let gateway = Caller::Gateway {
            local: true,
            generation: 1,
        };
        let (mut holder, holder_notice, _holder_bytes) = test_pipe(1, gateway);
        holder.client = "app".into(); // a self-reported label never grants App identity
        let (queued, queued_notice, _queued_bytes) = test_pipe(2, gateway);
        let (app, app_notice, _app_bytes) = test_pipe(3, Caller::App);
        {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.mode = Mode::Piping;
            seat.holder = Some(holder);
            seat.queue.extend([queued, app]);
        }
        daemon.ports.lock().unwrap().push(port);
        let old_key = daemon.gateway.lock().unwrap().api_key.clone();
        daemon
            .reconfigure(gateway_patch(json!({"regenerateKey": true})))
            .unwrap();
        assert_ne!(old_key, daemon.gateway.lock().unwrap().api_key);
        assert!(matches!(
            *holder_notice.borrow(),
            Control::End("revoked", _)
        ));
        assert!(matches!(
            *queued_notice.borrow(),
            Control::End("rejected", _)
        ));
        assert!(matches!(*app_notice.borrow(), Control::Granted));
        assert_eq!(
            daemon.admitted(gateway).unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn port_only_gateway_changes_preserve_authorization_and_local_scope_keeps_local_clients() {
        let mut seat = Seat::default();
        let local = Caller::Gateway {
            local: true,
            generation: 1,
        };
        let remote = Caller::Gateway {
            local: false,
            generation: 1,
        };
        seat.holder = Some(test_pipe(1, local).0);
        seat.queue
            .extend([test_pipe(2, remote).0, test_pipe(3, Caller::App).0]);
        let mut config = gateway::Config {
            scope: gateway::Scope::Lan,
            ..gateway::Config::default()
        };
        config.port += 1;
        let authority = Authority::new(&config);
        seat.revoke_unauthorized(&authority);
        assert!(seat.holder.is_some() && seat.queue.len() == 2);
        authority.lan.store(false, Ordering::SeqCst);
        seat.revoke_unauthorized(&authority);
        assert_eq!(seat.holder.as_ref().unwrap().id, 1);
        assert_eq!(seat.queue.len(), 1);
        authority.enabled.store(false, Ordering::SeqCst);
        authority.generation.store(2, Ordering::SeqCst);
        seat.revoke_unauthorized(&authority);
        assert!(seat.holder.is_none());
        assert_eq!(seat.queue[0].caller, Caller::App);
        assert!(authority.allows(Caller::App));
    }

    #[test]
    fn failed_patch_and_policy_escalation_leave_config_and_sessions_untouched() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        port.preparing = Some("preparing".into());
        daemon.ports.lock().unwrap().push(port);
        assert_eq!(
            daemon
                .patch("a", patch(json!({"name":"Changed", "baud":1})), Caller::App)
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            daemon
                .patch(
                    "a",
                    patch(json!({"name":"Changed", "protocol":"serial"})),
                    Caller::App
                )
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            daemon
                .patch(
                    "a",
                    patch(json!({"name":"Changed", "mode":"piping", "autoGrant":true})),
                    Caller::Gateway {
                        local: true,
                        generation: 1
                    }
                )
                .unwrap_err()
                .0,
            StatusCode::FORBIDDEN
        );
        let ports = daemon.ports.lock().unwrap();
        assert_eq!(ports[0].config.name, "Board");
        assert_eq!(ports[0].config.protocol, Protocol::H2loader);
        assert_eq!(ports[0].config.mode, Mode::Takeover);
        assert!(!daemon.dir.join("ports.json").exists());
    }

    #[test]
    fn concurrent_reconfiguration_persists_exactly_the_committed_key() {
        let (_directory, daemon) = fixture();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (daemon, barrier) = (daemon.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    daemon
                        .reconfigure(gateway_patch(json!({"regenerateKey":true})))
                        .unwrap();
                })
            })
            .collect();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }
        let persisted = gateway::read(&daemon.dir).unwrap();
        assert_eq!(persisted.api_key, daemon.gateway.lock().unwrap().api_key);
        assert_eq!(daemon.authority.generation.load(Ordering::SeqCst), 3);
        assert!(!fs::read_dir(daemon.dir.parent().unwrap())
            .unwrap()
            .any(|p| p
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("gateway.tmp-")));
    }

    #[test]
    fn persistence_failure_keeps_gateway_auth_and_config_unchanged() {
        let (_directory, daemon) = fixture();
        fs::create_dir(gateway::path(&daemon.dir)).unwrap();
        let key = daemon.gateway.lock().unwrap().api_key.clone();
        assert!(daemon
            .reconfigure(gateway_patch(json!({"regenerateKey":true})))
            .is_err());
        assert_eq!(key, daemon.gateway.lock().unwrap().api_key);
        assert_eq!(daemon.authority.generation.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retirement_checks_jobs_preparations_holders_and_queue_before_closing_admission() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        port.preparing = Some("preparing".into());
        daemon.ports.lock().unwrap().push(port);
        assert_eq!(daemon.retire().unwrap_err().0, StatusCode::CONFLICT);
        daemon
            .with_port("a", |port| {
                port.preparing = None;
                port.shared.live.lock().unwrap().job = Some(test_job());
                Ok(())
            })
            .unwrap();
        assert_eq!(daemon.retire().unwrap_err().0, StatusCode::CONFLICT);
        daemon
            .with_port("a", |port| {
                port.shared.live.lock().unwrap().job = None;
                port.shared.seat.lock().unwrap().holder = Some(test_pipe(1, Caller::App).0);
                Ok(())
            })
            .unwrap();
        assert_eq!(daemon.retire().unwrap_err().0, StatusCode::CONFLICT);
        daemon
            .with_port("a", |port| {
                let mut seat = port.shared.seat.lock().unwrap();
                seat.holder = None;
                seat.queue.push_back(test_pipe(2, Caller::App).0);
                Ok(())
            })
            .unwrap();
        assert_eq!(daemon.retire().unwrap_err().0, StatusCode::CONFLICT);
        daemon
            .with_port("a", |port| {
                port.shared.seat.lock().unwrap().queue.clear();
                Ok(())
            })
            .unwrap();
        let (status, response) = daemon.retire().unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["instanceId"], daemon.instance_id);
        assert_eq!(response["stopping"], true);
        assert!(*daemon.shutdown.borrow());
        assert_eq!(
            daemon.admitted(Caller::App).unwrap_err().0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(daemon.retire().is_ok());
    }

    #[test]
    fn retirement_waits_for_forwarded_preparation_and_rejects_late_work() {
        let (_directory, daemon) = fixture();
        let first = daemon.forwarded_work(Caller::App).unwrap();
        let second = daemon.forwarded_work(Caller::App).unwrap();
        assert_eq!(daemon.snapshot()["remoteBusy"], true);
        assert_eq!(daemon.retire().unwrap_err().0, StatusCode::CONFLICT);
        assert!(!*daemon.shutdown.borrow());
        drop(first);
        assert_eq!(daemon.retire().unwrap_err().0, StatusCode::CONFLICT);
        drop(second);
        assert_eq!(daemon.snapshot()["remoteBusy"], false);
        assert!(daemon.retire().is_ok());
        assert!(matches!(
            daemon.forwarded_work(Caller::App),
            Err((StatusCode::SERVICE_UNAVAILABLE, _))
        ));
    }

    fn wait_for_history(daemon: &Daemon, id: &str) -> Job {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let ports = daemon.ports.lock().unwrap();
            let live = ports[0].shared.live.lock().unwrap();
            if let Some(job) = live.history.iter().find(|job| job.id == id) {
                return job.clone();
            }
            drop(live);
            drop(ports);
            assert!(
                std::time::Instant::now() < deadline,
                "preparation did not finish"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn preparation_is_visible_cancellable_and_deduplicated() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::H2loader);
        daemon.ports.lock().unwrap().push(port);
        let (release, wait) = mpsc::channel();
        let job_id = uuid::Uuid::new_v4().to_string();
        let mut body = start(JobKind::Install, Some("/nonexistent/package".into()));
        body.job_id = Some(job_id.clone());
        let (_, response) = daemon
            .start_job_with("a", body, Caller::App, move || {
                wait.recv().unwrap();
            })
            .unwrap();
        assert_eq!(response["id"], job_id);
        let view = daemon.with_port("a", |port| Ok(daemon.view(port))).unwrap();
        assert_eq!(view["job"]["phase"], "preparing");
        assert_eq!(view["job"]["id"], job_id);
        let mut retry = start(JobKind::Install, None);
        retry.job_id = Some(job_id.clone());
        assert_eq!(
            daemon.start_job("a", retry, Caller::App).unwrap_err().0,
            StatusCode::CONFLICT
        );
        daemon.cancel_job(&job_id).unwrap();
        release.send(()).unwrap();
        assert_eq!(
            wait_for_history(&daemon, &job_id).state,
            JobState::Cancelled
        );
        assert!(rx.try_recv().is_err());
        assert!(!daemon.dir.join("packages").exists());
        let mut retry = start(JobKind::Install, None);
        retry.job_id = Some(job_id);
        assert_eq!(
            daemon.start_job("a", retry, Caller::App).unwrap_err().0,
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn preparation_errors_are_recorded_and_release_the_reservation() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::H2loader);
        port.config.mode = Mode::Piping;
        port.shared.seat.lock().unwrap().mode = Mode::Piping;
        daemon.ports.lock().unwrap().push(port);
        assert_eq!(
            daemon
                .start_job("a", start(JobKind::Install, None), Caller::App)
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        assert!(!daemon.dir.join("packages").exists());
        daemon
            .with_port("a", |port| {
                port.config.mode = Mode::Takeover;
                port.shared.seat.lock().unwrap().mode = Mode::Takeover;
                Ok(())
            })
            .unwrap();
        let (_, response) = daemon
            .start_job("a", start(JobKind::Install, None), Caller::App)
            .unwrap();
        let job = wait_for_history(&daemon, response["id"].as_str().unwrap());
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(job.error.as_deref(), Some("请先选择固件包"));
        assert!(daemon.ports.lock().unwrap()[0].preparing.is_none());
        drop(rx);
        let (_, response) = daemon
            .start_job("a", start(JobKind::Refresh, None), Caller::App)
            .unwrap();
        let job = wait_for_history(&daemon, response["id"].as_str().unwrap());
        assert_eq!(job.state, JobState::Failed);
        assert!(daemon.ports.lock().unwrap()[0].preparing.is_none());
    }

    #[test]
    fn preparation_reservation_is_atomic_per_port() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::H2loader);
        daemon.ports.lock().unwrap().push(port);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|index| {
                let (daemon, barrier) = (daemon.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    daemon.reserve_job(
                        "a",
                        JobKind::Install,
                        Caller::App,
                        &format!("job-{index}"),
                        None,
                    )
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| r.as_ref().is_err_and(|e| e.0 == StatusCode::CONFLICT))
                .count(),
            1
        );
    }

    #[test]
    fn queue_limits_apply_to_bytes_and_messages_and_http_reports_full() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::Serial);
        let writer = port
            .shared
            .seat
            .lock()
            .unwrap()
            .writer
            .as_ref()
            .unwrap()
            .clone();
        for _ in 0..(QUEUE_BYTES / MAX_FRAME) {
            writer
                .send(vec![0; MAX_FRAME], String::new(), WriteOwner::App)
                .unwrap();
        }
        assert_eq!(
            writer
                .send(vec![0], String::new(), WriteOwner::App)
                .unwrap_err()
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        while rx.try_recv().is_ok() {}
        assert_eq!(writer.budget.available_permits(), QUEUE_BYTES);
        for _ in 0..QUEUE_MESSAGES {
            writer
                .send(Vec::new(), String::new(), WriteOwner::App)
                .unwrap();
        }
        daemon.ports.lock().unwrap().push(port);
        assert_eq!(
            daemon
                .send(
                    "a",
                    SendText {
                        text: "x".into(),
                        line_ending: None
                    },
                    Caller::App
                )
                .unwrap_err()
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(Chunk::new(vec![0; MAX_FRAME + 1], &writer.budget).is_err());
    }

    #[tokio::test]
    async fn device_output_coalesces_callbacks_and_charges_inflight_bytes() {
        let output = DeviceOutput::new();
        assert!(output.push(&vec![0; MAX_FRAME + 1]).is_err());
        assert_eq!(output.budget.available_permits(), QUEUE_BYTES);
        // Cancel an empty receive, as select! does when another branch wins.
        assert!(tokio::time::timeout(Duration::ZERO, output.recv())
            .await
            .is_err());
        for _ in 0..1024 {
            output.push(b"x").unwrap();
        }
        assert_eq!(output.pending.lock().unwrap().len(), 1);
        let chunk = output.recv().await;
        assert_eq!(chunk.bytes, vec![b'x'; 1024]);
        assert_eq!(output.budget.available_permits(), QUEUE_BYTES - 1024);
        drop(chunk);
        assert_eq!(output.budget.available_permits(), QUEUE_BYTES);

        // Crossing a frame boundary preserves every byte and does not make an
        // oversized WS frame. Dequeuing does not refund in-flight data.
        output.push(&vec![1; MAX_FRAME - 1]).unwrap();
        output.push(&[2, 3]).unwrap();
        let first = output.recv().await;
        let second = output.recv().await;
        assert_eq!(first.bytes, vec![1; MAX_FRAME - 1]);
        assert_eq!(second.bytes, [2, 3]);
        for _ in 0..2 {
            output.push(&vec![4; MAX_FRAME]).unwrap();
        }
        output.push(&vec![5; MAX_FRAME - 1]).unwrap();
        assert_eq!(output.budget.available_permits(), 0);
        assert!(output.push(b"overflow").is_err());
        drop((first, second));
        assert_eq!(output.budget.available_permits(), MAX_FRAME + 1);
        for _ in 0..3 {
            let chunk = output.recv().await;
            assert!(chunk.bytes.len() <= MAX_FRAME);
            assert_eq!(chunk.bytes, vec![chunk.bytes[0]; chunk.bytes.len()]);
        }
        assert_eq!(output.budget.available_permits(), QUEUE_BYTES);
    }

    #[test]
    fn slow_device_reader_is_explicitly_revoked() {
        let (_directory, daemon) = fixture();
        let shared = daemon.new_shared();
        let (mut pipe, notices, _bytes) = test_pipe(1, Caller::App);
        let cancel = Arc::new(AtomicBool::new(false));
        pipe.job_cancel = Some(cancel.clone());
        shared.seat.lock().unwrap().holder = Some(pipe);
        for _ in 0..QUEUE_BYTES / MAX_FRAME {
            shared.relay(&vec![0; MAX_FRAME]);
        }
        assert!(shared.seat.lock().unwrap().holder.is_some());
        assert!(!cancel.load(Ordering::SeqCst));
        shared.relay(b"overflow"); // a non-draining reader exhausts the byte budget
        assert!(shared.seat.lock().unwrap().holder.is_none());
        assert!(matches!(*notices.borrow(), Control::End("revoked", _)));
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn failed_port_persistence_does_not_add_or_remove_ports() {
        let (_directory, daemon) = fixture();
        daemon.ports.lock().unwrap().push(port(&daemon));
        fs::create_dir(daemon.dir.join("ports.json")).unwrap();
        let result = daemon.add(
            AddPort {
                path: "/synthetic/another".into(),
                name: None,
                protocol: None,
                baud: None,
                enabled: None,
            },
            Caller::App,
        );
        assert!(result.is_err());
        assert_eq!(daemon.ports.lock().unwrap().len(), 1);
        assert!(daemon.remove("a", Caller::App).is_err());
        assert_eq!(daemon.ports.lock().unwrap()[0].config.id, "a");
    }

    #[test]
    fn instance_generations_persist_and_reject_corrupt_or_exhausted_state() {
        let dir = tempfile::tempdir().unwrap();
        let lock = acquire_daemon_lock(dir.path()).unwrap().unwrap();
        assert_eq!(advance_instance_generation(dir.path()).unwrap(), 1);
        drop(lock);
        let _lock = acquire_daemon_lock(dir.path()).unwrap().unwrap();
        assert_eq!(advance_instance_generation(dir.path()).unwrap(), 2);
        let path = dir.path().join("instance-generation.json");
        for bytes in [b"broken".as_slice(), b"9007199254740991".as_slice()] {
            fs::write(&path, bytes).unwrap();
            assert!(advance_instance_generation(dir.path()).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(advance_instance_generation(dir.path()).is_err());
    }

    #[test]
    fn exact_usb_path_rejects_conflicting_fields_even_without_serial() {
        for serial in [None, Some(String::new())] {
            let mut config = config();
            let saved = UsbIdentity {
                vid: 1,
                pid: 2,
                serial,
                product: Some("Board".into()),
                manufacturer: Some("Vendor".into()),
            };
            config.usb = Some(saved.clone());
            let found = SystemPort {
                path: config.path.clone(),
                name: "Board".into(),
                usb: Some(saved),
            };
            assert_eq!(
                resolve_paths(std::slice::from_ref(&config), std::slice::from_ref(&found)),
                vec![Some(config.path.clone())]
            );
            for field in 0..3 {
                let mut other = found.clone();
                match field {
                    0 => other.usb.as_mut().unwrap().vid += 1,
                    1 => other.usb.as_mut().unwrap().pid += 1,
                    _ => other.usb = None,
                }
                assert_eq!(resolve_paths(&[config.clone()], &[other]), vec![None]);
            }
            let mut loader = found.clone();
            loader.usb.as_mut().unwrap().product = Some("Loader".into());
            loader.usb.as_mut().unwrap().manufacturer = Some("Firmware vendor".into());
            assert_eq!(
                resolve_paths(&[config.clone()], &[loader]),
                vec![Some(config.path.clone())]
            );
            let mut missing_optional = found;
            missing_optional.usb.as_mut().unwrap().product = None;
            assert_eq!(
                resolve_paths(&[config.clone()], &[missing_optional]),
                vec![Some(config.path)]
            );
        }
    }

    #[test]
    fn usb_following_requires_unique_config_and_system_identity_and_reserves_endpoints() {
        let usb = UsbIdentity {
            vid: 1,
            pid: 2,
            serial: Some("unique".into()),
            product: None,
            manufacturer: None,
        };
        let mut first = config();
        first.usb = Some(usb.clone());
        let mut second = first.clone();
        second.id = "b".into();
        second.path = "/synthetic/old-b".into();
        let found = SystemPort {
            path: "/synthetic/new".into(),
            name: "Board".into(),
            usb: Some(usb),
        };
        assert_eq!(
            resolve_paths(&[first.clone()], std::slice::from_ref(&found)),
            vec![Some(found.path.clone())]
        );
        assert_eq!(
            resolve_paths(&[first.clone(), second], std::slice::from_ref(&found)),
            vec![None, None]
        );
        let mut interface = found.clone();
        interface.path += "-interface";
        assert_eq!(
            resolve_paths(&[first.clone()], &[found.clone(), interface]),
            vec![None]
        );
        let mut recycled = found.clone();
        recycled.path = first.path.clone();
        recycled.usb.as_mut().unwrap().serial = Some("different".into());
        assert_eq!(
            resolve_paths(&[first.clone()], &[recycled.clone()]),
            vec![None]
        );
        assert_eq!(
            resolve_paths(&[first.clone()], &[recycled, found.clone()]),
            vec![Some(found.path.clone())]
        );
        let mut exact = first.clone();
        exact.path = found.path.clone();
        assert_eq!(
            resolve_paths(&[exact.clone(), exact], std::slice::from_ref(&found)),
            vec![Some(found.path), None]
        );
        let owners = Arc::new(Mutex::new(HashSet::new()));
        let held = EndpointLease::acquire(&owners, "/synthetic/path").unwrap();
        assert!(EndpointLease::acquire(&owners, "/synthetic/path").is_none());
        drop(held);
        assert!(EndpointLease::acquire(&owners, "/synthetic/path").is_some());
    }

    #[test]
    fn moved_usb_retires_old_worker_and_fences_late_publications() {
        let (_directory, daemon) = fixture();
        let mut port = port(&daemon);
        let _rx = mock_worker(&mut port, Protocol::H2loader);
        let (release, waiting) = mpsc::channel();
        let worker = port.worker.as_mut().unwrap();
        let old = std::mem::replace(
            &mut worker.handle,
            std::thread::spawn(move || {
                let _ = waiting.recv();
            }),
        );
        old.join().unwrap();
        let stop = worker.stop.clone();
        let usb = UsbIdentity {
            vid: 1,
            pid: 2,
            serial: Some("unique".into()),
            product: None,
            manufacturer: None,
        };
        port.config.usb = Some(usb.clone());
        port.shared.live.lock().unwrap().job = Some(test_job());
        let shared = port.shared.clone();
        daemon.ports.lock().unwrap().push(port);
        daemon.reconcile_system(vec![SystemPort {
            path: "/synthetic/new".into(),
            name: "Board".into(),
            usb: Some(usb),
        }]);
        assert!(stop.load(Ordering::SeqCst));
        assert!(!shared.worker_update(1, |live| {
            live.connection = Connection::Connected;
            live.error = Some("late".into());
        }));
        let mut ports = daemon.ports.lock().unwrap();
        let port = &mut ports[0];
        assert_eq!(port.config.path, "/synthetic/new");
        assert!(port.worker.is_none() && port.stopping.is_some());
        assert_eq!(
            shared.live.lock().unwrap().connection,
            Connection::Connecting
        );
        assert!(shared
            .live
            .lock()
            .unwrap()
            .job
            .as_ref()
            .unwrap()
            .cancel
            .load(Ordering::SeqCst));
        assert_eq!(daemon.system.lock().unwrap()[0].path, "/synthetic/new");
        release.send(()).unwrap();
        port.stopping.take().unwrap().join().unwrap();
        Daemon::stop_worker(port, Connection::Off);
        assert!(!shared.worker_update(1, |live| live.connection = Connection::Error));
        assert_eq!(shared.live.lock().unwrap().connection, Connection::Off);
    }

    #[test]
    fn ws_metadata_is_required_after_sanitizing() {
        for uri in [
            "/pipe",
            "/pipe?client=%20%0a&title=test",
            "/pipe?client=test&title=%00%20",
        ] {
            let uri: hyper::Uri = uri.parse().unwrap();
            assert!(
                pipe_text(&uri, "client", 64).is_err()
                    || pipe_text(&uri, "title", ws::MAX_TASK_TITLE_CHARS).is_err()
            );
        }
        let uri = "/pipe?client=agent%0a&title=flash%20firmware"
            .parse()
            .unwrap();
        assert_eq!(pipe_text(&uri, "client", 64).unwrap(), "agent");
        assert_eq!(
            pipe_text(&uri, "title", ws::MAX_TASK_TITLE_CHARS).unwrap(),
            "flash firmware"
        );
        // URLSearchParams and other form encoders send spaces as `+`.
        let uri = "/pipe?client=coding+agent&title=C%2B%2B+build"
            .parse()
            .unwrap();
        assert_eq!(pipe_text(&uri, "client", 64).unwrap(), "coding agent");
        assert_eq!(
            pipe_text(&uri, "title", ws::MAX_TASK_TITLE_CHARS).unwrap(),
            "C++ build"
        );
    }

    #[test]
    fn session_boundary_flushes_final_partial_rx_and_snapshot_exposes_generations() {
        let (_directory, daemon) = fixture();
        let port = port(&daemon);
        port.shared.rx(b"last line");
        port.shared.flush_partial();
        port.shared.rx(b"next session\n");
        let page = port.shared.logs.lock().unwrap().tail(10);
        let page = serde_json::to_value(page).unwrap();
        assert_eq!(page["entries"][0]["text"], "last line");
        assert_eq!(page["entries"][1]["text"], "next session");
        daemon.ports.lock().unwrap().push(port);
        let snapshot = daemon.snapshot();
        assert_eq!(snapshot["instanceId"], daemon.instance_id);
        let generation = snapshot["ports"][0]["logGeneration"].as_u64().unwrap();
        daemon
            .with_port("a", |port| {
                port.shared.logs.lock().unwrap().clear();
                Ok(())
            })
            .unwrap();
        assert!(
            daemon.snapshot()["ports"][0]["logGeneration"]
                .as_u64()
                .unwrap()
                > generation
        );
        let (_, logs) = daemon.logs("a", &"/logs".parse().unwrap()).unwrap();
        assert_eq!(logs["instanceId"], daemon.instance_id);
        assert_eq!(
            logs["generation"],
            daemon.snapshot()["ports"][0]["logGeneration"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn watch_honors_instance_change_zero_budget_and_timeout_cap() {
        let (_directory, daemon) = fixture();
        let before = tokio::time::Instant::now();
        daemon
            .watch_snapshot(
                &"/watch?since=99&instance=old&timeoutMs=20000"
                    .parse()
                    .unwrap(),
            )
            .await;
        daemon
            .watch_snapshot(
                &format!(
                    "/watch?since=99&instance={}&timeoutMs=0",
                    daemon.instance_id
                )
                .parse()
                .unwrap(),
            )
            .await;
        assert_eq!(before.elapsed(), Duration::ZERO);
        daemon
            .watch_snapshot(&"/watch?since=99&timeoutMs=999999".parse().unwrap())
            .await;
        assert_eq!(before.elapsed(), WATCH_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_body_and_slow_sink_have_bounded_deadlines() {
        struct PendingBody;
        impl hyper::body::Body for PendingBody {
            type Data = Bytes;
            type Error = Infallible;
            fn poll_frame(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Infallible>>>
            {
                std::task::Poll::Pending
            }
        }
        struct SlowSink;
        impl futures_util::Sink<tokio_tungstenite::tungstenite::Message> for SlowSink {
            type Error = Infallible;
            fn poll_ready(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Infallible>> {
                std::task::Poll::Pending
            }
            fn start_send(
                self: std::pin::Pin<&mut Self>,
                _: tokio_tungstenite::tungstenite::Message,
            ) -> Result<(), Infallible> {
                Ok(())
            }
            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Infallible>> {
                std::task::Poll::Pending
            }
            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Infallible>> {
                std::task::Poll::Ready(Ok(()))
            }
        }
        let before = tokio::time::Instant::now();
        assert_eq!(
            collect_body(PendingBody).await.unwrap_err().0,
            StatusCode::REQUEST_TIMEOUT
        );
        assert_eq!(before.elapsed(), BODY_TIMEOUT);
        assert!(
            !websocket_send(
                &mut SlowSink,
                tokio_tungstenite::tungstenite::Message::Text("test".into())
            )
            .await
        );
        assert_eq!(before.elapsed(), BODY_TIMEOUT + SOCKET_SEND_TIMEOUT);
        assert_eq!(
            collect_body(Full::new(Bytes::from(vec![0; MAX_BODY + 1])))
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn network_enumeration_failure_is_not_an_offline_snapshot() {
        let (_directory, daemon) = offline_network_fixture();
        let original = daemon.listeners.lock().unwrap().clone().unwrap();
        let mut active = original.adopt().unwrap();
        let offline_error = original.network_error();
        daemon.refresh_gateway_network_with(|| Err(std::io::Error::other("enumeration failed")));
        assert!(Arc::ptr_eq(
            &original,
            daemon.listeners.lock().unwrap().as_ref().unwrap()
        ));
        assert!(daemon
            .gateway_error
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains("query failed"));
        assert!(daemon.pending.lock().unwrap().is_none());
        assert_eq!(daemon.gateway_view()["listening"], true);
        assert_eq!(daemon.snapshot()["gateway"]["urls"], json!(original.urls()));
        daemon.refresh_gateway_network_with(|| Ok(Vec::new()));
        assert_ne!(*daemon.gateway_error.lock().unwrap(), offline_error);
        daemon.adopt_pending(&mut active);
        assert_eq!(*daemon.gateway_error.lock().unwrap(), offline_error);
        daemon.dirty.store(false, Ordering::SeqCst);
        daemon.refresh_gateway_network_with(|| Ok(Vec::new()));
        assert!(!daemon.dirty.load(Ordering::SeqCst));
        let _transaction = daemon.reconfiguration.lock().unwrap();
        daemon.refresh_gateway_network_with(|| panic!("manual transactions take priority"));
    }

    #[tokio::test]
    async fn occupied_startup_ports_select_a_stable_available_lan_port() {
        use std::net::{Ipv4Addr, TcpListener};
        let Some(ip) = gateway::lan_ips().first().copied() else {
            return;
        };
        for whole_port in [false, true] {
            let (_directory, daemon) = offline_network_fixture();
            let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let target_port = reservation.local_addr().unwrap().port();
            let blocker = if whole_port {
                reservation
            } else {
                let blocker = TcpListener::bind((ip, target_port)).unwrap();
                drop(reservation);
                blocker
            };
            let mut config = daemon.gateway.lock().unwrap().clone();
            config.port = target_port;
            let startup = Arc::new(gateway::Listeners::start_for_ips(&config, &[ip]).unwrap());
            assert!(startup.lan_enabled());
            assert!(!startup.needs_port_retry());
            assert!(startup.network_error().is_none());
            let address = startup.local_addr().unwrap();
            let selected_port = address.port();
            assert_ne!(selected_port, target_port);
            assert_eq!(startup.lan_addresses(), vec![ip]);
            config.port = selected_port;
            *daemon.gateway.lock().unwrap() = config;
            *daemon.bound.lock().unwrap() = address;
            *daemon.gateway_error.lock().unwrap() = startup.network_error();
            *daemon.listeners.lock().unwrap() = Some(startup.clone());
            daemon.write_info().unwrap();
            let info_before = fs::read(info_path(&daemon.dir)).unwrap();
            let active = startup.adopt().unwrap();
            daemon.refresh_gateway_network_with(|| Ok(vec![ip]));
            assert!(daemon.pending.lock().unwrap().is_none());
            // Releasing the old port must not move the service again.
            drop(blocker);
            daemon.refresh_gateway_network_with(|| Ok(vec![ip]));
            assert!(daemon.pending.lock().unwrap().is_none());
            assert!(daemon.gateway_error.lock().unwrap().is_none());
            assert_eq!(*daemon.bound.lock().unwrap(), address);
            assert_eq!(fs::read(info_path(&daemon.dir)).unwrap(), info_before);
            assert_eq!(daemon.gateway_view()["port"], selected_port);
            assert_eq!(
                daemon.gateway_view()["urls"],
                json!([
                    format!("http://127.0.0.1:{selected_port}"),
                    format!("http://{ip}:{selected_port}")
                ])
            );
            tokio::time::timeout(Duration::from_secs(2), async {
                let _local = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, selected_port))
                    .await
                    .unwrap();
                assert!(!active.accept().await.unwrap().lan);
                let _remote = tokio::net::TcpStream::connect((ip, selected_port))
                    .await
                    .unwrap();
                assert!(active.accept().await.unwrap().lan);
            })
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn local_manual_adoption_failure_preserves_discovery_and_retries() {
        use std::net::{Ipv4Addr, TcpListener};
        let (_directory, daemon) = fixture();
        let mut config = daemon.gateway.lock().unwrap().clone();
        config.port = 0;
        let original = Arc::new(gateway::Listeners::prepare_for_ips(&config, None, &[]).unwrap());
        let old_address = original.local_addr().unwrap();
        config.port = old_address.port();
        *daemon.gateway.lock().unwrap() = config;
        *daemon.bound.lock().unwrap() = old_address;
        *daemon.listeners.lock().unwrap() = Some(original.clone());
        daemon.write_info().unwrap();
        let info_before = fs::read(info_path(&daemon.dir)).unwrap();
        let mut active = original.adopt().unwrap();
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let new_address = reservation.local_addr().unwrap();
        drop(reservation);
        daemon
            .reconfigure(gateway_patch(json!({"port": new_address.port()})))
            .unwrap();
        assert_eq!(daemon.gateway.lock().unwrap().port, new_address.port());
        for _ in 0..2 {
            daemon.adopt_pending_with(&mut active, |_| {
                Err(std::io::Error::other("registration failed"))
            });
            assert!(daemon
                .gateway_error
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .contains("adoption failed"));
            assert!(daemon.pending.lock().unwrap().is_some());
            assert_eq!(*daemon.bound.lock().unwrap(), old_address);
            assert_eq!(daemon.gateway_view()["urls"], json!(original.urls()));
            assert_eq!(fs::read(info_path(&daemon.dir)).unwrap(), info_before);
            daemon.refresh_gateway_network_with(|| {
                panic!("local retry must not enumerate interfaces")
            });
        }
        let request = async {
            let response: Value = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap()
                .get(format!("http://{old_address}/v1/health"))
                .bearer_auth(&daemon.token)
                .header("Connection", "close")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(response["caller"], "app");
        };
        let serve = async {
            let accepted = active.accept().await.unwrap();
            let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
            serve_connection(daemon.clone(), accepted, permit).await;
        };
        tokio::join!(request, serve);
        daemon.adopt_pending(&mut active);
        assert!(daemon.pending.lock().unwrap().is_none());
        assert!(daemon.gateway_error.lock().unwrap().is_none());
        assert_eq!(*daemon.bound.lock().unwrap(), new_address);
        let info: Info =
            serde_json::from_slice(&fs::read(info_path(&daemon.dir)).unwrap()).unwrap();
        assert_eq!(info.port, new_address.port());
        assert_eq!(
            daemon.gateway_view()["urls"],
            json!([format!("http://{new_address}")])
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            let _client = tokio::net::TcpStream::connect(new_address).await.unwrap();
            assert!(!active.accept().await.unwrap().lan);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn network_refresh_recovers_and_adoption_failure_keeps_local_http() {
        let (_directory, daemon) = offline_network_fixture();
        let Some(ip) = gateway::lan_ips().first().copied() else {
            return;
        };
        let original = daemon.listeners.lock().unwrap().clone().unwrap();
        let mut active = original.adopt().unwrap();
        let config_before = fs::read(gateway::path(&daemon.dir)).unwrap();
        let info_before = fs::read(info_path(&daemon.dir)).unwrap();
        let key = daemon.gateway.lock().unwrap().api_key.clone();
        let generation = daemon.authority.generation.load(Ordering::SeqCst);
        daemon.refresh_gateway_network_with(|| Ok(vec![ip]));
        assert_eq!(
            daemon.gateway_view()["urls"],
            json!(original.urls()),
            "unadopted candidates must not be advertised"
        );
        daemon.adopt_pending_with(&mut active, |_| {
            Err(std::io::Error::other("registration failed"))
        });
        assert!(Arc::ptr_eq(
            &original,
            daemon.listeners.lock().unwrap().as_ref().unwrap()
        ));
        assert!(daemon.pending.lock().unwrap().is_some());
        daemon.refresh_gateway_network_with(|| Ok(vec![ip]));
        daemon.adopt_pending(&mut active);
        assert_eq!(
            daemon
                .listeners
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .lan_addresses(),
            vec![ip]
        );
        assert!(daemon.gateway_error.lock().unwrap().is_none());
        daemon.refresh_gateway_network_with(|| Ok(vec![ip, ip]));
        assert!(
            daemon.pending.lock().unwrap().is_none(),
            "unchanged sets must not rebuild sockets"
        );
        // When a failed extra target disappears, keep the warning until the
        // runtime adopts the cloned, still-working interface socket.
        daemon.refresh_gateway_network_with(|| Ok(vec![ip, std::net::Ipv4Addr::LOCALHOST]));
        daemon.adopt_pending(&mut active);
        assert!(daemon.gateway_error.lock().unwrap().is_some());
        daemon.refresh_gateway_network_with(|| Ok(vec![ip]));
        assert!(daemon.gateway_error.lock().unwrap().is_some());
        daemon.adopt_pending(&mut active);
        assert!(daemon.gateway_error.lock().unwrap().is_none());
        assert!(daemon.pending.lock().unwrap().is_none());
        daemon.refresh_gateway_network_with(|| Ok(Vec::new()));
        daemon.adopt_pending(&mut active);
        assert_eq!(daemon.gateway_view()["urls"], json!(original.urls()));
        assert_eq!(daemon.gateway_view()["listening"], true);
        let request = async {
            let response: Value = reqwest::Client::new()
                .get(format!(
                    "http://{}/v1/health",
                    original.local_addr().unwrap()
                ))
                .bearer_auth(&daemon.token)
                .header("Connection", "close")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(response["caller"], "app");
        };
        let serve = async {
            let accepted = active.accept().await.unwrap();
            assert!(!accepted.lan);
            let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
            serve_connection(daemon.clone(), accepted, permit).await;
        };
        tokio::join!(request, serve);
        daemon.refresh_gateway_network_with(|| Ok(vec![ip]));
        daemon.adopt_pending(&mut active);
        assert_eq!(
            daemon
                .listeners
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .lan_addresses(),
            vec![ip]
        );
        assert_eq!(fs::read(gateway::path(&daemon.dir)).unwrap(), config_before);
        assert_eq!(fs::read(info_path(&daemon.dir)).unwrap(), info_before);
        assert_eq!(daemon.gateway.lock().unwrap().api_key, key);
        assert_eq!(
            daemon.authority.generation.load(Ordering::SeqCst),
            generation
        );
    }

    #[tokio::test]
    async fn failed_lan_bind_retries_and_manual_scope_change_supersedes_refresh() {
        use std::net::TcpListener;
        let (_directory, daemon) = offline_network_fixture();
        let Some(ip) = gateway::lan_ips().first().copied() else {
            return;
        };
        let original = daemon.listeners.lock().unwrap().clone().unwrap();
        let mut active = original.adopt().unwrap();
        let blocker = TcpListener::bind((ip, original.local_addr().unwrap().port())).unwrap();
        daemon.refresh_gateway_network_with(|| Ok(vec![ip]));
        daemon.adopt_pending(&mut active);
        assert_eq!(daemon.gateway_view()["urls"], json!(original.urls()));
        assert!(daemon.gateway_error.lock().unwrap().is_some());
        drop(blocker);
        // Concurrent process tests may fork with this CLOEXEC socket open. Its
        // last descriptor closes at exec, so exercise the poller's retry contract
        // rather than assume the port is reusable in the same instant as drop.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            daemon.refresh_gateway_network_with(|| Ok(vec![ip]));
            daemon.adopt_pending(&mut active);
            let recovery_error = daemon.gateway_error.lock().unwrap().clone();
            if recovery_error.is_none() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "LAN recovery failed: {recovery_error:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            daemon
                .listeners
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .lan_addresses(),
            vec![ip]
        );
        daemon.refresh_gateway_network_with(|| Ok(Vec::new()));
        assert!(
            daemon
                .pending
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .network_refresh
        );
        daemon
            .reconfigure(gateway_patch(json!({"scope": "local"})))
            .unwrap();
        assert!(
            !daemon
                .pending
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .network_refresh
        );
        daemon.adopt_pending(&mut active);
        daemon.refresh_gateway_network_with(|| panic!("local scope never enumerates LAN"));
        assert!(daemon
            .listeners
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .lan_addresses()
            .is_empty());
        assert_eq!(daemon.gateway.lock().unwrap().scope, gateway::Scope::Local);
    }

    #[tokio::test]
    async fn plaintext_lan_http_and_ws_share_the_service_with_loopback_mcp() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let Some(ip) = gateway::lan_ips().first().copied() else {
            return;
        };
        let (_directory, daemon) = fixture();
        let mut connected = port(&daemon);
        let _commands = mock_worker(&mut connected, Protocol::Serial);
        daemon.ports.lock().unwrap().push(connected);
        let config = gateway::Config {
            scope: gateway::Scope::Lan,
            port: 0,
            ..gateway::Config::default()
        };
        let listeners = gateway::Listeners::prepare(&config, None).unwrap();
        let port_number = listeners.local_addr().unwrap().port();
        let gateway_key = config.api_key.clone();
        *daemon.gateway.lock().unwrap() = config;
        daemon.authority.lan.store(true, Ordering::SeqCst);
        let active = listeners.adopt().unwrap();
        let tasks = tokio::task::JoinSet::new();
        let serving = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                let mut tasks = tasks;
                let capacity = Arc::new(Semaphore::new(10));
                loop {
                    tokio::select! {
                        accepted = active.accept() => {
                            let accepted = accepted.unwrap();
                            let permit = capacity.clone().try_acquire_owned().unwrap();
                            tasks.spawn(serve_connection(daemon.clone(), accepted, permit));
                        }
                        _ = tasks.join_next(), if !tasks.is_empty() => {},
                    }
                }
            })
        };
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let local = format!("http://127.0.0.1:{port_number}/v1/health");
        let remote = format!("http://{ip}:{port_number}/v1/health");
        let app: Value = http
            .get(&local)
            .bearer_auth(&daemon.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(app["caller"], "app");
        let gateway: Value = http
            .get(&local)
            .bearer_auth(&gateway_key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(gateway["caller"], "gateway");
        let gateway: Value = http
            .get(&remote)
            .bearer_auth(&gateway_key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(gateway["caller"], "gateway");
        assert_eq!(
            http.get(&remote)
                .bearer_auth(&daemon.token)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        for query_token in [false, true] {
            let stream = tokio::net::TcpStream::connect((ip, port_number))
                .await
                .unwrap();
            let extra = if query_token {
                "&token=never-in-a-url"
            } else {
                ""
            };
            let mut request = format!(
                "ws://{ip}:{port_number}/v1/ports/a/pipe?client=test&title=Transport%20test&reason=transport-test{extra}"
            )
            .into_client_request()
            .unwrap();
            request.headers_mut().insert(
                "Authorization",
                format!("Bearer {gateway_key}").parse().unwrap(),
            );
            let result = tokio_tungstenite::client_async(request, stream).await;
            if query_token {
                assert!(
                    matches!(result, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 401)
                );
            } else {
                let (mut socket, response) = result.unwrap();
                assert_eq!(response.status(), 101);
                socket.close(None).await.unwrap();
            }
        }
        serving.abort();
        daemon.shutdown.send_replace(true);
    }

    #[test]
    fn remote_websocket_requires_headers_and_never_receives_app_identity() {
        let (_directory, daemon) = fixture();
        daemon.gateway.lock().unwrap().scope = gateway::Scope::Lan;
        let local: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let remote: SocketAddr = "192.0.2.2:1234".parse().unwrap();
        let query = Request::builder()
            .uri("/v1/ports/a/pipe?token=secret")
            .body(())
            .unwrap();
        assert_eq!(pipe_token(&query, local, false).as_deref(), Some("secret"));
        assert!(pipe_token(&query, remote, true).is_none());
        assert!(pipe_token(&query, local, true).is_none());
        let header = Request::builder()
            .uri("/v1/ports/a/pipe")
            .header("Authorization", "Bearer secret")
            .header("X-Forwarded-For", "127.0.0.1")
            .body(())
            .unwrap();
        assert_eq!(pipe_token(&header, remote, true).as_deref(), Some("secret"));
        let both = Request::builder()
            .uri("/v1/ports/a/pipe?token=secret")
            .header("Authorization", "Bearer secret")
            .body(())
            .unwrap();
        assert!(pipe_token(&both, remote, true).is_none());
        assert_eq!(
            daemon.authorize(&daemon.token, local, false),
            Some(Caller::App)
        );
        assert!(daemon.authorize(&daemon.token, remote, true).is_none());
        assert!(daemon.authorize(&daemon.token, local, true).is_none());
        let key = daemon.gateway.lock().unwrap().api_key.clone();
        assert!(matches!(
            daemon.authorize(&key, local, false),
            Some(Caller::Gateway { local: true, .. })
        ));
        assert!(matches!(
            daemon.authorize(&key, local, true),
            Some(Caller::Gateway { local: false, .. })
        ));
        assert!(matches!(
            daemon.authorize(&key, remote, true),
            Some(Caller::Gateway { local: false, .. })
        ));
    }

    #[tokio::test]
    async fn occupied_lan_bind_patch_saves_available_port_and_adopts_before_advertising() {
        let Some(ip) = gateway::lan_ips().first().copied() else {
            return;
        };
        let (_directory, daemon) = fixture();
        let config = gateway::Config {
            port: 0,
            ..gateway::Config::default()
        };
        let listeners = Arc::new(gateway::Listeners::prepare(&config, None).unwrap());
        let address = listeners.local_addr().unwrap();
        {
            let mut saved = daemon.gateway.lock().unwrap();
            saved.port = address.port();
        }
        *daemon.bound.lock().unwrap() = address;
        *daemon.listeners.lock().unwrap() = Some(listeners.clone());
        let _blocker = std::net::TcpListener::bind((ip, address.port())).unwrap();
        let mut active = listeners.adopt().unwrap();
        daemon.write_info().unwrap();
        let info_before = fs::read(info_path(&daemon.dir)).unwrap();
        let result = daemon
            .reconfigure(gateway_patch(json!({"scope": "lan"})))
            .unwrap();
        assert_eq!(result.0, StatusCode::OK);
        let saved = gateway::read(&daemon.dir).unwrap();
        assert_eq!(saved.scope, gateway::Scope::Lan);
        assert_ne!(saved.port, address.port());
        assert_eq!(result.1["port"], saved.port);
        assert_eq!(daemon.gateway.lock().unwrap().port, saved.port);
        assert_eq!(*daemon.bound.lock().unwrap(), address);
        assert!(daemon.pending.lock().unwrap().is_some());
        assert_eq!(daemon.gateway_view()["urls"], json!(listeners.urls()));
        assert_eq!(fs::read(info_path(&daemon.dir)).unwrap(), info_before);
        assert_eq!(daemon.authority.generation.load(Ordering::SeqCst), 1);
        daemon.adopt_pending(&mut active);
        assert_eq!(daemon.bound.lock().unwrap().port(), saved.port);
        assert_eq!(daemon.gateway_view()["boundPort"], saved.port);
        assert!(daemon.gateway_error.lock().unwrap().is_none());
        assert!(daemon.gateway_view()["urls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|url| { url.as_str().unwrap().ends_with(&format!(":{}", saved.port)) }));
        let info: Info =
            serde_json::from_slice(&fs::read(info_path(&daemon.dir)).unwrap()).unwrap();
        assert_eq!(info.port, saved.port);
    }

    #[test]
    fn auto_probe_never_bypasses_transport_or_ownership_failures() {
        for name in [
            "unavailable",
            "io",
            "closed",
            "permission",
            "not_found",
            "unsupported",
            "no_memory",
            "invalid_argument",
            "invalid_state",
            "full",
            "cancelled",
            "error",
            "unknown",
        ] {
            assert!(
                matches!(
                    h2loader_open_failure(name.into(), true, false),
                    Outcome::Failed(Connection::Error, _)
                ),
                "{name} must not open plain serial"
            );
        }
        assert!(matches!(
            h2loader_open_failure("busy".into(), true, false),
            Outcome::Failed(Connection::Occupied, _)
        ));
        for name in ["timeout", "format", "truncated", "core_unavailable"] {
            assert!(
                matches!(
                    h2loader_open_failure(name.into(), true, false),
                    Outcome::NotH2loader(_)
                ),
                "{name} may select plain serial"
            );
            assert!(
                matches!(
                    h2loader_open_failure(name.into(), false, false),
                    Outcome::Failed(Connection::Error, _)
                ),
                "explicit H2Loader must keep its error"
            );
            assert!(matches!(h2loader_open_failure(name.into(), true, true), Outcome::Failed(Connection::Error, _)),
                "a known Auto H2Loader must keep probing during reboot instead of falling back to raw serial");
        }
    }

    #[test]
    fn query_and_token_helpers() {
        let uri: hyper::Uri = "/v1/watch?since=12&limit=3".parse().unwrap();
        assert_eq!(query(&uri, "since"), Some(12));
        assert_eq!(query(&uri, "limit"), Some(3));
        assert_eq!(query(&uri, "after"), None);
        assert!(constant_eq(b"abc", b"abc"));
        assert!(!constant_eq(b"abc", b"abd"));
        assert!(valid_baud(115_200).is_ok());
        assert!(valid_baud(10).is_err());
    }

    #[test]
    fn decodes_command_availability_and_job_requirements() {
        // Reported by an ESP32-S3 Loader in MFG mode: no reboot-to-App.
        let mut status = json!({ "commandAvailability": 4_144_446u64, "capabilities": 7 });
        h2loader::annotate(&mut status);
        assert!(!h2loader::allows(&status, "rebootApp"));
        for command in [
            "rebootLoader",
            "status",
            "stagePayload",
            "rebootUpgrade",
            "mfg",
        ] {
            assert!(h2loader::allows(&status, command), "{command}");
        }
        assert_eq!(status["transports"], json!(["uart", "wifi", "ble"]));
        assert!(job_commands(JobKind::Install)
            .iter()
            .all(|c| h2loader::allows(&status, c)));
        assert!(!job_commands(JobKind::RebootApp)
            .iter()
            .all(|c| h2loader::allows(&status, c)));
    }

    #[test]
    fn configs_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let saved = Saved {
            version: 1,
            ports: vec![PortConfig {
                id: "a".into(),
                name: "Board".into(),
                path: "/dev/cu.test".into(),
                protocol: Protocol::H2loader,
                baud: 115_200,
                enabled: true,
                mode: Mode::Piping,
                h2loader_seen: false,
                auto_grant: false,
                usb: None,
                created_at: 1,
            }],
        };
        private_write(
            &dir.path().join("ports.json"),
            &serde_json::to_vec(&saved).unwrap(),
        )
        .unwrap();
        let loaded = load_configs(dir.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].protocol, Protocol::H2loader);
        fs::write(dir.path().join("ports.json"), b"{broken").unwrap();
        assert!(adopt_configs(dir.path()).is_empty());
        assert!(!dir.path().join("ports.json").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// Serves a fixture through the real connection path (HTTP/1.1 + HTTP/2).
    async fn serve_fixture(daemon: Arc<Daemon>) -> SocketAddr {
        let config = gateway::Config {
            port: 0,
            ..daemon.gateway.lock().unwrap().clone()
        };
        let listeners = gateway::Listeners::prepare(&config, None).unwrap();
        let address = listeners.local_addr().unwrap();
        let active = listeners.adopt().unwrap();
        tokio::spawn(async move {
            let _listeners = listeners;
            let capacity = Arc::new(Semaphore::new(MAX_CONNECTIONS));
            loop {
                let accepted = active.accept().await.unwrap();
                let permit = capacity.clone().try_acquire_owned().unwrap();
                tokio::spawn(serve_connection(daemon.clone(), accepted, permit));
            }
        });
        address
    }

    async fn eventually(mut check: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !check() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn retirement_preserves_forwarded_jobs_until_their_session_is_released() {
        let (_owner_directory, owner) = fixture();
        let mut port = port(&owner);
        let rx = mock_worker(&mut port, Protocol::H2loader);
        port.config.protocol = Protocol::H2loader;
        port.present = true;
        let shared = port.shared.clone();
        owner.ports.lock().unwrap().push(port);
        let address = serve_fixture(owner.clone()).await;
        let key = owner.gateway.lock().unwrap().api_key.clone();
        let (_directory, local) = fixture();
        local.remotes.start();
        local
            .remotes
            .add(&format!("127.0.0.1:{}", address.port()), &key)
            .await
            .unwrap();
        let id = remote::local_id(&owner.node.id, "a");
        eventually(|| local.remotes.port_view(&id).is_some()).await;
        let body = Bytes::from(
            json!({"kind":"refresh", "jobId": uuid::Uuid::new_v4().to_string()}).to_string(),
        );
        let response = local
            .remote_port(
                &Method::POST,
                &id,
                &["jobs"],
                &"/jobs".parse().unwrap(),
                &body,
                Caller::App,
            )
            .await
            .unwrap();
        assert_eq!(response.0, StatusCode::ACCEPTED);
        assert!(matches!(worker_command(&rx).await, Command::Job));
        assert_eq!(local.retire().unwrap_err().0, StatusCode::CONFLICT);
        assert!(!*local.shutdown.borrow());
        assert!(!shared
            .live
            .lock()
            .unwrap()
            .job
            .as_ref()
            .unwrap()
            .cancel
            .load(Ordering::SeqCst));
        shared.finish_job(1, Ok(()));
        // Keep the job's session protected until explicitly released (or reaped
        // after idle), even if remote state snapshots have not caught up.
        assert_eq!(local.retire().unwrap_err().0, StatusCode::CONFLICT);
        let connection = local.remotes.port_view(&id).unwrap()["remote"]["control"]["connectionId"]
            .as_u64()
            .unwrap();
        local.remotes.release(&id, connection).unwrap();
        assert!(local.retire().is_ok());
    }

    #[tokio::test]
    async fn cancelled_grpc_upload_removes_partial_package() {
        use crate::serial::grpc::pb;
        let (_directory, daemon) = fixture();
        let address = serve_fixture(daemon.clone()).await;
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let key = daemon.gateway.lock().unwrap().api_key.clone();
        let token: tonic::metadata::MetadataValue<_> = format!("Bearer {key}").parse().unwrap();
        let mut client = pb::gateway_client::GatewayClient::with_interceptor(
            channel,
            move |mut request: tonic::Request<()>| {
                request
                    .metadata_mut()
                    .insert("authorization", token.clone());
                Ok(request)
            },
        );
        let (chunks, stream) = tokio::sync::mpsc::channel(2);
        let upload = tokio::spawn(async move {
            client
                .upload_package(tokio_stream::wrappers::ReceiverStream::new(stream))
                .await
        });
        chunks
            .send(pb::PackageChunk {
                kind: Some(pb::package_chunk::Kind::Header(pb::package_chunk::Header {
                    name: "cancelled.update.tar.zlib".into(),
                    size: 1024,
                    sha256: "ab".repeat(32),
                })),
            })
            .await
            .unwrap();
        chunks
            .send(pb::PackageChunk {
                kind: Some(pb::package_chunk::Kind::Data(b"partial upload".to_vec())),
            })
            .await
            .unwrap();
        let uploads = daemon.dir.join("packages/uploads");
        // Cancel only after bytes have reached disk, while the handler waits
        // for the rest of the request stream.
        eventually(|| {
            fs::read_dir(&uploads).ok().is_some_and(|entries| {
                entries.flatten().any(|entry| {
                    fs::metadata(entry.path().join("cancelled.update.tar.zlib"))
                        .is_ok_and(|meta| meta.len() > 0)
                })
            })
        })
        .await;
        upload.abort();
        assert!(upload.await.unwrap_err().is_cancelled());
        drop(chunks);
        eventually(|| fs::read_dir(&uploads).unwrap().next().is_none()).await;
        assert!(daemon.uploads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn slow_grpc_upload_is_active_before_its_response_exists() {
        use crate::serial::grpc::pb;
        use sha2::{Digest, Sha256};
        let (_directory, daemon) = fixture();
        let address = serve_fixture(daemon.clone()).await;
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let key = daemon.gateway.lock().unwrap().api_key.clone();
        let token: tonic::metadata::MetadataValue<_> = format!("Bearer {key}").parse().unwrap();
        let mut client = pb::gateway_client::GatewayClient::with_interceptor(
            channel,
            move |mut request: tonic::Request<()>| {
                request
                    .metadata_mut()
                    .insert("authorization", token.clone());
                Ok(request)
            },
        );
        let payload = b"abcde";
        let sha256: String = Sha256::digest(payload)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let (chunks, stream) = tokio::sync::mpsc::channel(2);
        chunks
            .send(pb::PackageChunk {
                kind: Some(pb::package_chunk::Kind::Header(pb::package_chunk::Header {
                    name: "invalid.update.tar.zlib".into(),
                    size: payload.len() as u64,
                    sha256,
                })),
            })
            .await
            .unwrap();
        let producer = tokio::spawn(async move {
            for byte in payload {
                // Total time exceeds the 35-second idle budget, while every
                // chunk stays within UploadPackage's 15-second receive deadline.
                tokio::time::sleep(Duration::from_secs(8)).await;
                chunks
                    .send(pb::PackageChunk {
                        kind: Some(pb::package_chunk::Kind::Data(vec![*byte])),
                    })
                    .await
                    .unwrap();
            }
        });
        let result = tokio::time::timeout(
            Duration::from_secs(50),
            client.upload_package(tokio_stream::wrappers::ReceiverStream::new(stream)),
        )
        .await
        .unwrap()
        .unwrap_err();
        producer.await.unwrap();
        // These bytes are deliberately not firmware. Reaching validation means
        // the entire slow stream arrived instead of the connection being closed.
        assert_eq!(result.code(), tonic::Code::InvalidArgument);
        assert!(result.message().starts_with("固件包无效"));
    }

    #[tokio::test]
    async fn cascade_imports_ports_and_arbitrates_sessions_on_the_owning_node() {
        let (_owner_directory, owner) = fixture();
        let mut port = port(&owner);
        let rx = mock_worker(&mut port, Protocol::Serial);
        port.config.protocol = Protocol::Serial;
        port.present = true;
        let shared = port.shared.clone();
        owner.ports.lock().unwrap().push(port);
        let address = serve_fixture(owner.clone()).await;
        let key = owner.gateway.lock().unwrap().api_key.clone();
        let url = format!("127.0.0.1:{}", address.port());

        let (_directory, local) = fixture();
        local.remotes.start();
        let refused = local.remotes.probe(&url, "gmk_wrong-key-0000000000").await;
        assert!(refused.unwrap_err().1.contains("API Key"));
        let (_, probe) = local.remotes.probe(&url, &key).await.unwrap();
        assert_eq!(probe["node"]["id"], owner.node.id);
        assert_eq!(probe["ports"], 1);
        assert!(probe["certificate"].is_null(), "loopback is plaintext");
        local.remotes.add(&url, &key).await.unwrap();
        assert_eq!(
            local.remotes.add(&url, &key).await.unwrap_err().0,
            StatusCode::CONFLICT
        );

        let id = remote::local_id(&owner.node.id, "a");
        eventually(|| {
            local.remotes.nodes_json()[0]["status"] == "online"
                && local.remotes.port_view(&id).is_some()
        })
        .await;
        let view = local.remotes.port_view(&id).unwrap();
        assert_eq!(view["name"], "Board");
        assert_eq!(view["connection"], "connected");
        assert_eq!(view["node"], owner.node.id);
        assert_eq!(view["remote"]["control"]["state"], "none");
        // The local snapshot lists imported ports under their node only.
        let snapshot = local.snapshot();
        assert!(snapshot["ports"].as_array().unwrap().is_empty());
        assert_eq!(snapshot["nodes"][0]["ports"][0]["id"], id);

        // A command opens a session that the owning node grants and fences.
        local
            .remotes
            .send(&id, "hello".into(), Some("crlf"))
            .await
            .unwrap();
        let Command::Send(packet) = worker_command(&rx).await else {
            panic!("send")
        };
        assert_eq!(packet.chunk.bytes, b"hello\r\n");
        let holder = owner.view(&owner.ports.lock().unwrap()[0])["holder"].clone();
        assert_eq!(holder["kind"], "client");
        assert!(holder["client"].as_str().unwrap().starts_with("GizMoth · "));
        assert_eq!(holder["title"], "通过级联操作串口");
        assert_eq!(
            local.remotes.port_view(&id).unwrap()["remote"]["control"]["connectionId"],
            holder["id"]
        );
        // Device loss keeps the same gRPC lease. Physical commands fail without
        // buffering, then resume on that lease when the worker becomes ready.
        shared.transport_down(1, Connection::Waiting, "synthetic unplug");
        let unavailable = local
            .remotes
            .send(&id, "offline".into(), None)
            .await
            .unwrap_err();
        assert_eq!(unavailable.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            owner.view(&owner.ports.lock().unwrap()[0])["holder"]["id"],
            holder["id"]
        );
        assert!(shared.transport_ready(1));
        local
            .remotes
            .send(&id, "resumed".into(), Some("none"))
            .await
            .unwrap();
        let Command::Send(resumed) = worker_command(&rx).await else {
            panic!("resumed send")
        };
        assert_eq!(resumed.chunk.bytes, b"resumed");

        assert_eq!(
            local.remotes.port_view(&id).unwrap()["remote"]["control"]["state"],
            "granted"
        );
        assert!(
            owner
                .send(
                    "a",
                    SendText {
                        text: "ui".into(),
                        line_ending: None
                    },
                    Caller::App
                )
                .is_err(),
            "the owner's interface pauses while a remote session holds the port"
        );

        let (_, page) = local.remotes.logs(&id, None, 50).await.unwrap();
        assert_eq!(page["instanceId"], owner.instance_id);
        assert!(page["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["text"].as_str().unwrap().contains("请求独占通信")));

        // Releasing ends the session; control returns to the owning node.
        let connection = local.remotes.port_view(&id).unwrap()["remote"]["control"]["connectionId"]
            .as_u64()
            .unwrap();
        assert!(local.remotes.release(&id, connection + 1).is_err());
        local.remotes.release(&id, connection).unwrap();
        eventually(|| shared.seat.lock().unwrap().holder.is_none()).await;
        owner
            .send(
                "a",
                SendText {
                    text: "ui".into(),
                    line_ending: None,
                },
                Caller::App,
            )
            .unwrap();

        let Command::Send(_) = worker_command(&rx).await else {
            panic!("the owner's own send")
        };

        // The desktop reaches imported ports through this node's HTTP API.
        let address = serve_fixture(local.clone()).await;
        let http = reqwest::Client::new();
        let url = |path: &str| format!("http://{address}{path}");
        let app = |request: reqwest::RequestBuilder| request.bearer_auth("app-token").send();
        let snapshot: Value = app(http.get(url("/v1/ports")))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(snapshot["nodes"][0]["status"], "online");
        assert_eq!(snapshot["node"]["id"], local.node.id);
        let view: Value = app(http.get(url(&format!("/v1/ports/{id}"))))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(view["name"], "Board");
        let logs = app(http.get(url(&format!("/v1/ports/{id}/logs?limit=5"))))
            .await
            .unwrap();
        assert_eq!(logs.status(), 200);
        let sent = app(http
            .post(url(&format!("/v1/ports/{id}/send")))
            .json(&json!({ "text": "over http", "lineEnding": "none" })))
        .await
        .unwrap();
        assert_eq!(sent.status(), 202);
        let Command::Send(packet) = worker_command(&rx).await else {
            panic!("send")
        };
        assert_eq!(packet.chunk.bytes, b"over http");
        let patched = app(http
            .patch(url(&format!("/v1/ports/{id}")))
            .json(&json!({ "enabled": false })))
        .await
        .unwrap();
        assert_eq!(
            patched.status(),
            409,
            "configuration stays with the owning node"
        );
        let connection = local.remotes.port_view(&id).unwrap()["remote"]["control"]["connectionId"]
            .as_u64()
            .unwrap();
        let released = app(http.post(url(&format!(
            "/v1/ports/{id}/connections/{connection}/disconnect"
        ))))
        .await
        .unwrap();
        assert_eq!(released.status(), 200);
        eventually(|| shared.seat.lock().unwrap().holder.is_none()).await;
        // Agents using this node's API key may read imported ports, not command them.
        let key = local.gateway.lock().unwrap().api_key.clone();
        let agent = http
            .post(url(&format!("/v1/ports/{id}/send")))
            .bearer_auth(&key)
            .json(&json!({ "text": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(agent.status(), 403);
        let agent_admin = http
            .post(url("/v1/remotes/probe"))
            .bearer_auth(&key)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(agent_admin.status(), 403);

        // Local-only operations are refused for imported ports.
        let missing = remote::local_id(&owner.node.id, "missing");
        assert_eq!(
            local
                .remotes
                .send(&missing, "x".into(), None)
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        local.remotes.remove(&owner.node.id).unwrap();
        assert!(local.remotes.port_view(&id).is_none());
        assert!(!local.remotes.has_nodes());
    }

    #[tokio::test]
    async fn grpc_sessions_require_open_and_refuse_commands_while_queued() {
        use crate::serial::grpc::pb;
        let (_directory, owner) = fixture();
        let mut port = port(&owner);
        let _rx = mock_worker(&mut port, Protocol::Serial);
        port.config.protocol = Protocol::Serial;
        port.config.auto_grant = false;
        owner.ports.lock().unwrap().push(port);
        let address = serve_fixture(owner.clone()).await;
        let key = owner.gateway.lock().unwrap().api_key.clone();
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let token: tonic::metadata::MetadataValue<_> = format!("Bearer {key}").parse().unwrap();
        let mut client = pb::gateway_client::GatewayClient::with_interceptor(
            channel,
            move |mut request: tonic::Request<()>| {
                request
                    .metadata_mut()
                    .insert("authorization", token.clone());
                Ok(request)
            },
        );
        let node = client
            .get_node(pb::GetNodeRequest {})
            .await
            .unwrap()
            .into_inner();
        assert_eq!(node.node_id, owner.node.id);
        assert_eq!(node.boot_id, owner.instance_id);

        let (input, requests) = tokio::sync::mpsc::channel(4);
        input
            .send(pb::SessionInput {
                request_id: "1".into(),
                kind: Some(pb::session_input::Kind::Refresh(
                    pb::session_input::Refresh {
                        job_id: String::new(),
                    },
                )),
            })
            .await
            .unwrap();
        let refused = client
            .session(tokio_stream::wrappers::ReceiverStream::new(requests))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), tonic::Code::InvalidArgument);

        // Manual approval: the session waits in the queue and cannot command.
        let (input, requests) = tokio::sync::mpsc::channel(4);
        input
            .send(pb::SessionInput {
                request_id: String::new(),
                kind: Some(pb::session_input::Kind::Open(pb::session_input::Open {
                    port_id: "a".into(),
                    client: "agent".into(),
                    reason: "test".into(),
                    title: "Test device".into(),
                    device_data: false,
                })),
            })
            .await
            .unwrap();
        let mut output = client
            .session(tokio_stream::wrappers::ReceiverStream::new(requests))
            .await
            .unwrap()
            .into_inner();
        let first = output.message().await.unwrap().unwrap();
        assert!(matches!(
            first.kind,
            Some(pb::session_output::Kind::Queued(_))
        ));
        input
            .send(pb::SessionInput {
                request_id: "send".into(),
                kind: Some(pb::session_input::Kind::Send(pb::session_input::Send {
                    text: "x".into(),
                    line_ending: 0,
                })),
            })
            .await
            .unwrap();
        let result = loop {
            match output.message().await.unwrap().unwrap().kind {
                Some(pb::session_output::Kind::Result(result)) => break result,
                _ => continue,
            }
        };
        assert!(!result.ok);
        assert_eq!(result.code, tonic::Code::FailedPrecondition as i32);
        let queue = owner.view(&owner.ports.lock().unwrap()[0])["queue"].clone();
        assert_eq!(queue[0]["client"], "agent");
        drop(input);
        eventually(|| {
            owner.ports.lock().unwrap()[0]
                .shared
                .seat
                .lock()
                .unwrap()
                .queue
                .is_empty()
        })
        .await;
    }

    #[tokio::test]
    async fn lan_cascade_uses_h2c_without_certificates_and_fences_listener_identity() {
        let Some(ip) = gateway::lan_ips().first().copied() else {
            return;
        };
        let (_owner_directory, owner) = fixture();
        let config = gateway::Config {
            scope: gateway::Scope::Lan,
            port: 0,
            ..owner.gateway.lock().unwrap().clone()
        };
        let listeners =
            Arc::new(gateway::Listeners::prepare_for_ips(&config, None, &[ip]).unwrap());
        let address = listeners.local_addr().unwrap();
        let port = address.port();
        *owner.gateway.lock().unwrap() = gateway::Config { port, ..config };
        *owner.bound.lock().unwrap() = address;
        *owner.listeners.lock().unwrap() = Some(listeners.clone());
        owner.authority.lan.store(true, Ordering::SeqCst);
        let mut active = listeners.adopt().unwrap();
        drop(listeners);
        let server = owner.clone();
        let mut rebind = owner.rebind.subscribe();
        let serving = tokio::spawn(async move {
            let capacity = Arc::new(Semaphore::new(MAX_CONNECTIONS));
            loop {
                tokio::select! {
                    _ = rebind.changed() => server.adopt_pending(&mut active),
                    accepted = active.accept() => {
                        let accepted = accepted.unwrap();
                        let permit = capacity.clone().try_acquire_owned().unwrap();
                        tokio::spawn(serve_connection(server.clone(), accepted, permit));
                    }
                }
            }
        });
        let key = owner.gateway.lock().unwrap().api_key.clone();
        let url = format!("{ip}:{port}");
        let loopback = format!("127.0.0.1:{port}");
        let (_directory, local) = fixture();

        // HTTP/2 prior knowledge on the LAN socket needs no TLS negotiation or
        // certificate files. The ordinary REST API shares that h2c connection.
        let http = reqwest::Client::builder()
            .no_proxy()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let response = http
            .get(format!("http://{url}/v1/health"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(response.version(), hyper::Version::HTTP_2);
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["caller"], "gateway");
        for token in [owner.token.as_str(), "gmk_wrong-key-0000000000"] {
            let response = http
                .get(format!("http://{url}/v1/health"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        }
        assert!(local
            .remotes
            .probe(&url, "gmk_wrong-key-0000000000")
            .await
            .is_err());
        assert!(
            local.remotes.probe(&url, &owner.token).await.is_err(),
            "a caller on this machine still entered through a LAN listener"
        );
        let (_, probe) = local.remotes.probe(&url, &key).await.unwrap();
        assert_eq!(probe["node"]["id"], owner.node.id);
        assert_eq!(probe["url"], format!("http://{url}"));
        assert!(probe.get("certificate").is_none());
        assert!(owner.gateway_view().get("tlsCertificateChain").is_none());
        assert!(!owner.dir.join("chain.pem").exists());
        assert!(!owner.dir.join("key.pem").exists());
        local.remotes.add(&url, &key).await.unwrap();
        assert!(local.remotes.has_nodes());

        owner
            .reconfigure(gateway_patch(json!({"scope": "local"})))
            .unwrap();
        eventually(|| {
            owner
                .listeners
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .lan_addresses()
                .is_empty()
        })
        .await;
        // Already-open LAN h2c connections lose remote authorization as well.
        assert_eq!(
            http.get(format!("http://{url}/v1/health"))
                .bearer_auth(&key)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        assert!(local.remotes.probe(&url, &key).await.is_err());
        assert!(local.remotes.probe(&loopback, &key).await.is_ok());
        assert_eq!(
            http.get(format!("http://{loopback}/v1/health"))
                .bearer_auth(&owner.token)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()["caller"],
            "app"
        );

        owner
            .reconfigure(gateway_patch(json!({"scope": "lan"})))
            .unwrap();
        eventually(|| {
            owner
                .listeners
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .lan_addresses()
                .contains(&ip)
        })
        .await;
        assert!(local.remotes.probe(&url, &key).await.is_ok());
        assert!(local.remotes.probe(&url, &owner.token).await.is_err());
        serving.abort();
        owner.shutdown.send_replace(true);
    }
}
