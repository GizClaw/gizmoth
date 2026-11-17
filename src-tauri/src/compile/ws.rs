//! `gizmoth.compile.v1`: one build per WebSocket. The first `build` request
//! queues the job; status, queue position, exit and artifact events arrive as
//! JSON text frames and logs as binary frames. Closing the socket cancels the
//! build, including while it waits in the queue.
use super::{jobs, Service, PROTOCOL};
use crate::serial::daemon::CompileAccess;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// A slow reader may stall log delivery briefly; a stuck one ends the build.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

pub fn commands() -> Vec<Value> {
    let spec = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["workspace", "command"],
        "properties": {
            "workspace": { "type": "string", "description": "Absolute path of the project directory on the GizMoth computer; synchronized incrementally into the VM (local connections only)" },
            "command": { "type": "array", "minItems": 1, "maxItems": 256, "items": { "type": "string" }, "description": "argv run as the builder user in the workspace, e.g. [\"bazel\",\"build\",\"//app:firmware\"] or [\"bash\",\"-lc\",\"make -j8\"]" },
            "cwd": { "type": "string", "default": "", "description": "Relative directory inside the workspace" },
            "env": { "type": "object", "default": {}, "additionalProperties": { "type": "string" } },
            "artifacts": { "type": "array", "default": [], "maxItems": 64, "items": { "type": "string" }, "description": "Relative glob patterns (*, ?, **) collected after success, e.g. bazel-bin/**/*.update.tar.zlib" },
            "exclude": { "type": "array", "default": [], "maxItems": 64, "items": { "type": "string" }, "description": "Workspace globs not synchronized (bazel-* links are always skipped)" },
            "timeoutSeconds": { "type": ["integer", "null"], "default": null, "minimum": 1, "maximum": 604800 },
            "collectOnFailure": { "type": "boolean", "default": false }
        }
    });
    let manifest = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["apiVersion", "kind", "metadata", "spec"],
        "properties": {
            "apiVersion": { "type": "string", "const": jobs::CONFIG_API_VERSION },
            "kind": { "type": "string", "const": jobs::CONFIG_KIND },
            "metadata": {
                "type": "object", "additionalProperties": false, "required": ["name"],
                "properties": {
                    "name": { "type": "string", "minLength": 1, "maxLength": 120, "description": "Build title; trimmed, nonempty, with no control characters" }
                }
            },
            "spec": spec.clone()
        }
    });
    vec![
        json!({ "name": "build", "description": "Queue this socket's build; send a versioned CompileTask directly as params (legacy flat params also accepted); send once", "inputSchema": { "oneOf": [manifest, spec] } }),
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
struct BuildManifest {
    api_version: String,
    kind: String,
    metadata: BuildMetadata,
    spec: jobs::BuildRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildMetadata {
    name: String,
}

fn parse_build(params: Value, legacy_title: &str) -> Result<(String, jobs::BuildRequest), String> {
    // Any manifest field commits to that format. Strict deserialization at
    // every level rejects mixed flat/manifest submissions instead of ignoring
    // a setting or silently falling back to the legacy form.
    let (title, request) = if ["apiVersion", "kind", "metadata", "spec"]
        .iter()
        .any(|key| params.get(key).is_some())
    {
        let manifest: BuildManifest =
            serde_json::from_value(params).map_err(|error| error.to_string())?;
        if manifest.api_version != jobs::CONFIG_API_VERSION {
            return Err(format!("Unsupported apiVersion {}", manifest.api_version));
        }
        if manifest.kind != jobs::CONFIG_KIND {
            return Err(format!("Unsupported kind {}", manifest.kind));
        }
        let name = manifest.metadata.name;
        let title = name.trim();
        if title.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
            return Err("metadata.name must be nonempty, at most 120 characters, with no control characters".into());
        }
        (title.to_string(), manifest.spec)
    } else {
        let request = serde_json::from_value(params).map_err(|error| error.to_string())?;
        (legacy_title.to_string(), request)
    };
    super::validate_request(&request)?;
    Ok((title, request))
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

/// Serves one compile socket, retaining authorization through submission.
pub(crate) async fn serve<S>(
    service: Arc<Service>,
    socket: S,
    client: String,
    title: String,
    access: CompileAccess,
) where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message> + Unpin,
{
    let (mut sink, mut stream) = socket.split();
    if !access.allowed() {
        let _ = tokio::time::timeout(Duration::from_secs(2), sink.close()).await;
        return;
    }
    let hello = json!({ "type": "hello", "protocol": PROTOCOL, "client": client, "title": title, "commands": commands() });
    if !send(&mut sink, Message::Text(hello.to_string().into())).await {
        return;
    }
    let mut job: Option<Arc<jobs::Job>> = None;
    let mut events: Option<broadcast::Receiver<jobs::Event>> = None;
    let mut authorization_tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        if !access.allowed() {
            break;
        }
        tokio::select! {
            biased;
            _ = authorization_tick.tick() => {}
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
                    "build" if !access.local() => Err((403, "Only local connections can build host workspaces".into())),
                    "build" => match parse_build(params, &title) {
                        Err(error) => Err((400, error)),
                        Ok((build_title, build)) => {
                            let (service, client, title, access) = (service.clone(), client.clone(), build_title, access.clone());
                            match tokio::task::spawn_blocking(move || access.with_admission(|| service.submit(client, title, build))).await {
                                Ok(Ok((submitted, receiver))) => {
                                    let summary = submitted.summary();
                                    job = Some(submitted);
                                    events = Some(receiver);
                                    Ok(summary)
                                }
                                Ok(Err(error)) => Err(error),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(spec: Value) -> Value {
        json!({
            "apiVersion": "gizmoth.dev/v1", "kind": "CompileTask",
            "metadata": { "name": "Build firmware" }, "spec": spec
        })
    }

    fn minimal_spec() -> Value {
        json!({ "workspace": "/tmp", "command": ["bazel", "build", "//..."] })
    }

    #[test]
    fn build_parser_rejects_unsupported_versions_kinds_and_unknown_fields() {
        let valid = manifest(minimal_spec());
        let mut invalid = Vec::new();
        for (field, value) in [
            ("apiVersion", json!("gizmoth.dev/v2")),
            ("kind", json!("OtherTask")),
            ("apiVersion", Value::Null),
            ("kind", json!(12)),
            ("metadata", Value::Null),
            ("spec", json!([])),
            ("client", json!("spoofed-client")),
            ("unknown", json!(true)),
        ] {
            let mut bad = valid.clone();
            bad[field] = value;
            invalid.push(bad);
        }
        for field in ["apiVersion", "kind", "metadata", "spec"] {
            let mut bad = valid.clone();
            bad.as_object_mut().unwrap().remove(field);
            invalid.push(bad);
        }
        let mut bad = valid.clone();
        bad["metadata"]["unknown"] = json!(true);
        invalid.push(bad);
        let mut bad = valid;
        bad["spec"]["unknown"] = json!(true);
        invalid.push(bad);
        let mut bad = minimal_spec();
        bad["unknown"] = json!(true);
        invalid.push(bad);
        for bad in invalid {
            assert!(parse_build(bad.clone(), "Legacy title").is_err(), "{bad}");
        }
    }

    #[test]
    fn build_parser_rejects_mixed_shapes_and_requires_workspace_and_command() {
        let valid = manifest(minimal_spec());
        for (field, value) in minimal_spec().as_object().unwrap() {
            let mut mixed = valid.clone();
            mixed[field] = value.clone();
            assert!(parse_build(mixed, "Legacy title").is_err());
        }
        for field in ["apiVersion", "kind", "metadata", "spec"] {
            let mut mixed = minimal_spec();
            mixed[field] = valid[field].clone();
            assert!(parse_build(mixed, "Legacy title").is_err());
        }
        for field in ["workspace", "command"] {
            let mut spec = minimal_spec();
            spec.as_object_mut().unwrap().remove(field);
            assert!(parse_build(spec.clone(), "Legacy title").is_err());
            assert!(parse_build(manifest(spec), "Legacy title").is_err());
        }
        assert!(parse_build(json!({ "config": valid }), "Legacy title").is_err());
    }

    #[test]
    fn build_parser_validates_and_trims_metadata_name() {
        for name in [
            "",
            "  ",
            "Build\nfirmware",
            "Build\u{7f}",
            "\tBuild",
            &"x".repeat(121),
        ] {
            let mut bad = manifest(minimal_spec());
            bad["metadata"]["name"] = json!(name);
            assert!(parse_build(bad, "Legacy title").is_err(), "{name:?}");
        }
        for name in [Value::Null, json!(123)] {
            let mut bad = manifest(minimal_spec());
            bad["metadata"]["name"] = name;
            assert!(parse_build(bad, "Legacy title").is_err());
        }
        let mut missing = manifest(minimal_spec());
        missing["metadata"] = json!({});
        assert!(parse_build(missing, "Legacy title").is_err());
        let mut valid = manifest(minimal_spec());
        valid["metadata"]["name"] = json!("  Build firmware  ");
        assert_eq!(
            parse_build(valid, "Legacy title").unwrap().0,
            "Build firmware"
        );
        let mut valid = manifest(minimal_spec());
        valid["metadata"]["name"] = json!("构".repeat(120));
        assert_eq!(
            parse_build(valid, "Legacy title")
                .unwrap()
                .0
                .chars()
                .count(),
            120
        );
    }

    #[test]
    fn build_parser_validates_all_settings_for_both_formats_before_submission() {
        for (field, value) in [
            ("workspace", json!("relative")),
            ("command", json!([])),
            ("command", json!(["bad\u{0}argument"])),
            ("cwd", json!("../escape")),
            ("cwd", Value::Null),
            ("env", json!({ "BAD-NAME": "value" })),
            ("env", json!({ "CC": 1 })),
            ("artifacts", json!(["/etc/passwd"])),
            ("exclude", json!(["../escape"])),
            ("timeoutSeconds", json!(0)),
            ("timeoutSeconds", json!(604801)),
            ("timeoutSeconds", json!(-1)),
            ("timeoutSeconds", json!(1.5)),
            ("collectOnFailure", json!("yes")),
        ] {
            let mut spec = minimal_spec();
            spec[field] = value;
            assert!(parse_build(spec.clone(), "Legacy title").is_err(), "{spec}");
            assert!(
                parse_build(manifest(spec.clone()), "Legacy title").is_err(),
                "{spec}"
            );
        }
    }

    #[test]
    fn legacy_and_manifest_submissions_have_equivalent_normalized_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let spec = json!({ "workspace": directory.path().join("."), "command": ["bazel", "build", "//..."] });
        let (legacy_title, legacy) = parse_build(spec.clone(), "Build firmware").unwrap();
        let (manifest_title, versioned) = parse_build(manifest(spec), "Connection title").unwrap();
        let (legacy, _) = service
            .submit("connection-client".into(), legacy_title, legacy)
            .unwrap();
        let (versioned, _) = service
            .submit("connection-client".into(), manifest_title, versioned)
            .unwrap();
        assert_eq!(legacy.client, "connection-client");
        assert_eq!(versioned.client, "connection-client");
        assert_eq!(legacy.configuration(), versioned.configuration());
        assert_eq!(
            versioned.configuration(),
            manifest(json!({
                "workspace": directory.path().canonicalize().unwrap(),
                "command": ["bazel", "build", "//..."], "cwd": "", "env": {},
                "artifacts": [], "exclude": [], "timeoutSeconds": null, "collectOnFailure": false
            }))
        );
    }

    #[test]
    fn manifest_submission_preserves_every_setting_and_uses_its_title() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let spec = json!({
            "workspace": directory.path(), "command": ["bash", "-lc", "make -j8"],
            "cwd": "firmware", "env": { "CC": "clang", "FLAGS": "-O2 'a b'\nsecond line", "EMPTY": "" },
            "artifacts": ["out/**/*.bin", "logs/*.txt"], "exclude": ["cache/**", "*.tmp"],
            "timeoutSeconds": 600, "collectOnFailure": true
        });
        let (title, request) = parse_build(manifest(spec.clone()), "Connection title").unwrap();
        let (job, _) = service
            .submit("connection-client".into(), title, request)
            .unwrap();
        let mut expected = manifest(spec);
        expected["spec"]["workspace"] = json!(directory.path().canonicalize().unwrap());
        assert_eq!(job.summary()["title"], "Build firmware");
        assert_eq!(job.configuration(), expected);
        let (title, request) = parse_build(expected["spec"].clone(), "Build firmware").unwrap();
        let (legacy, _) = service
            .submit("connection-client".into(), title, request)
            .unwrap();
        assert_eq!(legacy.configuration(), expected);
        let (title, request) =
            parse_build(job.configuration(), "Another connection title").unwrap();
        assert_eq!(title, job.title);
        assert_eq!(serde_json::to_value(request).unwrap(), expected["spec"]);
    }

    #[test]
    fn build_discovery_advertises_strict_manifest_and_legacy_alternatives() {
        let commands = commands();
        let schema = &commands[0]["inputSchema"];
        let alternatives = schema["oneOf"].as_array().unwrap();
        assert_eq!(alternatives.len(), 2);
        let manifest = &alternatives[0];
        let legacy = &alternatives[1];
        assert_eq!(
            manifest["required"],
            json!(["apiVersion", "kind", "metadata", "spec"])
        );
        assert_eq!(
            manifest["properties"]["apiVersion"]["const"],
            "gizmoth.dev/v1"
        );
        assert_eq!(manifest["properties"]["kind"]["const"], "CompileTask");
        assert_eq!(
            manifest["properties"]["metadata"]["required"],
            json!(["name"])
        );
        assert_eq!(
            manifest["properties"]["metadata"]["additionalProperties"],
            false
        );
        assert_eq!(manifest["properties"]["spec"], *legacy);
        assert_eq!(legacy["required"], json!(["workspace", "command"]));
        assert_eq!(legacy["additionalProperties"], false);
        assert_eq!(manifest["additionalProperties"], false);
        assert_eq!(
            legacy["properties"]["timeoutSeconds"]["type"],
            json!(["integer", "null"])
        );
    }
}
