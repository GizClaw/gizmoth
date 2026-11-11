//! The gRPC side of the Gateway: gizmoth.gateway.v1 served from the daemon's
//! own listeners. Sessions reuse the WebSocket pipe's queue, grants, leases and
//! job fencing, so both transports share one arbitration per port.
use super::*;
use crate::serial::grpc::{self as contract, pb};
use hyper::body::{Frame, SizeHint};
use sha2::{Digest, Sha256};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::AsyncWriteExt;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Status, Streaming};

const MAX_PACKAGE: u64 = 256 * 1024 * 1024;
const MAX_UPLOADS: usize = 4;
const UPLOAD_TTL_MS: u64 = 30 * 60 * 1000;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(15);
/// Package chunks are expected to be well below this decoded message limit.
const MAX_MESSAGE: usize = 2 * 1024 * 1024;

/// The connection identity, attached by `serve_connection`, never by a caller.
#[derive(Clone, Copy)]
pub(in crate::serial) struct PeerInfo {
    pub peer: SocketAddr,
    /// True for a LAN listener, regardless of the peer's source address.
    pub lan: bool,
}

pub(in crate::serial) struct Upload {
    id: String,
    directory: PathBuf,
    path: PathBuf,
    expires_at: u64,
}

pub(super) fn is_grpc<B>(request: &Request<B>) -> bool {
    request
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/grpc"))
}

pub(super) type Server = pb::gateway_server::GatewayServer<NodeService>;

pub(super) fn server(daemon: Arc<Daemon>) -> Server {
    pb::gateway_server::GatewayServer::new(NodeService { daemon })
        .max_decoding_message_size(MAX_MESSAGE)
        .max_encoding_message_size(16 * 1024 * 1024)
}

