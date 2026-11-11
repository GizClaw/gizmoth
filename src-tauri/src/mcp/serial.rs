//! Read-only serial tool group. Queries use the resident daemon without
//! starting it; device commands require an exclusively granted WebSocket.
use super::Outcome;
use crate::serial::{client, daemon::load_configs, gateway, state_dir};
use serde_json::{json, Value};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const SHORT: Duration = Duration::from_secs(10);
const MAX_WAIT: u64 = 120;
const MAX_WATCH: Duration = Duration::from_secs(20);
const WATCH_RESERVE: Duration = Duration::from_millis(100);

fn port_arg() -> Value {
    json!({ "type": "string", "minLength": 1, "description": "Port id, name or device path, as returned by serial_list" })
}
fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
    })
}

pub fn tools() -> Vec<Value> {
    vec![
        tool("serial_list", "List serial ports",
            "All serial ports GizMoth manages: connection, detected protocol (h2loader or plain serial), current activity, job progress and the cached H2Loader device summary.",
            json!({}), &[]),
        tool("serial_system_ports", "Find local serial ports",
            "The serial service's latest discovery of local ports, including ones not yet added to GizMoth. Does not start the service; returns empty ports if it is stopped.",
            json!({}), &[]),
        tool("serial_status", "Serial port status",
            "Cached status of one port: connection, byte counters, current job and recent jobs, and for H2Loader devices board, chip, UID, running App or Loader version, Stage and partitions. Does not query the device.",
            json!({ "port": port_arg() }), &["port"]),
        tool("serial_logs", "Read serial log",
            "A port's recorded log. Without `after`, returns the newest lines; to read newer lines pass the returned `next` as `after`, along with the returned `instanceId` and `generation`. If identity is missing or changed, reset:true and the current tail is returned. Sources: rx (device output), tx (sent), system (service events).",
            json!({ "port": port_arg(), "after": { "type": "integer", "minimum": 0 }, "instanceId": { "type": "string" }, "generation": { "type": "integer", "minimum": 0 }, "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 200 } }),
            &["port"]),
        tool("serial_job", "Follow a job",
            "The port's current job, or its most recent finished job. With waitSeconds, waits up to that long for the current job to finish. This query does not require an ownership grant.",
            json!({ "port": port_arg(), "waitSeconds": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT, "default": 0 } }),
            &["port"]),
        tool("serial_pipe", "Discover device WebSocket",
            concat!(
                "Read-only discovery of GizMoth's device WebSocket URL, authentication headers and command schemas (protocol gizmoth.device.v1). ",
                "Obtaining the URL does not request ownership or start the service. Connecting IS the take request; closing IS release. ",
                "Connect from a script using the returned headers, identify the client and reason, and keep the same socket open. ",
                "Automatic approval defaults to true: requests wait for existing work, then receive FIFO grants without a manual mode switch. ",
                "With autoGrant off, approval is required in GizMoth. A waiting socket receives {\"type\":\"queued\",\"state\":\"pending\",\"position\":n,\"holder\":...} ",
                "and stays connected until {\"type\":\"granted\"}. Wait for granted before commands. ",
                "Send JSON text frames {\"id\":\"unique-request-id\",\"method\":\"send|install|reboot|refresh|cancel\",\"params\":{...}} ",
                "on that granted socket, following the returned commands[].inputSchema. Binary client frames and raw text are rejected. ",
                "Replies are {\"type\":\"result\",\"id\":...,\"ok\":true,\"result\":{...}} or {\"type\":\"result\",\"id\":...,\"ok\":false,\"error\":{\"code\":...,\"message\":...}}. ",
                "Pending errors also carry state:pending and preserve the connection and queue position. ",
                "Job progress/completion is pushed as {\"type\":\"job\",\"job\":{...}} on the same socket; binary server frames contain device logs. ",
                "Install requires an absolute package path on the service computer and an actual local connection; LAN installs are denied. ",
                "Status, logs and job queries need no grant. Closing cancels session-owned work at safe I/O boundaries; the next grant waits for cleanup. ",
                "A client name or API key never shares a grant across sockets. GizMoth alone owns the physical port and runs H2Loader; never open the device path yourself.",
            ),
            json!({
                "port": port_arg(),
                "client": { "type": "string", "minLength": 1, "maxLength": 64, "description": "Who is asking, e.g. coding-agent" },
                "reason": { "type": "string", "minLength": 1, "maxLength": 200, "description": "Why you need the port, shown to the user" }
            }),
            &["port", "client", "reason"]),
    ]
}

pub(super) fn unknown_tool_error(name: &str) -> String {
    let method = match name {
        "serial_send" => "send",
        "serial_install" => "install",
        "serial_reboot" => "reboot",
        "serial_refresh" => "refresh",
        "serial_cancel" => "cancel",
        "serial_add" | "serial_connect" => {
            return format!("{name} is no longer an MCP/CLI tool. Manage ports in GizMoth; MCP/CLI is query-only. For device commands, use serial_pipe to discover the WebSocket, connect and wait for granted. Closing the socket releases it.");
        }
        _ => return format!("Unknown tool {name}; use --list-tools"),
    };
    format!("{name} is no longer an MCP/CLI tool. Use serial_pipe for read-only WebSocket discovery, connect and wait for granted, then send a JSON command with method {method:?} on that same socket. Closing the socket releases it; there is no separate take/release API.")
}

pub fn service(data_dir: &Path) -> Value {
    match client::running(data_dir).map(|conn| conn.request("GET", "/v1/health", None, SHORT)) {
        Some(Ok(health)) => json!({ "running": true, "h2loaderCore": health["h2loaderCore"] }),
        _ => json!({ "running": false }),
    }
}

fn summary(port: &Value) -> Value {
    let job = &port["job"];
    json!({
        "id": port["id"],
        "name": port["name"],
        "path": port["path"],
        "enabled": port["enabled"],
        "present": port["present"],
        "connection": port["connection"],
        "protocol": port["protocol"],
        "detected": port["detected"],
        "activity": port["activity"],
        "error": port["error"],
        "mode": port["mode"],
        "owner": port["holder"],
        "waiting": port["queue"].as_array().map_or(0, Vec::len),
        "job": if job.is_null() { Value::Null } else { job_summary(job) },
        "device": port["h2loader"].as_object().map(|s| json!({
            "board": s.get("board"), "target": s.get("target"), "chip": s.get("chip"),
            "uid": s.get("deviceUid"), "role": s["active"]["role"], "version": s["active"]["version"],
            "commands": s.get("commands"),
        })),
    })
}
fn job_summary(job: &Value) -> Value {
    json!({
        "id": job["id"], "kind": job["kind"], "state": job["state"], "phase": job["phase"],
        "acknowledged": job["acknowledged"], "total": job["total"], "source": job["source"],
        "error": job["error"], "package": job["package"],
    })
}

fn find(ports: &[Value], key: &str) -> Result<Value, String> {
    if let Some(port) = ports
        .iter()
        .find(|p| p["id"] == key)
        .or_else(|| ports.iter().find(|p| p["path"] == key))
    {
        return Ok(port.clone());
    }
    let mut matches = ports.iter().filter(|p| {
        p["name"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(key))
    });
    let first = matches
        .next()
        .ok_or_else(|| format!("No serial port matches {key}; call serial_list"))?;
    let others: Vec<_> = matches.collect();
    if others.is_empty() {
        return Ok(first.clone());
    }
    let choices = std::iter::once(first)
        .chain(others)
        .map(|p| {
            format!(
                "{} ({})",
                p["id"].as_str().unwrap_or_default(),
                p["path"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "Ambiguous serial port name {key:?}; matches: {choices}. Use an exact port id or device path"
    ))
}

fn ports(conn: &client::Conn) -> Result<Vec<Value>, String> {
    Ok(conn.request("GET", "/v1/ports", None, SHORT)?["ports"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

fn pipe_arg<'a>(args: &'a Value, key: &str, max_chars: usize) -> Result<&'a str, String> {
    let value = str_arg(args, key)?.trim();
    if value.is_empty() {
        return Err(format!("{key} is required"));
    }
    if value.chars().count() > max_chars || value.chars().any(char::is_control) {
        return Err(format!(
            "{key} must be at most {max_chars} characters with no control characters"
        ));
    }
    Ok(value)
}

fn snapshot_ports(snapshot: &Value) -> &[Value] {
    snapshot["ports"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn system_ports_result(ports: Value) -> Outcome {
    if !ports.is_array() {
        return Err("The serial service returned an invalid system port list".into());
    }
    Ok(json!({ "service": "running", "ports": ports }))
}

fn read_logs(
    port: &Value,
    after: Option<u64>,
    instance: Option<&str>,
    generation: Option<u64>,
    limit: u64,
    mut request: impl FnMut(&str) -> Outcome,
) -> Outcome {
    let mut reset = after.is_some()
        && (instance.is_none()
            || generation.is_none()
            || after
                .zip(port["logSeq"].as_u64())
                .is_some_and(|(after, current)| after > current));
    let id = port["id"].as_str().unwrap_or_default();
    let limit = limit.clamp(1, 500);
    let path = match after.filter(|_| !reset) {
        Some(after) => format!("/v1/ports/{id}/logs?after={after}&limit={limit}"),
        None => format!("/v1/ports/{id}/logs?limit={limit}"),
    };
    let mut page = request(&path)?;
    if after.is_some()
        && !reset
        && (page["instanceId"].as_str() != instance || page["generation"].as_u64() != generation)
    {
        reset = true;
        page = request(&format!("/v1/ports/{id}/logs?limit={limit}"))?;
    }
    let lines: Vec<String> = page["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    format!(
                        "{} [{}] {}",
                        entry["seq"],
                        entry["source"].as_str().unwrap_or(""),
                        entry["text"].as_str().unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "port": port["name"], "next": page["next"], "truncated": page["truncated"],
        "instanceId": page["instanceId"], "generation": page["generation"],
        "reset": reset, "lines": lines,
    }))
}

fn latest_job(port: &Value) -> Value {
    let current = &port["job"];
    let last = port["history"]
        .as_array()
        .and_then(|history| history.first())
        .unwrap_or(current);
    let job = if current.is_null() { last } else { current };
    json!({
        "running": !current.is_null(),
        "job": if job.is_null() { Value::Null } else { job_summary(job) },
        "activity": port["activity"],
        "device": summary(port)["device"],
    })
}

fn watch_budget(remaining: Duration) -> Option<(Duration, u64)> {
    let request = remaining.min(MAX_WATCH + WATCH_RESERVE);
    let server = request.checked_sub(WATCH_RESERVE)?;
    let millis = server.as_millis() as u64;
    (millis > 0).then_some((request, millis))
}

fn follow_job(
    key: &str,
    wait: Duration,
    now: impl Fn() -> Instant,
    mut request: impl FnMut(&str, Duration) -> Outcome,
) -> Outcome {
    let deadline = now() + wait;
    let initial_budget = if wait.is_zero() {
        SHORT
    } else {
        wait.min(SHORT)
    };
    let mut snapshot = request("/v1/ports", initial_budget)?;
    let mut port = find(snapshot_ports(&snapshot), key)?;
    // Keep following the selected port if its name changes while waiting.
    let id = port["id"].as_str().unwrap_or(key).to_owned();
    let job_id = port["job"]["id"].as_str().map(str::to_owned);
    while !wait.is_zero()
        && job_id
            .as_deref()
            .is_some_and(|id| port["job"]["id"].as_str() == Some(id))
    {
        let Some((timeout, timeout_ms)) = watch_budget(deadline.saturating_duration_since(now()))
        else {
            break;
        };
        let since = snapshot["version"].as_u64().unwrap_or(0);
        let instance = percent_encoding::utf8_percent_encode(
            snapshot["instanceId"].as_str().unwrap_or_default(),
            percent_encoding::NON_ALPHANUMERIC,
        );
        let path = format!("/v1/watch?since={since}&instance={instance}&timeoutMs={timeout_ms}");
        match request(&path, timeout) {
            Ok(value) => {
                snapshot = value;
                port = find(snapshot_ports(&snapshot), &id)?;
            }
            Err(_) if now() >= deadline => break,
            Err(error) => return Err(error),
        }
    }
    if let Some(job_id) = job_id {
        if port["job"]["id"].as_str() != Some(&job_id) {
            let job = port["history"]
                .as_array()
                .and_then(|history| {
                    history
                        .iter()
                        .find(|job| job["id"].as_str() == Some(&job_id))
                })
                .ok_or_else(|| {
                    format!(
                        "Job {job_id} is no longer current and is unavailable in recent history"
                    )
                })?;
            let mut result = latest_job(&port);
            result["running"] = json!(false);
            result["job"] = job_summary(job);
            return Ok(result);
        }
    }
    Ok(latest_job(&port))
}

/// Semantic checks finish before even looking up a running service.
fn preflight(name: &str, args: &Value) -> Result<(), String> {
    match name {
        "serial_list" | "serial_system_ports" => return Ok(()),
        "serial_status" | "serial_logs" | "serial_job" | "serial_pipe" => {
            str_arg(args, "port")?;
        }
        _ => return Err(unknown_tool_error(name)),
    }
    if name == "serial_pipe" {
        pipe_arg(args, "client", 64)?;
        pipe_arg(args, "reason", 200)?;
    }
    Ok(())
}

pub fn call(data_dir: &Path, name: &str, args: &Value) -> Outcome {
    preflight(name, args)?;
    // The first job snapshot also proves the service is alive. Avoid a
    // separate health request outside waitSeconds' request budget.
    let running = if name == "serial_job" {
        client::Conn::read(&state_dir(data_dir))
    } else {
        client::running(data_dir)
    };
    let conn = match running {
        Some(conn) => conn,
        None => {
            let configured = if name == "serial_system_ports" {
                Vec::new()
            } else {
                load_configs(&state_dir(data_dir))
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id, "name": c.name, "path": c.path, "protocol": c.protocol,
                            "enabled": c.enabled, "connection": "off",
                        })
                    })
                    .collect()
            };
            return Ok(json!({
                "service": "stopped",
                "note": "GizMoth's serial service is not running. Open GizMoth and enable Service or connect a configured port; MCP/CLI queries do not start it.",
                "ports": configured,
            }));
        }
    };
    let port = || -> Result<Value, String> { find(&ports(&conn)?, str_arg(args, "port")?) };
    match name {
        "serial_list" => Ok(json!({
            "service": "running",
            "ports": ports(&conn)?.iter().map(summary).collect::<Vec<_>>(),
        })),
        "serial_system_ports" => {
            system_ports_result(conn.request("GET", "/v1/system-ports", None, SHORT)?)
        }
        "serial_status" => {
            let mut p = port()?;
            if let Some(map) = p.as_object_mut() {
                map.remove("usb");
            }
            Ok(p)
        }
        "serial_logs" => read_logs(
            &port()?,
            args["after"].as_u64(),
            args["instanceId"].as_str(),
            args["generation"].as_u64(),
            args["limit"].as_u64().unwrap_or(200),
            |path| conn.request("GET", path, None, SHORT),
        ),
        "serial_job" => follow_job(
            str_arg(args, "port")?,
            Duration::from_secs(args["waitSeconds"].as_u64().unwrap_or(0).min(MAX_WAIT)),
            Instant::now,
            |path, timeout| conn.request("GET", path, None, timeout),
        ),
        "serial_pipe" => {
            let client = pipe_arg(args, "client", 64)?;
            let reason = pipe_arg(args, "reason", 200)?;
            let snapshot = conn.request("GET", "/v1/ports", None, SHORT)?;
            crate::serial::ws::ensure_protocol(&snapshot)?;
            let p = find(snapshot_ports(&snapshot), str_arg(args, "port")?)?;
            let base = snapshot["pipeBase"]
                .as_str()
                .ok_or("The serial service did not advertise a device WebSocket URL; update GizMoth and enable Service")?;
            let encode = |text: &str| {
                percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
            };
            let key = gateway::read(&state_dir(data_dir))
                .filter(|config| config.enabled)
                .map(|config| config.api_key)
                .ok_or(
                    "GizMoth's Service is off; ask the user to enable it in Settings > Service",
                )?;
            Ok(json!({
                "url": format!("{base}/{}/pipe?client={}&reason={}", p["id"].as_str().unwrap_or_default(), encode(client), encode(reason)),
                "headers": { "Authorization": format!("Bearer {key}") },
                "protocol": crate::serial::ws::PROTOCOL,
                "commands": crate::serial::ws::commands(),
                "note": "Discovery only: connect with the authentication headers to request ownership, wait for granted, then send JSON commands on that same socket. Closing releases it and cancels session-owned work at safe I/O boundaries.",
                "mode": p["mode"],
                "owner": p["holder"],
                "waiting": p["queue"].as_array().map_or(0, Vec::len),
            }))
        }
        _ => Err(unknown_tool_error(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn pipe_discovery_does_not_start_a_stopped_service() {
        let dir = tempfile::tempdir().unwrap();
        let value = call(
            dir.path(),
            "serial_pipe",
            &json!({"port":"synthetic","client":"agent","reason":"inspect"}),
        )
        .unwrap();
        assert_eq!(value["service"], "stopped");
        assert!(value.get("url").is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    fn port(id: &str, name: &str, path: &str) -> Value {
        json!({ "id": id, "name": name, "path": path, "job": null, "history": [], "activity": "idle" })
    }

    fn snapshot(version: u64, instance: &str, acknowledged: u64) -> Value {
        let mut port = port("port-a", "Console", "/synthetic/a");
        port["job"] = json!({
            "id": "job-a", "kind": "install", "state": "running", "phase": "transfer",
            "acknowledged": acknowledged, "total": 10, "source": "mcp",
        });
        port["activity"] = json!("installing");
        json!({ "version": version, "instanceId": instance, "ports": [port] })
    }

    #[test]
    fn exact_id_precedes_exact_path_and_duplicate_names() {
        let ports = [
            port("port-b", "PORT-A", "port-a"),
            port("port-a", "Console", "/synthetic/a"),
            port("port-c", "port-a", "/synthetic/c"),
        ];
        assert_eq!(find(&ports, "port-a").unwrap()["id"], "port-a");
    }

    #[test]
    fn exact_path_precedes_duplicate_case_insensitive_names() {
        let ports = [
            port("port-a", "/SYNTHETIC/B", "/synthetic/a"),
            port("port-b", "Console", "/synthetic/b"),
            port("port-c", "/synthetic/b", "/synthetic/c"),
        ];
        assert_eq!(find(&ports, "/synthetic/b").unwrap()["id"], "port-b");
    }

    #[test]
    fn names_must_be_unique_including_capitalization() {
        let ports = [
            port("port-a", "Console", "/synthetic/a"),
            port("port-b", "CONSOLE", "/synthetic/b"),
            port("port-c", "Console", "/synthetic/c"),
        ];
        for key in ["Console", "console", "CONSOLE"] {
            let error = find(&ports, key).unwrap_err();
            assert!(error.contains("Ambiguous"));
            for choice in [
                "port-a (/synthetic/a)",
                "port-b (/synthetic/b)",
                "port-c (/synthetic/c)",
            ] {
                assert!(error.contains(choice), "{error}");
            }
        }
        assert_eq!(find(&ports[..1], "console").unwrap()["id"], "port-a");
        assert!(find(&ports, "missing")
            .unwrap_err()
            .contains("No serial port"));
    }

    #[test]
    fn removed_tools_never_fall_back_to_an_existing_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_dir(dir.path());
        std::fs::create_dir_all(&state).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let info = crate::serial::daemon::Info {
            pid: std::process::id(),
            port: listener.local_addr().unwrap().port(),
            token: "test-token".into(),
            api_version: crate::serial::API_VERSION,
            started_at: 0,
        };
        std::fs::write(
            state.join("daemon.json"),
            serde_json::to_vec(&info).unwrap(),
        )
        .unwrap();
        for name in [
            "serial_add",
            "serial_connect",
            "serial_send",
            "serial_install",
            "serial_reboot",
            "serial_refresh",
            "serial_cancel",
        ] {
            let error = call(dir.path(), name, &json!({})).unwrap_err();
            assert!(error.contains("serial_pipe"), "{error}");
            assert!(error.contains("granted"), "{error}");
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(std::fs::read_dir(&state).unwrap().count(), 1);
    }

    #[test]
    fn pipe_discovery_reads_metadata_and_advertises_structured_commands_only() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let dir = tempfile::tempdir().unwrap();
        let state = state_dir(dir.path());
        std::fs::create_dir_all(&state).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let info = crate::serial::daemon::Info {
            pid: std::process::id(),
            port: listener.local_addr().unwrap().port(),
            token: "test-token".into(),
            api_version: crate::serial::API_VERSION,
            started_at: 0,
        };
        std::fs::write(
            state.join("daemon.json"),
            serde_json::to_vec(&info).unwrap(),
        )
        .unwrap();
        let config = gateway::Config {
            api_key: "gmk_synthetic_key".into(),
            ..gateway::Config::default()
        };
        std::fs::write(gateway::path(&state), serde_json::to_vec(&config).unwrap()).unwrap();
        let snapshot = json!({
            "pipeBase":"ws://127.0.0.1:7420/v1/ports",
            "deviceWebSocketProtocol":crate::serial::ws::PROTOCOL,
            "ports":[{"id":"port-a","name":"Console","mode":"takeover","holder":null,"queue":[]}]
        });
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for body in [json!({"running":true}), snapshot] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 1024];
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut buf).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buf[..count]);
                }
                requests.push(String::from_utf8(bytes).unwrap());
                let body = body.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
            requests
        });
        let result = call(
            dir.path(),
            "serial_pipe",
            &json!({"port":"port-a","client":"agent one","reason":"inspect & refresh"}),
        )
        .unwrap();
        assert_eq!(result["url"], "ws://127.0.0.1:7420/v1/ports/port-a/pipe?client=agent%20one&reason=inspect%20%26%20refresh");
        assert_eq!(
            result["headers"]["Authorization"],
            "Bearer gmk_synthetic_key"
        );
        assert_eq!(result["protocol"], "gizmoth.device.v1");
        assert_eq!(result["commands"], json!(crate::serial::ws::commands()));
        assert_eq!(result["mode"], "takeover");
        assert_eq!(result["waiting"], 0);
        assert!(!result["note"].as_str().unwrap().contains("token="));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /v1/health "));
        assert!(requests[1].starts_with("GET /v1/ports "));
    }

    #[test]
    fn discovery_wraps_synthetic_daemon_array_as_an_object() {
        let ports = json!([{
            "path": "/synthetic/a", "name": "Console",
            "usb": { "vid": 1, "pid": 2, "serial": "synthetic" },
        }]);
        let value = system_ports_result(ports.clone()).unwrap();
        assert!(value.is_object());
        assert_eq!(value["service"], "running");
        assert_eq!(value["ports"], ports);
        assert!(system_ports_result(json!({ "ports": [] })).is_err());
    }

    #[test]
    fn stale_log_cursor_reads_current_tail_and_forwards_page_identity() {
        let mut port = port("port-a", "Console", "/synthetic/a");
        port["logSeq"] = json!(12);
        let result = read_logs(&port, Some(500), Some("old"), Some(1), 20, |path| {
            assert_eq!(path, "/v1/ports/port-a/logs?limit=20");
            Ok(json!({
                "instanceId": "restarted", "generation": 3, "next": 12, "truncated": true,
                "entries": [{ "seq": 12, "source": "rx", "text": "new output" }],
            }))
        })
        .unwrap();
        assert_eq!(result["reset"], true);
        assert_eq!(result["instanceId"], "restarted");
        assert_eq!(result["generation"], 3);
        assert_eq!(result["next"], 12);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["lines"], json!(["12 [rx] new output"]));

        port["logSeq"] = json!(0);
        let empty = read_logs(&port, Some(500), Some("old"), Some(1), 20, |path| {
            assert_eq!(path, "/v1/ports/port-a/logs?limit=20");
            Ok(json!({
                "instanceId": "restarted", "generation": 0, "next": 0,
                "truncated": false, "entries": [],
            }))
        })
        .unwrap();
        assert_eq!(empty["reset"], true);
        assert_eq!(empty["next"], 0);
        assert_eq!(empty["lines"], json!([]));
    }

    #[test]
    fn current_log_cursor_keeps_incremental_and_tail_queries() {
        let mut port = port("port-a", "Console", "/synthetic/a");
        port["logSeq"] = json!(12);
        for (after, expected) in [
            (Some(10), "/v1/ports/port-a/logs?after=10&limit=20"),
            (Some(12), "/v1/ports/port-a/logs?after=12&limit=20"),
            (None, "/v1/ports/port-a/logs?limit=20"),
        ] {
            let result = read_logs(&port, after, Some("same"), Some(4), 20, |path| {
                assert_eq!(path, expected);
                Ok(json!({
                    "instanceId": "same", "generation": 4, "next": 12,
                    "truncated": false, "entries": [],
                }))
            })
            .unwrap();
            assert_eq!(result["reset"], false);
            assert_eq!(result["instanceId"], "same");
            assert_eq!(result["generation"], 4);
            assert_eq!(result["next"], 12);
        }
    }

    #[test]
    fn changed_or_missing_log_identity_returns_the_current_tail() {
        let mut port = port("port-a", "Console", "/synthetic/a");
        port["logSeq"] = json!(100);
        for (instance, generation) in [(Some("old"), Some(4)), (Some("new"), Some(3)), (None, None)]
        {
            let calls = Cell::new(0);
            let result = read_logs(&port, Some(10), instance, generation, 20, |path| {
                calls.set(calls.get() + 1);
                if instance.is_some() && calls.get() == 1 {
                    assert_eq!(path, "/v1/ports/port-a/logs?after=10&limit=20");
                } else {
                    assert_eq!(path, "/v1/ports/port-a/logs?limit=20");
                }
                Ok(json!({ "instanceId": "new", "generation": 4, "next": 100,
                    "entries": [{ "seq": 1, "source": "rx", "text": "new" }] }))
            })
            .unwrap();
            assert_eq!(result["reset"], true);
            assert_eq!(result["lines"], json!(["1 [rx] new"]));
            assert_eq!(calls.get(), if instance.is_some() { 2 } else { 1 });
        }
    }

    #[test]
    fn follow_job_returns_selected_job_even_if_another_has_started() {
        for in_history in [true, false] {
            let calls = Cell::new(0);
            let result = follow_job("port-a", Duration::from_secs(5), Instant::now, |_, _| {
                calls.set(calls.get() + 1);
                let mut value = snapshot(calls.get(), "instance", 1);
                if calls.get() == 2 {
                    let mut finished = value["ports"][0]["job"].clone();
                    finished["state"] = json!("failed");
                    value["ports"][0]["job"]["id"] = json!("job-b");
                    if in_history {
                        value["ports"][0]["history"] = json!([finished]);
                    }
                }
                Ok(value)
            });
            assert_eq!(calls.get(), 2);
            if in_history {
                let result = result.unwrap();
                assert_eq!(result["running"], false);
                assert_eq!(result["job"]["id"], "job-a");
                assert_eq!(result["job"]["state"], "failed");
            } else {
                assert!(result
                    .unwrap_err()
                    .contains("unavailable in recent history"));
            }
        }
    }

    #[test]
    fn pipe_identity_is_trimmed_and_validated_before_url_encoding() {
        let args = json!({ "client": "  agent  ", "reason": "  inspect output  " });
        assert_eq!(pipe_arg(&args, "client", 64).unwrap(), "agent");
        assert_eq!(pipe_arg(&args, "reason", 200).unwrap(), "inspect output");
        for client in [" ".into(), "x".repeat(65), "agent\nother".into()] {
            assert!(pipe_arg(&json!({ "client": client }), "client", 64).is_err());
        }
        assert!(pipe_arg(&json!({ "client": "测".repeat(64) }), "client", 64).is_ok());
        assert!(pipe_arg(&json!({ "reason": "x".repeat(201) }), "reason", 200).is_err());
    }

    #[test]
    fn watch_budgets_reserve_response_time_and_cap_server_waits() {
        assert_eq!(
            watch_budget(Duration::from_secs(1)),
            Some((Duration::from_secs(1), 900))
        );
        assert_eq!(
            watch_budget(Duration::from_secs(120)),
            Some((Duration::from_millis(20_100), 20_000))
        );
        assert!(watch_budget(Duration::ZERO).is_none());
        assert!(watch_budget(WATCH_RESERVE).is_none());
    }

    #[test]
    fn zero_wait_reads_one_snapshot_without_watching() {
        let calls = Cell::new(0);
        let result = follow_job("Console", Duration::ZERO, Instant::now, |path, timeout| {
            calls.set(calls.get() + 1);
            assert_eq!(path, "/v1/ports");
            assert_eq!(timeout, SHORT);
            Ok(snapshot(7, "instance-a", 1))
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(result["running"], true);
        assert_eq!(result["job"]["acknowledged"], 1);
    }

    #[test]
    fn one_second_wait_budgets_initial_request_and_watch_then_returns_latest() {
        let clock = Cell::new(Instant::now());
        let calls = Cell::new(0);
        let result = follow_job(
            "Console",
            Duration::from_secs(1),
            || clock.get(),
            |path, timeout| {
                let call = calls.get();
                calls.set(call + 1);
                if call == 0 {
                    assert_eq!(path, "/v1/ports");
                    assert_eq!(timeout, Duration::from_secs(1));
                    clock.set(clock.get() + Duration::from_millis(100));
                    Ok(snapshot(7, "instance a", 1))
                } else {
                    assert_eq!(call, 1);
                    assert_eq!(
                        path,
                        "/v1/watch?since=7&instance=instance%20a&timeoutMs=800"
                    );
                    assert_eq!(timeout, Duration::from_millis(900));
                    clock.set(clock.get() + Duration::from_millis(850));
                    Ok(snapshot(8, "instance a", 2))
                }
            },
        )
        .unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(result["running"], true);
        assert_eq!(result["job"]["acknowledged"], 2);
    }

    #[test]
    fn expired_watch_returns_cached_job_but_early_failures_are_errors() {
        for expire in [true, false] {
            let started = Instant::now();
            let clock = Cell::new(started);
            let calls = Cell::new(0);
            let result = follow_job(
                "port-a",
                Duration::from_secs(1),
                || clock.get(),
                |_, _| {
                    let call = calls.get();
                    calls.set(call + 1);
                    if call == 0 {
                        Ok(snapshot(1, "instance-a", 3))
                    } else {
                        if expire {
                            clock.set(started + Duration::from_secs(1));
                        }
                        Err("request failed".into())
                    }
                },
            );
            if expire {
                let value = result.unwrap();
                assert_eq!(value["running"], true);
                assert_eq!(value["job"]["acknowledged"], 3);
            } else {
                assert_eq!(result.unwrap_err(), "request failed");
            }
            assert_eq!(calls.get(), 2);
        }
    }

    #[test]
    fn initial_snapshot_failure_remains_a_bounded_error() {
        let result = follow_job(
            "port-a",
            Duration::from_secs(1),
            Instant::now,
            |path, timeout| {
                assert_eq!(path, "/v1/ports");
                assert_eq!(timeout, Duration::from_secs(1));
                Err("snapshot unavailable".into())
            },
        );
        assert_eq!(result.unwrap_err(), "snapshot unavailable");
    }

    #[test]
    fn watches_track_new_instances_and_keep_selected_port_until_cancellation() {
        let clock = Cell::new(Instant::now());
        let calls = Cell::new(0);
        let result = follow_job(
            "Console",
            Duration::from_secs(120),
            || clock.get(),
            |path, timeout| {
                let call = calls.get();
                calls.set(call + 1);
                clock.set(clock.get() + Duration::from_millis(1));
                match call {
                    0 => {
                        assert_eq!(path, "/v1/ports");
                        assert_eq!(timeout, SHORT);
                        Ok(snapshot(99, "old", 1))
                    }
                    1 => {
                        assert_eq!(path, "/v1/watch?since=99&instance=old&timeoutMs=20000");
                        assert_eq!(timeout, MAX_WATCH + WATCH_RESERVE);
                        let mut value = snapshot(1, "new", 2);
                        value["ports"][0]["name"] = json!("Renamed");
                        value["ports"].as_array_mut().unwrap().push(port(
                            "port-b",
                            "Console",
                            "/synthetic/b",
                        ));
                        Ok(value)
                    }
                    2 => {
                        assert_eq!(path, "/v1/watch?since=1&instance=new&timeoutMs=20000");
                        let mut value = snapshot(2, "new", 2);
                        let mut cancelled = value["ports"][0]["job"].take();
                        cancelled["state"] = json!("cancelled");
                        value["ports"][0]["history"] = json!([cancelled]);
                        value["ports"][0]["activity"] = json!("idle");
                        Ok(value)
                    }
                    _ => panic!("watched a finished job"),
                }
            },
        )
        .unwrap();
        assert_eq!(calls.get(), 3);
        assert_eq!(result["running"], false);
        assert_eq!(result["job"]["id"], "job-a");
        assert_eq!(result["job"]["state"], "cancelled");
    }
}
