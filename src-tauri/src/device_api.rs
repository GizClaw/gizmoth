use crate::{
    credentials,
    project::{self, Project},
};
use reqwest::{Client, Response};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub(crate) fn client() -> Result<Client, String> {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "无法初始化网络连接".into())
}

fn network_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "请求超时；操作可能已被设备执行，请刷新设备程序列表确认后再操作。".into()
    } else {
        "无法连接设备服务，请检查网络和设备服务地址。".into()
    }
}

pub(crate) async fn decode(mut response: Response) -> Result<Value, String> {
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 | 403 => "设备凭据无效或没有权限，请重新绑定。".into(),
            404 => "设备服务或程序不存在。".into(),
            409 => "设备离线或忙碌，请稍后刷新。".into(),
            413 => "程序包超过服务器限制，请导出后通过设备可访问的 URL 安装。".into(),
            501 => "当前固件不支持此功能，请更新设备固件。".into(),
            504 => "设备响应超时；操作可能已执行，请刷新设备程序列表确认。".into(),
            status => format!("设备服务返回 HTTP {status}，请检查设备状态。"),
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err("设备响应超过大小限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "设备返回了无效 JSON".into())
}

async fn invoke(
    app: &tauri::AppHandle,
    device_id: &str,
    tool: &str,
    args: Value,
) -> Result<Value, String> {
    let (device, key) = credentials::get(app, device_id)?;
    let response = client()?
        .post(format!(
            "{}/gizclaw/v1/device/tool/v0/invoke",
            device.base_url
        ))
        .bearer_auth(key.as_str())
        .json(&json!({"tool": tool, "args": args}))
        .send()
        .await
        .map_err(network_error)?;
    let body = decode(response).await?;
    body.get("result")
        .filter(|v| v.is_object())
        .cloned()
        .ok_or("设备返回格式不匹配".into())
}

pub async fn list(app: &tauri::AppHandle, device_id: &str) -> Result<Value, String> {
    let result = invoke(app, device_id, "lua.app.list", json!({})).await?;
    if !result.get("apps").is_some_and(Value::is_array) {
        return Err("设备未返回程序列表".into());
    }
    Ok(result)
}

pub async fn install(
    app: &tauri::AppHandle,
    device_id: &str,
    project: Project,
) -> Result<Value, String> {
    let (config, bytes) = project::package(&project)?;
    if bytes.len() > 512 * 1024 {
        return Err(
            "压缩包超过 512 KiB，无法直接上传。请导出后使用设备可访问的 HTTP(S) URL 安装。".into(),
        );
    }
    let (device, key) = credentials::get(app, device_id)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let response = client()?
        .post(format!(
            "{}/gizclaw/v1/device/lua-app/install",
            device.base_url
        ))
        .query(&[
            ("content_length", bytes.len().to_string()),
            ("sha256", hash),
        ])
        .bearer_auth(key.as_str())
        .header("Content-Type", "application/octet-stream")
        .body(bytes)
        .send()
        .await
        .map_err(network_error)?;
    let body = decode(response).await?;
    if body.pointer("/app/app_id").and_then(Value::as_str) != Some(config.app_id.as_str()) {
        return Err("安装响应与当前程序不匹配，请刷新程序列表确认。".into());
    }
    Ok(body)
}

pub async fn run(app: &tauri::AppHandle, device_id: &str, app_id: &str) -> Result<Value, String> {
    if !project::valid_id(app_id) {
        return Err("程序 ID 无效".into());
    }
    invoke(app, device_id, "lua.app.run", json!({"app_id": app_id})).await
}
