//! Loopback-only origin for packaged Web firmware. WKWebView cannot isolate the
//! tauri:// custom scheme; an HTTP origin with COOP/COEP can use WASM pthreads.
use std::{
    collections::HashMap,
    fs,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, WebviewWindow};
use tiny_http::{Header, Method, Response, Server};

const PACKAGE_PREFIX: &str = "/__gizmoth-preview/packages/";
const MAX_PACKAGE: usize = 512 * 1024;
const MAX_PACKAGES: usize = 32;
struct Package {
    bytes: Vec<u8>,
    owner: String,
    origin: String,
    expires: Instant,
}
type Packages = Arc<Mutex<HashMap<String, Package>>>;
struct RuntimeServer {
    origin: String,
    packages: Packages,
}
static SERVER: Mutex<Option<RuntimeServer>> = Mutex::new(None);

fn asset_path(path: &str) -> Option<String> {
    if matches!(path, "/" | "/index.html") {
        return Some("index.html".into());
    }
    let name = path.strip_prefix("/assets/")?;
    if name.is_empty()
        || name.contains("..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return None;
    }
    Some(format!("assets/{name}"))
}
fn bundled_asset(app: &AppHandle, path: &str) -> Option<(Vec<u8>, String)> {
    #[cfg(feature = "native-e2e")]
    if let Some(bytes) = super::e2e_firmware::asset(app, path) {
        return Some((bytes, "application/octet-stream".into()));
    }
    asset_path(path)
        .and_then(|p| app.asset_resolver().get(p))
        .map(|asset| (asset.bytes, asset.mime_type))
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static runtime header")
}

