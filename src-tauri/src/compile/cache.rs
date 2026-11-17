//! Shared build caches on the host disk, served to every compile VM through
//! its private vsock socket. Namespaces are plain HTTP key/value stores:
//! Bazel uses `/bazel/ac/<sha256>` and `/bazel/cas/<sha256>`, ccache its own
//! keys under `/ccache/`. Only these built-in namespaces are configured.
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, SystemTime},
};

const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: u64 = 8 << 30;

#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub bytes: u64,
    pub files: u64,
    pub hits: u64,
    pub misses: u64,
    pub uploads: u64,
}

#[derive(Clone, Debug)]
pub struct Namespace {
    pub name: String,
    pub max_bytes: u64,
}

pub struct Store {
    /// Moves with the working directory; no VM runs while it changes.
    root: RwLock<PathBuf>,
    usage: Mutex<HashMap<String, Usage>>,
    namespaces: Mutex<HashMap<String, Namespace>>,
    /// Serializes committed-file changes and their usage accounting. Upload
    /// bodies are written outside this lock, below the store's `.incoming`.
    committed: Mutex<()>,
    scanned: AtomicU64,
    changed: Box<dyn Fn() + Send + Sync>,
}

pub fn valid_namespace(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_key(key: &str) -> bool {
    let parts: Vec<&str> = key.split('/').collect();
    !key.is_empty()
        && parts.len() <= 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 160
                && !part.starts_with('.')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

fn walk(dir: &Path, visit: &mut dyn FnMut(&Path, &fs::Metadata)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // Older stores staged uploads inside each namespace. These files have
        // never been committed cache entries and must not be counted or evicted.
        if entry.file_name() == ".incoming" {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(&path, visit);
        } else if meta.is_file() {
            visit(&path, &meta);
        }
    }
}

impl Store {
    pub fn new(root: PathBuf, changed: Box<dyn Fn() + Send + Sync>) -> Arc<Store> {
        // Not recursive: a working directory on a missing disk stays missing.
        let _ = fs::create_dir(&root);
        let store = Arc::new(Store {
            root: RwLock::new(root),
            usage: Mutex::new(HashMap::new()),
            namespaces: Mutex::new(HashMap::new()),
            committed: Mutex::new(()),
            scanned: AtomicU64::new(0),
            changed,
        });
        let scanner = store.clone();
        std::thread::spawn(move || scanner.rescan());
        store
    }

    pub fn root(&self) -> PathBuf {
        self.root.read().unwrap().clone()
    }

    /// Points the store at the moved cache directory and rescans it.
    pub fn relocate(self: &Arc<Self>, root: PathBuf) {
        let _ = fs::create_dir(&root);
        *self.root.write().unwrap() = root;
        let scanner = self.clone();
        std::thread::spawn(move || scanner.rescan());
    }

    /// Recomputes on-disk usage of every namespace directory.
    pub fn rescan(&self) {
        let guard = self.committed.lock().unwrap();
        let mut totals: HashMap<String, Usage> = HashMap::new();
        if let Ok(entries) = fs::read_dir(self.root()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !valid_namespace(&name) {
                    continue;
                }
                let usage = totals.entry(name).or_default();
                walk(&entry.path(), &mut |_, meta| {
                    usage.bytes += meta.len();
                    usage.files += 1;
                });
            }
        }
        let mut usage = self.usage.lock().unwrap();
        for current in usage.values_mut() {
            current.bytes = 0;
            current.files = 0;
        }
        for (name, total) in totals {
            let current = usage.entry(name).or_default();
            current.bytes = total.bytes;
            current.files = total.files;
        }
        self.scanned
            .store(crate::serial::now_ms(), Ordering::SeqCst);
        drop(usage);
        drop(guard);
        (self.changed)();
    }

    pub fn configure(&self, namespaces: Vec<Namespace>) {
        let mut map = self.namespaces.lock().unwrap();
        *map = namespaces
            .into_iter()
            .map(|n| (n.name.clone(), n))
            .collect();
        let names: Vec<String> = map.keys().cloned().collect();
        drop(map);
        for name in names {
            self.evict(&name);
        }
    }

    pub fn usage(&self, name: &str) -> Usage {
        self.usage
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    pub fn scanned_at(&self) -> u64 {
        self.scanned.load(Ordering::SeqCst)
    }

    fn namespace(&self, name: &str) -> Option<Namespace> {
        self.namespaces.lock().unwrap().get(name).cloned()
    }

    fn path(&self, namespace: &str, key: &str) -> Option<PathBuf> {
        (valid_namespace(namespace) && valid_key(key))
            .then(|| self.root().join(namespace).join(key))
    }

    /// Removes a namespace's entries. Running builds may repopulate it at once.
    pub fn clear(&self, name: &str) -> io::Result<()> {
        if !valid_namespace(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid cache name",
            ));
        }
        let guard = self.committed.lock().unwrap();
        let root = self.root();
        let dir = root.join(name);
        if dir.exists() {
            let trash = root.join(format!(".trash-{name}-{}", uuid::Uuid::new_v4()));
            fs::rename(&dir, &trash)?;
            std::thread::spawn(move || {
                let _ = fs::remove_dir_all(trash);
            });
        }
        let mut usage = self.usage.lock().unwrap();
        let entry = usage.entry(name.to_string()).or_default();
        entry.bytes = 0;
        entry.files = 0;
        drop(usage);
        drop(guard);
        (self.changed)();
        Ok(())
    }

    fn record(&self, name: &str, update: impl FnOnce(&mut Usage)) {
        update(
            self.usage
                .lock()
                .unwrap()
                .entry(name.to_string())
                .or_default(),
        );
    }

    /// Least-recently-used eviction down to 90% of the namespace limit.
    fn evict(&self, name: &str) {
        let guard = self.committed.lock().unwrap();
        let Some(namespace) = self.namespace(name) else {
            return;
        };
        if namespace.max_bytes == 0 || self.usage(name).bytes <= namespace.max_bytes {
            return;
        }
        let mut files = Vec::new();
        walk(&self.root().join(name), &mut |path, meta| {
            files.push((
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                meta.len(),
                path.to_path_buf(),
            ));
        });
        files.sort();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        let target = namespace.max_bytes / 10 * 9;
        let mut remaining = files.len() as u64;
        for (_, size, path) in files {
            if total <= target {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
                remaining -= 1;
            }
        }
        self.record(name, |usage| {
            usage.bytes = total;
            usage.files = remaining;
        });
        drop(guard);
        (self.changed)();
    }
}