pub(super) async fn call(
    mut server: Server,
    request: Request<hyper::body::Incoming>,
) -> Response<tonic::body::Body> {
    use tonic::codegen::Service;
    match server.call(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// Counts an open gRPC response until its body is dropped, so the connection
/// is not closed as idle while a stream is quiet.
pub(super) fn tracked(body: tonic::body::Body, open: &Arc<AtomicU64>) -> tonic::body::Body {
    open.fetch_add(1, Ordering::SeqCst);
    tonic::body::Body::new(Tracked {
        inner: body,
        open: open.clone(),
    })
}

struct Tracked {
    inner: tonic::body::Body,
    open: Arc<AtomicU64>,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }
}

impl hyper::body::Body for Tracked {
    type Data = Bytes;
    type Error = Status;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Status>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

pub(super) struct NodeService {
    daemon: Arc<Daemon>,
}

fn sanitize(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}

fn control_output(control: &Control) -> pb::SessionOutput {
    use pb::session_output::{Ended, Granted, Kind, Queued};
    pb::SessionOutput {
        kind: Some(match control {
            Control::Queued(position, holder) => Kind::Queued(Queued {
                position: *position as u32,
                holder: holder.clone().unwrap_or_default(),
            }),
            Control::Granted => Kind::Granted(Granted {}),
            Control::End(kind, reason) => Kind::Ended(Ended {
                kind: kind.to_string(),
                reason: reason.clone(),
            }),
        }),
    }
}

impl Daemon {
    fn node_info(&self) -> pb::Node {
        pb::Node {
            node_id: self.node.id.clone(),
            boot_id: self.instance_id.clone(),
            name: self.node.name.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            api_version: API_VERSION,
            capabilities: contract::CAPABILITIES
                .iter()
                .map(|c| c.to_string())
                .collect(),
        }
    }

    /// Only this node's own ports: a cascade exports one level.
    fn topology(&self) -> pb::Topology {
        let ports = self.ports.lock().unwrap();
        pb::Topology {
            node: Some(self.node_info()),
            revision: *self.version.borrow(),
            ports: ports
                .iter()
                .map(|port| contract::port_to_pb(&self.view(port), &self.node.id))
                .collect(),
        }
    }

    fn upload_path(&self, package_id: &str) -> Option<PathBuf> {
        let now = now_ms();
        self.uploads
            .lock()
            .unwrap()
            .iter()
            .find(|upload| upload.id == package_id && upload.expires_at > now)
            .map(|upload| upload.path.clone())
    }

    fn register_upload(&self, upload: Upload) {
        let now = now_ms();
        let mut uploads = self.uploads.lock().unwrap();
        uploads.push(upload);
        let mut index = 0;
        while index < uploads.len() {
            if uploads[index].expires_at <= now || uploads.len() > MAX_UPLOADS {
                let _ = fs::remove_dir_all(&uploads.remove(index).directory);
            } else {
                index += 1;
            }
        }
    }

    /// Installs an uploaded package. The path is service-generated, so the
    /// local-file restriction on remote callers does not apply to it.
    fn session_install(
        self: &Arc<Self>,
        id: &str,
        lease: u64,
        caller: Caller,
        install: pb::session_input::Install,
    ) -> Reply {
        self.with_port(id, |port| {
            port.shared.seat.lock().unwrap().authorize_command(
                Some(lease),
                caller,
                &port.shared.authority,
            )
        })?;
        let path = self.upload_path(&install.package_id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "固件包不存在或已过期，请重新上传".to_string(),
            )
        })?;
        self.start_job(
            id,
            StartJob {
                kind: if install.stage_only {
                    JobKind::Stage
                } else {
                    JobKind::Install
                },
                path: Some(path.to_string_lossy().into_owned()),
                source: Some("ws".into()),
                job_id: (!install.job_id.is_empty()).then_some(install.job_id),
                lease: Some(lease),
            },
            caller,
        )
    }

    fn session_command(
        self: &Arc<Self>,
        id: &str,
        lease: u64,
        caller: Caller,
        request_id: &str,
        kind: pb::session_input::Kind,
    ) -> Reply {
        use pb::session_input::Kind;
        let (method, params) = match kind {
            Kind::Open(_) => return Err(bad("会话已打开")),
            Kind::Install(install) => return self.session_install(id, lease, caller, install),
            Kind::Send(send) => (
                "send",
                json!({
                    "text": send.text,
                    "lineEnding": match pb::LineEnding::try_from(send.line_ending) {
                        Ok(pb::LineEnding::Crlf) => "crlf",
                        Ok(pb::LineEnding::Cr) => "cr",
                        Ok(pb::LineEnding::None) => "none",
                        _ => "lf",
                    },
                }),
            ),
            Kind::Reboot(reboot) => {
                let target = match pb::RebootTarget::try_from(reboot.target) {
                    Ok(pb::RebootTarget::App) => "app",
                    Ok(pb::RebootTarget::Loader) => "loader",
                    _ => return Err(bad("请指定重启目标")),
                };
                let mut params = json!({ "target": target });
                if !reboot.job_id.is_empty() {
                    params["jobId"] = json!(reboot.job_id);
                }
                ("reboot", params)
            }
            Kind::Refresh(refresh) => {
                let mut params = json!({});
                if !refresh.job_id.is_empty() {
                    params["jobId"] = json!(refresh.job_id);
                }
                ("refresh", params)
            }
            Kind::Cancel(cancel) => ("cancel", json!({ "jobId": cancel.job_id })),
        };
        let frame = json!({ "id": request_id, "method": method, "params": params });
        let request = ws::parse(&frame.to_string()).map_err(|(_, message)| bad(message))?;
        self.ws_command(id, lease, caller, &request)
    }
}

fn command_result(
    request_id: String,
    port_id: &str,
    outcome: Reply,
) -> pb::session_output::CommandResult {
    match outcome {
        Ok((_, value)) => pb::session_output::CommandResult {
            request_id,
            ok: true,
            code: 0,
            message: String::new(),
            job: (value.get("kind").is_some() && value.get("id").is_some())
                .then(|| contract::job_to_pb(&value, port_id))
                .flatten(),
        },
        Err(error) => {
            let status = contract::status(error);
            pb::session_output::CommandResult {
                request_id,
                ok: false,
                code: status.code() as i32,
                message: status.message().to_string(),
                job: None,
            }
        }
    }
}

