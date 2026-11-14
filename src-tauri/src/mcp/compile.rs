//! Read-only compile tool group: slots, queue, caches, build artifacts and the
//! compile WebSocket discovery. Builds themselves run on that WebSocket.
use super::Outcome;
use crate::serial::{client, gateway, state_dir};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

const SHORT: Duration = Duration::from_secs(10);

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
        tool("compile_status", "Compile service status",
            "GizMoth's Linux build VMs: slots (parallel builds) and their state, queued and recent builds, the system image, and shared cache sizes (Bazel remote cache, ccache, custom). Does not start the service.",
            json!({}), &[]),
        tool("compile_pipe", "Discover compile WebSocket",
            concat!(
                "Read-only discovery of the compile WebSocket URL, authentication headers and request schema (protocol gizmoth.compile.v1). ",
                "Connect with the headers and send one JSON text frame {\"id\":\"b1\",\"method\":\"build\",\"params\":{\"workspace\":\"/abs/project\",\"command\":[\"bazel\",\"build\",\"//app:fw\"],\"artifacts\":[\"bazel-bin/**/*.update.tar.zlib\"]}}. ",
                "The workspace is synchronized incrementally into a Linux VM (Ubuntu, arm64) and the command runs there as user builder with shared Bazel and ccache caches. ",
                "Queued sockets receive {\"type\":\"queued\",\"position\":n}; then status/started/synced/exit/artifact events as JSON text and logs as binary frames, ending with {\"type\":\"finished\"}. ",
                "Keep the socket open: closing it cancels the build, also while queued. Collected artifacts get an ID and absolute paths; install one on a device with the device WebSocket's install {\"artifact\":\"<id>\"}."
            ),
            json!({
                "client": { "type": "string", "minLength": 1, "maxLength": 64, "description": "Client name shown in GizMoth, e.g. claude-code" },
                "title": { "type": "string", "minLength": 1, "maxLength": 120, "description": "What this build is for, shown in the queue" },
            }),
            &["client", "title"]),
        tool("compile_artifacts", "List build artifacts",
            "Build outputs collected from compile VMs, newest first, with artifact ID, source job, workspace, command and each file's absolute path, size and SHA-256. Agents share these; pass the ID to the device WebSocket install command.",
            json!({
                "workspace": { "type": "string", "description": "Only artifacts built from this absolute workspace path" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20 },
            }),
            &[]),
        tool("compile_job", "Read a build",
            "Status of one build and a page of its log. Without `after`, returns the log tail; pass the returned `next` as `after` to continue.",
            json!({
                "job": { "type": "string", "minLength": 1, "description": "Job ID from the compile WebSocket or compile_status" },
                "after": { "type": "integer", "minimum": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536 },
            }),
            &["job"]),
    ]
}

fn text<'a>(args: &'a Value, key: &str, max: usize) -> Result<&'a str, String> {
    let value = args[key].as_str().map(str::trim).unwrap_or_default();
    if value.is_empty() || value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(format!("{key} must be 1–{max} printable characters"));
    }
    Ok(value)
}

fn stopped() -> Value {
    json!({
        "service": "stopped",
        "note": "GizMoth's service is not running. Open GizMoth (Compiling) or enable Settings > Service; MCP/CLI queries do not start it.",
    })
}

pub fn call(data_dir: &Path, name: &str, args: &Value) -> Outcome {
    if name == "compile_pipe" {
        text(args, "client", 64)?;
        text(args, "title", 120)?;
    }
    let Some(conn) = client::running(data_dir) else {
        return Ok(stopped());
    };
    match name {
        "compile_status" => {
            let mut snapshot = conn.request("GET", "/v1/compile", None, SHORT)?;
            if let Some(map) = snapshot.as_object_mut() {
                map.remove("settings");
            }
            snapshot["service"] = "running".into();
            Ok(snapshot)
        }
        "compile_artifacts" => {
            let list = conn.request("GET", "/v1/compile/artifacts", None, SHORT)?;
            let workspace = args["workspace"].as_str();
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            let artifacts: Vec<Value> = list
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|a| {
                    workspace
                        .is_none_or(|w| a["workspace"].as_str() == Some(w.trim_end_matches('/')))
                })
                .take(limit)
                .collect();
            Ok(json!({ "service": "running", "artifacts": artifacts }))
        }
        "compile_job" => {
            let id = text(args, "job", 64)?;
            if uuid::Uuid::parse_str(id).is_err() {
                return Err("job must be a job ID".into());
            }
            let mut job = conn.request("GET", &format!("/v1/compile/jobs/{id}"), None, SHORT)?;
            let mut path = format!(
                "/v1/compile/jobs/{id}/log?limit={}",
                args["limit"].as_u64().unwrap_or(65536)
            );
            if let Some(after) = args["after"].as_u64() {
                path.push_str(&format!("&after={after}"));
            }
            job["log"] = conn.request("GET", &path, None, SHORT)?;
            Ok(job)
        }
        "compile_pipe" => {
            let health = conn.request("GET", "/v1/ports", None, SHORT)?;
            let base = health["pipeBase"].as_str().ok_or(
                "The service did not advertise a WebSocket URL; update GizMoth and enable Service",
            )?;
            let base = base.strip_suffix("/v1/ports").unwrap_or(base);
            let key = gateway::read(&state_dir(data_dir))
                .filter(|config| config.enabled)
                .map(|config| config.api_key)
                .ok_or(
                    "GizMoth's Service is off; ask the user to enable it in Settings > Service",
                )?;
            let encode = |value: &str| {
                percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
            };
            let snapshot = conn.request("GET", "/v1/compile", None, SHORT)?;
            Ok(json!({
                "url": format!("{base}/v1/compile/ws?client={}&title={}", encode(text(args, "client", 64)?), encode(text(args, "title", 120)?)),
                "headers": { "Authorization": format!("Bearer {key}") },
                "protocol": crate::compile::PROTOCOL,
                "commands": crate::compile::ws::commands(),
                "available": snapshot["available"],
                "image": snapshot["image"]["status"],
                "slots": snapshot["slots"].as_array().map_or(0, Vec::len),
                "queued": snapshot["queue"].as_array().map_or(0, Vec::len),
                "note": "Discovery only. One build per socket; closing the socket cancels it. The first build may first prepare the Linux system image (a few minutes).",
            }))
        }
        _ => Err(format!("Unknown tool {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_never_start_a_stopped_service() {
        let dir = tempfile::tempdir().unwrap();
        for (name, args) in [
            ("compile_status", json!({})),
            ("compile_artifacts", json!({})),
            (
                "compile_pipe",
                json!({"client":"agent","title":"Build firmware"}),
            ),
        ] {
            assert_eq!(call(dir.path(), name, &args).unwrap()["service"], "stopped");
        }
        assert!(call(
            dir.path(),
            "compile_pipe",
            &json!({"client":"","title":"x"})
        )
        .is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
