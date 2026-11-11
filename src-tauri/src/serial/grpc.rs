//! gizmoth.gateway.v1: the node-to-node gRPC contract
//! (`proto/gizmoth/gateway/v1/gateway.proto`).
//!
//! The daemon describes ports as JSON views for its HTTP API and interface.
//! These conversions map that view onto the typed contract and back, so a
//! remote port reaches the interface in exactly the local view's shape.
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fs, io, path::Path};

pub mod pb {
    #![allow(clippy::all, clippy::pedantic)]
    tonic::include_proto!("gizmoth.gateway.v1");
}

/// Capabilities advertised by `GetNode`.
pub const CAPABILITIES: &[&str] = &["topology", "logs", "session", "upload", "jobs"];

/// A persistent node identity. The name is the computer's host name.
#[derive(Clone, Debug)]
pub struct NodeIdentity {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedNode {
    node_id: String,
}

/// `<state>/node.json`; created once and kept across restarts and updates.
pub fn load_node(dir: &Path, write: impl Fn(&Path, &[u8]) -> io::Result<()>) -> NodeIdentity {
    let path = dir.join("node.json");
    let saved = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SavedNode>(&bytes).ok())
        .filter(|saved| uuid::Uuid::parse_str(&saved.node_id).is_ok());
    let id = match saved {
        Some(saved) => saved.node_id,
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            let _ = write(
                &path,
                &serde_json::to_vec_pretty(&SavedNode {
                    node_id: id.clone(),
                })
                .unwrap_or_default(),
            );
            id
        }
    };
    NodeIdentity {
        id,
        name: host_name(),
    }
}

pub fn host_name() -> String {
    #[cfg(unix)]
    let name = {
        let mut buffer = [0u8; 256];
        // SAFETY: the buffer is valid for its length; the result is checked.
        let rc =
            unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len()) };
        if rc == 0 {
            let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
            String::from_utf8_lossy(&buffer[..end]).into_owned()
        } else {
            String::new()
        }
    };
    #[cfg(not(unix))]
    let name = std::env::var("COMPUTERNAME").unwrap_or_default();
    let name = name.trim().trim_end_matches(".local").trim();
    let name: String = name.chars().filter(|c| !c.is_control()).take(80).collect();
    if name.is_empty() {
        "GizMoth".into()
    } else {
        name
    }
}

/// Maps the daemon's HTTP errors onto gRPC status codes.
pub fn status((code, message): (StatusCode, String)) -> tonic::Status {
    let code = match code {
        StatusCode::BAD_REQUEST => tonic::Code::InvalidArgument,
        StatusCode::UNAUTHORIZED => tonic::Code::Unauthenticated,
        StatusCode::FORBIDDEN => tonic::Code::PermissionDenied,
        StatusCode::NOT_FOUND => tonic::Code::NotFound,
        StatusCode::CONFLICT => tonic::Code::FailedPrecondition,
        StatusCode::TOO_MANY_REQUESTS => tonic::Code::ResourceExhausted,
        StatusCode::REQUEST_TIMEOUT => tonic::Code::DeadlineExceeded,
        StatusCode::SERVICE_UNAVAILABLE => tonic::Code::Unavailable,
        _ => tonic::Code::Internal,
    };
    tonic::Status::new(code, message)
}

/// The reverse mapping, for remote failures surfaced through the HTTP API.
pub fn http_error(status: &tonic::Status) -> (StatusCode, String) {
    let code = match status.code() {
        tonic::Code::InvalidArgument | tonic::Code::OutOfRange => StatusCode::BAD_REQUEST,
        tonic::Code::Unauthenticated => StatusCode::UNAUTHORIZED,
        tonic::Code::PermissionDenied => StatusCode::FORBIDDEN,
        tonic::Code::NotFound => StatusCode::NOT_FOUND,
        tonic::Code::FailedPrecondition | tonic::Code::AlreadyExists | tonic::Code::Aborted => {
            StatusCode::CONFLICT
        }
        tonic::Code::ResourceExhausted => StatusCode::TOO_MANY_REQUESTS,
        tonic::Code::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
        tonic::Code::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    };
    (code, status.message().to_string())
}

// ------------------------------------------------------------ enum tables

