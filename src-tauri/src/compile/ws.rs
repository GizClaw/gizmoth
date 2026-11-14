//! `gizmoth.compile.v1`: one build per WebSocket. The first `build` request
//! queues the job; status, queue position, exit and artifact events arrive as
//! JSON text frames and logs as binary frames. Closing the socket cancels the
//! build, including while it waits in the queue.
use super::{jobs, Service, PROTOCOL};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// A slow reader may stall log delivery briefly; a stuck one ends the build.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

pub fn commands() -> Vec<Value> {
    let build = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["workspace", "command"],
        "properties": {
            "workspace": { "type": "string", "description": "Absolute path of the project directory on the GizMoth computer; synchronized incrementally into the VM (local connections only)" },
            "command": { "type": "array", "minItems": 1, "maxItems": 256, "items": { "type": "string" }, "description": "argv run as the builder user in the workspace, e.g. [\"bazel\",\"build\",\"//app:firmware\"] or [\"bash\",\"-lc\",\"make -j8\"]" },
            "cwd": { "type": "string", "description": "Relative directory inside the workspace" },
            "env": { "type": "object", "additionalProperties": { "type": "string" } },
            "artifacts": { "type": "array", "maxItems": 64, "items": { "type": "string" }, "description": "Relative glob patterns (*, ?, **) collected after success, e.g. bazel-bin/**/*.update.tar.zlib" },
            "exclude": { "type": "array", "maxItems": 64, "items": { "type": "string" }, "description": "Workspace globs not synchronized (bazel-* links are always skipped)" },
            "timeoutSeconds": { "type": "integer", "minimum": 1, "maximum": 604800 },
            "collectOnFailure": { "type": "boolean", "default": false }
        }
    });
    vec![
        json!({ "name": "build", "description": "Queue this socket's build; send once", "inputSchema": build }),
        json!({ "name": "cancel", "description": "Cancel this socket's build (closing the socket also cancels it)", "inputSchema": { "type": "object", "additionalProperties": false, "properties": {} } }),
        json!({ "name": "status", "description": "Current status of this socket's build", "inputSchema": { "type": "object", "additionalProperties": false, "properties": {} } }),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: Value,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BuildParams {
    workspace: std::path::PathBuf,
    command: Vec<String>,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    artifacts: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    collect_on_failure: bool,
}

fn valid_id(id: &Value) -> bool {
    match id {
        Value::String(text) => {
            !text.is_empty() && text.chars().count() <= 128 && !text.chars().any(char::is_control)
        }
        Value::Number(number) => number.is_i64() || number.is_u64(),
        _ => false,
    }
}

fn reply(id: Value, outcome: Result<Value, (u16, String)>) -> Value {
    match outcome {
        Ok(result) => json!({ "type": "result", "id": id, "ok": true, "result": result }),
        Err((code, message)) => {
            json!({ "type": "result", "id": id, "ok": false, "error": { "code": code, "message": message } })
        }
    }
}

async fn send<S>(sink: &mut S, message: Message) -> bool
where
    S: Sink<Message> + Unpin,
{
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, sink.send(message)).await,
        Ok(Ok(()))
    )
}

/// Serves one compile socket. `local` callers may reference host paths.
pub async fn serve<S>(service: Arc<Service>, socket: S, client: String, title: String, local: bool)
where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message> + Unpin,
{
    let (mut sink, mut stream) = socket.split();
    let hello = json!({ "type": "hello", "protocol": PROTOCOL, "client": client, "title": title, "commands": commands() });
    if !send(&mut sink, Message::Text(hello.to_string().into())).await {
        return;
    }
    let mut job: Option<Arc<jobs::Job>> = None;
    let mut events: Option<broadcast::Receiver<jobs::Event>> = None;
    loop {
        tokio::select! {
            biased;
            event = async { events.as_mut().unwrap().recv().await }, if events.is_some() => {
                match event {
                    Ok(jobs::Event::Log(bytes)) => {
                        if !send(&mut sink, Message::Binary(bytes)).await { break; }
                    }
                    Ok(jobs::Event::Frame(frame)) => {
                        let finished = frame["type"] == "finished";
                        if !send(&mut sink, Message::Text(frame.to_string().into())).await { break; }
                        if finished {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        let frame = json!({ "type": "lagged", "missedEvents": missed, "hint": "Read the full log with compile_job" });
                        if !send(&mut sink, Message::Text(frame.to_string().into())).await { break; }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            message = stream.next() => {
                let text = match message {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Ping(data))) => {
                        if !send(&mut sink, Message::Pong(data)).await { break; }
                        continue;
                    }
                    Some(Ok(Message::Binary(_))) => {
                        let frame = reply(Value::Null, Err((400, "Compile requests must be JSON text frames".into())));
                        if !send(&mut sink, Message::Text(frame.to_string().into())).await { break; }
                        continue;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                let request: Request = match serde_json::from_str(&text) {
                    Ok(request) => request,
                    Err(error) => {
                        let frame = reply(Value::Null, Err((400, error.to_string())));
                        if !send(&mut sink, Message::Text(frame.to_string().into())).await { break; }
                        continue;
                    }
                };
                if !valid_id(&request.id) {
                    let frame = reply(request.id, Err((400, "id must be a nonempty string (at most 128 characters) or integer".into())));
                    if !send(&mut sink, Message::Text(frame.to_string().into())).await { break; }
                    continue;
                }
                let params = request.params.unwrap_or_else(|| json!({}));
                let outcome = match request.method.as_str() {
                    "build" if job.is_some() => Err((409, "This socket already has a build; open another socket for another build".into())),
                    "build" if !local => Err((403, "Only local connections can build host workspaces".into())),
                    "build" => match serde_json::from_value::<BuildParams>(params) {
                        Err(error) => Err((400, error.to_string())),
                        Ok(params) => {
                            let build = jobs::BuildRequest {
                                workspace: params.workspace,
                                command: params.command,
                                cwd: params.cwd,
                                env: params.env,
                                artifacts: params.artifacts,
                                exclude: params.exclude,
                                timeout_seconds: params.timeout_seconds,
                                collect_on_failure: params.collect_on_failure,
                            };
                            let (service, client, title) = (service.clone(), client.clone(), title.clone());
                            match tokio::task::spawn_blocking(move || service.submit(client, title, build)).await {
                                Ok(Ok((submitted, receiver))) => {
                                    let summary = submitted.summary();
                                    job = Some(submitted);
                                    events = Some(receiver);
                                    Ok(summary)
                                }
                                Ok(Err(error)) => Err((400, error)),
                                Err(_) => Err((500, "internal error".into())),
                            }
                        }
                    },
                    "cancel" => match &job {
                        Some(current) => {
                            service.cancel_job(current);
                            Ok(current.summary())
                        }
                        None => Err((409, "No build on this socket".into())),
                    },
                    "status" => job.as_ref().map(|j| j.summary()).ok_or((409, "No build on this socket".into())),
                    other => Err((400, format!("Unknown compile method {other}"))),
                };
                if !send(&mut sink, Message::Text(reply(request.id, outcome).to_string().into())).await { break; }
            }
        }
    }
    if let Some(job) = job {
        if !job.status().finished() {
            job.note("The WebSocket closed; cancelling");
            service.cancel_job(&job);
        }
    }
    // Completes the closing handshake, including the reply to a client Close.
    let _ = tokio::time::timeout(Duration::from_secs(2), sink.close()).await;
}
