//! Loopback-only origin for packaged Web firmware. WKWebView cannot isolate the
//! tauri:// custom scheme; an HTTP origin with COOP/COEP can use WASM pthreads.
use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    header, server::conn::http1, service::service_fn, Method, Request, Response, StatusCode,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    collections::HashMap,
    convert::Infallible,
    fs,
    net::{Ipv4Addr, TcpListener},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, WebviewWindow};
use tokio::sync::Semaphore;

const PACKAGE_PREFIX: &str = "/__gizmoth-preview/packages/";
const MAX_PACKAGE: usize = 512 * 1024;
const MAX_PACKAGES: usize = 32;
const MAX_CONNECTIONS: usize = 32;
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);
type Assets = Arc<dyn Fn(&str) -> Option<(Vec<u8>, String)> + Send + Sync>;
type RuntimeResponse = Response<Full<Bytes>>;
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
    let (directory, name) = if let Some(name) = path.strip_prefix("/assets/") {
        ("assets", name)
    } else {
        let name = path
            .strip_prefix("/fonts/")
            .filter(|name| name.ends_with(".woff2"))?;
        ("fonts", name)
    };
    if name.is_empty()
        || name.contains("..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return None;
    }
    Some(format!("{directory}/{name}"))
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

fn empty_response(status: StatusCode) -> RuntimeResponse {
    Response::builder()
        .status(status)
        .header(header::CONNECTION, "close")
        .header(header::CONTENT_LENGTH, 0)
        .body(Full::new(Bytes::new()))
        .expect("static runtime response")
}

fn respond<B>(
    request: Request<B>,
    host: &str,
    stored: &Packages,
    assets: &Assets,
) -> RuntimeResponse {
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let valid_host = hosts
        .next()
        .is_some_and(|value| value.as_bytes() == host.as_bytes())
        && hosts.next().is_none();
    if !valid_host || !matches!(*request.method(), Method::GET | Method::HEAD) {
        return empty_response(StatusCode::FORBIDDEN);
    }
    // Match the original request target without decoding or broadening routes
    // to accept absolute-form proxy URLs.
    let target = request.uri().to_string();
    let path = target.split('?').next().unwrap_or("");
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
        assets(path)
    };
    let Some((bytes, mime)) = resource else {
        return empty_response(StatusCode::NOT_FOUND);
    };
    let mut response = Response::builder()
        .header(header::CONNECTION, "close")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("Cross-Origin-Opener-Policy", "same-origin")
        .header("Cross-Origin-Embedder-Policy", "require-corp")
        .header(
            header::CONTENT_SECURITY_POLICY,
            super::simulator::SIMULATOR_CSP,
        );
    if let Some(origin) = allow_origin {
        response = response.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    }
    // HEAD advertises the GET length without sending or retaining its body.
    let body = if request.method() == Method::HEAD {
        Bytes::new()
    } else {
        Bytes::from(bytes)
    };
    response
        .body(Full::new(body))
        .unwrap_or_else(|_| empty_response(StatusCode::INTERNAL_SERVER_ERROR))
}

async fn serve(listener: tokio::net::TcpListener, host: String, stored: Packages, assets: Assets) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let host: Arc<str> = host.into();
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(connection) => connection,
            Err(_) => {
                // Avoid spinning on transient OS resource exhaustion.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            // Do not queue an unbounded number of idle or slow connections.
            continue;
        };
        let host = host.clone();
        let stored = stored.clone();
        let assets = assets.clone();
        tokio::spawn(async move {
            // Keep the slot while any blocking asset read is still running,
            // even if the connection deadline has already closed its socket.
            let slot = Arc::new(slot);
            let service = service_fn(move |request| {
                let host = host.clone();
                let stored = stored.clone();
                let assets = assets.clone();
                let slot = slot.clone();
                async move {
                    let response = tokio::task::spawn_blocking(move || {
                        let _slot = slot;
                        respond(request, &host, &stored, &assets)
                    })
                    .await
                    .unwrap_or_else(|_| empty_response(StatusCode::INTERNAL_SERVER_ERROR));
                    Ok::<_, Infallible>(response)
                }
            });
            let mut http = http1::Builder::new();
            http.keep_alive(false)
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_TIMEOUT)
                .max_headers(64)
                .max_buf_size(16 * 1024);
            // Also bound response writes to clients that stop reading.
            let _ = tokio::time::timeout(
                CONNECTION_TIMEOUT,
                http.serve_connection(TokioIo::new(stream), service),
            )
            .await;
        });
    }
}

