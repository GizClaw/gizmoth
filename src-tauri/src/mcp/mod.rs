//! `gizmoth --mcp`: GizMoth's global stdio MCP server.
//!
//! Each domain contributes a tool group. Tools never touch hardware
//! themselves: they call GizMoth's local services over HTTP (serial ports go
//! through the resident serial daemon), so any number of MCP clients and the
//! app share one owner per device.
mod serial;

use serde_json::{json, Value};
use std::{
    io::{BufRead, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};

pub const FLAG: &str = "--mcp";

const INSTRUCTIONS: &str = "GizMoth on this computer. Serial tools (serial_*) manage the serial ports GizMoth's resident serial service owns: list ports and what each is doing, read H2Loader device status and logs, connect or disconnect a port, install a firmware package, reboot a device and send text to a plain serial port. Every call goes through the service, which is the only process that opens ports, so never open the same port yourself. Installing firmware or rebooting changes the device: confirm the target port and package with the user first, then follow the job with serial_job.";

type Outcome = Result<Value, String>;

pub(crate) fn tools() -> Vec<Value> {
    let mut tools = vec![json!({
        "name": "gizmoth_status",
        "title": "GizMoth status",
        "description": "Version of this GizMoth and which local services are running.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
    })];
    tools.extend(serial::tools());
    tools
}

pub(crate) fn call(data_dir: &Path, name: &str, args: &Value) -> Outcome {
    // Both transports validate before service admission, including unknown names.
    let tool = tools()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(|| format!("Unknown tool {name}; use --list-tools"))?;
    let args = validate_args(&tool["inputSchema"], args)?;
    match name {
        "gizmoth_status" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "serial": serial::service(data_dir),
        })),
        _ => serial::call(data_dir, name, &args),
    }
}

/// The CLI emits the shared tool result without an MCP protocol envelope.
pub(crate) fn cli_call(data_dir: &Path, name: &str, args: &Value) -> Outcome {
    call(data_dir, name, args)
}

/// JSON Schema integers include integral decimal/exponent forms. Normalize them
/// once so existing typed tool handlers receive the same integer representation.
fn integer_value(value: &Value) -> Option<Value> {
    if value.is_i64() || value.is_u64() {
        return Some(value.clone());
    }
    let number = value.as_f64()?;
    if !number.is_finite() || number.fract() != 0.0 {
        return None;
    }
    if (0.0..18_446_744_073_709_551_616.0).contains(&number) {
        Some(json!(number as u64))
    } else if (-9_223_372_036_854_775_808.0..0.0).contains(&number) {
        Some(json!(number as i64))
    } else {
        None
    }
}

/// The current registry uses flat scalar schemas. Return validated, normalized
/// arguments before either CLI or MCP can reach an auto-starting service action.
fn validate_args(schema: &Value, args: &Value) -> Outcome {
    let args = args
        .as_object()
        .ok_or("Tool arguments must be a JSON object")?;
    if let Some(required) = schema["required"].as_array() {
        for key in required.iter().filter_map(Value::as_str) {
            if !args.contains_key(key) {
                return Err(format!("{key} is required"));
            }
        }
    }
    let properties = schema["properties"]
        .as_object()
        .ok_or("Invalid tool schema")?;
    let mut normalized = args.clone();
    for (key, value) in args {
        let rule = properties
            .get(key)
            .ok_or_else(|| format!("Unknown tool argument {key}"))?;
        let canonical = match rule["type"].as_str() {
            Some("string") if value.is_string() => Some(value.clone()),
            Some("boolean") if value.is_boolean() => Some(value.clone()),
            Some("integer") => integer_value(value),
            _ => None,
        }
        .ok_or_else(|| format!("Invalid type for {key}; expected {}", rule["type"]))?;
        normalized.insert(key.clone(), canonical);
        if rule["enum"]
            .as_array()
            .is_some_and(|options| !options.contains(value))
        {
            return Err(format!(
                "Invalid value for {key}; expected one of {}",
                rule["enum"]
            ));
        }
        if let Some(number) = value.as_f64() {
            if rule["minimum"]
                .as_f64()
                .is_some_and(|minimum| number < minimum)
                || rule["maximum"]
                    .as_f64()
                    .is_some_and(|maximum| number > maximum)
            {
                return Err(format!("{key} is outside its allowed range"));
            }
        }
        if let Some(text) = value.as_str() {
            let length = text.chars().count() as u64;
            if rule["minLength"].as_u64().is_some_and(|min| length < min) {
                return Err(format!("{key} must not be empty"));
            }
            if let Some(max) = rule["maxLength"].as_u64() {
                if length > max {
                    return Err(format!("{key} exceeds {max} characters"));
                }
            }
        }
    }
    Ok(Value::Object(normalized))
}

