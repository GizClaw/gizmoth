use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, sync::Mutex};
use tauri::Manager;
use url::Url;
use zeroize::Zeroizing;

const SERVICE: &str = "com.gizclaw.gizmoth.devices";
const INVALID: &str = "设备凭据链接无效，请使用设备提供的 HTTPS API key 链接。";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub icon: Option<String>,
}

pub struct Credential {
    pub base_url: String,
    pub name: String,
    pub key: Zeroizing<String>,
    pub icon: Option<String>,
}

pub struct DeviceStore(pub Mutex<()>);

pub fn parse(raw: &str) -> Result<Credential, String> {
    if raw.is_empty()
        || raw.len() > 1024
        || raw
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(INVALID.into());
    }
    let url = Url::parse(raw).map_err(|_| INVALID)?;
    let key = url.path().strip_prefix("/api-keys/").ok_or(INVALID)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || key.len() > 95
        || !key.starts_with("gizclaw_sk_v1_")
        || key.len() <= 14
        || !key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(INVALID.into());
    }
    let mut name = None;
    let mut icon = None;
    for pair in url
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|s| !s.is_empty())
    {
        let (field, value) = pair.split_once('=').ok_or(INVALID)?;
        let decode = |part: &str| -> Result<String, String> {
            let bytes = part.as_bytes();
            for (index, byte) in bytes.iter().enumerate() {
                if *byte == b'%'
                    && (index + 2 >= bytes.len()
                        || !bytes[index + 1].is_ascii_hexdigit()
                        || !bytes[index + 2].is_ascii_hexdigit())
                {
                    return Err(INVALID.into());
                }
            }
            percent_encoding::percent_decode_str(&part.replace('+', " "))
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(|_| INVALID.into())
        };
        let field = decode(field)?;
        let value = decode(value)?;
        match field.as_str() {
            "name"
                if name.is_none()
                    && !value.trim().is_empty()
                    && value.chars().count() <= 40
                    && !value.chars().any(char::is_control) =>
            {
                name = Some(value.to_string())
            }
            "icon"
                if icon.is_none()
                    && !value.is_empty()
                    && value.len() <= 32
                    && value.as_bytes()[0].is_ascii_alphanumeric()
                    && value.bytes().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-'
                    }) =>
            {
                icon = Some(value.to_string())
            }
            _ => return Err(INVALID.into()),
        }
    }
    Ok(Credential {
        base_url: url.origin().ascii_serialization(),
        name: name.unwrap_or_else(|| "我的设备".into()),
        key: Zeroizing::new(key.to_owned()),
        icon,
    })
}

fn registry(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?;
    fs::create_dir_all(&dir).map_err(|_| "无法创建应用数据目录")?;
    Ok(dir.join("devices.json"))
}

pub fn list(app: &tauri::AppHandle) -> Result<Vec<Device>, String> {
    let path = registry(app)?;
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| "设备列表损坏，请检查应用数据目录。".into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(_) => Err("无法读取设备列表".into()),
    }
}

fn save(app: &tauri::AppHandle, devices: &[Device]) -> Result<(), String> {
    let path = registry(app)?;
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(devices).map_err(|_| "无法序列化设备列表")?;
    fs::write(&temporary, bytes).map_err(|_| "无法保存设备列表")?;
    fs::rename(&temporary, path).map_err(|_| "无法替换设备列表".into())
}

fn entry(id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, id).map_err(|_| "系统凭据存储不可用".into())
}

pub fn import(app: &tauri::AppHandle, credential: Credential) -> Result<Device, String> {
    let state = app.state::<DeviceStore>();
    let _guard = state.0.lock().map_err(|_| "设备存储正忙")?;
    let mut hash = Sha256::new();
    hash.update(credential.base_url.as_bytes());
    hash.update(b"\n");
    hash.update(credential.key.as_bytes());
    let id = format!("{:x}", hash.finalize());
    let mut devices = list(app)?;
    let device = Device {
        id,
        name: credential.name,
        base_url: credential.base_url,
        icon: credential.icon,
    };
    let existing = devices.iter().any(|d| d.id == device.id);
    entry(&device.id)?
        .set_password(&credential.key)
        .map_err(|_| "无法写入系统凭据存储，请允许钥匙串访问。")?;
    devices.retain(|d| d.id != device.id);
    devices.push(device.clone());
    if let Err(error) = save(app, &devices) {
        if !existing {
            let _ = entry(&device.id)?.delete_credential();
        }
        return Err(error);
    }
    Ok(device)
}

pub fn get(app: &tauri::AppHandle, id: &str) -> Result<(Device, Zeroizing<String>), String> {
    let device = list(app)?
        .into_iter()
        .find(|d| d.id == id)
        .ok_or("设备未绑定")?;
    let key = entry(id)?
        .get_password()
        .map_err(|_| "无法读取设备凭据，请重新绑定或允许钥匙串访问。")?;
    Ok((device, Zeroizing::new(key)))
}

pub fn remove(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let state = app.state::<DeviceStore>();
    let _guard = state.0.lock().map_err(|_| "设备存储正忙")?;
    let mut devices = list(app)?;
    if !devices.iter().any(|d| d.id == id) {
        return Err("设备未绑定".into());
    }
    match entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => (),
        Err(_) => return Err("无法删除系统凭据".into()),
    }
    devices.retain(|d| d.id != id);
    save(app, &devices)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_is_parsed_without_fetching() {
        let c = parse("https://example.com/api-keys/gizclaw_sk_v1_test?name=My+device").unwrap();
        assert_eq!(c.base_url, "https://example.com");
        assert_eq!(c.name, "My device");
        assert_eq!(&*c.key, "gizclaw_sk_v1_test");
    }
    #[test]
    fn matches_shared_binding_url_fixtures() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/ble-binding-v1.json")).unwrap();
        for case in fixture["url_cases"].as_array().unwrap() {
            let credential = parse(case["url"].as_str().unwrap()).unwrap();
            assert_eq!(credential.base_url, case["origin"].as_str().unwrap());
            assert_eq!(credential.key.as_str(), case["secret"].as_str().unwrap());
            let name = case["name"].as_str().unwrap();
            if !name.is_empty() {
                assert_eq!(credential.name, name);
            }
        }
    }
    #[test]
    fn rejects_malformed_query_encoding() {
        for suffix in [
            "?name=%ZZ",
            "?name=%FF",
            "?name",
            "?icon=Upper",
            "?name=%00",
        ] {
            assert!(parse(&format!(
                "https://example.com/api-keys/gizclaw_sk_v1_test{suffix}"
            ))
            .is_err());
        }
    }
    #[test]
    fn rejects_unsafe_credentials_and_redacts_errors() {
        for raw in [
            "http://example.com/api-keys/gizclaw_sk_v1_secret",
            "https://user@example.com/api-keys/gizclaw_sk_v1_secret",
            "https://example.com/api-keys/gizclaw_sk_v1_secret#x",
            "https://example.com/api-keys/gizclaw_sk_v1_secret?name=A&name=B",
            "https://example.com/api-keys/gizclaw_sk_v1_secret?extra=x",
        ] {
            assert_eq!(parse(raw).err().unwrap(), INVALID);
        }
    }
}
