//! The resident serial daemon: the single owner of every configured port.
//!
//! - One worker thread per enabled, present port holds the only open handle.
//! - The daemon alone enumerates ports; clients read its snapshot.
//! - A loopback HTTP API, authenticated by a per-launch token stored in a
//!   private file, serves the desktop window and the MCP entry point.
use super::{
    gateway,
    h2loader::{self, Events, Reboot, Session},
    logs::{Ring, Source},
    now_ms, state_dir, system_ports, Mode, PortConfig, Protocol, SystemPort, UsbIdentity,
    API_VERSION, BUILD_ID,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{server::conn::http1, service::service_fn, Method, Request, Response, StatusCode};
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
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

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
/// an in-flight socket/device write. Message counts are bounded by the channels.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteOwner {
    App,
    Gateway(Caller),
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
    /// Why the client asked for the port, shown to the user.
    reason: String,
    /// When it asked (queued) or was granted (holder).
    since: u64,
    /// Actual authenticated identity; the displayed client name is not trusted.
    caller: Caller,
    /// Device bytes for the holder, bounded by bytes and message count.
    out: tokio::sync::mpsc::Sender<Chunk>,
    budget: Arc<Semaphore>,
    /// Latest queue/grant/end state. Positions may coalesce; terminal notices
    /// cannot be stuck behind an unbounded queue of stale positions.
    control: watch::Sender<Control>,
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
                json!({ "type": "queued", "position": position, "holder": holder }).to_string()
            }
            Control::Granted => json!({ "type": "granted" }).to_string(),
            Control::End(kind, reason) => json!({ "type": kind, "reason": reason }).to_string(),
        }
    }
}