/// A VM's cache listener; dropping it ends the accept thread and removes the socket.
pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept so the thread sees the flag and exits.
        let _ = UnixStream::connect(&self.path);
        let _ = fs::remove_file(&self.path);
    }
}

/// Serves HTTP/1.1 on a VM's cache socket until the returned server is dropped.
pub fn serve(path: PathBuf, store: Arc<Store>) -> io::Result<Server> {
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if flag.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { break };
            let store = store.clone();
            std::thread::spawn(move || {
                let _ = connection(stream, &store);
            });
        }
    });
    Ok(Server { path, stop })
}

struct Request {
    method: String,
    path: String,
    length: Option<u64>,
    chunked: bool,
    close: bool,
    expect_continue: bool,
}

/// Limit the read itself: checking after read_line would already have allocated
/// an arbitrarily large guest-controlled line. Budgets include line endings.
fn read_protocol_line(
    reader: &mut impl BufRead,
    remaining: &mut usize,
) -> io::Result<Option<String>> {
    if *remaining == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protocol lines too large",
        ));
    }
    let mut line = String::new();
    let n = reader.take(*remaining as u64).read_line(&mut line)?;
    *remaining -= n;
    if n == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protocol line too large or incomplete",
        ));
    }
    Ok(Some(line))
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Option<Request>> {
    let mut remaining = MAX_HEADER;
    let Some(line) = read_protocol_line(reader, &mut remaining)? else {
        return Ok(None);
    };
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad request line",
        ));
    };
    let mut request = Request {
        method: method.to_string(),
        path: path.to_string(),
        length: None,
        chunked: false,
        close: version == "HTTP/1.0",
        expect_continue: false,
    };
    loop {
        let header =
            read_protocol_line(reader, &mut remaining)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        match name.as_str() {
            "content-length" => request.length = value.parse().ok(),
            "transfer-encoding" => request.chunked = value.to_ascii_lowercase().contains("chunked"),
            "connection" => request.close = value.eq_ignore_ascii_case("close"),
            "expect" => request.expect_continue = value.eq_ignore_ascii_case("100-continue"),
            _ => {}
        }
    }
    Ok(Some(request))
}