const PROTOCOLS: &[(&str, pb::Protocol)] = &[
    ("auto", pb::Protocol::Auto),
    ("h2loader", pb::Protocol::H2loader),
    ("serial", pb::Protocol::Serial),
];
const CONNECTIONS: &[(&str, pb::Connection)] = &[
    ("off", pb::Connection::Off),
    ("waiting", pb::Connection::Waiting),
    ("connecting", pb::Connection::Connecting),
    ("connected", pb::Connection::Connected),
    ("occupied", pb::Connection::Occupied),
    ("error", pb::Connection::Error),
];
const ACTIVITIES: &[(&str, pb::Activity)] = &[
    ("idle", pb::Activity::Idle),
    ("handshaking", pb::Activity::Handshaking),
    ("sending", pb::Activity::Sending),
    ("transferring", pb::Activity::Transferring),
    ("rebooting", pb::Activity::Rebooting),
    ("verifying", pb::Activity::Verifying),
    ("reconnecting", pb::Activity::Reconnecting),
];
const JOB_KINDS: &[(&str, pb::JobKind)] = &[
    ("refresh", pb::JobKind::Refresh),
    ("stage", pb::JobKind::Stage),
    ("install", pb::JobKind::Install),
    ("rebootApp", pb::JobKind::RebootApp),
    ("rebootLoader", pb::JobKind::RebootLoader),
    ("rebootUpgrade", pb::JobKind::RebootUpgrade),
];
const JOB_STATES: &[(&str, pb::JobState)] = &[
    ("queued", pb::JobState::Queued),
    ("running", pb::JobState::Running),
    ("succeeded", pb::JobState::Succeeded),
    ("failed", pb::JobState::Failed),
    ("cancelled", pb::JobState::Cancelled),
];
const LOG_SOURCES: &[(&str, pb::LogSource)] = &[
    ("rx", pb::LogSource::Rx),
    ("tx", pb::LogSource::Tx),
    ("system", pb::LogSource::System),
];

fn encode<E: Copy + Into<i32>>(table: &[(&str, E)], value: &Value) -> i32 {
    value
        .as_str()
        .and_then(|text| table.iter().find(|(name, _)| *name == text))
        .map(|(_, value)| (*value).into())
        .unwrap_or(0)
}

