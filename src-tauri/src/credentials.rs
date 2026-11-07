use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};
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
        || !valid_key(key)
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

fn valid_key(key: &str) -> bool {
    key.len() <= 95
        && key.len() > 14
        && key.starts_with("gizclaw_sk_v1_")
        && key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub has_key: bool,
    pub needs_migration: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationResult {
    pub legacy_removed: bool,
}

fn app_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录".into())
}

const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;
fn read_devices(root: &Path) -> Result<Vec<Device>, String> {
    let path = root.join("devices.json");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Ok(meta) if meta.file_type().is_file() && meta.len() <= MAX_REGISTRY_BYTES => (),
        _ => return Err("设备列表文件无效".into()),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc_nofollow());
    }
    let file = options.open(path).map_err(|_| "无法读取设备列表")?;
    let mut bytes = Vec::new();
    file.take(MAX_REGISTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取设备列表")?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err("设备列表文件过大".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "设备列表损坏，请检查应用数据目录。".into())
}
pub fn list(app: &tauri::AppHandle) -> Result<Vec<Device>, String> {
    read_devices(&app_dir(app)?)
}
fn save_devices(root: &Path, devices: &[Device]) -> Result<(), String> {
    let path = root.join("devices.json");
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.file_type().is_file() {
            return Err("设备列表文件无效".into());
        }
    }
    let bytes = serde_json::to_vec_pretty(devices).map_err(|_| "无法序列化设备列表")?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err("设备列表文件过大".into());
    }
    atomic_write(&path, &bytes)
}

fn entry(id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, id).map_err(|_| "系统凭据存储不可用".into())
}

// IDs are hashes, so they cannot escape the private directory.
fn secret_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err("设备编号无效".into());
    }
    let dir = root.join("private");
    match fs::symlink_metadata(&dir) {
        Ok(meta) if !meta.file_type().is_dir() => return Err("设备凭据目录无效".into()),
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(root).map_err(|_| "无法创建应用数据目录")?;
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&dir).map_err(|_| "无法创建设备凭据目录")?;
        }
        Err(_) => return Err("无法访问设备凭据目录".into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| "无法保护设备凭据目录")?;
    }
    Ok(dir.join(id))
}

fn read_secret(root: &Path, id: &str) -> Result<Option<Zeroizing<String>>, String> {
    let path = secret_path(root, id)?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.file_type().is_file() && meta.len() <= 95 => (),
        _ => return Err("设备凭据文件无效".into()),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc_nofollow());
    }
    let file = options.open(path).map_err(|_| "无法读取设备凭据")?;
    let mut value = Zeroizing::new(String::new());
    file.take(96)
        .read_to_string(&mut value)
        .map_err(|_| "设备凭据文件无效")?;
    if !valid_key(&value) {
        return Err("设备凭据文件无效".into());
    }
    Ok(Some(value))
}

#[cfg(unix)]
fn libc_nofollow() -> i32 {
    // O_NOFOLLOW differs across Unix platforms.
    #[cfg(target_os = "macos")]
    {
        0x100
    }
    #[cfg(not(target_os = "macos"))]
    {
        0x20000
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
fn write_secret(root: &Path, id: &str, key: &str) -> Result<(), String> {
    if !valid_key(key) {
        return Err("设备 API key 格式无效。".into());
    }
    let path = secret_path(root, id)?;
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.file_type().is_file() {
            return Err("设备凭据文件无效".into());
        }
    }
    atomic_write(&path, key.as_bytes())
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp).map_err(|_| "无法保存设备数据")?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "无法保存设备数据")?;
        fs::rename(&tmp, path).map_err(|_| "无法替换设备数据")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

fn delete_secret(root: &Path, id: &str) -> Result<(), String> {
    let path = secret_path(root, id)?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(meta) if meta.file_type().is_file() => {
            fs::remove_file(path).map_err(|_| "无法删除设备凭据".into())
        }
        _ => Err("设备凭据文件无效".into()),
    }
}

fn find_device(devices: &[Device], id: &str) -> Result<Device, String> {
    devices
        .iter()
        .find(|d| d.id == id)
        .cloned()
        .ok_or_else(|| "设备未绑定".into())
}

