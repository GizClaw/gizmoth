//! Blocking loopback client shared by the desktop and stdio MCP entry. Each
//! request has one deadline, including fragmented writes and reads.
use super::{daemon::info_path, state_dir, DAEMON_FLAG};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::{File, OpenOptions, TryLockError},
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

const MAX_RESPONSE: usize = 16 * 1024 * 1024;
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const RETIRE_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: &str = "串口服务请求超时";
const STARTUP_WAIT: Duration = Duration::from_secs(30);
// The mutex serializes operations within this process. startup.lock serializes
// discovery/startup across processes; update.lock stays held during an update.
static LIFECYCLE: Mutex<()> = Mutex::new(());
static UPDATE_BARRIERS: LazyLock<Mutex<HashMap<PathBuf, File>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn open_lifecycle_lock(data_dir: &Path, name: &str) -> Result<File, String> {
    let dir = state_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|error| format!("无法准备串口服务协调锁：{error}"))?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(name))
        .map_err(|error| format!("无法准备串口服务协调锁：{error}"))
}

fn lock_startup(data_dir: &Path, wait: Duration) -> Result<File, String> {
    let file = open_lifecycle_lock(data_dir, "startup.lock")?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => {
                let delay = deadline.saturating_duration_since(Instant::now());
                if delay.is_zero() {
                    return Err("串口服务正在启动或切换，等待超时，请稍后重试。".into());
                }
                std::thread::sleep(delay.min(Duration::from_millis(25)));
            }
            Err(TryLockError::Error(error)) => {
                return Err(format!("无法锁定串口服务启动协调锁：{error}"))
            }
        }
    }
}

// Always acquire startup.lock first. Ordinary clients then cannot contend on
// this lock: a conflict means an updater retained its reservation.
fn lock_update_barrier(data_dir: &Path) -> Result<File, String> {
    let file = open_lifecycle_lock(data_dir, "update.lock")?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err("串口服务正在准备应用更新，请稍后重试。".into()),
        Err(TryLockError::Error(error)) => Err(format!("无法锁定串口服务更新屏障：{error}")),
    }
}