/// Website storage is origin-scoped even inside a private WebView store.
/// Keep this port across app launches; never silently move an existing store.
fn local_server(directory: &std::path::Path) -> Result<Server, String> {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Address {
        port: u16,
    }
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let path = directory.join("simulator-runtime.json");
    let saved: Option<Address> = match fs::read(&path) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(|e| e.to_string())?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    if saved.as_ref().is_some_and(|address| address.port < 1024) {
        return Err("模拟器存储服务地址无效。".into());
    }
    let port = saved.as_ref().map_or(0, |address| address.port);
    let server = Server::http((std::net::Ipv4Addr::LOCALHOST, port))
        .map_err(|e| format!("无法打开模拟器存储服务，请检查本机端口是否被占用：{e}"))?;
    if saved.is_none() {
        let address = Address {
            port: server
                .server_addr()
                .to_ip()
                .ok_or("模拟器服务地址无效。")?
                .port(),
        };
        let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        fs::write(
            &temp,
            serde_json::to_vec(&address).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    }
    Ok(server)
}

pub fn origin(app: &AppHandle) -> Result<String, String> {
    let mut current = SERVER.lock().map_err(|e| e.to_string())?;
    if let Some(server) = current.as_ref() {
        return Ok(server.origin.clone());
    }
    let server = local_server(&app.path().app_data_dir().map_err(|e| e.to_string())?)?;
    let address = server
        .server_addr()
        .to_ip()
        .ok_or("无法启动本机模拟器服务。")?;
    let host = address.to_string();
    let origin = format!("http://{host}");
    let packages: Packages = Arc::new(Mutex::new(HashMap::new()));
    let server = Arc::new(server);
    // WebViews fetch entry modules and preloads concurrently. A slow response
    // must not hold up the entire simulator origin during startup or reopen.
    for worker in 0..4 {
        let server = server.clone();
        let stored = packages.clone();
        let app = app.clone();
        let host = host.clone();
        std::thread::Builder::new()
            .name(format!("simulator-runtime-{worker}"))
            .spawn(move || {
                for request in server.incoming_requests() {
                    let valid_host = request
                        .headers()
                        .iter()
                        .any(|h| h.field.equiv("Host") && h.value.as_str() == host);
                    if !valid_host || !matches!(request.method(), Method::Get | Method::Head) {
                        let _ = request.respond(Response::empty(403));
                        continue;
                    }
                    let path = request.url().split('?').next().unwrap_or("");
                    let mut allow_origin = None;
                    let resource = if let Some(id) = path.strip_prefix(PACKAGE_PREFIX) {
                        stored.lock().ok().and_then(|mut all| {
                            all.retain(|_, p| p.expires > Instant::now());
                            all.get(id).map(|p| {
                                allow_origin = Some(p.origin.clone());
                                (p.bytes.clone(), "application/octet-stream".to_string())
                            })
                        })
                    } else {
                        bundled_asset(&app, path)
                    };
                    let Some((bytes, mime)) = resource else {
                        let _ = request.respond(Response::empty(404));
                        continue;
                    };
                    let mut response = Response::from_data(bytes)
                        .with_header(header("Content-Type", &mime))
                        .with_header(header("Cache-Control", "no-store"))
                        .with_header(header("X-Content-Type-Options", "nosniff"))
                        .with_header(header("Cross-Origin-Opener-Policy", "same-origin"))
                        .with_header(header("Cross-Origin-Embedder-Policy", "require-corp"))
                        .with_header(header(
                            "Content-Security-Policy",
                            super::simulator::SIMULATOR_CSP,
                        ));
                    if let Some(origin) = allow_origin {
                        response =
                            response.with_header(header("Access-Control-Allow-Origin", &origin));
                    }
                    let _ = request.respond(response);
                }
            })
            .map_err(|e| e.to_string())?;
    }
    *current = Some(RuntimeServer {
        origin: origin.clone(),
        packages,
    });
    Ok(origin)
}

#[derive(serde::Serialize)]
pub struct PackageLocation {
    id: String,
    url: String,
}

#[tauri::command]
pub fn simulator_package_put(
    app: AppHandle,
    window: WebviewWindow,
    bytes: Vec<u8>,
) -> Result<PackageLocation, String> {
    if !window.label().starts_with("simulator-") {
        return Err("只能从模拟器准备临时应用。".into());
    }
    if bytes.is_empty() || bytes.len() > MAX_PACKAGE {
        return Err("模拟器程序包必须在 512 KiB 以内。".into());
    }
    let base = origin(&app)?;
    let current = SERVER.lock().map_err(|e| e.to_string())?;
    let runtime = current.as_ref().ok_or("模拟器服务未启动。")?;
    let mut all = runtime.packages.lock().map_err(|e| e.to_string())?;
    all.retain(|_, p| p.expires > Instant::now());
    if all.len() >= MAX_PACKAGES {
        return Err("临时程序包过多，请稍后重试。".into());
    }
    let id = format!("{}.lua-app.tar.zlib", uuid::Uuid::new_v4());
    let origin = window
        .url()
        .map_err(|e| e.to_string())?
        .origin()
        .ascii_serialization();
    all.insert(
        id.clone(),
        Package {
            bytes,
            owner: window.label().into(),
            origin,
            expires: Instant::now() + Duration::from_secs(600),
        },
    );
    Ok(PackageLocation {
        url: format!("{base}{PACKAGE_PREFIX}{id}"),
        id,
    })
}

#[tauri::command]
pub fn simulator_package_remove(window: WebviewWindow, id: String) -> Result<(), String> {
    let current = SERVER.lock().map_err(|e| e.to_string())?;
    if let Some(runtime) = current.as_ref() {
        let mut all = runtime.packages.lock().map_err(|e| e.to_string())?;
        if all.get(&id).is_some_and(|p| p.owner == window.label()) {
            all.remove(&id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stored_origin_survives_restart_and_never_silently_changes_port() {
        let directory = tempfile::tempdir().unwrap();
        let first = local_server(directory.path()).unwrap();
        let address = first.server_addr().to_ip().unwrap();
        assert!(local_server(directory.path()).is_err());
        drop(first);
        // tiny_http releases its listener on its accept thread after Drop wakes it.
        let mut reopened = None;
        for _ in 0..100 {
            if let Ok(server) = local_server(directory.path()) {
                reopened = Some(server);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(reopened.unwrap().server_addr().to_ip().unwrap(), address);
    }

    #[test]
    fn invalid_saved_ports_are_not_replaced_with_an_empty_storage_origin() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("simulator-runtime.json"),
            br#"{"port":0}"#,
        )
        .unwrap();
        assert!(local_server(directory.path()).is_err());
    }

    #[test]
    fn serves_only_bundled_entry_and_flat_assets() {
        assert_eq!(asset_path("/index.html").as_deref(), Some("index.html"));
        assert_eq!(
            asset_path("/assets/Simulator-123.js").as_deref(),
            Some("assets/Simulator-123.js")
        );
        for path in [
            "/private/key",
            "/assets/../private",
            "/assets/%2e%2e",
            "/assets/",
            "/foo/index.html",
        ] {
            assert_eq!(asset_path(path), None);
        }
    }
}
