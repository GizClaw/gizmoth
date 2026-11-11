//! Device command protocol carried by an exclusively granted WebSocket.
//! Discovery is read-only; connecting requests ownership and disconnecting releases it.
use serde::Deserialize;
use serde_json::{json, Value};

pub const PROTOCOL: &str = "gizmoth.device.v1";
pub const MAX_COMMANDS: usize = 4096;

/// Older services use raw byte piping on the same URL. Never advertise command
/// schemas against one: an old service would write command JSON to the device.
pub fn ensure_protocol(snapshot: &Value) -> Result<(), String> {
    if snapshot["deviceWebSocketProtocol"] == PROTOCOL {
        Ok(())
    } else {
        Err("The serial service does not support gizmoth.device.v1; update or restart GizMoth before connecting. Legacy raw piping is not compatible with device commands".into())
    }
}

pub fn commands() -> Vec<Value> {
    let job_id = json!({"type":"string", "format":"uuid", "description":"Optional client-generated UUID. Preserve it after an ambiguous response; query the job before retrying."});
    let definitions = [
        (
            "send",
            "Send text to a plain serial device",
            json!({
                "text": {"type":"string", "maxLength":4096},
                "lineEnding": {"type":"string", "enum":["lf","crlf","cr","none"], "default":"lf"}
            }),
            vec!["text"],
        ),
        (
            "install",
            "Install a local H2Loader firmware package; returns a job",
            json!({
                "package": {"type":"string", "minLength":1, "maxLength":4096, "description":"Absolute .update.tar.zlib path on the service computer; local connections only"},
                "stageOnly": {"type":"boolean", "default":false},
                "jobId": job_id.clone()
            }),
            vec!["package"],
        ),
        (
            "reboot",
            "Reboot an H2Loader device and verify reconnection; returns a job",
            json!({
                "target": {"type":"string", "enum":["app","loader"]}, "jobId":job_id.clone()
            }),
            vec!["target"],
        ),
        (
            "refresh",
            "Request fresh H2Loader status; returns a job",
            json!({"jobId":job_id.clone()}),
            vec![],
        ),
        (
            "cancel",
            "Cancel this connection's job at its next safe I/O boundary",
            json!({"jobId":job_id}),
            vec!["jobId"],
        ),
    ];
    definitions.into_iter().map(|(name, description, properties, required)| json!({
        "name":name, "description":description,
        "inputSchema": {"type":"object", "properties":properties, "required":required, "additionalProperties":false}
    })).collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: Value,
    pub method: String,
    #[serde(default = "empty_params")]
    pub params: Value,
}
fn empty_params() -> Value {
    json!({})
}

pub fn parse(text: &str) -> Result<Request, (Value, String)> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| (Value::Null, error.to_string()))?;
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let request: Request =
        serde_json::from_value(value).map_err(|error| (id.clone(), error.to_string()))?;
    let valid_id = match &request.id {
        Value::String(text) => {
            !text.is_empty() && text.chars().count() <= 128 && !text.chars().any(char::is_control)
        }
        Value::Number(number) => number.is_i64() || number.is_u64(),
        _ => false,
    };
    if !valid_id {
        return Err((
            id,
            "id must be a nonempty string (at most 128 characters) or integer".into(),
        ));
    }
    validate(&request.method, &request.params).map_err(|error| (id, error))?;
    Ok(request)
}

fn validate(method: &str, params: &Value) -> Result<(), String> {
    let definition = commands()
        .into_iter()
        .find(|definition| definition["name"] == method)
        .ok_or_else(|| format!("Unknown device command {method}"))?;
    let schema = &definition["inputSchema"];
    let params = params.as_object().ok_or("params must be an object")?;
    for key in schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
    {
        if !params.contains_key(key) {
            return Err(format!("{key} is required"));
        }
    }
    for (key, value) in params {
        let rule = schema["properties"]
            .get(key)
            .ok_or_else(|| format!("Unknown command parameter {key}"))?;
        match rule["type"].as_str() {
            Some("string") if value.is_string() => {}
            Some("boolean") if value.is_boolean() => {}
            _ => return Err(format!("Invalid type for {key}")),
        }
        if rule["enum"]
            .as_array()
            .is_some_and(|options| !options.contains(value))
        {
            return Err(format!("Invalid value for {key}"));
        }
        if let Some(text) = value.as_str() {
            let length = text.chars().count() as u64;
            if rule["minLength"].as_u64().is_some_and(|min| length < min)
                || rule["maxLength"].as_u64().is_some_and(|max| length > max)
            {
                return Err(format!("Invalid length for {key}"));
            }
            if rule["format"] == "uuid" && uuid::Uuid::parse_str(text).is_err() {
                return Err(format!("{key} must be a UUID"));
            }
        }
    }
    if method == "install"
        && !std::path::Path::new(params["package"].as_str().unwrap()).is_absolute()
    {
        return Err("package must be an absolute path on the service computer".into());
    }
    Ok(())
}

pub fn result(id: Value, outcome: Result<Value, (u16, String)>, pending: bool) -> Value {
    match outcome {
        Ok(result) => json!({"type":"result", "id":id, "ok":true, "result":result}),
        Err((code, message)) => {
            let mut result = json!({"type":"result", "id":id, "ok":false, "error":{"code":code,"message":message}});
            if pending {
                result["state"] = json!("pending");
            }
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_raw_services_cannot_be_used_as_command_transports() {
        assert!(ensure_protocol(&json!({"pipeBase":"ws://127.0.0.1:7420/v1/ports"})).is_err());
        assert!(ensure_protocol(&json!({"deviceWebSocketProtocol":"other"})).is_err());
        assert!(ensure_protocol(&json!({"deviceWebSocketProtocol":PROTOCOL})).is_ok());
    }

    #[test]
    fn commands_are_strict_and_never_treat_json_as_device_bytes() {
        for frame in [
            json!({"id":"s", "method":"send", "params":{"text":"status","lineEnding":"lf"}}),
            json!({"id":1, "method":"reboot", "params":{"target":"app"}}),
            json!({"id":"r", "method":"refresh"}),
        ] {
            assert!(parse(&frame.to_string()).is_ok());
        }
        for frame in [
            json!({"method":"send", "params":{"text":"status"}}),
            json!({"id":null, "method":"refresh"}),
            json!({"id":"x", "method":"send", "params":{"text":17}}),
            json!({"id":"x", "method":"send", "params":{"text":"status","lease":1}}),
            json!({"id":"x", "method":"reboot", "params":{"target":"guessed"}}),
            json!({"id":"x", "method":"install", "params":{"package":"relative.pkg"}}),
            json!({"id":"x", "method":"cancel", "params":{"jobId":"invalid"}}),
            json!({"id":"x", "method":"refresh", "params":null}),
            json!({"id":"x", "method":"refresh", "port":"another"}),
            json!({"id":"x", "method":"take"}),
            json!({"id":"x", "method":"release"}),
        ] {
            assert!(parse(&frame.to_string()).is_err(), "{frame}");
        }
        assert!(parse("status\n").is_err());
    }
}