pub fn import(app: &tauri::AppHandle, credential: Credential) -> Result<Device, String> {
    let state = app.state::<DeviceStore>();
    let _guard = state.0.lock().map_err(|_| "设备存储正忙")?;
    import_at(&app_dir(app)?, credential)
}
fn import_at(root: &Path, credential: Credential) -> Result<Device, String> {
    import_with(root, credential, |devices| save_devices(root, devices))
}
fn import_with(
    root: &Path,
    credential: Credential,
    persist: impl FnOnce(&[Device]) -> Result<(), String>,
) -> Result<Device, String> {
    let mut hash = Sha256::new();
    hash.update(credential.base_url.as_bytes());
    hash.update(b"\n");
    hash.update(credential.key.as_bytes());
    let hash_id = format!("{:x}", hash.finalize());
    let mut devices = read_devices(root)?;
    // An edited key retains its device ID. Match an existing local key only within the same origin.
    let mut id = hash_id.clone();
    // An original hash ID can already belong to a device whose key was edited.
    let mut sequence = 0u64;
    while devices.iter().any(|d| d.id == id) {
        let current = read_secret(root, &id)?;
        if current.as_ref().map(|key| key.as_str()) == Some(credential.key.as_str())
            || current.is_none()
        {
            break;
        }
        sequence += 1;
        id = format!("{:x}", Sha256::digest(format!("{hash_id}:{sequence}")));
    }
    for device in &devices {
        if device.base_url == credential.base_url
            && read_secret(root, &device.id)?
                .as_ref()
                .map(|key| key.as_str())
                == Some(credential.key.as_str())
        {
            id = device.id.clone();
            break;
        }
    }
    let device = Device {
        id,
        name: credential.name,
        base_url: credential.base_url,
        icon: credential.icon,
    };
    let previous = read_secret(root, &device.id)?;
    write_secret(root, &device.id, &credential.key)?;
    devices.retain(|d| d.id != device.id);
    devices.push(device.clone());
    if let Err(error) = persist(&devices) {
        let rollback = match previous {
            Some(key) => write_secret(root, &device.id, &key),
            None => delete_secret(root, &device.id),
        };
        return Err(if rollback.is_err() {
            "保存设备列表失败，且无法恢复设备凭据".into()
        } else {
            error
        });
    }
    Ok(device)
}

pub fn get(app: &tauri::AppHandle, id: &str) -> Result<(Device, Zeroizing<String>), String> {
    let state = app.state::<DeviceStore>();
    let _guard = state.0.lock().map_err(|_| "设备存储正忙")?;
    let device = find_device(&list(app)?, id)?;
    let key = read_secret(&app_dir(app)?, id)?
        .ok_or("设备没有本地 API key，请在设备页面输入密钥或手动迁移旧密钥。")?;
    Ok((device, key))
}