fn respond(
    stream: &mut UnixStream,
    status: u16,
    reason: &str,
    length: u64,
    close: bool,
) -> io::Result<()> {
    let connection = if close { "close" } else { "keep-alive" };
    write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Length: {length}\r\nConnection: {connection}\r\n\r\n")
}

/// A reader over a chunked or length-delimited request body.
struct Body<'a, R> {
    inner: &'a mut R,
    left: u64,
    chunked: bool,
    done: bool,
}

impl<R: BufRead> Read for Body<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if self.chunked && self.left == 0 {
            let mut remaining = MAX_HEADER;
            let mut line = read_protocol_line(self.inner, &mut remaining)?
                .ok_or(io::ErrorKind::UnexpectedEof)?;
            if line.trim().is_empty() {
                line = read_protocol_line(self.inner, &mut remaining)?
                    .ok_or(io::ErrorKind::UnexpectedEof)?;
            }
            let size = u64::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if size == 0 {
                // Trailer lines share a budget, independent of the chunk size.
                let mut remaining = MAX_HEADER;
                loop {
                    let trailer = read_protocol_line(self.inner, &mut remaining)?
                        .ok_or(io::ErrorKind::UnexpectedEof)?;
                    if trailer.trim().is_empty() {
                        break;
                    }
                }
                self.done = true;
                return Ok(0);
            }
            self.left = size;
        }
        if self.left == 0 {
            self.done = true;
            return Ok(0);
        }
        let want = (buffer.len() as u64).min(self.left) as usize;
        let n = self.inner.read(&mut buffer[..want])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= n as u64;
        Ok(n)
    }
}