async fn deliver(
    tx: &tokio::sync::mpsc::Sender<Result<pb::SessionOutput, Status>>,
    output: pb::SessionOutput,
) -> bool {
    matches!(
        tokio::time::timeout(SOCKET_SEND_TIMEOUT, tx.send(Ok(output))).await,
        Ok(Ok(()))
    )
}

async fn run_session(
    daemon: Arc<Daemon>,
    id: String,
    caller: Caller,
    joined: Joined,
    mut input: Streaming<pb::SessionInput>,
    tx: tokio::sync::mpsc::Sender<Result<pb::SessionOutput, Status>>,
) {
    use pb::session_output::Kind;
    let Joined {
        pipe_id,
        shared,
        mut device,
        mut notices,
        permit,
    } = joined;
    let _permit = permit;
    let mut updates = daemon.version.subscribe();
    let mut requests = HashSet::new();
    let mut completed_jobs = HashSet::new();
    let mut current_job = Value::Null;
    let initial = notices.borrow_and_update().clone();
    if !deliver(&tx, control_output(&initial)).await || matches!(initial, Control::End(..)) {
        daemon.leave_pipe(&id, &shared, pipe_id, "gRPC 会话已断开");
        return;
    }
    loop {
        tokio::select! {
            biased;
            changed = notices.changed() => {
                if changed.is_err() { break; }
                let notice = notices.borrow_and_update().clone();
                let end = matches!(notice, Control::End(..));
                if !deliver(&tx, control_output(&notice)).await || end { break; }
            }
            changed = updates.changed() => {
                if changed.is_err() { break; }
                let mut failed = false;
                for event in shared.job_events(pipe_id, &mut completed_jobs, &mut current_job) {
                    let Some(job) = contract::job_to_pb(&event["job"], &id) else { continue };
                    if !deliver(&tx, pb::SessionOutput { kind: Some(Kind::Job(job)) }).await {
                        failed = true;
                        break;
                    }
                }
                if failed { break; }
            }
            message = input.message() => {
                let message = match message {
                    Ok(Some(message)) => message,
                    Ok(None) | Err(_) => break,
                };
                let request_id = message.request_id.clone();
                let pending = {
                    let seat = shared.seat.lock().unwrap();
                    shared.authority.allows(caller)
                        && seat.queue.iter().any(|p| p.id == pipe_id && p.caller == caller)
                };
                let outcome = (|| -> Reply {
                    let kind = message.kind.ok_or_else(|| bad("缺少命令"))?;
                    if request_id.is_empty()
                        || request_id.chars().count() > 128
                        || request_id.chars().any(char::is_control)
                    {
                        return Err(bad("request_id must be nonempty, at most 128 characters"));
                    }
                    if pending {
                        return Err((StatusCode::CONFLICT, "尚未获得独占通信权，请等待 granted".into()));
                    }
                    if requests.len() >= ws::MAX_COMMANDS {
                        return Err((StatusCode::TOO_MANY_REQUESTS, "会话命令数量已达上限，请完成任务后重新连接".into()));
                    }
                    if !requests.insert(request_id.clone()) {
                        return Err((StatusCode::CONFLICT, "重复的命令 ID；请查询结果，不要重复下发".into()));
                    }
                    daemon.session_command(&id, pipe_id, caller, &request_id, kind)
                })();
                let result = command_result(request_id, &id, outcome);
                if !deliver(&tx, pb::SessionOutput { kind: Some(Kind::Result(result)) }).await { break; }
            }
            chunk = device.recv() => match chunk {
                Some(Chunk { bytes, _permit: permit }) => {
                    let sent = deliver(&tx, pb::SessionOutput { kind: Some(Kind::DeviceData(bytes)) }).await;
                    // Keep the in-flight bytes charged until the send completes.
                    drop(permit);
                    if !sent { break; }
                }
                None => break,
            },
        }
    }
    daemon.leave_pipe(&id, &shared, pipe_id, "gRPC 会话已断开");
}

