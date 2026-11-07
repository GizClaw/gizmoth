//! Loopback-only origin for packaged Web firmware. WKWebView cannot isolate the
//! tauri:// custom scheme; an HTTP origin with COOP/COEP can use WASM pthreads.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, WebviewWindow};
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
fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static runtime header")
}

pub fn origin(app: &AppHandle) -> Result<String, String> {
    let mut current = SERVER.lock().map_err(|e| e.to_string())?;
    if let Some(server) = current.as_ref() {
        return Ok(server.origin.clone());
    }
    let server = Server::http((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    let address = server
        .server_addr()
        .to_ip()
        .ok_or("无法启动本机模拟器服务。")?;
    let host = address.to_string();
    let origin = format!("http://{host}");
    let packages: Packages = Arc::new(Mutex::new(HashMap::new()));
    let stored = packages.clone();
    let app = app.clone();
    std::thread::Builder::new()
        .name("simulator-runtime".into())
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
                    asset_path(path)
                        .and_then(|p| app.asset_resolver().get(p))
                        .map(|asset| (asset.bytes, asset.mime_type))
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
                    response = response.with_header(header("Access-Control-Allow-Origin", &origin));
                }
                let _ = request.respond(response);
            }
        })
        .map_err(|e| e.to_string())?;
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