fn connection(stream: UnixStream, store: &Store) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(300)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::with_capacity(64 * 1024, stream);
    while let Some(request) = read_request(&mut reader)? {
        let close = request.close;
        let path = request
            .path
            .split('?')
            .next()
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string();
        let (namespace, key) = path.split_once('/').unwrap_or((path.as_str(), ""));
        let (namespace, key) = (namespace.to_string(), key.to_string());
        let active = store.namespace(&namespace);
        let target = store.path(&namespace, &key);
        match (request.method.as_str(), active, target) {
            ("GET" | "HEAD", Some(_), Some(target)) => match File::open(&target) {
                Ok(mut file) => {
                    let length = file.metadata()?.len();
                    let _ = file.set_modified(SystemTime::now());
                    store.record(&namespace, |u| u.hits += 1);
                    respond(&mut writer, 200, "OK", length, close)?;
                    if request.method == "GET" {
                        io::copy(&mut file, &mut writer)?;
                    }
                }
                Err(_) => {
                    store.record(&namespace, |u| u.misses += 1);
                    respond(&mut writer, 404, "Not Found", 0, close)?;
                }
            },
            ("PUT", Some(_), Some(target)) => {
                if request.expect_continue {
                    writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
                }
                if !request.chunked && request.length.is_none() {
                    respond(&mut writer, 411, "Length Required", 0, true)?;
                    return Ok(());
                }
                if request.length.is_some_and(|length| length > MAX_BODY) {
                    respond(&mut writer, 413, "Payload Too Large", 0, true)?;
                    return Ok(());
                }
                let mut body = Body {
                    inner: &mut reader,
                    left: request.length.unwrap_or(0),
                    chunked: request.chunked,
                    done: false,
                };
                match put(store, &namespace, &key, &target, &mut body) {
                    Ok(()) => respond(&mut writer, 200, "OK", 0, close)?,
                    Err(status) => {
                        // Drain so the connection stays usable.
                        io::copy(&mut body, &mut io::sink())?;
                        respond(&mut writer, status, "Rejected", 0, close)?;
                    }
                }
            }
            ("DELETE", Some(_), Some(target)) => {
                {
                    let _guard = store.committed.lock().unwrap();
                    if let Ok(meta) = fs::metadata(&target) {
                        if fs::remove_file(&target).is_ok() {
                            store.record(&namespace, |u| {
                                u.bytes = u.bytes.saturating_sub(meta.len());
                                u.files = u.files.saturating_sub(1);
                            });
                        }
                    }
                }
                respond(&mut writer, 200, "OK", 0, close)?;
            }
            (_, None, _) => {
                // Drain any body of a request to an unknown namespace.
                if request.method == "PUT" {
                    if request.expect_continue {
                        respond(&mut writer, 403, "Forbidden", 0, true)?;
                        return Ok(());
                    }
                    let mut body = Body {
                        inner: &mut reader,
                        left: request.length.unwrap_or(0),
                        chunked: request.chunked,
                        done: false,
                    };
                    io::copy(&mut body, &mut io::sink())?;
                }
                respond(
                    &mut writer,
                    if request.method == "PUT" { 403 } else { 404 },
                    "Unavailable",
                    0,
                    close,
                )?;
            }
            _ => respond(&mut writer, 400, "Bad Request", 0, true)?,
        }
        writer.flush()?;
        if close {
            break;
        }
    }
    Ok(())
}