/// Releases this process's reservation before restoring a retired service.
pub fn cancel_update(data_dir: &Path) {
    let _guard = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(lock) = UPDATE_BARRIERS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(data_dir)
    {
        // Close alone cannot release flock while a concurrently spawned child
        // retains the inherited open file description before exec/CLOEXEC.
        if let Err(error) = lock.unlock() {
            eprintln!("无法释放串口服务更新屏障：{error}");
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Conn {
    pid: u32,
    port: u16,
    token: String,
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| REQUEST_TIMEOUT.to_owned())
}

fn socket_timeout(deadline: Instant) -> Result<Duration, String> {
    // Socket timeouts have coarser precision than Instant. Never pass a
    // near-zero value that the platform can round to an invalid timeval.
    Ok(remaining(deadline)?.max(Duration::from_millis(1)))
}

fn io_error(error: std::io::Error) -> String {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        REQUEST_TIMEOUT.to_owned()
    } else {
        error.to_string()
    }
}

fn response_error(value: &Value) -> String {
    value["error"]
        .as_str()
        .unwrap_or("串口服务请求失败")
        .to_owned()
}

impl Conn {
    /// Reads the private daemon record from a serial state directory.
    pub fn read(dir: &Path) -> Option<Self> {
        let bytes = std::fs::read(info_path(dir)).ok()?;
        let info: super::daemon::Info = serde_json::from_slice(&bytes).ok()?;
        (info.api_version == super::API_VERSION).then_some(Self {
            pid: info.pid,
            port: info.port,
            token: info.token,
        })
    }

    fn health(&self) -> Result<Value, String> {
        self.request("GET", "/v1/health", None, HEALTH_TIMEOUT)
    }

    pub fn healthy(&self) -> bool {
        self.health().is_ok()
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let (status, value) = self.request_response(method, path, body, timeout)?;
        if (200..300).contains(&status) {
            Ok(value)
        } else {
            Err(response_error(&value))
        }
    }

    fn request_response(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<(u16, Value), String> {
        let deadline = Instant::now().checked_add(timeout).ok_or(REQUEST_TIMEOUT)?;
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let mut stream =
            TcpStream::connect_timeout(&address, remaining(deadline)?.min(Duration::from_secs(2)))
                .map_err(|error| {
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) {
                        REQUEST_TIMEOUT.to_owned()
                    } else {
                        "串口服务未运行".to_owned()
                    }
                })?;
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.port,
            self.token,
            body.len()
        );
        for mut bytes in [head.as_bytes(), body.as_slice()] {
            while !bytes.is_empty() {
                stream
                    .set_write_timeout(Some(socket_timeout(deadline)?))
                    .map_err(io_error)?;
                match stream.write(bytes) {
                    Ok(0) => return Err("串口服务连接已关闭".into()),
                    Ok(count) => bytes = &bytes[count..],
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(io_error(error)),
                }
            }
        }
        let mut response = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            stream
                .set_read_timeout(Some(socket_timeout(deadline)?))
                .map_err(io_error)?;
            let count = match stream.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(io_error(error)),
            };
            if count == 0 {
                break;
            }
            if count > MAX_RESPONSE.saturating_sub(response.len()) {
                return Err("串口服务响应过大".into());
            }
            response.extend_from_slice(&buffer[..count]);
        }
        remaining(deadline)?;
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or("串口服务响应无效")?;
        let head = String::from_utf8_lossy(&response[..split]);
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .ok_or("串口服务响应无效")?;
        let value = serde_json::from_slice(&response[split + 4..])
            .map_err(|_| "串口服务响应无效".to_owned())?;
        remaining(deadline)?;
        Ok((status, value))
    }
}

fn live(data_dir: &Path) -> Option<(Conn, Value)> {
    let conn = Conn::read(&state_dir(data_dir))?;
    let health = conn.health().ok()?;
    Some((conn, health))
}

/// Observation and recovery actions can still reach an older busy daemon.
/// This never starts, replaces, or retires a service.
pub fn running(data_dir: &Path) -> Option<Conn> {
    live(data_dir).map(|(conn, _)| conn)
}

/// Validates a desktop snapshot against the current discovery owner. A running
/// response is valid only for the same PID/port/token; an offline response also
/// needs a failed health probe and a stable record across that probe. Call while
/// serializing observation issuance so a replaced owner's late response cannot
/// acquire a newer observation epoch.
pub fn observation_owner(data_dir: &Path, expected: Option<&Conn>) -> Result<Option<Conn>, String> {
    let dir = state_dir(data_dir);
    let current = Conn::read(&dir);
    let changed = || "串口服务在读取期间已切换，请重试。".to_owned();
    if let Some(expected) = expected {
        return if current.as_ref() == Some(expected) {
            Ok(current)
        } else {
            Err(changed())
        };
    }
    if current.as_ref().is_some_and(Conn::healthy) || Conn::read(&dir) != current {
        return Err(changed());
    }
    Ok(current)
}

pub struct Discovery {
    pub conn: Conn,
    pub update_error: Option<String>,
}

fn current_build(health: &Value) -> bool {
    health["buildId"].as_str() == Some(super::BUILD_ID)
}

/// Read-only discovery never starts a stopped service. If an older one is
/// already running, it replaces it only after an atomic idle retirement. A busy
/// or legacy daemon remains observable, with an actionable migration error.
pub fn discover(data_dir: &Path) -> Result<Option<Discovery>, String> {
    discover_with(data_dir, start_current)
}