pub fn status(app: &tauri::AppHandle, id: &str) -> Result<KeyStatus, String> {
    let state = app.state::<DeviceStore>();
    let _guard = state.0.lock().map_err(|_| "设备存储正忙")?;
    find_device(&list(app)?, id)?;
    let has_key = read_secret(&app_dir(app)?, id)?.is_some();
    Ok(KeyStatus {
        has_key,
        needs_migration: !has_key,
    })
}
pub fn key(app: &tauri::AppHandle, id: &str) -> Result<Option<String>, String> {
    let state = app.state::<DeviceStore>();
    let _guard = state.0.lock().map_err(|_| "设备存储正忙")?;
    find_device(&list(app)?, id)?;
    Ok(read_secret(&app_dir(app)?, id)?.map(|key| key.to_string()))
}
pub fn save_key(app: &tauri::AppHandle, id: &str, key: Zeroizing<String>) -> Result<(), String> {
    let state = app.state::<DeviceStore>();
    let _guard = state.0.lock().map_err(|_| "设备存储正忙")?;
    find_device(&list(app)?, id)?;
    write_secret(&app_dir(app)?, id, &key)
}
fn migrate_with(
    root: &Path,
    id: &str,
    read: impl FnOnce() -> Result<Zeroizing<String>, String>,
    delete: impl FnOnce() -> bool,
) -> Result<MigrationResult, String> {
    if read_secret(root, id)?.is_some() {
        return Ok(MigrationResult {
            legacy_removed: false,
        });
    }
    let key = read()?;
    write_secret(root, id, &key)?;
    // Flush the containing directory before deleting the only legacy copy.
    #[cfg(unix)]
    {
        fs::File::open(root.join("private"))
            .and_then(|dir| dir.sync_all())
            .and_then(|_| fs::File::open(root)?.sync_all())
            .map_err(|_| "无法确认设备凭据已保存，请重试迁移。")?;
    }
    Ok(MigrationResult {
        legacy_removed: delete(),
    })
}
pub fn migrate(app: &tauri::AppHandle, id: &str) -> Result<MigrationResult, String> {
    let state = app.state::<DeviceStore>();
    let _guard = state.0.lock().map_err(|_| "设备存储正忙")?;
    find_device(&list(app)?, id)?;
    migrate_with(
        &app_dir(app)?,
        id,
        || {
            entry(id)?
                .get_password()
                .map(Zeroizing::new)
                .map_err(|_| "无法读取旧设备凭据，请手动输入 API key。".into())
        },
        || {
            entry(id)
                .and_then(|e| {
                    e.delete_credential()
                        .map_err(|_| "无法删除旧设备凭据".into())
                })
                .is_ok()
        },
    )
}
pub fn remove(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let state = app.state::<DeviceStore>();
    let _guard = state.0.lock().map_err(|_| "设备存储正忙")?;
    remove_at(&app_dir(app)?, id)
}
fn remove_at(root: &Path, id: &str) -> Result<(), String> {
    remove_with(root, id, |devices| save_devices(root, devices))
}
fn remove_with(
    root: &Path,
    id: &str,
    persist: impl FnOnce(&[Device]) -> Result<(), String>,
) -> Result<(), String> {
    let mut devices = read_devices(root)?;
    find_device(&devices, id)?;
    let previous = read_secret(root, id)?;
    delete_secret(root, id)?;
    devices.retain(|d| d.id != id);
    if let Err(error) = persist(&devices) {
        if let Some(key) = previous {
            if write_secret(root, id, &key).is_err() {
                return Err("保存设备列表失败，且无法恢复设备凭据".into());
            }
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "gizmoth-device-test-{}-{}",
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn credential(key: &str) -> Credential {
        parse(&format!("https://example.com/api-keys/{key}")).unwrap()
    }
    const FIRST: &str = "gizclaw_sk_v1_synthetic_first";
    const SECOND: &str = "gizclaw_sk_v1_synthetic_second";

    #[test]
    fn local_roundtrip_edit_reimport_and_remove_keep_identity() {
        let f = Fixture::new();
        let first = import_at(&f.0, credential(FIRST)).unwrap();
        assert_eq!(
            read_secret(&f.0, &first.id)
                .unwrap()
                .as_ref()
                .map(|key| key.as_str()),
            Some(FIRST)
        );
        write_secret(&f.0, &first.id, SECOND).unwrap();
        let edited = import_at(&f.0, credential(SECOND)).unwrap();
        assert_eq!(first.id, edited.id);
        assert_eq!(read_devices(&f.0).unwrap().len(), 1);
        let other = import_at(&f.0, credential(FIRST)).unwrap();
        assert_ne!(first.id, other.id);
        assert_eq!(read_devices(&f.0).unwrap().len(), 2);
        assert!(!fs::read_to_string(f.0.join("devices.json"))
            .unwrap()
            .contains(SECOND));
        remove_at(&f.0, &first.id).unwrap();
        assert!(read_secret(&f.0, &first.id).unwrap().is_none());
        assert_eq!(read_devices(&f.0).unwrap().len(), 1);
    }
    #[test]
    fn failed_registry_persistence_restores_previous_state() {
        let f = Fixture::new();
        let device = import_at(&f.0, credential(FIRST)).unwrap();
        write_secret(&f.0, &device.id, SECOND).unwrap();
        assert!(import_with(
            &f.0,
            credential(SECOND),
            |_| Err("synthetic failure".into())
        )
        .is_err());
        assert_eq!(
            read_secret(&f.0, &device.id)
                .unwrap()
                .as_ref()
                .map(|key| key.as_str()),
            Some(SECOND)
        );
        assert!(import_with(&f.0, credential(FIRST), |_| Err("synthetic failure".into())).is_err());
        assert_eq!(fs::read_dir(f.0.join("private")).unwrap().count(), 1);
        assert!(remove_with(&f.0, &device.id, |_| Err("synthetic failure".into())).is_err());
        assert_eq!(
            read_secret(&f.0, &device.id)
                .unwrap()
                .as_ref()
                .map(|key| key.as_str()),
            Some(SECOND)
        );
        assert_eq!(read_devices(&f.0).unwrap().len(), 1);
    }
    #[test]
    fn migration_is_explicit_and_never_overwrites_local_key() {
        let f = Fixture::new();
        let id = "a".repeat(64);
        assert!(read_secret(&f.0, &id).unwrap().is_none());
        let migrated =
            migrate_with(&f.0, &id, || Ok(Zeroizing::new(FIRST.into())), || false).unwrap();
        assert!(!migrated.legacy_removed);
        assert_eq!(
            read_secret(&f.0, &id)
                .unwrap()
                .as_ref()
                .map(|key| key.as_str()),
            Some(FIRST)
        );
        write_secret(&f.0, &id, SECOND).unwrap();
        let repeated = migrate_with(
            &f.0,
            &id,
            || panic!("must not read Keychain"),
            || panic!("must not delete Keychain"),
        )
        .unwrap();
        assert!(!repeated.legacy_removed);
        assert_eq!(
            read_secret(&f.0, &id)
                .unwrap()
                .as_ref()
                .map(|key| key.as_str()),
            Some(SECOND)
        );
        let other = "b".repeat(64);
        assert!(
            migrate_with(&f.0, &other, || Ok(Zeroizing::new(FIRST.into())), || true)
                .unwrap()
                .legacy_removed
        );
    }
    #[test]
    fn failed_migration_never_deletes_legacy_key() {
        let f = Fixture::new();
        let id = "a".repeat(64);
        assert!(migrate_with(
            &f.0,
            &id,
            || Ok(Zeroizing::new("synthetic-invalid-secret".into())),
            || panic!("must retain legacy key")
        )
        .is_err());
        assert!(read_secret(&f.0, &id).unwrap().is_none());
        let path = secret_path(&f.0, &id).unwrap();
        fs::create_dir(path).unwrap();
        assert!(migrate_with(
            &f.0,
            &id,
            || Ok(Zeroizing::new(FIRST.into())),
            || panic!("must retain legacy key")
        )
        .is_err());
    }
    #[test]
    fn invalid_keys_are_redacted_and_preserve_existing_key() {
        let f = Fixture::new();
        let id = "a".repeat(64);
        write_secret(&f.0, &id, FIRST).unwrap();
        for invalid in [
            "gizclaw_sk_v1_",
            "gizclaw_sk_v1_bad value",
            "gizclaw_sk_v1_bad/secret",
            &format!("gizclaw_sk_v1_{}", "x".repeat(96)),
        ] {
            let error = write_secret(&f.0, &id, invalid).unwrap_err();
            assert!(!error.contains(invalid));
            assert_eq!(
                read_secret(&f.0, &id)
                    .unwrap()
                    .as_ref()
                    .map(|key| key.as_str()),
                Some(FIRST)
            );
        }
        assert!(valid_key(&format!("gizclaw_sk_v1_{}", "x".repeat(81))));
        fs::write(f.0.join("private").join(&id), "invalid synthetic file").unwrap();
        assert_eq!(read_secret(&f.0, &id).unwrap_err(), "设备凭据文件无效");
    }
    #[cfg(unix)]
    #[test]
    fn private_permissions_and_symlink_rejection() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let f = Fixture::new();
        let id = "a".repeat(64);
        write_secret(&f.0, &id, FIRST).unwrap();
        write_secret(&f.0, &id, SECOND).unwrap();
        assert_eq!(
            fs::metadata(f.0.join("private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(f.0.join("private").join(&id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let link = "b".repeat(64);
        symlink(
            f.0.join("private").join(&id),
            f.0.join("private").join(&link),
        )
        .unwrap();
        assert!(read_secret(&f.0, &link).is_err());
        assert!(write_secret(&f.0, &link, FIRST).is_err());
        assert!(delete_secret(&f.0, &link).is_err());
        let other = Fixture::new();
        symlink(f.0.join("private"), other.0.join("private")).unwrap();
        assert!(read_secret(&other.0, &id).is_err());
        symlink(f.0.join("private").join(&id), other.0.join("devices.json")).unwrap();
        assert!(read_devices(&other.0).is_err());
        assert!(save_devices(&other.0, &[]).is_err());
    }
    #[test]
    fn registry_reads_are_bounded_and_errors_redacted() {
        let f = Fixture::new();
        fs::write(f.0.join("devices.json"), "synthetic secret").unwrap();
        assert_eq!(
            read_devices(&f.0).err().unwrap(),
            "设备列表损坏，请检查应用数据目录。"
        );
        fs::write(
            f.0.join("devices.json"),
            vec![b'x'; MAX_REGISTRY_BYTES as usize + 1],
        )
        .unwrap();
        assert!(read_devices(&f.0).is_err());
    }
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