pub(crate) fn cli_output(outcome: Outcome, mut stdout: impl Write, mut stderr: impl Write) -> i32 {
    match outcome {
        Ok(value) => match writeln!(stdout, "{value}").and_then(|_| stdout.flush()) {
            Ok(()) => 0,
            Err(error) => {
                let _ = writeln!(stderr, "Unable to write tool result: {error}");
                1
            }
        },
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            1
        }
    }
}

fn tool_result(outcome: Outcome) -> Value {
    match outcome {
        Ok(value) => {
            let mut result = json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
            });
            // MCP structuredContent is an object. Other JSON results are still
            // valid tool content, but must stay in the text representation.
            if value.is_object() {
                result["structuredContent"] = value;
            }
            result
        }
        Err(error) => {
            json!({ "content": [{ "type": "text", "text": error }], "isError": true })
        }
    }
}

fn handle(data_dir: &Path, message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or_default();
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "gizmoth", "title": "GizMoth", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap_or_default();
            let empty = json!({});
            let args = message["params"].get("arguments").unwrap_or(&empty);
            Ok(tool_result(call(data_dir, name, args)))
        }
        _ => Err(json!({ "code": -32601, "message": format!("Method not found: {method}") })),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

const MAX_REQUESTS: usize = 16;
const MAX_CONTROL_REQUESTS: usize = 4;

struct RequestSlot<'a>(&'a AtomicUsize);

impl<'a> RequestSlot<'a> {
    fn acquire(counter: &'a AtomicUsize, limit: usize) -> Option<Self> {
        let mut count = counter.load(Ordering::Relaxed);
        loop {
            if count >= limit {
                return None;
            }
            match counter.compare_exchange_weak(
                count,
                count + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self(counter)),
                Err(current) => count = current,
            }
        }
    }
}

impl Drop for RequestSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn write_reply<W: Write>(output: &Mutex<W>, failed: &AtomicBool, reply: &Value) {
    let mut output = output.lock().unwrap();
    if writeln!(output, "{reply}")
        .and_then(|_| output.flush())
        .is_err()
    {
        failed.store(true, Ordering::Relaxed);
    }
}

fn serve<R: BufRead, W: Write + Send>(
    input: R,
    output: W,
    handler: impl Fn(&Value) -> Option<Value> + Sync,
) {
    let output = Mutex::new(output);
    let failed = AtomicBool::new(false);
    let requests = AtomicUsize::new(0);
    let control_requests = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for line in input.lines() {
            if failed.load(Ordering::Relaxed) {
                break;
            }
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let message = match serde_json::from_str::<Value>(&line) {
                Ok(message) => message,
                Err(_) => {
                    write_reply(
                        &output,
                        &failed,
                        &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }),
                    );
                    continue;
                }
            };
            let Some(id) = message.get("id").cloned() else {
                continue;
            };
            // Keep protocol controls and cancellation available even when all
            // ordinary slots are occupied by long-running job watches.
            let control = matches!(
                message["method"].as_str(),
                Some("initialize" | "ping" | "tools/list")
            ) || (message["method"] == "tools/call"
                && message["params"]["name"] == "serial_cancel");
            let (counter, limit) = if control {
                (&control_requests, MAX_CONTROL_REQUESTS)
            } else {
                (&requests, MAX_REQUESTS)
            };
            let Some(slot) = RequestSlot::acquire(counter, limit) else {
                write_reply(
                    &output,
                    &failed,
                    &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": "Server busy; retry the request" } }),
                );
                continue;
            };
            let output = &output;
            let failed = &failed;
            let handler = &handler;
            scope.spawn(move || {
                let _slot = slot;
                if let Some(reply) = handler(&message) {
                    write_reply(output, failed, &reply);
                }
            });
        }
    });
}