impl NodeService {
    fn caller<T>(&self, request: &tonic::Request<T>) -> Result<Caller, Status> {
        let info = request
            .extensions()
            .get::<PeerInfo>()
            .copied()
            .ok_or_else(|| Status::internal("missing connection identity"))?;
        let token = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default();
        let caller = self
            .daemon
            .authorize(token, info.peer, info.lan)
            .ok_or_else(|| Status::unauthenticated("unauthorized"))?;
        drop(self.daemon.admitted(caller).map_err(contract::status)?);
        self.daemon.last_request.store(now_ms(), Ordering::SeqCst);
        Ok(caller)
    }
}

#[tonic::async_trait]
impl pb::gateway_server::Gateway for NodeService {
    async fn get_node(
        &self,
        request: tonic::Request<pb::GetNodeRequest>,
    ) -> Result<tonic::Response<pb::Node>, Status> {
        self.caller(&request)?;
        Ok(tonic::Response::new(self.daemon.node_info()))
    }

    type WatchTopologyStream = ReceiverStream<Result<pb::Topology, Status>>;

    async fn watch_topology(
        &self,
        request: tonic::Request<pb::WatchTopologyRequest>,
    ) -> Result<tonic::Response<Self::WatchTopologyStream>, Status> {
        let caller = self.caller(&request)?;
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let daemon = self.daemon.clone();
        tokio::spawn(async move {
            let mut updates = daemon.version.subscribe();
            let mut stopped = daemon.shutdown.subscribe();
            loop {
                if !daemon.authority.allows(caller) {
                    let _ = tx
                        .send(Err(Status::unauthenticated("Gateway 授权已更改")))
                        .await;
                    break;
                }
                if tx.send(Ok(daemon.topology())).await.is_err() {
                    break;
                }
                tokio::select! {
                    changed = updates.changed() => if changed.is_err() { break },
                    _ = stopped.wait_for(|stop| *stop) => break,
                    _ = tx.closed() => break,
                    // Recheck authorization even while nothing changes.
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                }
            }
        });
        Ok(tonic::Response::new(ReceiverStream::new(rx)))
    }

    async fn read_logs(
        &self,
        request: tonic::Request<pb::ReadLogsRequest>,
    ) -> Result<tonic::Response<pb::LogPage>, Status> {
        self.caller(&request)?;
        let request = request.into_inner();
        let limit = if request.limit == 0 {
            500
        } else {
            u64::from(request.limit)
        };
        let (_, page) = self
            .daemon
            .log_page(&request.port_id, request.after, limit)
            .map_err(contract::status)?;
        Ok(tonic::Response::new(contract::log_page_to_pb(
            &page,
            &self.daemon.instance_id,
        )))
    }

    type SessionStream = ReceiverStream<Result<pb::SessionOutput, Status>>;

    async fn session(
        &self,
        request: tonic::Request<Streaming<pb::SessionInput>>,
    ) -> Result<tonic::Response<Self::SessionStream>, Status> {
        let caller = self.caller(&request)?;
        let mut input = request.into_inner();
        let first = tokio::time::timeout(BODY_TIMEOUT, input.message())
            .await
            .map_err(|_| Status::deadline_exceeded("等待 open 超时"))??
            .ok_or_else(|| Status::invalid_argument("会话需要先发送 open"))?;
        let Some(pb::session_input::Kind::Open(open)) = first.kind else {
            return Err(Status::invalid_argument("第一条消息必须是 open"));
        };
        let client = sanitize(&open.client, 64);
        let reason = sanitize(&open.reason, 200);
        if client.is_empty() || reason.is_empty() {
            return Err(Status::invalid_argument("open 需要客户端名称和占据原因"));
        }
        let joined = self
            .daemon
            .join_pipe(&open.port_id, client, reason, caller, open.device_data)
            .map_err(contract::status)?;
        let (tx, rx) = tokio::sync::mpsc::channel(QUEUE_MESSAGES);
        tokio::spawn(run_session(
            self.daemon.clone(),
            open.port_id,
            caller,
            joined,
            input,
            tx,
        ));
        Ok(tonic::Response::new(ReceiverStream::new(rx)))
    }

