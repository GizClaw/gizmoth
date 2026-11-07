use crate::{
    credentials,
    device_api::{client, decode},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, time::Duration};

#[derive(Clone, Serialize, Deserialize)]
pub struct Hardware {
    pub id: String,
    pub hwd: String,
    pub description: Option<String>,
}
const TYPES: &[&str] = &[
    "speaker", "display", "led", "battery", "wifi", "ble", "mic", "modem",
];

fn manifest(value: Value) -> Result<Vec<Hardware>, String> {
    let entries = value
        .get("devices")
        .and_then(Value::as_array)
        .ok_or("设备能力列表格式不匹配")?;
    if entries.len() > 64 {
        return Err("设备能力列表超过限制".into());
    }
    let mut ids = HashSet::new();
    let mut result = vec![];
    for value in entries {
        let item: Hardware =
            serde_json::from_value(value.clone()).map_err(|_| "设备能力信息无效")?;
        if item.id.is_empty() || item.id.len() > 128 || !ids.insert(item.id.clone()) {
            return Err("设备能力 ID 无效或重复".into());
        }
        if TYPES.contains(&item.hwd.as_str()) {
            result.push(item);
        }
    }
    Ok(result)
}

async fn get(client: &reqwest::Client, base: &str, key: &str, path: &str) -> Result<Value, String> {
    let response = client
        .get(format!("{base}/gizclaw/v1/device{path}"))
        .bearer_auth(key)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| "读取设备状态失败，请检查网络后刷新。")?;
    decode(response).await
}

fn checked_result(instance: &Hardware, result: Value) -> Result<Value, String> {
    if result["id"].as_str() != Some(instance.id.as_str())
        || result["hwd"].as_str() != Some(instance.hwd.as_str())
        || !result["value"].is_object()
    {
        return Err("设备返回了不匹配的状态，请刷新。".into());
    }
    Ok(result["value"].clone())
}

pub async fn overview(app: &tauri::AppHandle, id: &str) -> Result<Value, String> {
    let (device, key) = credentials::get(app, id)?;
    let client = client()?;
    let (runtime, status, capabilities) = tokio::join!(
        get(&client, &device.base_url, &key, "/runtime"),
        get(&client, &device.base_url, &key, "/status"),
        get(&client, &device.base_url, &key, "/mhs/v0/manifest")
    );
    let mut errors = vec![];
    let runtime = runtime
        .inspect_err(|e| {
            errors.push(e.clone());
        })
        .unwrap_or(Value::Null);
    let status = status
        .inspect_err(|e| {
            errors.push(e.clone());
        })
        .unwrap_or(Value::Null);
    let instances = capabilities
        .and_then(manifest)
        .inspect_err(|e| {
            errors.push(e.clone());
        })
        .unwrap_or_default();
    let online = runtime.get("online").and_then(Value::as_bool);
    if online.is_none() && !runtime.is_null() {
        errors.push("设备未返回有效在线状态".into());
    }
    let mut pending = tokio::task::JoinSet::new();
    if online == Some(true) {
        for instance in &instances {
            let instance = instance.clone();
            let request = client
                .post(format!("{}/gizclaw/v1/device/mhs/v0/read", device.base_url))
                .bearer_auth(key.as_str())
                .timeout(Duration::from_secs(15))
                .json(&json!({"id":instance.id,"hwd":instance.hwd}));
            pending.spawn(async move {
                let result = match request.send().await {
                    Ok(response) => decode(response)
                        .await
                        .and_then(|v| checked_result(&instance, v)),
                    Err(_) => Err("读取失败，请刷新重试".into()),
                };
                match result {
                    Ok(value) => json!({"instance":instance,"value":value}),
                    Err(error) => json!({"instance":instance,"error":error}),
                }
            });
        }
    }
    let mut hardware = vec![];
    while let Some(result) = pending.join_next().await {
        hardware.push(result.map_err(|_| "设备状态读取中断")?);
    }
    if online != Some(true) {
        hardware = instances.iter().map(|instance| json!({"instance":instance,"error":if online == Some(false) {"设备离线"} else {"连接状态未知，请刷新"}})).collect();
    }
    hardware.sort_by(|a, b| {
        a["instance"]["id"]
            .as_str()
            .cmp(&b["instance"]["id"].as_str())
    });
    Ok(
        json!({"online":online,"lastSeen":runtime.get("last_seen_at").and_then(Value::as_str),"firmwareVersion":status.get("firmware_version").and_then(Value::as_str),"hardware":hardware,"errors":errors}),
    )
}

fn level_value(hwd: &str, level: u8) -> Result<Value, String> {
    if level > 100 {
        return Err("数值必须在 0–100 之间".into());
    }
    match hwd {
        "speaker" => Ok(json!({"volume_percent":level})),
        "display" | "led" if level.is_multiple_of(10) => Ok(json!({"brightness_percent":level})),
        _ => Err("此设置不支持写入，或亮度不是 10% 档位。".into()),
    }
}

pub async fn set_level(
    app: &tauri::AppHandle,
    device_id: &str,
    instance: Hardware,
    level: u8,
) -> Result<Value, String> {
    let value = level_value(&instance.hwd, level)?;
    let (device, key) = credentials::get(app, device_id)?;
    let client = client()?;
    let instances = manifest(get(&client, &device.base_url, &key, "/mhs/v0/manifest").await?)?;
    if !instances
        .iter()
        .any(|i| i.id == instance.id && i.hwd == instance.hwd)
    {
        return Err("设备已不再声明此设置，请刷新。".into());
    }
    let response = client
        .post(format!(
            "{}/gizclaw/v1/device/mhs/v0/write",
            device.base_url
        ))
        .bearer_auth(key.as_str())
        .timeout(Duration::from_secs(20))
        .json(&json!({"id":instance.id,"hwd":instance.hwd,"value":value}))
        .send()
        .await
        .map_err(|_| "设置结果未能确认；请刷新状态后再操作，不要立即重试。")?;
    let result = checked_result(&instance, decode(response).await?)?;
    let field = if instance.hwd == "speaker" {
        "volume_percent"
    } else {
        "brightness_percent"
    };
    if !result[field].as_u64().is_some_and(|v| v <= 100) {
        return Err("设备没有返回有效的设置值，请刷新确认。".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_and_levels_are_validated() {
        assert!(manifest(
            json!({"devices":[{"id":"x","hwd":"speaker"},{"id":"x","hwd":"display"}]})
        )
        .is_err());
        assert_eq!(
            manifest(json!({"devices":[{"id":"x","hwd":"unknown"}]}))
                .unwrap()
                .len(),
            0
        );
        assert!(level_value("battery", 50).is_err());
        assert!(level_value("display", 45).is_err());
        assert!(level_value("speaker", 101).is_err());
        assert_eq!(
            level_value("speaker", 43).unwrap(),
            json!({"volume_percent":43})
        );
        let instance = Hardware {
            id: "speaker.main".into(),
            hwd: "speaker".into(),
            description: None,
        };
        assert!(
            checked_result(&instance, json!({"id":"other","hwd":"speaker","value":{}})).is_err()
        );
    }
}