fn discover_with(
    data_dir: &Path,
    start: impl FnOnce(&Path) -> Result<Conn, String>,
) -> Result<Option<Discovery>, String> {
    let _guard = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
    let _startup = lock_startup(data_dir, STARTUP_WAIT)?;
    let _barrier = lock_update_barrier(data_dir)?;
    let Some((conn, health)) = live(data_dir) else {
        return Ok(None);
    };
    if current_build(&health) {
        return Ok(Some(Discovery {
            conn,
            update_error: None,
        }));
    }
    match retire(data_dir, &conn).and_then(|()| start(data_dir)) {
        Ok(conn) => Ok(Some(Discovery {
            conn,
            update_error: None,
        })),
        Err(error) => match running(data_dir) {
            Some(conn) => Ok(Some(Discovery {
                conn,
                update_error: Some(error),
            })),
            None => Err(error),
        },
    }
}

/// Probes the stable lifetime lock without ever unlinking or retaining it
/// across a replacement spawn. Old implementations without an OS lock are
/// never assumed safe: they must first accept the atomic retire endpoint.
fn ownership_released(dir: &Path) -> Result<bool, String> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("daemon.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(format!("无法确认串口服务已释放设备：{error}")),
    };
    match File::try_lock(&file) {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => Err(format!("无法确认串口服务已释放设备：{error}")),
    }
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return true; // Invalid/unknown ownership must not authorize replacement.
    }
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return true;
        };
        // Signal zero checks existence without delivering a signal.
        let exists = unsafe { libc::kill(pid, 0) == 0 };
        exists || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0},
            System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
        };
        match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
            Ok(handle) => unsafe {
                let exited = WaitForSingleObject(handle, 0) == WAIT_OBJECT_0;
                let _ = CloseHandle(handle);
                !exited
            },
            Err(error) => {
                error.code() != windows_core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0)
            }
        }
    }
}

/// A failed health probe alone cannot prove an older daemon released devices:
/// pre-migration implementations do not hold an OS lock. Preserve their record
/// and refuse replacement while the recorded process may still exist.
fn require_no_owner(dir: &Path) -> Result<(), String> {
    if !ownership_released(dir)? {
        return Err("串口服务正在启动或退出，请稍后重试。".into());
    }
    if let Some(conn) = Conn::read(dir) {
        if process_alive(conn.pid) {
            return Err("串口服务仍在运行但未响应。请等待任务结束并手动停止服务后重试。".into());
        }
    } else if info_path(dir).exists() {
        return Err("无法确认旧版串口服务状态，请手动停止该服务后重试。".into());
    }
    Ok(())
}

fn wait_for_exit(dir: &Path, conn: &Conn, deadline: Instant) -> Result<(), String> {
    loop {
        let owner = Conn::read(dir);
        if owner.as_ref().is_some_and(|owner| owner != conn) {
            return Err("串口服务在切换期间重新启动，请稍后重试。".into());
        }
        // Normal retirement removes discovery only after every worker joins.
        // A stale record is accepted only when its process has actually exited;
        // an unresponsive legacy daemon may have no OS lock at all.
        if ownership_released(dir)? && (owner.is_none() || !process_alive(conn.pid)) {
            return Ok(());
        }
        let delay = remaining(deadline)
            .map_err(|_| "串口服务尚未释放设备，请稍后重试更新。".to_owned())?
            .min(Duration::from_millis(100));
        std::thread::sleep(delay);
    }
}

fn retire(data_dir: &Path, conn: &Conn) -> Result<(), String> {
    let deadline = Instant::now() + RETIRE_TIMEOUT;
    match conn.request_response("POST", "/v1/retire", None, HEALTH_TIMEOUT) {
        Ok((status, value)) if (200..300).contains(&status) && value["stopping"] == true => {}
        Ok((409, _)) => {
            return Err("串口服务正在使用中，无法更新。请等待或取消任务、释放外部连接并拒绝排队请求后重试。".into());
        }
        Ok((404 | 405 | 501, _)) => {
            return Err("旧版串口服务不支持安全更新。请先完成或取消任务、释放外部连接并拒绝排队请求，再手动停止串口服务后重试。".into());
        }
        Ok((status, value)) if !(200..300).contains(&status) => {
            return Err(response_error(&value));
        }
        Ok(_) => return Err("串口服务未确认安全停止，请手动停止服务后重试。".into()),
        // The shutdown can close HTTP before the acknowledgement reaches us.
        // Only completed discovery cleanup AND released OS ownership can turn
        // this uncertain response into a successful retirement.
        Err(_) => {}
    }
    wait_for_exit(&state_dir(data_dir), conn, deadline)
}