/// Ownership, raw-transport readiness and physical writes share this mutex.
/// A revocation waits for an in-flight write; queued packets are lease tagged.
struct Seat {
    holder: Option<Pipe>,
    queue: VecDeque<Pipe>,
    approved: Option<u64>,
    mode: Mode,
    auto_grant: bool,
    ready: bool,
    writer: Option<Writer>,
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
        json!({ "id": pipe.id, "client": pipe.client, "reason": pipe.reason, "since": pipe.since })
    }
    fn revoke(&mut self, reason: &str) -> Option<String> {
        let pipe = self.holder.take()?;
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
            WriteOwner::Gateway(caller) => self.mode == Mode::Takeover && authority.allows(caller),
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

pub struct Shared {
    live: Mutex<Live>,
    logs: Mutex<Ring>,
    dirty: Arc<AtomicBool>,
    seat: Mutex<Seat>,
    next_worker: AtomicU64,
    authority: Arc<Authority>,
    /// An Auto port has answered as H2Loader before; piping then uses its baud.
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
        let full = seat.holder.as_ref().is_some_and(|pipe| {
            Chunk::new(bytes.to_vec(), &pipe.budget)
                .and_then(|chunk| pipe.out.try_send(chunk).map_err(|_| ()))
                .is_err()
        });
        if full {
            seat.revoke("客户端接收过慢，透传已结束");
            seat.promote(&self.authority);
            self.dirty.store(true, Ordering::SeqCst);
        }
    }
    /// Ends the holder's session; queued requests keep waiting.
    fn revoke(&self, reason: &str) {
        let client = self.seat.lock().unwrap().revoke(reason);
        if let Some(client) = client {
            self.system(format!("{client} 的占据已结束：{reason}"));
        }
    }
    fn transport_down(&self, worker: u64, reason: &str) {
        let mut seat = self.seat.lock().unwrap();
        if seat.writer.as_ref().is_some_and(|w| w.worker == worker) {
            seat.ready = false;
            seat.revoke(reason);
        }
    }
    fn raw_ready(&self, worker: u64) -> bool {
        let mut seat = self.seat.lock().unwrap();
        if seat.writer.as_ref().is_none_or(|w| w.worker != worker) {
            return false;
        }
        seat.ready = true;
        if let Some(client) = seat.promote(&self.authority) {
            self.system(format!("授予 {client} 串口使用权"));
        }
        true
    }
    fn flush_partial(&self) {
        if self.logs.lock().unwrap().flush_partial(now_ms()) {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }
    fn holder(&self, config: &PortConfig, running: bool) -> Value {
        if let Some(pipe) = self.seat.lock().unwrap().holder.as_ref() {
            let mut entry = Seat::entry(pipe);
            entry["kind"] = json!("client");
            return entry;
        }
        match config.mode {
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
    /// Bound sockets retained for transactional reconfiguration and TLS renewal.
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
            live.activity = if mode == Protocol::Serial || config.mode == Mode::Piping {
                Activity::Idle
            } else {
                Activity::Handshaking
            };
        }) {
            break;
        }
        let outcome = match (config.mode, mode) {
            (Mode::Piping, _) => {
                let baud = if config.protocol == Protocol::H2loader
                    || (config.protocol == Protocol::Auto
                        && (config.h2loader_seen || shared.h2loader_seen.load(Ordering::SeqCst)))
                {
                    460_800
                } else {
                    config.baud
                };
                run_raw(&config, baud, &shared, worker, &rx, &stop, true)
            }
            (_, Protocol::Serial) => run_raw(
                &config,
                config.baud,
                &shared,
                worker,
                &rx,
                &stop,
                mode != config.protocol,
            ),
            (_, _) => run_h2loader(
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
        shared.transport_down(worker, "串口连接已结束");
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
    shared.transport_down(worker, "串口连接已结束");
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
        || !shared.raw_ready(worker)
    {
        return Outcome::Stopped;
    }
    if config.mode == Mode::Piping {
        shared.system(format!("已连接（透传，{baud} baud）"));
    } else if fallback {
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

fn h2loader_open_failure(name: String, auto: bool) -> Outcome {
    if name == "busy" {
        return Outcome::Failed(Connection::Occupied, h2loader::describe(&name));
    }
    // Only a failed protocol probe may select plain serial. Retrying an open
    // rejected by the PAL would bypass its ownership and access checks.
    if auto
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
            return h2loader_open_failure(name, auto);
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
    loop {
        if stop.load(Ordering::SeqCst) {
            return Outcome::Stopped;
        }
        interrupt.store(false, Ordering::SeqCst);
        match rx.try_recv() {
            Ok(Command::Job) => {
                run_h2loader_job(&mut session, &events, shared);
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

fn run_h2loader_job(session: &mut Session, events: &H2Events, shared: &Shared) {
    let worker = events.worker;
    let Some((kind, cancel, snapshot)) = shared
        .live
        .lock()
        .unwrap()
        .job
        .as_ref()
        .map(|job| (job.kind, job.cancel.clone(), job.snapshot.clone()))
    else {
        return;
    };
    *events.job_cancel.borrow_mut() = Some(cancel);
    let phase = |activity: Activity, phase: &str| {
        shared.worker_update(worker, |live| {
            live.activity = activity;
            if let Some(job) = live.job.as_mut() {
                job.state = JobState::Running;
                job.phase = phase.into();
            }
        });
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
    shared.finish_job(worker, result);
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
        if next == Connection::Connecting {
            seat.revoke("串口正在重新连接");
        } else {
            seat.end(if next == Connection::Waiting {
                "设备已断开"
            } else {
                "串口已断开"
            });
        }
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
            seat.mode = config.mode;
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
            } else if port.worker.is_none() && wanted && port.stopping.is_none() {
                self.start_worker(port);
            } else if port.worker.is_none() && port.config.enabled && !port.present {
                port.shared
                    .update(|live| live.connection = Connection::Waiting);
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
            map.insert(
                "holder".into(),
                port.shared.holder(&port.config, port.worker.is_some()),
            );
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
            "gateway": gateway,
            "pipeBase": format!("ws://127.0.0.1:{}/v1/ports", self.bound.lock().unwrap().port()),
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

fn pipe_token<B>(request: &Request<B>, peer: SocketAddr, tls: bool) -> Option<String> {
    let query = query_str(request.uri(), "token");
    if tls || !peer.ip().is_loopback() {
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
    tls_certificate_chain: Option<String>,
    tls_private_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartJob {
    kind: JobKind,
    path: Option<String>,
    source: Option<String>,
    job_id: Option<String>,
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

fn query_str(uri: &hyper::Uri, key: &str) -> Option<String> {
    uri.query()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| {
            percent_encoding::percent_decode_str(v)
                .decode_utf8_lossy()
                .into_owned()
        })
}

/// Why GizMoth must not act on a piping port itself.
fn piping_refusal(port: &Port) -> (StatusCode, String) {
    let message = match port.shared.seat.lock().unwrap().holder.as_ref() {
        Some(pipe) => format!(
            "串口处于透传模式，正被 {} 占据；切换为接管后 GizMoth 才能操作",
            pipe.client
        ),
        None => "串口处于透传模式，GizMoth 只记录并转发；切换为接管后才能操作".into(),
    };
    (StatusCode::CONFLICT, message)
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
        let view = {
            let _admission = self.admitted(caller)?;
            let mut ports = self.ports.lock().unwrap();
            let index = ports
                .iter()
                .position(|p| p.config.id == id)
                .ok_or_else(not_found)?;
            let port = &ports[index];
            let next = body.candidate(&port.config)?;
            let restart = next.protocol != port.config.protocol
                || next.baud != port.config.baud
                || next.mode != port.config.mode;
            if restart
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
            let mode_changed = next.mode != port.config.mode;
            port.config = next;
            {
                let mut seat = port.shared.seat.lock().unwrap();
                seat.mode = port.config.mode;
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
                    Mode::Piping => "切换为透传模式：GizMoth 只记录并转发",
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
        if let Some(handle) = port.stopping.take() {
            self.retired_workers.lock().unwrap().push(handle);
        }
        self.dirty.store(true, Ordering::SeqCst);
        Ok((StatusCode::OK, json!({ "removed": id })))
    }

    fn send(&self, id: &str, body: SendText, caller: Caller) -> Reply {
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
            if port.config.mode == Mode::Piping {
                return Err(piping_refusal(port));
            }
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
            let writer = seat
                .writer
                .as_ref()
                .filter(|w| w.worker == worker.id && seat.ready)
                .ok_or_else(|| (StatusCode::CONFLICT, "串口连接已关闭".into()))?;
            let owner = match caller {
                Caller::App => WriteOwner::App,
                caller => WriteOwner::Gateway(caller),
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
    ) -> Result<u64, (StatusCode, String)> {
        if port.config.mode == Mode::Piping {
            return Err(piping_refusal(port));
        }
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
            let worker = Self::job_eligible(port, kind, None)?;
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
        let reserved_worker = self.reserve_job(id, body.kind, caller, &job_id)?;
        let registered = self.with_port(id, |port| {
            if port.preparing.as_deref() != Some(&job_id) {
                return Err((StatusCode::CONFLICT, "任务准备已取消".into()));
            }
            if Self::job_eligible(port, body.kind, Some(&job_id))? != reserved_worker {
                return Err((StatusCode::CONFLICT, "串口已重新连接，请重试".into()));
            }
            let job = Job {
                id: job_id.clone(),
                kind: body.kind,
                state: JobState::Queued,
                phase: "preparing".into(),
                acknowledged: 0,
                total: 0,
                source: if caller != Caller::App {
                    "gateway".into()
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
            };
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
                let worker_id = Self::job_eligible(port, body.kind, Some(&job.id))?;
                if worker_id != job.worker {
                    return Err((StatusCode::CONFLICT, "串口已重新连接，请重试".into()));
                }
                let worker = port.worker.as_ref().unwrap();
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
        }
    }

    /// Both FIFO grants and explicit approvals wait for a usable raw worker.
    fn grant_next(port: &Port) {
        if !port.config.enabled {
            return;
        }
        let mut seat = port.shared.seat.lock().unwrap();
        seat.mode = port.config.mode;
        seat.auto_grant = port.config.auto_grant;
        if let Some(client) = seat.promote(&port.shared.authority) {
            port.shared.system(format!("授予 {client} 串口使用权"));
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
        let mut saved: Vec<_> = ports.iter().map(|p| p.config.clone()).collect();
        saved[index].mode = Mode::Piping;
        persist(saved).map_err(|e| bad(e.to_string()))?;
        let port = &mut ports[index];
        if port.config.mode != Mode::Piping {
            port.config.mode = Mode::Piping;
            Self::stop_worker_locked(port, Connection::Connecting, &mut seat);
        }
        seat.revoke("GizMoth 已将串口交给其他客户端");
        seat.mode = Mode::Piping;
        seat.approved = Some(request);
        seat.notify_positions();
        drop(seat);
        Self::grant_next(port);
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
        let limit = query(uri, "limit").unwrap_or(500).clamp(1, 2000) as usize;
        self.with_port(id, |port| {
            let ring = port.shared.logs.lock().unwrap();
            let page = match query(uri, "after") {
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

    fn authorize(&self, token: &str, peer: SocketAddr, tls: bool) -> Option<Caller> {
        // LAN sockets never receive desktop privileges, including local callers
        // connecting to a LAN address. Proxy headers cannot change this identity.
        let local = !tls && peer.ip().is_loopback();
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
            "tlsCertificateChain": config.tls_certificate_chain,
            "tlsPrivateKey": config.tls_private_key,
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
        if !*self.admission.lock().unwrap() || self.pending.lock().unwrap().is_some() {
            return;
        }
        let config = self.gateway.lock().unwrap().clone();
        if !config.enabled || config.scope != gateway::Scope::Lan {
            return;
        }
        let Some(current) = self
            .listeners
            .lock()
            .unwrap()
            .clone()
            .filter(|l| l.has_tls())
        else {
            // A permanent certificate error requires an explicit settings save,
            // not a certificate-file read on every network poll.
            return;
        };
        let mut ips = match enumerate() {
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
        let expected_error = ips
            .is_empty()
            .then(|| "LAN is offline; loopback service remains available".into());
        if ips == current.lan_addresses()
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
        match adopt(&next.listeners) {
            Ok(adopted) => {
                *active = adopted;
                self.set_gateway_error(next.listeners.network_error());
                *self.listeners.lock().unwrap() = Some(next.listeners);
                self.dirty.store(true, Ordering::SeqCst);
            }
            Err(error) => self.set_gateway_error(Some(format!(
                "LAN listener adoption failed: {error}; previous listeners remain available"
            ))),
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
                // binds that same address with a different TLS configuration.
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
        let reload_tls = body.tls_certificate_chain.is_some() || body.tls_private_key.is_some();
        if let Some(path) = body.tls_certificate_chain {
            next.tls_certificate_chain = (!path.trim().is_empty()).then(|| PathBuf::from(path));
        }
        if let Some(path) = body.tls_private_key {
            next.tls_private_key = (!path.trim().is_empty()).then(|| PathBuf::from(path));
        }
        let moved = next.enabled != current.enabled
            || next.scope != current.scope
            || next.port != current.port
            || reload_tls
            || self.gateway_error.lock().unwrap().is_some();
        let listener = if moved {
            let old = self.listeners.lock().unwrap().clone();
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
        let address = match listener.as_ref() {
            Some(listener) => listener.local_addr().map_err(|e| bad(e.to_string()))?,
            None => *self.bound.lock().unwrap(),
        };
        let config_path = gateway::path(&self.dir);
        private_write(
            &config_path,
            &serde_json::to_vec_pretty(&next).unwrap_or_default(),
        )
        .map_err(|e| bad(e.to_string()))?;
        if moved {
            if let Err(error) = self.write_info_at(address) {
                // No in-memory/listener/authorization mutation has happened.
                let _ = private_write(
                    &config_path,
                    &serde_json::to_vec_pretty(&current).unwrap_or_default(),
                );
                return Err(bad(error.to_string()));
            }
        }
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
            *self.gateway_error.lock().unwrap() = listener.network_error();
            *self.listeners.lock().unwrap() = Some(listener.clone());
            *self.pending.lock().unwrap() = Some(PendingListeners {
                listeners: listener,
                network_refresh: false,
            });
            *self.bound.lock().unwrap() = address;
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

    async fn handle(
        self: Arc<Self>,
        request: Request<hyper::body::Incoming>,
        peer: SocketAddr,
        tls: bool,
    ) -> Reply {
        let Some(caller) = bearer(&request).and_then(|token| self.authorize(&token, peer, tls))
        else {
            return Err((StatusCode::UNAUTHORIZED, "unauthorized".into()));
        };
        let app = caller == Caller::App;
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
        match (&method, s.as_slice()) {
            (&Method::GET, ["v1", "health"]) => Ok((
                StatusCode::OK,
                json!({
                    "apiVersion": API_VERSION,
                    "buildId": BUILD_ID,
                    "instanceId": self.instance_id,
                    "instanceGeneration": self.instance_generation,
                    "pid": std::process::id(),
                    "startedAt": self.started_at,
                    "h2loaderCore": h2loader::AVAILABLE,
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
                let _admission = self.admitted(caller)?;
                self.cancel_job(id)
            }
            (&Method::POST, ["v1", "ports", id, "requests", request, action]) if app => {
                let request: u64 = request.parse().map_err(|_| not_found())?;
                match *action {
                    "accept" => self.accept(id, request),
                    "reject" => self.reject(id, request),
                    _ => Err(not_found()),
                }
            }
            (&Method::POST, ["v1", "ports", id, "holder", "release"]) if app => {
                self.with_port(id, |port| {
                    port.shared.revoke("GizMoth 收回了串口");
                    Self::grant_next(port);
                    Ok((StatusCode::OK, self.view(port)))
                })
            }
            (_, ["v1", "ports", _, "requests", ..]) | (_, ["v1", "ports", _, "holder", ..])
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

/// Log line for bytes a holder sent: text as text, anything else as a short
/// hex summary so binary protocol frames stay readable in the log.
fn log_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text)
            if text
                .chars()
                .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')) =>
        {
            text.trim_end().to_string()
        }
        _ => {
            let head: Vec<String> = bytes.iter().take(24).map(|b| format!("{b:02x}")).collect();
            let more = if bytes.len() > 24 { " …" } else { "" };
            format!("[{} 字节] {}{more}", bytes.len(), head.join(" "))
        }
    }
}

fn is_pipe_request(request: &Request<hyper::body::Incoming>) -> bool {
    request.method() == Method::GET && request.uri().path().ends_with("/pipe")
}

/// `GET /v1/ports/{id}/pipe` upgraded to a WebSocket: the caller becomes the
/// port's single holder. Binary or text frames are written to the device;
/// device bytes come back as binary frames. GizMoth records both directions.
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
        Err(bad(format!("缺少 {key}：请提供客户端名称和占据原因")))
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

async fn pipe(
    daemon: Arc<Daemon>,
    mut request: Request<hyper::body::Incoming>,
    peer: SocketAddr,
    tls: bool,
) -> Response<Full<Bytes>> {
    use tokio_tungstenite::tungstenite::{
        handshake::derive_accept_key,
        protocol::{Role, WebSocketConfig},
    };
    let uri = request.uri().clone();
    let token = pipe_token(&request, peer, tls).unwrap_or_default();
    let Some(caller) = daemon.authorize(&token, peer, tls) else {
        return respond(Err((StatusCode::UNAUTHORIZED, "unauthorized".into())));
    };
    let parts: Vec<_> = uri.path().trim_matches('/').split('/').collect();
    let id = match parts.as_slice() {
        ["v1", "ports", id, "pipe"] => id.to_string(),
        _ => return respond(Err(not_found())),
    };
    // Reject missing/empty metadata before allocating channels or a seat.
    let (client, reason) = match (
        pipe_text(&uri, "client", 64),
        pipe_text(&uri, "reason", 200),
    ) {
        (Ok(client), Ok(reason)) => (client, reason),
        (Err(error), _) | (_, Err(error)) => return respond(Err(error)),
    };
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
    let Ok(pipe_permit) = daemon.pipe_slots.clone().try_acquire_owned() else {
        return respond(Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "透传连接已满".into(),
        )));
    };
    let (out, mut device) = tokio::sync::mpsc::channel::<Chunk>(QUEUE_MESSAGES);
    let (control, mut notices) = watch::channel(Control::Queued(1, None));
    let pipe_id = daemon.next_pipe.fetch_add(1, Ordering::SeqCst);
    let joined = (|| {
        // Recheck the authenticated generation under the same admission lock
        // as key changes and retirement; a pre-authenticated join cannot slip in.
        let _admission = daemon.admitted(caller)?;
        daemon.with_port(&id, |port| {
            if !port.config.enabled || (!port.present && port.worker.is_none()) {
                return Err((StatusCode::CONFLICT, "串口未连接".into()));
            }
            let mut seat = port.shared.seat.lock().unwrap();
            if seat.queue.len() >= MAX_PIPES {
                return Err((StatusCode::SERVICE_UNAVAILABLE, "串口请求队列已满".into()));
            }
            seat.mode = port.config.mode;
            seat.auto_grant = port.config.auto_grant;
            seat.queue.push_back(Pipe {
                id: pipe_id,
                client: client.clone(),
                reason: reason.clone(),
                since: now_ms(),
                caller,
                out,
                budget: Arc::new(Semaphore::new(QUEUE_BYTES)),
                control,
            });
            seat.promote(&daemon.authority);
            seat.notify_positions();
            drop(seat);
            port.shared
                .system(format!("{client} 请求占据串口：{reason}"));
            Ok(port.shared.clone())
        })
    })();
    let shared = match joined {
        Ok(shared) => shared,
        Err(error) => return respond(Err(error)),
    };
    daemon.last_request.store(now_ms(), Ordering::SeqCst);
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        let _pipe_permit = pipe_permit;
        let leave = |daemon: &Arc<Daemon>| {
            let mut seat = shared.seat.lock().unwrap();
            let held = seat.holder.as_ref().is_some_and(|p| p.id == pipe_id);
            if held {
                seat.holder = None;
            }
            seat.queue.retain(|p| p.id != pipe_id);
            if seat.approved == Some(pipe_id) {
                seat.approved = None;
            }
            seat.notify_positions();
            drop(seat);
            let _ = daemon.with_port(&id, |port| {
                Daemon::grant_next(port);
                Ok(())
            });
            shared.dirty.store(true, Ordering::SeqCst);
        };
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
        let initial = notices.borrow_and_update().clone();
        if !websocket_send(&mut sink, Message::Text(initial.frame().into())).await
            || matches!(initial, Control::End(..))
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
                chunk = device.recv() => match chunk {
                    Some(Chunk { bytes, _permit: permit }) => {
                        let sent = websocket_send(&mut sink, Message::Binary(bytes.into())).await;
                        // Keep the in-flight bytes charged until the send completes.
                        drop(permit);
                        if !sent { break; }
                    }
                    None => break,
                },
                message = stream.next() => {
                    let bytes = match message {
                        Some(Ok(Message::Binary(bytes))) => bytes.to_vec(),
                        Some(Ok(Message::Text(text))) => text.as_bytes().to_vec(),
                        Some(Ok(Message::Ping(data))) => {
                            if !websocket_send(&mut sink, Message::Pong(data)).await { break; }
                            continue;
                        }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => continue,
                    };
                    let result = {
                        let seat = shared.seat.lock().unwrap();
                        if !seat.holder.as_ref().is_some_and(|p| p.id == pipe_id && p.caller == caller)
                            || !shared.authority.allows(caller) {
                            Err((StatusCode::CONFLICT, "尚未获得串口或授权已结束".into()))
                        } else if !seat.ready {
                            Err((StatusCode::CONFLICT, "串口正在重新连接，透传已结束".into()))
                        } else if let Some(writer) = seat.writer.as_ref() {
                            let line = log_text(&bytes);
                            writer.send(bytes, line, WriteOwner::Pipe { id: pipe_id, caller })
                        } else { Err((StatusCode::CONFLICT, "串口传输不可用".into())) }
                    };
                    if let Err((_, reason)) = result {
                        // Full/disconnected queues visibly terminate the protocol
                        // instead of silently dropping a frame and retaining it.
                        let error = json!({ "type": "error", "reason": reason }).to_string();
                        let _ = websocket_send(&mut sink, Message::Text(error.into())).await;
                        break;
                    }
                }
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
    let (peer, tls) = (accepted.peer, accepted.tls);
    let mut stopped = daemon.shutdown.subscribe();
    let Ok(stream) = accepted.handshake().await else {
        return;
    };
    let (activity, mut activity_rx) = watch::channel(0u64);
    let service = service_fn(move |request| {
        activity.send_modify(|sequence| *sequence += 1);
        let daemon = daemon.clone();
        async move {
            let response = tokio::time::timeout(RESPONSE_TIMEOUT, async move {
                if is_pipe_request(&request) {
                    return pipe(daemon, request, peer, tls).await;
                }
                respond(daemon.handle(request, peer, tls).await)
            })
            .await
            .unwrap_or_else(|_| respond(Err((StatusCode::REQUEST_TIMEOUT, "请求处理超时".into()))));
            Ok::<_, Infallible>(response)
        }
    });
    let mut http = http1::Builder::new();
    // Reset the idle budget for every request. WebSockets have their own pipe
    // permit and send deadlines after Hyper hands the connection over.
    http.keep_alive(true)
        .timer(TokioTimer::new())
        .header_read_timeout(BODY_TIMEOUT);
    let connection = http
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
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
            _ = &mut idle => break,
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
    let gateway_config = gateway::load_or_create(&dir, private_write);
    // A busy Gateway port must not keep the serial service from starting:
    // fall back to a private loopback port and report the error.
    let (listener, gateway_error) = match gateway::Listeners::prepare(&gateway_config, None) {
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
    let (rebind, mut rebind_rx) = watch::channel(0u64);
    let (version, _) = watch::channel(0u64);
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let authority = Arc::new(Authority::new(&gateway_config));
    let listener = Arc::new(listener);
    let daemon = Arc::new(Daemon {
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
        dirty: Arc::new(AtomicBool::new(true)),
        version,
        last_request: AtomicU64::new(now_ms()),
        shutdown,
        dir: dir.clone(),
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
                        let serving = daemon.gateway.lock().unwrap().enabled;
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
                        let (peer, tls) = (accepted.peer, accepted.tls);
                        let capacity = if !tls && peer.ip().is_loopback() { &local_capacity } else { &remote_capacity };
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
    remove_owned_info(&dir, &daemon.token);
    let _ = fs::remove_dir_all(dir.join("packages"));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

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
        });
        (directory, daemon)
    }

    fn offline_network_fixture() -> (tempfile::TempDir, Arc<Daemon>) {
        let (directory, daemon) = fixture();
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let certificate = daemon.dir.join("chain.pem");
        let key = daemon.dir.join("key.pem");
        fs::write(&certificate, generated.cert.pem()).unwrap();
        fs::write(&key, generated.key_pair.serialize_pem()).unwrap();
        let mut config = gateway::Config {
            scope: gateway::Scope::Lan,
            port: 0,
            tls_certificate_chain: Some(certificate.clone()),
            tls_private_key: Some(key.clone()),
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
        // All later refreshes must use the cached TLS configuration.
        fs::remove_file(certificate).unwrap();
        fs::remove_file(key).unwrap();
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
        let (tx, rx) = mpsc::sync_channel(QUEUE_MESSAGES);
        port.config.enabled = true;
        port.shared.update(|live| {
            live.worker = Some(1);
            live.connection = Connection::Connected;
            live.detected = Some(detected);
        });
        {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.writer = Some(Writer {
                worker: 1,
                tx: tx.clone(),
                budget: Arc::new(Semaphore::new(QUEUE_BYTES)),
            });
            seat.ready = detected == Protocol::Serial;
        }
        port.worker = Some(Worker {
            id: 1,
            tx,
            stop: Arc::new(AtomicBool::new(false)),
            interrupt: Arc::new(AtomicBool::new(false)),
            handle: std::thread::spawn(|| {}),
        });
        rx
    }

    fn test_pipe(
        id: u64,
        caller: Caller,
    ) -> (
        Pipe,
        watch::Receiver<Control>,
        tokio::sync::mpsc::Receiver<Chunk>,
    ) {
        let (control, notices) = watch::channel(Control::Queued(1, None));
        let (out, device) = tokio::sync::mpsc::channel(1);
        (
            Pipe {
                id,
                client: format!("client-{id}"),
                reason: "test".into(),
                since: 1,
                caller,
                out,
                budget: Arc::new(Semaphore::new(QUEUE_BYTES)),
                control,
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
    fn manual_and_fifo_grants_wait_for_raw_transport() {
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
        assert!(port.shared.raw_ready(1));
        assert!(matches!(*second_notice.borrow(), Control::Granted));
        assert!(matches!(*first_notice.borrow(), Control::Queued(..)));
        port.shared.transport_down(1, "reconnect");
        assert!(matches!(
            *second_notice.borrow(),
            Control::End("revoked", _)
        ));
        {
            let mut seat = port.shared.seat.lock().unwrap();
            seat.auto_grant = true;
            assert!(seat.promote(&daemon.authority).is_none());
        }
        assert!(port.shared.raw_ready(1));
        assert_eq!(
            port.shared.seat.lock().unwrap().holder.as_ref().unwrap().id,
            1
        );
        assert!(!port.shared.raw_ready(99));
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
        assert!(stop.load(Ordering::SeqCst));
        assert_eq!(load_configs(&daemon.dir)[0].mode, Mode::Piping);
        let mut ports = daemon.ports.lock().unwrap();
        assert_eq!(ports[0].config.mode, Mode::Piping);
        assert!(ports[0].worker.is_none());
        assert!(shared.seat.lock().unwrap().queue.is_empty());
        ports[0].stopping.take().unwrap().join().unwrap();
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
    fn gateway_takeover_writes_are_fenced_by_authorization_generation() {
        let (_directory, daemon) = fixture();
        daemon.authority.enabled.store(true, Ordering::SeqCst);
        let mut port = port(&daemon);
        let rx = mock_worker(&mut port, Protocol::Serial);
        let caller = Caller::Gateway {
            local: true,
            generation: 1,
        };
        let writer = port.shared.seat.lock().unwrap().writer.clone().unwrap();
        writer
            .send(
                b"stale".to_vec(),
                "stale".into(),
                WriteOwner::Gateway(caller),
            )
            .unwrap();
        daemon.authority.generation.store(2, Ordering::SeqCst);
        let Command::Send(packet) = rx.try_recv().unwrap() else {
            panic!("send")
        };
        let mut output = Vec::new();
        write_packet(&mut output, &port.shared, packet, &AtomicBool::new(false)).unwrap();
        assert!(output.is_empty());
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
                    daemon.reserve_job("a", JobKind::Install, Caller::App, &format!("job-{index}"))
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

    #[test]
    fn slow_device_reader_is_explicitly_revoked() {
        let (_directory, daemon) = fixture();
        let shared = daemon.new_shared();
        let (pipe, notices, _bytes) = test_pipe(1, Caller::App);
        shared.seat.lock().unwrap().holder = Some(pipe);
        shared.relay(b"first");
        shared.relay(b"second"); // the single-message mock channel is full
        assert!(shared.seat.lock().unwrap().holder.is_none());
        assert!(matches!(*notices.borrow(), Control::End("revoked", _)));
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
            "/pipe?client=%20%0a&reason=test",
            "/pipe?client=test&reason=%00%20",
        ] {
            let uri: hyper::Uri = uri.parse().unwrap();
            assert!(
                pipe_text(&uri, "client", 64).is_err() || pipe_text(&uri, "reason", 200).is_err()
            );
        }
        let uri = "/pipe?client=agent%0a&reason=flash%20firmware"
            .parse()
            .unwrap();
        assert_eq!(pipe_text(&uri, "client", 64).unwrap(), "agent");
        assert_eq!(pipe_text(&uri, "reason", 200).unwrap(), "flash firmware");
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

    #[test]
    fn permanent_tls_failure_does_not_retry_certificate_files_on_network_polls() {
        let (_directory, daemon) = offline_network_fixture();
        let mut config = daemon.gateway.lock().unwrap().clone();
        config.scope = gateway::Scope::Local;
        let old = daemon.listeners.lock().unwrap().clone().unwrap();
        let local = gateway::Listeners::prepare_for_ips(&config, Some(&old), &[]).unwrap();
        *daemon.listeners.lock().unwrap() = Some(Arc::new(local));
        *daemon.gateway_error.lock().unwrap() = Some("certificate unavailable".into());
        daemon.refresh_gateway_network_with(|| panic!("no verified TLS exists"));
        assert_eq!(
            daemon.gateway_error.lock().unwrap().as_deref(),
            Some("certificate unavailable")
        );
        assert!(daemon.pending.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn manual_adoption_failure_requires_a_successful_runtime_retry_to_clear() {
        let (_directory, daemon) = offline_network_fixture();
        let original = daemon.listeners.lock().unwrap().clone().unwrap();
        let mut active = original.adopt().unwrap();
        let mut config = daemon.gateway.lock().unwrap().clone();
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        fs::write(
            config.tls_certificate_chain.as_ref().unwrap(),
            generated.cert.pem(),
        )
        .unwrap();
        fs::write(
            config.tls_private_key.as_ref().unwrap(),
            generated.key_pair.serialize_pem(),
        )
        .unwrap();
        config.port = 0;
        let candidate =
            Arc::new(gateway::Listeners::prepare_for_ips(&config, Some(&original), &[]).unwrap());
        let new_address = candidate.local_addr().unwrap();
        assert_ne!(new_address, original.local_addr().unwrap());
        config.port = new_address.port();
        fs::remove_file(config.tls_certificate_chain.as_ref().unwrap()).unwrap();
        fs::remove_file(config.tls_private_key.as_ref().unwrap()).unwrap();
        // Model the existing manual transaction's published std socket set,
        // followed by a failed registration with the runtime's IO reactor.
        *daemon.gateway.lock().unwrap() = config;
        *daemon.bound.lock().unwrap() = new_address;
        *daemon.listeners.lock().unwrap() = Some(candidate.clone());
        *daemon.pending.lock().unwrap() = Some(PendingListeners {
            listeners: candidate,
            network_refresh: false,
        });
        daemon.adopt_pending_with(&mut active, |_| {
            Err(std::io::Error::other("registration failed"))
        });
        let failure = daemon.gateway_error.lock().unwrap().clone();
        assert!(failure.as_ref().unwrap().contains("adoption failed"));
        daemon.refresh_gateway_network_with(|| Ok(Vec::new()));
        assert_eq!(*daemon.gateway_error.lock().unwrap(), failure);
        assert!(daemon.pending.lock().unwrap().is_some());
        daemon.adopt_pending_with(&mut active, |_| {
            Err(std::io::Error::other("registration failed"))
        });
        assert_eq!(*daemon.gateway_error.lock().unwrap(), failure);
        daemon.refresh_gateway_network_with(|| Ok(Vec::new()));
        daemon.adopt_pending(&mut active);
        assert_ne!(*daemon.gateway_error.lock().unwrap(), failure);
        assert!(daemon
            .gateway_error
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains("offline"));
        tokio::time::timeout(Duration::from_secs(2), async {
            let _client = tokio::net::TcpStream::connect(new_address).await.unwrap();
            assert!(!active.accept().await.unwrap().tls);
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
        assert!(daemon.pending.lock().unwrap().is_none());
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
            assert!(!accepted.tls);
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
        daemon.refresh_gateway_network_with(|| Ok(vec![ip]));
        daemon.adopt_pending(&mut active);
        assert!(daemon.gateway_error.lock().unwrap().is_none());
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
    async fn https_and_wss_share_the_service_while_mcp_stays_on_loopback_http() {
        use tokio_rustls::{rustls, TlsConnector};
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let Some(ip) = gateway::lan_ips().first().copied() else {
            return;
        };
        let (_directory, daemon) = fixture();
        let mut connected = port(&daemon);
        let _commands = mock_worker(&mut connected, Protocol::Serial);
        daemon.ports.lock().unwrap().push(connected);
        let generated = rcgen::generate_simple_self_signed(vec![ip.to_string()]).unwrap();
        let certificate = daemon.dir.join("chain.pem");
        let key = daemon.dir.join("key.pem");
        fs::write(&certificate, generated.cert.pem()).unwrap();
        fs::write(&key, generated.key_pair.serialize_pem()).unwrap();
        let config = gateway::Config {
            scope: gateway::Scope::Lan,
            port: 0,
            tls_certificate_chain: Some(certificate),
            tls_private_key: Some(key),
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
            .add_root_certificate(
                reqwest::Certificate::from_pem(generated.cert.pem().as_bytes()).unwrap(),
            )
            .build()
            .unwrap();
        let local = format!("http://127.0.0.1:{port_number}/v1/health");
        let remote = format!("https://{ip}:{port_number}/v1/health");
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
        let mut roots = rustls::RootCertStore::empty();
        roots.add(generated.cert.der().clone()).unwrap();
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(tls));
        for query_token in [false, true] {
            let stream = tokio::net::TcpStream::connect((ip, port_number))
                .await
                .unwrap();
            let name = rustls::pki_types::ServerName::try_from(ip.to_string()).unwrap();
            let stream = connector.connect(name, stream).await.unwrap();
            let extra = if query_token {
                "&token=never-in-a-url"
            } else {
                ""
            };
            let mut request = format!(
                "wss://{ip}:{port_number}/v1/ports/a/pipe?client=test&reason=transport-test{extra}"
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

    #[test]
    fn invalid_lan_certificate_patch_preserves_config_and_loopback_listener() {
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
        let result = daemon.reconfigure(gateway_patch(json!({
            "scope": "lan", "tlsCertificateChain": "/missing/chain.pem", "tlsPrivateKey": "/missing/key.pem"
        })));
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
        assert_eq!(daemon.gateway.lock().unwrap().scope, gateway::Scope::Local);
        assert_eq!(*daemon.bound.lock().unwrap(), address);
        assert!(daemon.pending.lock().unwrap().is_none());
        assert_eq!(daemon.gateway_view()["urls"], json!(listeners.urls()));
        assert_eq!(daemon.authority.generation.load(Ordering::SeqCst), 1);
        assert!(!gateway::path(&daemon.dir).exists());
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
                    h2loader_open_failure(name.into(), true),
                    Outcome::Failed(Connection::Error, _)
                ),
                "{name} must not open plain serial"
            );
        }
        assert!(matches!(
            h2loader_open_failure("busy".into(), true),
            Outcome::Failed(Connection::Occupied, _)
        ));
        for name in ["timeout", "format", "truncated", "core_unavailable"] {
            assert!(
                matches!(
                    h2loader_open_failure(name.into(), true),
                    Outcome::NotH2loader(_)
                ),
                "{name} may select plain serial"
            );
            assert!(
                matches!(
                    h2loader_open_failure(name.into(), false),
                    Outcome::Failed(Connection::Error, _)
                ),
                "explicit H2Loader must keep its error"
            );
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
    fn holder_bytes_are_logged_readably() {
        assert_eq!(log_text(b"status\r\n"), "status");
        assert_eq!(log_text(&[0x05, 0xff, 0x00]), "[3 字节] 05 ff 00");
        assert!(log_text(&[7u8; 40]).ends_with(" …"));
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
}