    async fn upload_package(
        &self,
        request: tonic::Request<Streaming<pb::PackageChunk>>,
    ) -> Result<tonic::Response<pb::PackageRef>, Status> {
        use pb::package_chunk::Kind;
        self.caller(&request)?;
        let mut input = request.into_inner();
        let header = match tokio::time::timeout(CHUNK_TIMEOUT, input.message())
            .await
            .map_err(|_| Status::deadline_exceeded("等待固件包超时"))??
        {
            Some(pb::PackageChunk {
                kind: Some(Kind::Header(header)),
            }) => header,
            _ => return Err(Status::invalid_argument("第一块必须是 header")),
        };
        if header.size == 0 || header.size > MAX_PACKAGE {
            return Err(Status::invalid_argument("固件包大小无效"));
        }
        let expected = header.sha256.to_ascii_lowercase();
        if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Status::invalid_argument("sha256 必须是 64 位十六进制"));
        }
        let name = sanitize(&header.name, 120).replace(['/', '\\'], "_");
        let name = match name.trim_matches('.') {
            "" => "package.update.tar.zlib".to_string(),
            name => name.to_string(),
        };
        let package_id = uuid::Uuid::new_v4().to_string();
        let directory = self
            .daemon
            .dir
            .join("packages")
            .join("uploads")
            .join(&package_id);
        let path = directory.join(&name);
        let received = async {
            fs::create_dir_all(&directory).map_err(|e| Status::internal(e.to_string()))?;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = tokio::fs::File::from_std(
                options
                    .open(&path)
                    .map_err(|e| Status::internal(e.to_string()))?,
            );
            let mut hasher = Sha256::new();
            let mut size = 0u64;
            loop {
                let chunk = tokio::time::timeout(CHUNK_TIMEOUT, input.message())
                    .await
                    .map_err(|_| Status::deadline_exceeded("固件包传输超时"))??;
                match chunk {
                    Some(pb::PackageChunk {
                        kind: Some(Kind::Data(data)),
                    }) => {
                        size += data.len() as u64;
                        if size > header.size {
                            return Err(Status::invalid_argument("固件包超过声明的大小"));
                        }
                        hasher.update(&data);
                        file.write_all(&data)
                            .await
                            .map_err(|e| Status::internal(e.to_string()))?;
                    }
                    Some(_) => return Err(Status::invalid_argument("header 只能出现一次")),
                    None => break,
                }
            }
            file.sync_all()
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
            if size != header.size {
                return Err(Status::invalid_argument("固件包不完整"));
            }
            let digest = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            if digest != expected {
                return Err(Status::invalid_argument("固件包校验失败"));
            }
            let inspect_path = path.to_string_lossy().into_owned();
            let mut info = tokio::task::spawn_blocking(move || h2loader::inspect(&inspect_path))
                .await
                .map_err(|_| Status::internal("内部错误"))?
                .map_err(|e| {
                    Status::invalid_argument(format!("固件包无效：{}", h2loader::describe(&e)))
                })?;
            info["name"] = json!(name);
            Ok(info)
        }
        .await;
        let info = match received {
            Ok(info) => info,
            Err(error) => {
                let _ = fs::remove_dir_all(&directory);
                return Err(error);
            }
        };
        let expires_at = now_ms() + UPLOAD_TTL_MS;
        self.daemon.register_upload(Upload {
            id: package_id.clone(),
            directory,
            path,
            expires_at,
        });
        Ok(tonic::Response::new(pb::PackageRef {
            package_id,
            info: contract::package_to_pb(&info),
            expires_at,
        }))
    }

    async fn get_job(
        &self,
        request: tonic::Request<pb::GetJobRequest>,
    ) -> Result<tonic::Response<pb::Job>, Status> {
        self.caller(&request)?;
        let job_id = request.into_inner().job_id;
        let ports = self.daemon.ports.lock().unwrap();
        for port in ports.iter() {
            let live = port.shared.live.lock().unwrap();
            if let Some(job) = live
                .job
                .iter()
                .chain(live.history.iter())
                .find(|job| job.id == job_id)
            {
                let value = serde_json::to_value(job).unwrap_or_default();
                if let Some(job) = contract::job_to_pb(&value, &port.config.id) {
                    return Ok(tonic::Response::new(job));
                }
            }
        }
        Err(Status::not_found("任务不存在"))
    }
}