/// Website storage is origin-scoped even inside a private WebView store.
/// Keep this port across app launches; never silently move an existing store.
fn local_server(directory: &std::path::Path) -> Result<TcpListener, String> {
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
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|e| format!("无法打开模拟器存储服务，请检查本机端口是否被占用：{e}"))?;
    server.set_nonblocking(true).map_err(|e| e.to_string())?;
    if saved.is_none() {
        let address = Address {
            port: server.local_addr().map_err(|e| e.to_string())?.port(),
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
    let host = server.local_addr().map_err(|e| e.to_string())?.to_string();
    let origin = format!("http://{host}");
    let packages: Packages = Arc::new(Mutex::new(HashMap::new()));
    let runtime = tauri::async_runtime::handle();
    let listener = {
        // origin() is also called by synchronous commands outside Tokio.
        let _guard = runtime.inner().enter();
        tokio::net::TcpListener::from_std(server).map_err(|e| e.to_string())?
    };
    let app = app.clone();
    runtime.spawn(serve(
        listener,
        host,
        packages.clone(),
        Arc::new(move |path| bundled_asset(&app, path)),
    ));
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
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct TestServer {
        address: SocketAddr,
        packages: Packages,
        task: tokio::task::JoinHandle<()>,
    }

    impl TestServer {
        fn start(assets: Assets) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let packages = Arc::new(Mutex::new(HashMap::new()));
            let task = tokio::spawn(serve(
                tokio::net::TcpListener::from_std(listener).unwrap(),
                address.to_string(),
                packages.clone(),
                assets,
            ));
            Self {
                address,
                packages,
                task,
            }
        }

        fn bundled() -> Self {
            Self::start(Arc::new(|path| {
                asset_path(path).map(|asset| {
                    let mime = if asset.ends_with(".css") {
                        "text/css"
                    } else if asset.ends_with(".woff2") {
                        "font/woff2"
                    } else {
                        "text/javascript"
                    };
                    (asset.into_bytes(), mime.into())
                })
            }))
        }

        async fn request(&self, method: &str, path: &str) -> Vec<u8> {
            exchange(
                self.address,
                format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n\r\n", self.address),
            )
            .await
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn exchange(address: SocketAddr, request: String) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            response
        })
        .await
        .expect("loopback response must complete and close promptly")
    }

    fn assert_response(response: &[u8], status: u16, body: &[u8]) -> HashMap<String, String> {
        let split = response.windows(4).position(|p| p == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&response[..split]).unwrap();
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap();
        assert!(
            status_line.starts_with(&format!("HTTP/1.1 {status} ")),
            "expected status {status}, received {status_line}"
        );
        assert_eq!(&response[split + 4..], body);
        let headers: HashMap<_, _> = lines
            .map(|line| {
                let (key, value) = line.split_once(": ").unwrap();
                (key.to_ascii_lowercase(), value.to_string())
            })
            .collect();
        assert_eq!(headers["connection"], "close");
        assert!(!headers.contains_key("transfer-encoding"));
        headers
    }

    #[test]
    fn stored_origin_survives_restart_and_never_silently_changes_port() {
        let directory = tempfile::tempdir().unwrap();
        let first = local_server(directory.path()).unwrap();
        let address = first.local_addr().unwrap();
        assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
        let path = directory.path().join("simulator-runtime.json");
        let saved = fs::read(&path).unwrap();
        assert!(local_server(directory.path()).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);
        drop(first);
        assert_eq!(
            local_server(directory.path())
                .unwrap()
                .local_addr()
                .unwrap(),
            address
        );
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

    #[tokio::test]
    async fn responses_have_fixed_lengths_security_headers_and_no_keep_alive() {
        let server = TestServer::bundled();
        for (path, body, mime) in [
            ("/", "index.html", "text/javascript"),
            ("/index.html?simulator=1", "index.html", "text/javascript"),
            ("/assets/core.js", "assets/core.js", "text/javascript"),
            ("/assets/ui.css", "assets/ui.css", "text/css"),
            (
                "/fonts/monda-latin.woff2",
                "fonts/monda-latin.woff2",
                "font/woff2",
            ),
        ] {
            for method in ["GET", "HEAD"] {
                let response = server.request(method, path).await;
                let headers = assert_response(
                    &response,
                    200,
                    if method == "HEAD" {
                        b""
                    } else {
                        body.as_bytes()
                    },
                );
                assert_eq!(headers["content-length"], body.len().to_string());
                assert_eq!(headers["content-type"], mime);
                assert_eq!(headers["cache-control"], "no-store");
                assert_eq!(headers["x-content-type-options"], "nosniff");
                assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
                assert_eq!(headers["cross-origin-embedder-policy"], "require-corp");
                assert_eq!(
                    headers["content-security-policy"],
                    super::super::simulator::SIMULATOR_CSP
                );
                assert!(!headers.contains_key("access-control-allow-origin"));
            }
        }
        let request = format!(
            "GET /assets/core.js HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\n\r\n",
            server.address
        );
        // Even a pipelined second request must not reuse this connection.
        let response = exchange(server.address, request.repeat(2)).await;
        assert_response(&response, 200, b"assets/core.js");
    }

    #[tokio::test]
    async fn listener_rejects_foreign_hosts_methods_and_private_paths() {
        let server = TestServer::bundled();
        for host in ["localhost", "evil.example", "127.0.0.1:1", ""] {
            let host_header = if host.is_empty() {
                String::new()
            } else {
                format!("Host: {host}\r\n")
            };
            let response = exchange(
                server.address,
                format!("GET /index.html HTTP/1.1\r\n{host_header}\r\n"),
            )
            .await;
            assert_response(&response, 403, b"");
        }
        for method in ["POST", "PUT", "DELETE", "OPTIONS"] {
            let response = server.request(method, "/index.html").await;
            assert_response(&response, 403, b"");
        }
        let response = exchange(
            server.address,
            format!(
                "GET /index.html HTTP/1.1\r\nHost: {}\r\nHost: evil.example\r\n\r\n",
                server.address,
            ),
        )
        .await;
        assert_response(&response, 403, b"");
        let response = server
            .request("GET", &format!("http://{}/assets/core.js", server.address))
            .await;
        assert_response(&response, 404, b"");
        for path in [
            "/private/key",
            "/fonts/private.json",
            "/assets/../private",
            "/assets/%2e%2e",
            "/foo/index.html",
        ] {
            let response = server.request("GET", path).await;
            assert_response(&response, 404, b"");
        }
    }

    #[tokio::test]
    async fn package_cors_remains_owner_specific_and_expired_packages_are_not_served() {
        let server = TestServer::bundled();
        for (id, expires) in [
            ("owned", Instant::now() + Duration::from_secs(60)),
            ("expired", Instant::now() - Duration::from_secs(1)),
        ] {
            server.packages.lock().unwrap().insert(
                id.into(),
                Package {
                    bytes: vec![0, 1, 255],
                    owner: "simulator-owner".into(),
                    origin: "http://tauri.localhost".into(),
                    expires,
                },
            );
        }
        for method in ["GET", "HEAD"] {
            let response = server
                .request(method, &format!("{PACKAGE_PREFIX}owned?cache=1"))
                .await;
            let headers = assert_response(
                &response,
                200,
                if method == "HEAD" { &[] } else { &[0, 1, 255] },
            );
            assert_eq!(headers["content-type"], "application/octet-stream");
            assert_eq!(headers["content-length"], "3");
            assert_eq!(
                headers["access-control-allow-origin"],
                "http://tauri.localhost"
            );
        }
        for id in ["expired", "unknown"] {
            let response = server
                .request("GET", &format!("{PACKAGE_PREFIX}{id}"))
                .await;
            let headers = assert_response(&response, 404, b"");
            assert!(!headers.contains_key("access-control-allow-origin"));
        }
        assert!(!server.packages.lock().unwrap().contains_key("expired"));
    }

    #[tokio::test]
    async fn idle_connections_do_not_delay_parallel_preloads_and_have_a_header_deadline() {
        let server = TestServer::bundled();
        let mut idle = Vec::new();
        for _ in 0..16 {
            let mut stream = tokio::net::TcpStream::connect(server.address)
                .await
                .unwrap();
            stream.write_all(b"GET / HTTP/1.1\r\nHost:").await.unwrap();
            idle.push(stream);
        }
        let mut preloads = tokio::task::JoinSet::new();
        for index in 0..12 {
            let address = server.address;
            preloads.spawn(async move {
                let path = if index % 2 == 0 { "core.js" } else { "ui.css" };
                let response = exchange(
                    address,
                    format!("GET /assets/{path} HTTP/1.1\r\nHost: {address}\r\n\r\n"),
                )
                .await;
                assert_response(&response, 200, format!("assets/{path}").as_bytes());
            });
        }
        while let Some(result) = preloads.join_next().await {
            result.unwrap();
        }
        let mut response = Vec::new();
        tokio::time::timeout(
            HEADER_TIMEOUT + Duration::from_secs(2),
            idle[0].read_to_end(&mut response),
        )
        .await
        .expect("partial headers must not hold a connection indefinitely")
        .unwrap();
        assert!(!response.starts_with(b"HTTP/1.1 200"));
    }

    #[tokio::test]
    async fn connection_deadline_also_bounds_waiting_for_an_asset() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started_tx));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = Mutex::new(release_rx);
        let server = TestServer::start(Arc::new(move |_| {
            if let Some(started) = started.lock().unwrap().take() {
                let _ = started.send(());
            }
            let _ = release.lock().unwrap().recv();
            Some((b"asset".to_vec(), "text/javascript".into()))
        }));
        let mut stream = tokio::net::TcpStream::connect(server.address)
            .await
            .unwrap();
        stream
            .write_all(
                format!(
                    "GET /assets/core.js HTTP/1.1\r\nHost: {}\r\n\r\n",
                    server.address
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(CONNECTION_TIMEOUT + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        tokio::time::resume();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(
            response.is_empty(),
            "deadline must close the connection without waiting for the asset read"
        );
        release_tx.send(()).unwrap();
    }

    #[tokio::test]
    async fn excess_connections_are_closed_instead_of_queued() {
        let server = TestServer::bundled();
        let mut idle = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            idle.push(
                tokio::net::TcpStream::connect(server.address)
                    .await
                    .unwrap(),
            );
        }
        let mut extra = tokio::net::TcpStream::connect(server.address)
            .await
            .unwrap();
        let mut response = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(2), extra.read_to_end(&mut response))
            .await
            .expect("connections beyond the cap must be closed promptly");
        assert!(
            matches!(read, Ok(0))
                || read.is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
        );
        assert!(response.is_empty());
        drop(idle);
    }

    #[test]
    fn serves_only_bundled_entry_and_flat_assets() {
        assert_eq!(asset_path("/index.html").as_deref(), Some("index.html"));
        assert_eq!(
            asset_path("/assets/Simulator-123.js").as_deref(),
            Some("assets/Simulator-123.js")
        );
        assert_eq!(
            asset_path("/fonts/monda-latin.woff2").as_deref(),
            Some("fonts/monda-latin.woff2")
        );
        for path in [
            "/fonts/../../private.woff2",
            "/fonts/private.json",
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