fn put(
    store: &Store,
    namespace: &str,
    key: &str,
    target: &Path,
    body: &mut dyn Read,
) -> Result<(), u16> {
    // A namespace clear must not remove bodies that are still being uploaded.
    let temporary_dir = store.root().join(".incoming");
    fs::create_dir_all(&temporary_dir).map_err(|_| 507u16)?;
    let temporary = temporary_dir.join(uuid::Uuid::new_v4().to_string());
    let result = (|| -> Result<(), u16> {
        let mut file = File::create(&temporary).map_err(|_| 507u16)?;
        let mut hasher = Sha256::new();
        let (mut buffer, mut total) = (vec![0u8; 256 * 1024], 0u64);
        loop {
            let n = body.read(&mut buffer).map_err(|_| 400u16)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > MAX_BODY {
                return Err(413);
            }
            hasher.update(&buffer[..n]);
            file.write_all(&buffer[..n]).map_err(|_| 507u16)?;
        }
        // Content-addressed entries must match their key.
        if namespace == "bazel" && key.starts_with("cas/") {
            let digest: String = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if !key.ends_with(&digest) {
                return Err(400);
            }
        }
        drop(file);
        let _guard = store.committed.lock().unwrap();
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|_| 507u16)?;
        }
        let previous = fs::metadata(target).map(|m| m.len()).ok();
        fs::rename(&temporary, target).map_err(|_| 507u16)?;
        store.record(namespace, |usage| {
            usage.uploads += 1;
            match previous {
                Some(old) => usage.bytes = usage.bytes.saturating_sub(old) + total,
                None => {
                    usage.bytes += total;
                    usage.files += 1;
                }
            }
        });
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return result;
    }
    store.evict(namespace);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(socket: &Path, request: &[u8]) -> String {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream.write_all(request).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    #[test]
    fn serves_bazel_and_ccache_namespaces_with_limits() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("cache"), Box::new(|| {}));
        store.configure(vec![
            Namespace {
                name: "bazel".into(),
                max_bytes: 0,
            },
            Namespace {
                name: "ccache".into(),
                max_bytes: 0,
            },
        ]);
        let socket = dir.path().join("s");
        let server = serve(socket.clone(), store.clone()).unwrap();
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"; // sha256("hello")
        let reply = http(&socket, format!("PUT /bazel/cas/{digest} HTTP/1.1\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").as_bytes());
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let reply = http(&socket, b"PUT /bazel/cas/0000000000000000000000000000000000000000000000000000000000000000 HTTP/1.1\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        let reply = http(
            &socket,
            format!("GET /bazel/cas/{digest} HTTP/1.1\r\nConnection: close\r\n\r\n").as_bytes(),
        );
        assert!(reply.ends_with("\r\n\r\nhello"), "{reply}");
        // Chunked upload, keep-alive with two requests on one connection.
        let reply = http(
            &socket,
            b"PUT /ccache/ab/cdef HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\nGET /ccache/ab/cdef HTTP/1.1\r\nConnection: close\r\n\r\n",
        );
        assert!(reply.ends_with("abcde"), "{reply}");
        assert!(http(
            &socket,
            b"GET /ccache/missing HTTP/1.1\r\nConnection: close\r\n\r\n"
        )
        .starts_with("HTTP/1.1 404"));
        assert!(http(
            &socket,
            b"GET /ccache/../x HTTP/1.1\r\nConnection: close\r\n\r\n"
        )
        .starts_with("HTTP/1.1 400"));
        assert!(http(
            &socket,
            b"PUT /unknown/k HTTP/1.1\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx"
        )
        .starts_with("HTTP/1.1 403"));
        let usage = store.usage("ccache");
        assert_eq!(
            (usage.bytes, usage.files, usage.hits, usage.misses),
            (5, 1, 1, 1)
        );
        store.clear("ccache").unwrap();
        assert_eq!(store.usage("ccache").bytes, 0);
        assert!(http(
            &socket,
            b"GET /ccache/ab/cdef HTTP/1.1\r\nConnection: close\r\n\r\n"
        )
        .starts_with("HTTP/1.1 404"));
        // Dropping the server stops listening and removes the socket.
        drop(server);
        assert!(!socket.exists());
        assert!(UnixStream::connect(&socket).is_err());
        // Relocating moves every later request to the new root.
        let moved = dir.path().join("moved");
        store.relocate(moved.clone());
        let target = store.path("bazel", "ac/x").unwrap();
        assert!(target.starts_with(&moved));
    }

    #[test]
    fn protocol_limits_reject_oversized_request_and_header_lines_before_reading_them() {
        for prefix in ["GET /", "GET /ccache/key HTTP/1.1\r\nX: "] {
            for ending in ["", "\r\n\r\n"] {
                let input = format!("{prefix}{}{ending}", "x".repeat(MAX_HEADER * 2));
                let mut reader = io::Cursor::new(input.into_bytes());
                assert!(read_request(&mut reader).is_err());
                assert_eq!(reader.position(), MAX_HEADER as u64);
            }
        }
    }

    #[test]
    fn protocol_limits_apply_to_cumulative_headers_and_accept_the_exact_budget() {
        let header = "X: small\r\n";
        let input = format!(
            "GET /ccache/key HTTP/1.1\r\n{}\r\n",
            header.repeat(MAX_HEADER / header.len() + 1)
        );
        let mut reader = io::Cursor::new(input.into_bytes());
        assert!(read_request(&mut reader).is_err());
        assert_eq!(reader.position(), MAX_HEADER as u64);

        let prefix = "GET /ccache/key HTTP/1.1\r\nX: ";
        let suffix = "\r\n\r\n";
        let input = format!(
            "{prefix}{}{suffix}",
            "x".repeat(MAX_HEADER - prefix.len() - suffix.len())
        );
        let mut reader = io::Cursor::new(input.into_bytes());
        assert!(read_request(&mut reader).unwrap().is_some());
        assert_eq!(reader.position(), MAX_HEADER as u64);
        assert!(read_request(&mut reader).unwrap().is_none());
    }

    #[test]
    fn protocol_limits_reject_oversized_chunk_sizes_and_extensions_before_reading_them() {
        for input in [
            "f".repeat(MAX_HEADER * 2),
            format!(
                "1;extension={}\r\nx\r\n0\r\n\r\n",
                "x".repeat(MAX_HEADER * 2)
            ),
        ] {
            let mut reader = io::Cursor::new(input.into_bytes());
            let mut body = Body {
                inner: &mut reader,
                left: 0,
                chunked: true,
                done: false,
            };
            assert_eq!(
                io::copy(&mut body, &mut io::sink()).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(reader.position(), MAX_HEADER as u64);
        }
    }

    #[test]
    fn protocol_limits_bound_each_trailer_and_the_cumulative_trailer_section() {
        let trailer = "X: small\r\n";
        for trailers in [
            format!("X: {}\r\n", "x".repeat(MAX_HEADER * 2)),
            trailer.repeat(MAX_HEADER / trailer.len() + 1),
        ] {
            let mut reader = io::Cursor::new(format!("0\r\n{trailers}\r\n").into_bytes());
            let mut body = Body {
                inner: &mut reader,
                left: 0,
                chunked: true,
                done: false,
            };
            assert_eq!(
                io::copy(&mut body, &mut io::sink()).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(reader.position(), 3 + MAX_HEADER as u64);
        }
    }

    #[test]
    fn protocol_limits_preserve_chunk_extensions_trailers_and_keep_alive_requests() {
        let mut reader = io::Cursor::new(
            b"PUT /ccache/key HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\n\r\n3;extension=value\r\nabc\r\n2\r\nde\r\n0;end=true\r\nX-Checksum: ignored\r\nX-Other: trailer\r\n\r\nGET /ccache/key HTTP/1.1\r\nConnection: close\r\n\r\n",
        );
        let request = read_request(&mut reader).unwrap().unwrap();
        assert_eq!(request.method, "PUT");
        assert_eq!(request.path, "/ccache/key");
        assert!(request.chunked);
        assert!(request.expect_continue);
        assert!(!request.close);
        let mut body = Body {
            inner: &mut reader,
            left: 0,
            chunked: true,
            done: false,
        };
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abcde");
        let request = read_request(&mut reader).unwrap().unwrap();
        assert_eq!(request.method, "GET");
        assert!(request.close);
        assert!(read_request(&mut reader).unwrap().is_none());
    }

    #[test]
    fn scanning_and_eviction_ignore_owned_upload_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        fs::create_dir_all(root.join("c/.incoming")).unwrap();
        fs::create_dir_all(root.join(".incoming")).unwrap();
        let pending = [root.join("c/.incoming/old"), root.join(".incoming/new")];
        for path in &pending {
            fs::write(path, vec![b'p'; 1_000]).unwrap();
        }
        fs::write(root.join("c/old"), vec![b'o'; 60]).unwrap();
        fs::write(root.join("c/new"), vec![b'n'; 60]).unwrap();
        File::open(root.join("c/old"))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        let store = Store::new(root, Box::new(|| {}));
        store.rescan();
        assert_eq!((store.usage("c").bytes, store.usage("c").files), (120, 2));
        store.configure(vec![Namespace {
            name: "c".into(),
            max_bytes: 100,
        }]);
        assert!(!store.path("c", "old").unwrap().exists());
        assert!(store.path("c", "new").unwrap().exists());
        assert!(pending.iter().all(|path| path.is_file()));
        assert_eq!((store.usage("c").bytes, store.usage("c").files), (60, 1));
    }

    #[test]
    fn in_flight_upload_survives_scanning_eviction_and_clear() {
        use std::sync::mpsc;
        struct PausedBody {
            bytes: Option<Vec<u8>>,
            ready: mpsc::Sender<()>,
            resume: mpsc::Receiver<()>,
        }
        impl Read for PausedBody {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if let Some(bytes) = self.bytes.take() {
                    buffer[..bytes.len()].copy_from_slice(&bytes);
                    return Ok(bytes.len());
                }
                // put has written the first chunk and is waiting for EOF.
                self.ready.send(()).unwrap();
                self.resume.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(0)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("cache"), Box::new(|| {}));
        store.configure(vec![Namespace {
            name: "c".into(),
            max_bytes: 0,
        }]);
        put(
            &store,
            "c",
            "old",
            &store.path("c", "old").unwrap(),
            &mut &[b'o'; 100][..],
        )
        .unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let uploading = store.clone();
        let task = std::thread::spawn(move || {
            put(
                &uploading,
                "c",
                "new",
                &uploading.path("c", "new").unwrap(),
                &mut PausedBody {
                    bytes: Some(vec![b'n'; 40]),
                    ready: ready_tx,
                    resume: resume_rx,
                },
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        store.rescan();
        assert_eq!((store.usage("c").bytes, store.usage("c").files), (100, 1));
        store.configure(vec![Namespace {
            name: "c".into(),
            max_bytes: 80,
        }]);
        assert_eq!((store.usage("c").bytes, store.usage("c").files), (0, 0));
        store.clear("c").unwrap();
        resume_tx.send(()).unwrap();
        task.join().unwrap().unwrap();
        assert_eq!(
            fs::read(store.path("c", "new").unwrap()).unwrap(),
            vec![b'n'; 40]
        );
        assert_eq!(
            (
                store.usage("c").bytes,
                store.usage("c").files,
                store.usage("c").uploads
            ),
            (40, 1, 2)
        );
    }

    #[test]
    fn concurrent_replacements_scans_and_eviction_keep_exact_usage() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("cache"), Box::new(|| {}));
        store.configure(vec![Namespace {
            name: "c".into(),
            max_bytes: 100,
        }]);
        let start = Arc::new(std::sync::Barrier::new(9));
        let tasks: Vec<_> = (0..8)
            .map(|worker| {
                let store = store.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for attempt in 0..16 {
                        let key = format!("k{}", worker % 2);
                        let body = vec![b'x'; 40 + attempt];
                        put(
                            &store,
                            "c",
                            &key,
                            &store.path("c", &key).unwrap(),
                            &mut body.as_slice(),
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        start.wait();
        for _ in 0..32 {
            store.rescan();
        }
        for task in tasks {
            task.join().unwrap();
        }
        let (mut bytes, mut files) = (0, 0);
        walk(&store.root().join("c"), &mut |_, meta| {
            bytes += meta.len();
            files += 1;
        });
        let usage = store.usage("c");
        assert_eq!(
            (usage.bytes, usage.files, usage.uploads),
            (bytes, files, 128)
        );
        assert!(bytes <= 100);
        assert_eq!(
            fs::read_dir(store.root().join(".incoming"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn failed_publication_removes_the_temporary_upload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("cache"), Box::new(|| {}));
        fs::create_dir_all(store.root().join("c")).unwrap();
        fs::write(store.root().join("c/blocked"), b"existing file").unwrap();
        let target = store.path("c", "blocked/key").unwrap();
        assert_eq!(
            put(&store, "c", "blocked/key", &target, &mut &b"body"[..]),
            Err(507)
        );
        assert_eq!(
            fs::read_dir(store.root().join(".incoming"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read(store.root().join("c/blocked")).unwrap(),
            b"existing file"
        );
    }

    #[test]
    fn eviction_keeps_recent_entries_under_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("cache"), Box::new(|| {}));
        store.configure(vec![Namespace {
            name: "c".into(),
            max_bytes: 100,
        }]);
        for index in 0..5 {
            let target = store.path("c", &format!("k{index}")).unwrap();
            put(
                &store,
                "c",
                &format!("k{index}"),
                &target,
                &mut vec![b'x'; 40].as_slice(),
            )
            .unwrap();
            let when = SystemTime::now() - Duration::from_secs(100 - index * 10);
            File::options()
                .write(true)
                .open(&target)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        assert!(store.usage("c").bytes <= 100);
        assert!(store.path("c", "k4").unwrap().exists());
    }
}