fn wait_for_process_exit(pid: u32, deadline: Instant) -> Result<(), String> {
    while process_alive(pid) {
        let delay = remaining(deadline)
            .map_err(|_| "串口服务尚未释放设备，请稍后重试更新。".to_owned())?
            .min(Duration::from_millis(100));
        std::thread::sleep(delay);
    }
    Ok(())
}

/// Explicit user-requested shutdown. Unlike automatic migration, this can use
/// the legacy shutdown endpoint, but it cannot return before that owner exits.
pub fn stop_service(data_dir: &Path) -> Result<(), String> {
    let _guard = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
    let dir = state_dir(data_dir);
    let Some(conn) = Conn::read(&dir) else {
        return require_no_owner(&dir);
    };
    let deadline = Instant::now() + RETIRE_TIMEOUT;
    let lifetime_lock = conn
        .health()
        .ok()
        .is_some_and(|health| health["buildId"].is_string());
    match conn.request_response("POST", "/v1/shutdown", None, HEALTH_TIMEOUT) {
        Ok((status, _)) if (200..300).contains(&status) => {}
        Ok((_, value)) => return Err(response_error(&value)),
        Err(_) => {} // A lost acknowledgement still requires confirmed exit.
    }
    wait_for_exit(&dir, &conn, deadline)?;
    if !lifetime_lock {
        // Legacy discovery cleanup is not sufficient: it may have detached
        // workers and does not hold an OS lock for the process lifetime.
        wait_for_process_exit(conn.pid, deadline)?;
    }
    Ok(())
}

/// Safely stops an existing service before the native updater replaces this
/// executable. Returns whether updater cancellation should restore the service.
pub fn prepare_update(data_dir: &Path) -> Result<bool, String> {
    let _guard = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
    let mut barriers = UPDATE_BARRIERS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if barriers.contains_key(data_dir) {
        return Ok(false);
    }
    let _startup = lock_startup(data_dir, STARTUP_WAIT)?;
    let lock = lock_update_barrier(data_dir)?;
    let restore = if let Some((conn, _)) = live(data_dir) {
        retire(data_dir, &conn)?;
        true
    } else {
        require_no_owner(&state_dir(data_dir))?;
        false
    };
    barriers.insert(data_dir.to_path_buf(), lock);
    Ok(restore)
}

/// Returns the current implementation, migrating an older idle daemon before
/// admitting new work. Busy/unsupported old services are never force-stopped.
pub fn ensure(data_dir: &Path) -> Result<Conn, String> {
    ensure_with(data_dir, start_current)
}

fn ensure_with(
    data_dir: &Path,
    start: impl FnOnce(&Path) -> Result<Conn, String>,
) -> Result<Conn, String> {
    let _guard = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
    let _startup = lock_startup(data_dir, STARTUP_WAIT)?;
    let _barrier = lock_update_barrier(data_dir)?;
    if let Some((conn, health)) = live(data_dir) {
        if current_build(&health) {
            return Ok(conn);
        }
        retire(data_dir, &conn)?;
    } else {
        require_no_owner(&state_dir(data_dir))?;
    }
    start(data_dir)
}

