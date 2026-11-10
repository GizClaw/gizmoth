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
};

pub const FLAG: &str = "--mcp";

const INSTRUCTIONS: &str = "GizMoth on this computer. Serial tools (serial_*) manage the serial ports GizMoth's resident serial service owns: list ports and what each is doing, read H2Loader device status and logs, connect or disconnect a port, install a firmware package, reboot a device and send text to a plain serial port. Every call goes through the service, which is the only process that opens ports, so never open the same port yourself. Installing firmware or rebooting changes the device: confirm the target port and package with the user first, then follow the job with serial_job.";

type Outcome = Result<Value, String>;

fn tools() -> Vec<Value> {
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

fn call(data_dir: &Path, name: &str, args: &Value) -> Outcome {
    match name {
        "gizmoth_status" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "serial": serial::service(data_dir),
        })),
        _ if name.starts_with("serial_") => serial::call(data_dir, name, args),
        _ => Err(format!("Unknown tool {name}")),
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
            let args = &message["params"]["arguments"];
            Ok(tool_result(call(data_dir, name, args)))
        }
        _ => Err(json!({ "code": -32601, "message": format!("Method not found: {method}") })),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

pub fn run(data_dir: &Path) -> i32 {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(data_dir, &message),
            Err(_) => Some(
                json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }),
            ),
        };
        if let Some(reply) = reply {
            if writeln!(stdout, "{reply}")
                .and_then(|_| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

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