fn decode<E: Copy + Into<i32>>(table: &[(&'static str, E)], value: i32) -> Value {
    table
        .iter()
        .find(|(_, item)| (*item).into() == value)
        .map(|(name, _)| json!(name))
        .unwrap_or(Value::Null)
}

pub fn job_kind_name(kind: i32) -> Option<&'static str> {
    JOB_KINDS
        .iter()
        .find(|(_, item)| i32::from(*item) == kind)
        .map(|(name, _)| *name)
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}
fn number(value: &Value) -> u64 {
    value.as_u64().unwrap_or(0)
}
/// Proto3 scalars cannot be absent: 0 and "" encode null timestamps/strings.
fn optional_number(value: u64) -> Value {
    if value == 0 {
        Value::Null
    } else {
        json!(value)
    }
}
fn optional_text(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

// ------------------------------------------------------- JSON -> contract

fn image(value: &Value) -> Option<pb::H2LoaderImage> {
    value.is_object().then(|| pb::H2LoaderImage {
        valid: value["valid"].as_bool().unwrap_or(false),
        package_size: number(&value["packageSize"]),
        image_size: number(&value["imageSize"]),
        package_checksum: text(&value["packageChecksum"]),
        image_checksum: text(&value["imageChecksum"]),
        version: text(&value["version"]),
        board: text(&value["board"]),
        target: text(&value["target"]),
        role: text(&value["role"]),
    })
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub fn h2loader_to_pb(value: &Value) -> Option<pb::H2LoaderStatus> {
    value.is_object().then(|| pb::H2LoaderStatus {
        board: text(&value["board"]),
        target: text(&value["target"]),
        chip: text(&value["chip"]),
        device_uid: text(&value["deviceUid"]),
        capabilities: number(&value["capabilities"]),
        command_availability: number(&value["commandAvailability"]),
        commands: strings(&value["commands"]),
        transports: strings(&value["transports"]),
        active: value["active"].is_object().then(|| pb::H2LoaderActive {
            role: text(&value["active"]["role"]),
            version: text(&value["active"]["version"]),
            image_checksum: text(&value["active"]["imageChecksum"]),
            image_size: number(&value["active"]["imageSize"]),
        }),
        running_partition: number(&value["runningPartition"]) as u32,
        next_partition: number(&value["nextPartition"]) as u32,
        boot_intent: text(&value["bootIntent"]),
        stage: image(&value["stage"]),
        partition1: image(&value["partition1"]),
        partition2: image(&value["partition2"]),
        last_result: value["lastResult"].as_i64().unwrap_or(0),
    })
}

pub fn package_to_pb(value: &Value) -> Option<pb::PackageInfo> {
    value.is_object().then(|| pb::PackageInfo {
        board: text(&value["board"]),
        target: text(&value["target"]),
        version: text(&value["version"]),
        role: text(&value["role"]),
        sha256: text(&value["sha256"]),
        bytes: number(&value["bytes"]),
        format: number(&value["format"]) as u32,
        name: text(&value["name"]),
    })
}

pub fn job_to_pb(value: &Value, port_id: &str) -> Option<pb::Job> {
    value.is_object().then(|| pb::Job {
        job_id: text(&value["id"]),
        port_id: port_id.to_string(),
        kind: encode(JOB_KINDS, &value["kind"]),
        state: encode(JOB_STATES, &value["state"]),
        phase: text(&value["phase"]),
        acknowledged: number(&value["acknowledged"]),
        total: number(&value["total"]),
        source: text(&value["source"]),
        created_at: number(&value["createdAt"]),
        finished_at: number(&value["finishedAt"]),
        error: text(&value["error"]),
        package: package_to_pb(&value["package"]),
    })
}

/// One local port view (`Daemon::view`) as a contract message.
pub fn port_to_pb(view: &Value, node_id: &str) -> pb::Port {
    let id = text(&view["id"]);
    let holder = &view["holder"];
    pb::Port {
        port_id: id.clone(),
        owner_node_id: node_id.to_string(),
        name: text(&view["name"]),
        path: text(&view["path"]),
        protocol: encode(PROTOCOLS, &view["protocol"]),
        detected: encode(PROTOCOLS, &view["detected"]),
        baud: number(&view["baud"]) as u32,
        enabled: view["enabled"].as_bool().unwrap_or(false),
        present: view["present"].as_bool(),
        connection: encode(CONNECTIONS, &view["connection"]),
        activity: encode(ACTIVITIES, &view["activity"]),
        error: text(&view["error"]),
        usb: view["usb"].is_object().then(|| pb::UsbIdentity {
            vid: number(&view["usb"]["vid"]) as u32,
            pid: number(&view["usb"]["pid"]) as u32,
            serial: text(&view["usb"]["serial"]),
            product: text(&view["usb"]["product"]),
            manufacturer: text(&view["usb"]["manufacturer"]),
        }),
        rx_bytes: number(&view["rxBytes"]),
        tx_bytes: number(&view["txBytes"]),
        last_rx_at: number(&view["lastRxAt"]),
        last_tx_at: number(&view["lastTxAt"]),
        connected_at: number(&view["connectedAt"]),
        h2loader: h2loader_to_pb(&view["h2loader"]),
        h2loader_at: number(&view["h2loaderAt"]),
        job: job_to_pb(&view["job"], &id),
        history: view["history"]
            .as_array()
            .map(|jobs| jobs.iter().filter_map(|job| job_to_pb(job, &id)).collect())
            .unwrap_or_default(),
        holder: holder.is_object().then(|| pb::Holder {
            kind: match holder["kind"].as_str() {
                Some("gizmoth") => pb::holder::Kind::Gizmoth.into(),
                Some("client") => pb::holder::Kind::Client.into(),
                _ => 0,
            },
            client: text(&holder["client"]),
            reason: text(&holder["reason"]),
            since: number(&holder["since"]),
        }),
        queue: view["queue"]
            .as_array()
            .map(|queue| {
                queue
                    .iter()
                    .map(|request| pb::QueuedRequest {
                        id: number(&request["id"]),
                        client: text(&request["client"]),
                        reason: text(&request["reason"]),
                        since: number(&request["since"]),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        piping: view["mode"] == "piping",
        auto_grant: view["autoGrant"].as_bool().unwrap_or(true),
        preparing: view["preparing"].as_bool().unwrap_or(false),
        log_seq: number(&view["logSeq"]),
        log_generation: number(&view["logGeneration"]),
        created_at: number(&view["createdAt"]),
    }
}

pub fn log_page_to_pb(page: &Value, boot_id: &str) -> pb::LogPage {
    pb::LogPage {
        entries: page["entries"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| pb::LogEntry {
                        seq: number(&entry["seq"]),
                        at: number(&entry["at"]),
                        source: encode(LOG_SOURCES, &entry["source"]),
                        text: text(&entry["text"]),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        next: number(&page["next"]),
        truncated: page["truncated"].as_bool().unwrap_or(false),
        generation: number(&page["generation"]),
        boot_id: boot_id.to_string(),
    }
}

// ------------------------------------------------------- contract -> JSON

fn image_json(image: &Option<pb::H2LoaderImage>) -> Value {
    let image = image.clone().unwrap_or_default();
    json!({
        "valid": image.valid,
        "packageSize": image.package_size,
        "imageSize": image.image_size,
        "packageChecksum": image.package_checksum,
        "imageChecksum": image.image_checksum,
        "version": image.version,
        "board": image.board,
        "target": image.target,
        "role": if image.role.is_empty() { "unknown".into() } else { image.role },
    })
}

pub fn h2loader_json(status: &pb::H2LoaderStatus) -> Value {
    let active = status.active.clone().unwrap_or_default();
    json!({
        "board": status.board,
        "target": status.target,
        "chip": status.chip,
        "deviceUid": status.device_uid,
        "capabilities": status.capabilities,
        "commandAvailability": status.command_availability,
        "commands": status.commands,
        "transports": status.transports,
        "active": {
            "role": if active.role.is_empty() { "unknown".into() } else { active.role },
            "version": active.version,
            "imageChecksum": active.image_checksum,
            "imageSize": active.image_size,
        },
        "runningPartition": status.running_partition,
        "nextPartition": status.next_partition,
        "bootIntent": if status.boot_intent.is_empty() { "auto".into() } else { status.boot_intent.clone() },
        "stage": image_json(&status.stage),
        "partition1": image_json(&status.partition1),
        "partition2": image_json(&status.partition2),
        "lastResult": status.last_result,
    })
}

pub fn package_json(package: &pb::PackageInfo) -> Value {
    json!({
        "board": package.board,
        "target": package.target,
        "version": package.version,
        "role": package.role,
        "sha256": package.sha256,
        "bytes": package.bytes,
        "format": package.format,
        "name": optional_text(&package.name),
    })
}

pub fn job_json(job: &pb::Job) -> Value {
    json!({
        "id": job.job_id,
        "kind": decode(JOB_KINDS, job.kind),
        "state": decode(JOB_STATES, job.state),
        "phase": job.phase,
        "acknowledged": job.acknowledged,
        "total": job.total,
        "source": job.source,
        "createdAt": job.created_at,
        "finishedAt": optional_number(job.finished_at),
        "error": optional_text(&job.error),
        "package": job.package.as_ref().map(package_json),
    })
}

/// A remote port in the local view's shape, under its local `id`.
pub fn port_json(port: &pb::Port, id: &str) -> Value {
    let holder = port.holder.as_ref().and_then(|holder| {
        match pb::holder::Kind::try_from(holder.kind).ok()? {
            pb::holder::Kind::Gizmoth => Some(json!({ "kind": "gizmoth" })),
            pb::holder::Kind::Client => Some(json!({
                "kind": "client",
                "id": 0,
                "client": holder.client,
                "reason": holder.reason,
                "since": holder.since,
            })),
            pb::holder::Kind::Unspecified => None,
        }
    });
    json!({
        "id": id,
        "name": port.name,
        "path": port.path,
        "protocol": match decode(PROTOCOLS, port.protocol) { Value::Null => json!("auto"), protocol => protocol },
        "detected": decode(PROTOCOLS, port.detected),
        "baud": port.baud,
        "enabled": port.enabled,
        "mode": if port.piping { "piping" } else { "takeover" },
        "autoGrant": port.auto_grant,
        "usb": port.usb.as_ref().map(|usb| json!({
            "vid": usb.vid,
            "pid": usb.pid,
            "serial": optional_text(&usb.serial),
            "product": optional_text(&usb.product),
            "manufacturer": optional_text(&usb.manufacturer),
        })),
        "createdAt": port.created_at,
        "present": port.present,
        "connection": match decode(CONNECTIONS, port.connection) { Value::Null => json!("off"), connection => connection },
        "activity": match decode(ACTIVITIES, port.activity) { Value::Null => json!("idle"), activity => activity },
        "error": optional_text(&port.error),
        "rxBytes": port.rx_bytes,
        "txBytes": port.tx_bytes,
        "lastRxAt": optional_number(port.last_rx_at),
        "lastTxAt": optional_number(port.last_tx_at),
        "connectedAt": optional_number(port.connected_at),
        "h2loader": port.h2loader.as_ref().map(h2loader_json),
        "h2loaderAt": optional_number(port.h2loader_at),
        "job": port.job.as_ref().map(job_json),
        "history": port.history.iter().map(job_json).collect::<Vec<_>>(),
        "holder": holder,
        "queue": port.queue.iter().map(|request| json!({
            "id": request.id,
            "client": request.client,
            "reason": request.reason,
            "since": request.since,
        })).collect::<Vec<_>>(),
        "preparing": port.preparing,
        "logSeq": port.log_seq,
        "logGeneration": port.log_generation,
    })
}

pub fn log_page_json(page: &pb::LogPage) -> Value {
    json!({
        "instanceId": page.boot_id,
        "generation": page.generation,
        "next": page.next,
        "truncated": page.truncated,
        "entries": page.entries.iter().map(|entry| json!({
            "seq": entry.seq,
            "at": entry.at,
            "source": match decode(LOG_SOURCES, entry.source) { Value::Null => json!("system"), source => source },
            "text": entry.text,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_views_round_trip_through_the_contract() {
        let empty = json!({"valid": false, "packageSize": 0, "imageSize": 0, "packageChecksum": "",
            "imageChecksum": "", "version": "", "board": "", "target": "", "role": "unknown"});
        let partition = json!({"valid": true, "packageSize": 1, "imageSize": 1, "packageChecksum": "p",
            "imageChecksum": "i", "version": "1.2.0", "board": "gizclaw", "target": "esp32s3", "role": "app"});
        let active =
            json!({"role": "app", "version": "1.2.0", "imageChecksum": "c", "imageSize": 9});
        let h2loader = json!({
            "board": "gizclaw", "target": "esp32s3", "chip": "ESP32-S3", "deviceUid": "uid",
            "capabilities": 1, "commandAvailability": 8, "commands": ["status"], "transports": ["uart"],
            "active": active, "runningPartition": 1, "nextPartition": 2, "bootIntent": "auto",
            "stage": empty.clone(), "partition1": partition, "partition2": empty, "lastResult": -2
        });
        let package = json!({"board": "gizclaw", "target": "esp32s3", "version": "1.3.0", "role": "app",
            "sha256": "ab", "bytes": 2, "format": 1, "name": "app.pkg"});
        let job = json!({"id": "j", "kind": "install", "state": "running", "phase": "stage",
            "acknowledged": 1, "total": 2, "source": "ws", "createdAt": 1, "finishedAt": null,
            "error": null, "package": package});
        let usb = json!({"vid": 0x1a86, "pid": 0x55d3, "serial": null, "product": "CH348", "manufacturer": "WCH"});
        let view = json!({
            "id": "a", "name": "Board", "path": "/dev/cu.usb", "protocol": "auto", "baud": 115200,
            "enabled": true, "mode": "piping", "autoGrant": false, "createdAt": 5, "usb": usb,
            "present": true, "connection": "connected", "detected": "h2loader", "activity": "idle",
            "error": null, "rxBytes": 10, "txBytes": 2, "lastRxAt": 7, "lastTxAt": null, "connectedAt": 3,
            "h2loader": h2loader, "h2loaderAt": 4, "job": job, "history": [],
            "holder": {"kind": "client", "id": 3, "client": "agent", "reason": "flash", "since": 6},
            "queue": [{"id": 4, "client": "other", "reason": "wait", "since": 8}],
            "preparing": false, "logSeq": 12, "logGeneration": 1
        });
        let port = port_to_pb(&view, "node");
        assert_eq!(port.owner_node_id, "node");
        let back = port_json(&port, "node--a");
        assert_eq!(back["id"], "node--a");
        for key in [
            "name",
            "path",
            "protocol",
            "baud",
            "enabled",
            "mode",
            "autoGrant",
            "connection",
            "detected",
            "activity",
            "error",
            "rxBytes",
            "lastRxAt",
            "lastTxAt",
            "h2loader",
            "job",
            "queue",
            "logSeq",
            "present",
            "usb",
        ] {
            assert_eq!(back[key], view[key], "{key}");
        }
        assert_eq!(back["holder"]["client"], "agent");
    }

    #[test]
    fn unknown_values_fall_back_safely() {
        let port = port_to_pb(
            &json!({"id": "a", "protocol": "future", "connection": 7}),
            "n",
        );
        let back = port_json(&port, "x");
        assert_eq!(back["protocol"], "auto");
        assert_eq!(back["connection"], "off");
        assert!(back["holder"].is_null());
    }

    #[test]
    fn node_identity_is_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &Path, bytes: &[u8]| fs::write(path, bytes);
        let first = load_node(dir.path(), write);
        assert!(uuid::Uuid::parse_str(&first.id).is_ok());
        assert_eq!(load_node(dir.path(), write).id, first.id);
        assert!(!first.name.is_empty());
    }
}