pub fn run(data_dir: &Path) -> i32 {
    let stdin = std::io::stdin();
    serve(stdin.lock(), std::io::stdout(), |message| {
        handle(data_dir, message)
    });
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_reads_use_shared_tools_without_starting_a_service() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "gizmoth_status",
            "serial_system_ports",
            "serial_list",
            "serial_status",
            "serial_logs",
            "serial_job",
        ] {
            let args = if matches!(name, "serial_status" | "serial_logs" | "serial_job") {
                json!({"port":"synthetic"})
            } else {
                json!({})
            };
            let value = cli_call(dir.path(), name, &args).unwrap();
            if name == "gizmoth_status" {
                assert_eq!(value["serial"]["running"], false);
            } else {
                assert_eq!(value["service"], "stopped");
            }
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            assert_eq!(cli_output(Ok(value.clone()), &mut stdout, &mut stderr), 0);
            assert_eq!(serde_json::from_slice::<Value>(&stdout).unwrap(), value);
            assert!(stderr.is_empty());
        }
        assert!(cli_call(dir.path(), "serial_typo", &json!({})).is_err());
        assert!(cli_call(dir.path(), "serial_add", &json!([])).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn cli_rejects_invalid_tool_arguments_before_service_start() {
        let dir = tempfile::tempdir().unwrap();
        for (name, args) in [
            ("serial_typo", json!({})),
            ("serial_add", json!({})),
            ("serial_add", json!({"path":""})),
            ("serial_add", json!({"path":"synthetic","baud":-9600})),
            ("serial_add", json!({"path":"synthetic","baud":299})),
            ("serial_add", json!({"path":"synthetic","baud":4000001})),
            ("serial_add", json!({"path":"synthetic","connect":"true"})),
            ("serial_connect", json!({"port":"","connected":true})),
            ("serial_install", json!({"port":"synthetic","package":""})),
            (
                "serial_pipe",
                json!({"port":"synthetic","client":" ","reason":"read"}),
            ),
            (
                "serial_pipe",
                json!({"port":"synthetic","client":"agent","reason":"read\nlog"}),
            ),
            (
                "serial_add",
                json!({"path":"synthetic","protocol":"guessed"}),
            ),
            ("serial_logs", json!({"port":"synthetic","limit":501})),
            ("serial_logs", json!({"port":"synthetic","after":-1})),
            ("serial_job", json!({"port":"synthetic","waitSeconds":1.5})),
            ("serial_status", json!({"wrong":"synthetic"})),
            ("gizmoth_status", json!({"extra":true})),
            (
                "serial_pipe",
                json!({"port":"synthetic","client":"x".repeat(65),"reason":"read"}),
            ),
        ] {
            assert!(cli_call(dir.path(), name, &args).is_err(), "{name}: {args}");
            let response = handle(
                dir.path(),
                &json!({
                    "jsonrpc":"2.0", "id":1, "method":"tools/call",
                    "params":{"name":name,"arguments":args}
                }),
            )
            .unwrap();
            assert_eq!(response["result"]["isError"], true, "{response}");
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn omitted_mcp_arguments_default_to_an_empty_object_only_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["gizmoth_status", "serial_list", "serial_system_ports"] {
            let response = handle(
                dir.path(),
                &json!({
                    "jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":name}
                }),
            )
            .unwrap();
            assert!(response["result"].get("isError").is_none(), "{response}");
            let expected = call(dir.path(), name, &json!({})).unwrap();
            assert_eq!(response["result"]["structuredContent"], expected);
            let explicit_null = handle(
                dir.path(),
                &json!({
                    "jsonrpc":"2.0", "id":2, "method":"tools/call",
                    "params":{"name":name,"arguments":null}
                }),
            )
            .unwrap();
            assert_eq!(explicit_null["result"]["isError"], true);
        }
        let required = handle(
            dir.path(),
            &json!({
                "jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{"name":"serial_status"}
            }),
        )
        .unwrap();
        assert_eq!(required["result"]["isError"], true);
        assert!(required["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("port is required"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn relative_firmware_paths_are_rejected_by_both_transports_without_service_state() {
        let dir = tempfile::tempdir().unwrap();
        for package in [
            "firmware.update.tar.zlib",
            "./firmware.update.tar.zlib",
            "../firmware.update.tar.zlib",
            "~/firmware.update.tar.zlib",
        ] {
            let args = json!({"port":"synthetic","package":package});
            let error = cli_call(dir.path(), "serial_install", &args).unwrap_err();
            assert!(error.contains("absolute path"), "{error}");
            let response = handle(
                dir.path(),
                &json!({
                    "jsonrpc":"2.0", "id":1, "method":"tools/call",
                    "params":{"name":"serial_install","arguments":args}
                }),
            )
            .unwrap();
            assert_eq!(response["result"]["isError"], true);
            assert!(response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("absolute path"));
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn schema_integers_normalize_before_typed_tool_dispatch() {
        let schema = tools()
            .into_iter()
            .find(|tool| tool["name"] == "serial_job")
            .unwrap()["inputSchema"]
            .clone();
        for representation in ["0", "0.0", "0e0", "30", "30.0", "30e0"] {
            let args: Value = serde_json::from_str(&format!(
                r#"{{"port":"synthetic","waitSeconds":{representation}}}"#
            ))
            .unwrap();
            let normalized = validate_args(&schema, &args).unwrap();
            let expected = if representation.starts_with('0') {
                0
            } else {
                30
            };
            assert_eq!(normalized["waitSeconds"].as_u64(), Some(expected));
        }
        for value in [json!(30.5), json!(121.0), json!(-1.0), json!(1e100)] {
            assert!(
                validate_args(&schema, &json!({"port":"synthetic","waitSeconds":value})).is_err()
            );
        }
        assert_eq!(
            integer_value(&json!(u64::MAX)).unwrap().as_u64(),
            Some(u64::MAX)
        );
        assert!(integer_value(&json!(18_446_744_073_709_551_616.0)).is_none());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            cli_call(
                dir.path(),
                "serial_job",
                &json!({"port":"synthetic","waitSeconds":0.0})
            )
            .unwrap()["service"],
            "stopped"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn cli_errors_write_only_stderr_and_fail() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            cli_output(Err("unknown tool".into()), &mut stdout, &mut stderr),
            1
        );
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"unknown tool\n");
        let mut stderr = Vec::new();
        assert_eq!(
            cli_output(
                Ok(json!({})),
                std::io::Cursor::new(&mut [0u8; 0][..]),
                &mut stderr
            ),
            1
        );
        assert!(String::from_utf8(stderr)
            .unwrap()
            .contains("Unable to write"));
    }

    #[test]
    fn outstanding_request_does_not_block_later_requests() {
        let input = std::io::Cursor::new("{\"id\":1}\n{\"id\":2}\ninvalid\n");
        let mut output = Vec::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let rx = Mutex::new(rx);
        serve(input, &mut output, |message| {
            if message["id"] == 1 {
                rx.lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
            } else {
                tx.send(()).unwrap();
            }
            Some(json!({ "id": message["id"] }))
        });
        let replies: Vec<Value> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 3);
        assert!(replies.iter().any(|reply| reply["id"] == 1));
        assert!(replies.iter().any(|reply| reply["id"] == 2));
        assert!(replies.iter().any(|reply| reply["error"]["code"] == -32700));
    }

    #[test]
    fn saturation_rejects_excess_requests_but_allows_cancellation() {
        let mut lines = String::new();
        for id in 0..=MAX_REQUESTS {
            lines.push_str(&format!(
                "{}\n",
                json!({"id": id, "method": "tools/call", "params": {"name": "serial_job"}})
            ));
        }
        lines.push_str(&format!(
            "{}\n",
            json!({"id": 100, "method": "tools/call", "params": {"name": "serial_cancel"}})
        ));
        let gate = (Mutex::new(false), std::sync::Condvar::new());
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let mut output = Vec::new();
        serve(std::io::Cursor::new(lines), &mut output, |message| {
            if message["params"]["name"] == "serial_cancel" {
                *gate.0.lock().unwrap() = true;
                gate.1.notify_all();
            } else {
                let count = active.fetch_add(1, Ordering::Relaxed) + 1;
                peak.fetch_max(count, Ordering::Relaxed);
                let mut released = gate.0.lock().unwrap();
                while !*released {
                    let (next, timeout) = gate
                        .1
                        .wait_timeout(released, std::time::Duration::from_secs(2))
                        .unwrap();
                    assert!(!timeout.timed_out(), "cancellation was not processed");
                    released = next;
                }
                active.fetch_sub(1, Ordering::Relaxed);
            }
            Some(json!({"jsonrpc": "2.0", "id": message["id"], "result": {}}))
        });
        let replies: Vec<Value> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), MAX_REQUESTS + 2);
        assert!(replies
            .iter()
            .any(|reply| reply["id"] == MAX_REQUESTS && reply["error"]["code"] == -32000));
        assert!(replies
            .iter()
            .any(|reply| reply["id"] == 100 && reply.get("result").is_some()));
        assert!(peak.load(Ordering::Relaxed) <= MAX_REQUESTS);
    }

    #[test]
    fn successful_tool_results_keep_json_text_and_only_object_structured_content() {
        for value in [
            json!({ "ports": [{ "path": "/synthetic/port", "name": "Console" }] }),
            json!([{ "path": "/synthetic/port" }]),
            json!("ready"),
            json!(42),
            json!(true),
            Value::Null,
        ] {
            let result = tool_result(Ok(value.clone()));
            assert_eq!(result["content"][0]["type"], "text");
            let text = result["content"][0]["text"].as_str().unwrap();
            assert_eq!(serde_json::from_str::<Value>(text).unwrap(), value);
            if value.is_object() {
                assert!(result["structuredContent"].is_object());
                assert_eq!(result["structuredContent"], value);
            } else {
                assert!(result.get("structuredContent").is_none());
            }
            assert!(result.get("isError").is_none());
        }
    }

    #[test]
    fn tool_errors_remain_valid_text_content() {
        let result = tool_result(Err("ambiguous port".into()));
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "ambiguous port");
        assert!(result.get("structuredContent").is_none());
    }

    #[test]
    fn discovery_while_stopped_returns_an_object_without_starting_a_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let response = handle(
            dir.path(),
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "serial_system_ports", "arguments": {} } }),
        )
        .unwrap();
        let result = &response["result"];
        assert!(result["structuredContent"].is_object());
        assert_eq!(result["structuredContent"]["service"], "stopped");
        assert_eq!(result["structuredContent"]["ports"], json!([]));
        assert!(!dir.path().join("serial/daemon.json").exists());
    }

    #[test]
    fn global_server_lists_tool_groups_and_reports_stopped_service() {
        let dir = tempfile::tempdir().unwrap();
        let init = handle(
            dir.path(),
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
        )
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "gizmoth");
        let list = handle(
            dir.path(),
            &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "gizmoth_status",
            "serial_list",
            "serial_install",
            "serial_job",
            "serial_cancel",
        ] {
            assert!(names.contains(&expected), "{expected}");
        }
        assert!(handle(
            dir.path(),
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
        )
        .is_none());
        let status = handle(
            dir.path(),
            &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "gizmoth_status", "arguments": {} } }),
        )
        .unwrap();
        assert_eq!(
            status["result"]["structuredContent"]["serial"]["running"],
            false
        );
        let list = handle(
            dir.path(),
            &json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "serial_list", "arguments": {} } }),
        )
        .unwrap();
        assert_eq!(list["result"]["structuredContent"]["service"], "stopped");
    }
}