fn start_current(data_dir: &Path) -> Result<Conn, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new(exe);
    command
        .arg(DAEMON_FLAG)
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: closing a terminal or the window keeps it alive.
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动串口服务：{error}"))?;
    let deadline = Instant::now() + Duration::from_secs(8);
    let result = loop {
        std::thread::sleep(Duration::from_millis(100));
        if let Some((conn, health)) = live(data_dir) {
            if current_build(&health) {
                break Ok(conn);
            }
            break Err("其他版本的串口服务仍在运行，请先安全停止该服务后重试。".into());
        }
        if let Ok(Some(_)) = child.try_wait() {
            break Err("串口服务启动失败，请稍后重试。".into());
        }
        if Instant::now() >= deadline {
            break Err("串口服务启动超时".into());
        }
    };
    // Reap on success, timeout, or a failed race without killing the process.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        net::{Shutdown, TcpListener},
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
        thread::JoinHandle,
    };

    struct Mock {
        conn: Conn,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.thread.take().unwrap().join().unwrap();
        }
    }

    fn mock(mut handler: impl FnMut(&str, &mut TcpStream) + Send + 'static) -> Mock {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("mock accept failed: {error}"),
                };
                // Darwin can inherit O_NONBLOCK from the listener. Reading a
                // not-yet-complete request must wait instead of treating
                // WouldBlock as EOF and silently dropping the retirement reply.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let request = String::from_utf8(request).unwrap();
                if let Some(path) = request.split_whitespace().nth(1) {
                    handler(path, &mut stream);
                }
            }
        });
        Mock {
            conn: Conn {
                pid: std::process::id(),
                port,
                token: String::new(),
            },
            stop,
            thread: Some(thread),
        }
    }

    fn reply(stream: &mut TcpStream, status: u16, value: Value) {
        let body = serde_json::to_vec(&value).unwrap();
        let head = format!(
            "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
    }

    fn record(data_dir: &Path, conn: &Conn) {
        let dir = state_dir(data_dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            info_path(&dir),
            serde_json::to_vec(&json!({
                "pid": conn.pid,
                "port": conn.port,
                "token": conn.token,
                "apiVersion": super::super::API_VERSION,
                "startedAt": 1,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn lifetime_lock(data_dir: &Path) -> File {
        let dir = state_dir(data_dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("daemon.lock"))
            .unwrap();
        lock.lock().unwrap();
        lock
    }

    #[test]
    fn request_deadline_covers_fragmented_reads() {
        let daemon = mock(|_, stream| {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n")
                .unwrap();
            for byte in b"            {}" {
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(35));
            }
        });
        let started = Instant::now();
        let error = daemon
            .conn
            .request("GET", "/v1/health", None, Duration::from_millis(120))
            .err()
            .unwrap();
        assert_eq!(error, REQUEST_TIMEOUT);
        assert!(started.elapsed() < Duration::from_millis(350));
    }

    #[test]
    fn zero_budget_never_connects() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let daemon = mock(move |_, _| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(
            daemon
                .conn
                .request("GET", "/v1/health", None, Duration::ZERO),
            Err(REQUEST_TIMEOUT.into())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn observations_require_the_current_pid_port_and_token() {
        let temp = tempfile::tempdir().unwrap();
        let owner = Conn {
            pid: 1,
            port: 2,
            token: "owner-token".into(),
        };
        record(temp.path(), &owner);
        assert!(observation_owner(temp.path(), Some(&owner)).unwrap() == Some(owner.clone()));
        for replacement in [
            Conn {
                pid: 3,
                ..owner.clone()
            },
            Conn {
                port: 4,
                ..owner.clone()
            },
            Conn {
                token: "replacement-token".into(),
                ..owner.clone()
            },
        ] {
            record(temp.path(), &replacement);
            assert!(observation_owner(temp.path(), Some(&owner)).is_err());
        }
    }

    #[test]
    fn offline_observation_reprobes_health_and_rejects_owner_changes_during_probe() {
        let temp = tempfile::tempdir().unwrap();
        let healthy = mock(|_, stream| reply(stream, 200, json!({})));
        record(temp.path(), &healthy.conn);
        assert!(observation_owner(temp.path(), None).is_err());

        let replacement_dir = temp.path().to_path_buf();
        let changing = mock(move |_, stream| {
            record(
                &replacement_dir,
                &Conn {
                    pid: 3,
                    port: 0,
                    token: "replacement".into(),
                },
            );
            reply(stream, 500, json!({"error": "offline"}));
        });
        record(temp.path(), &changing.conn);
        assert!(observation_owner(temp.path(), None).is_err());

        let stopped = mock(|_, stream| reply(stream, 500, json!({"error": "offline"})));
        record(temp.path(), &stopped.conn);
        assert!(observation_owner(temp.path(), None).unwrap() == Some(stopped.conn.clone()));
    }

    #[test]
    fn busy_and_legacy_retirement_preserve_the_owner() {
        for status in [409, 404] {
            let temp = tempfile::tempdir().unwrap();
            let _lock = lifetime_lock(temp.path());
            let daemon = mock(move |path, stream| {
                assert_eq!(path, "/v1/retire");
                reply(stream, status, json!({ "error": "busy or unsupported" }));
            });
            record(temp.path(), &daemon.conn);
            let error = retire(temp.path(), &daemon.conn).unwrap_err();
            assert!(
                error.contains(if status == 409 {
                    "正在使用中"
                } else {
                    "不支持安全更新"
                }),
                "unexpected retirement error for status {status}: {error}"
            );
            assert!(info_path(&state_dir(temp.path())).is_file());
            assert!(!ownership_released(&state_dir(temp.path())).unwrap());
        }
    }

    #[test]
    fn retirement_waits_for_discovery_cleanup_and_lifetime_lock_release() {
        for acknowledge in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let lock = lifetime_lock(temp.path());
            let mut lock = Some(lock);
            let dir = state_dir(temp.path());
            let daemon = mock(move |path, stream| {
                assert_eq!(path, "/v1/retire");
                if acknowledge {
                    reply(
                        stream,
                        200,
                        json!({ "stopping": true, "instanceId": "instance-a" }),
                    );
                }
                stream.shutdown(Shutdown::Both).unwrap();
                std::thread::sleep(Duration::from_millis(60));
                std::fs::remove_file(info_path(&dir)).unwrap();
                std::thread::sleep(Duration::from_millis(70));
                drop(lock.take());
            });
            record(temp.path(), &daemon.conn);
            let started = Instant::now();
            retire(temp.path(), &daemon.conn).unwrap();
            assert!(started.elapsed() >= Duration::from_millis(120));
            assert!(!info_path(&state_dir(temp.path())).exists());
            assert!(ownership_released(&state_dir(temp.path())).unwrap());
            assert!(state_dir(temp.path()).join("daemon.lock").is_file());
        }
    }

    #[test]
    fn missing_discovery_does_not_bypass_an_owned_lifetime_lock() {
        let temp = tempfile::tempdir().unwrap();
        let _lock = lifetime_lock(temp.path());
        let conn = Conn {
            pid: std::process::id(),
            port: 0,
            token: String::new(),
        };
        let result = wait_for_exit(
            &state_dir(temp.path()),
            &conn,
            Instant::now() + Duration::from_millis(30),
        );
        assert!(result.is_err());
        assert!(!ownership_released(&state_dir(temp.path())).unwrap());
    }

    #[test]
    fn explicit_stop_waits_for_release_before_a_restart_can_be_requested() {
        let temp = tempfile::tempdir().unwrap();
        let mut lock = Some(lifetime_lock(temp.path()));
        let dir = state_dir(temp.path());
        let daemon = mock(move |path, stream| match path {
            "/v1/health" => reply(stream, 200, json!({ "buildId": "previous-build" })),
            "/v1/shutdown" => {
                reply(stream, 200, json!({ "stopping": true }));
                stream.shutdown(Shutdown::Both).unwrap();
                std::thread::sleep(Duration::from_millis(60));
                std::fs::remove_file(info_path(&dir)).unwrap();
                std::thread::sleep(Duration::from_millis(70));
                drop(lock.take());
            }
            _ => panic!("unexpected endpoint"),
        });
        record(temp.path(), &daemon.conn);
        let started = Instant::now();
        stop_service(temp.path()).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(120));
        assert!(!info_path(&state_dir(temp.path())).exists());
        assert!(ownership_released(&state_dir(temp.path())).unwrap());
    }

    #[test]
    fn legacy_process_exit_wait_has_a_deadline_and_never_kills() {
        let started = Instant::now();
        assert!(
            wait_for_process_exit(std::process::id(), started + Duration::from_millis(30)).is_err()
        );
        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn updater_preparation_reports_restore_only_after_retiring_a_running_service() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!prepare_update(temp.path()).unwrap());
        cancel_update(temp.path());
        let mut lock = Some(lifetime_lock(temp.path()));
        let dir = state_dir(temp.path());
        let daemon = mock(move |path, stream| match path {
            "/v1/health" => reply(stream, 200, json!({ "buildId": super::super::BUILD_ID })),
            "/v1/retire" => {
                reply(stream, 200, json!({ "stopping": true }));
                std::fs::remove_file(info_path(&dir)).unwrap();
                drop(lock.take());
            }
            _ => panic!("unexpected endpoint"),
        });
        record(temp.path(), &daemon.conn);
        assert!(prepare_update(temp.path()).unwrap());
        assert!(!prepare_update(temp.path()).unwrap());
        assert!(ensure_with(temp.path(), |_| panic!("update must block startup")).is_err());
        cancel_update(temp.path());
        let _released = lock_update_barrier(temp.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_unlocks_even_while_an_inherited_descriptor_remains_open() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!prepare_update(temp.path()).unwrap());
        // A duplicate retains the same open file description, just as a child
        // does between fork and exec, before CLOEXEC closes its descriptors.
        let _inherited = UPDATE_BARRIERS
            .lock()
            .unwrap()
            .get(temp.path())
            .unwrap()
            .try_clone()
            .unwrap();
        assert!(lock_update_barrier(temp.path()).is_err());
        cancel_update(temp.path());
        let _released = lock_update_barrier(temp.path()).unwrap();
    }

    #[test]
    fn update_barrier_child() {
        let Some(dir) = std::env::var_os("GIZMOTH_TEST_UPDATE_BARRIER_DIR") else {
            return;
        };
        let blocked = std::env::var_os("GIZMOTH_TEST_UPDATE_BARRIER_BLOCKED").is_some();
        if let Some(ready) = std::env::var_os("GIZMOTH_TEST_STARTUP_READY") {
            std::fs::write(ready, b"ready").unwrap();
        }
        if std::env::var_os("GIZMOTH_TEST_STARTUP_DISCOVER").is_some() {
            assert!(discover_with(Path::new(&dir), |_| panic!(
                "offline discovery must not start"
            ))
            .unwrap()
            .is_none());
            return;
        }
        let mut started = false;
        let result = ensure_with(Path::new(&dir), |_| {
            started = true;
            Ok(Conn {
                pid: 0,
                port: 0,
                token: String::new(),
            })
        });
        assert_eq!(result.is_err(), blocked);
        assert_eq!(started, !blocked);
    }

    #[test]
    fn update_barrier_blocks_other_processes_until_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!prepare_update(temp.path()).unwrap());
        let run_child = |blocked: bool| {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .arg("--exact")
                .arg("serial::client::tests::update_barrier_child")
                .env("GIZMOTH_TEST_UPDATE_BARRIER_DIR", temp.path());
            if blocked {
                command.env("GIZMOTH_TEST_UPDATE_BARRIER_BLOCKED", "1");
            }
            assert!(command.status().unwrap().success());
        };
        run_child(true);
        cancel_update(temp.path());
        run_child(false);
    }

    #[test]
    fn ordinary_cross_process_contention_waits_for_discovery_and_startup() {
        for discover in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let startup = lock_startup(temp.path(), Duration::ZERO).unwrap();
            let barrier = lock_update_barrier(temp.path()).unwrap();
            let ready = temp.path().join("child-ready");
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .arg("--exact")
                .arg("serial::client::tests::update_barrier_child")
                .env("GIZMOTH_TEST_UPDATE_BARRIER_DIR", temp.path())
                .env("GIZMOTH_TEST_STARTUP_READY", &ready);
            if discover {
                command.env("GIZMOTH_TEST_STARTUP_DISCOVER", "1");
            }
            let mut child = command.spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                assert!(Instant::now() < deadline, "child did not become ready");
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(50));
            let waiting = child.try_wait().unwrap().is_none();
            drop(barrier);
            drop(startup);
            let status = child.wait().unwrap();
            assert!(
                waiting,
                "ordinary contention must wait instead of reporting an update"
            );
            assert!(status.success());
        }
    }

    #[test]
    fn startup_contention_has_a_bounded_distinct_error() {
        let temp = tempfile::tempdir().unwrap();
        let _startup = lock_startup(temp.path(), Duration::ZERO).unwrap();
        let error = lock_startup(temp.path(), Duration::from_millis(30)).unwrap_err();
        assert!(error.contains("启动或切换"));
        assert!(!error.contains("应用更新"));
    }

    #[test]
    fn discovery_preserves_busy_observation_and_retries_migration_when_idle() {
        let temp = tempfile::tempdir().unwrap();
        let busy = Arc::new(AtomicBool::new(true));
        let is_busy = busy.clone();
        let mut lock = Some(lifetime_lock(temp.path()));
        let dir = state_dir(temp.path());
        let daemon = mock(move |path, stream| match path {
            "/v1/health" => reply(stream, 200, json!({ "buildId": "previous-build" })),
            "/v1/retire" if is_busy.load(Ordering::SeqCst) => {
                reply(stream, 409, json!({ "error": "busy" }));
            }
            "/v1/retire" => {
                reply(stream, 200, json!({ "stopping": true }));
                std::fs::remove_file(info_path(&dir)).unwrap();
                drop(lock.take());
            }
            _ => panic!("unexpected endpoint"),
        });
        record(temp.path(), &daemon.conn);
        let observed = discover_with(temp.path(), |_| panic!("must not replace busy owner"))
            .unwrap()
            .unwrap();
        assert!(observed.conn == daemon.conn);
        assert!(observed.update_error.unwrap().contains("正在使用中"));
        assert!(ensure_with(temp.path(), |_| panic!("must not admit work to old build")).is_err());
        busy.store(false, Ordering::SeqCst);
        let mut started = false;
        let updated = discover_with(temp.path(), |_| {
            assert!(!info_path(&state_dir(temp.path())).exists());
            assert!(ownership_released(&state_dir(temp.path())).unwrap());
            started = true;
            Ok(daemon.conn.clone())
        })
        .unwrap()
        .unwrap();
        assert!(started);
        assert!(updated.update_error.is_none());
    }

    #[test]
    fn read_only_discovery_never_starts_a_stopped_service() {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            discover_with(temp.path(), |_| panic!("must not start from offline read"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unresponsive_legacy_process_is_never_treated_as_released() {
        let temp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // Refused health probe, with no daemon or hardware involved.
        let conn = Conn {
            pid: std::process::id(),
            port,
            token: String::new(),
        };
        record(temp.path(), &conn);
        // Models the old create-new lock file, which did not hold an OS lock.
        File::create(state_dir(temp.path()).join("daemon.lock")).unwrap();
        assert!(prepare_update(temp.path()).unwrap_err().contains("未响应"));
        assert!(ensure_with(temp.path(), |_| panic!("unsafe legacy overlap")).is_err());
        assert!(info_path(&state_dir(temp.path())).is_file());
    }
}
