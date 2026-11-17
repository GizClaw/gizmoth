//! Runs one command in its own session, streaming stdout/stderr frames to the
//! host. Cancellation and timeouts escalate SIGINT, SIGTERM, SIGKILL so Bazel
//! can stop a build cleanly while its server stays warm.
use crate::{protocol, sys};
use std::{
    fs::File,
    io::{self, Read},
    net::Shutdown,
    os::fd::{AsRawFd, OwnedFd},
    os::unix::{
        net::UnixStream,
        process::{CommandExt, ExitStatusExt},
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub type Sink = Arc<Mutex<UnixStream>>;

pub struct Spec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Change root before running (the packer runs inside the system image).
    pub root: Option<PathBuf>,
    /// Drop to this uid/gid with no supplementary groups.
    pub user: Option<u32>,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
}

impl Outcome {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.cancelled
    }
}

pub fn send(sink: &Sink, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut stream = sink.lock().unwrap_or_else(|e| e.into_inner());
    protocol::write_frame(&mut *stream, kind, payload)
}

pub fn send_json(sink: &Sink, message: &protocol::GuestMessage) -> io::Result<()> {
    let mut stream = sink.lock().unwrap_or_else(|e| e.into_inner());
    protocol::write_json(&mut *stream, message)
}

/// A line of agent narration in the job log, on stderr.
pub fn note(sink: &Sink, text: &str) {
    let _ = send(
        sink,
        protocol::STDERR,
        format!("\x1b[2m[gizmoth] {text}\x1b[0m\n").as_bytes(),
    );
}

const OUTPUT_POLL_MS: i32 = 40;

/// Own the readers so every return path closes their pipes and socket clones.
#[derive(Default)]
struct OutputReaders {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for OutputReaders {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for reader in self.threads.drain(..) {
            let _ = reader.join();
        }
    }
}

fn ready(fd: i32, events: i16) -> io::Result<bool> {
    let mut poll = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut poll, 1, OUTPUT_POLL_MS) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(result > 0)
}

/// A stalled host must not prevent the readers from stopping at the drain
/// deadline. Keep the frame lock, but use nonblocking sends on this call only.
fn send_output(sink: &Sink, kind: u8, bytes: &[u8], stop: &AtomicBool) -> io::Result<()> {
    let stream = loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        match sink.try_lock() {
            Ok(stream) => break stream,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(OUTPUT_POLL_MS as u64));
            }
        }
    };
    let mut frame = Vec::with_capacity(bytes.len() + 5);
    protocol::write_frame(&mut frame, kind, bytes)?;
    let mut written = 0;
    let result = (|| {
        while written < frame.len() {
            if stop.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "output drain stopped",
                ));
            }
            let size = unsafe {
                libc::send(
                    stream.as_raw_fd(),
                    frame[written..].as_ptr().cast(),
                    frame.len() - written,
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
            if size > 0 {
                written += size as usize;
            } else if size == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            } else {
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => {}
                    io::ErrorKind::WouldBlock => {
                        ready(stream.as_raw_fd(), libc::POLLOUT)?;
                    }
                    _ => return Err(error),
                }
            }
        }
        Ok(())
    })();
    // Never append another frame after abandoning a partial one.
    if result.is_err() && written > 0 {
        let _ = stream.shutdown(Shutdown::Both);
    }
    result
}

pub fn run(
    spec: Spec,
    sink: &Sink,
    cancel: &AtomicBool,
    timeout: Option<Duration>,
) -> io::Result<Outcome> {
    let program = spec
        .argv
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let mut command = Command::new(program);
    command
        .args(&spec.argv[1..])
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Allocate before fork: the pre_exec closure must stay async-signal-safe.
    let c = |path: &PathBuf| {
        std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
    };
    let root = spec.root.as_ref().map(c).transpose()?;
    let cwd = c(&spec.cwd)?;
    let user = spec.user;
    unsafe {
        command.pre_exec(move || {
            if let Some(root) = &root {
                if libc::chroot(root.as_ptr()) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if libc::chdir(cwd.as_ptr()) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if let Some(id) = user {
                if libc::setgroups(0, std::ptr::null()) < 0
                    || libc::setgid(id) < 0
                    || libc::setuid(id) < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let pgid = child.id() as i32;
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let mut readers = OutputReaders::default();
    for (pipe, kind) in [
        (
            child.stdout.take().map(|p| File::from(OwnedFd::from(p))),
            protocol::STDOUT,
        ),
        (
            child.stderr.take().map(|p| File::from(OwnedFd::from(p))),
            protocol::STDERR,
        ),
    ] {
        let Some(mut pipe) = pipe else { continue };
        let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let (sink, done, stop) = (sink.clone(), done_tx.clone(), readers.stop.clone());
        readers.threads.push(std::thread::spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            let mut forwarding = true;
            while !stop.load(Ordering::Acquire) {
                match ready(pipe.as_raw_fd(), libc::POLLIN) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => break,
                }
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) if forwarding => {
                        forwarding = send_output(&sink, kind, &buffer[..n], &stop).is_ok();
                    }
                    // Keep draining after a send error so the command can exit.
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                        ) => {}
                    Err(_) => break,
                }
            }
            let _ = done.send(());
        }));
    }
    drop(done_tx);
    let started = Instant::now();
    let mut outcome = Outcome::default();
    let mut stopping: Option<Instant> = None;
    let mut sent = 0;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if stopping.is_none() {
            if cancel.load(Ordering::SeqCst) {
                outcome.cancelled = true;
                stopping = Some(Instant::now());
            } else if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                outcome.timed_out = true;
                stopping = Some(Instant::now());
            }
        }
        if let Some(since) = stopping {
            let elapsed = since.elapsed();
            let step = if elapsed >= Duration::from_secs(12) {
                3
            } else if elapsed >= Duration::from_secs(5) {
                2
            } else {
                1
            };
            while sent < step {
                sent += 1;
                sys::kill_group(pgid, [libc::SIGINT, libc::SIGTERM, libc::SIGKILL][sent - 1]);
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    // Grandchildren may hold the pipes open; do not wait on them forever.
    let deadline = Instant::now() + Duration::from_secs(2);
    for _ in 0..readers.threads.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(left).is_err() {
            break;
        }
    }
    if outcome.cancelled || outcome.timed_out {
        sys::kill_group(pgid, libc::SIGKILL);
    }
    outcome.code = status.code();
    outcome.signal = status.signal();
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::atomic::AtomicU64};

    #[test]
    fn retained_pipes_release_readers_and_socket_without_killing_server() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for stalled_host in [false, true] {
            let path = std::env::temp_dir().join(format!(
                "gzm-command-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            struct Server {
                pid: i32,
                path: PathBuf,
            }
            impl Drop for Server {
                fn drop(&mut self) {
                    if self.pid > 1 {
                        unsafe { libc::kill(self.pid, libc::SIGKILL) };
                    }
                    let _ = fs::remove_dir_all(&self.path);
                }
            }
            let mut server = Server { pid: 0, path };
            let (mut host, guest) = UnixStream::pair().unwrap();
            host.set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let sink = Arc::new(Mutex::new(guest));
            let started = Instant::now();
            let result = run(
            Spec {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    if stalled_host {
                        "( /bin/dd if=/dev/zero bs=65536 count=32 2>/dev/null; exec /bin/sleep 30 ) & printf '%s' \"$!\" > \"$1\""
                    } else {
                        "/bin/sleep 30 & printf '%s' \"$!\" > \"$1\"; printf stdout; printf stderr >&2"
                    }.into(),
                    "fixture".into(),
                    server.path.join("pid").to_string_lossy().into_owned(),
                ],
                cwd: server.path.clone(),
                env: vec![],
                root: None,
                user: None,
            },
            &sink,
            &AtomicBool::new(false),
            None,
        );
            server.pid = fs::read_to_string(server.path.join("pid"))
                .unwrap()
                .parse()
                .unwrap();
            assert!(result.unwrap().success());
            assert!(started.elapsed() < Duration::from_secs(5));
            assert_eq!(
                unsafe { libc::kill(server.pid, 0) },
                0,
                "build server was killed"
            );
            assert_eq!(
                Arc::strong_count(&sink),
                1,
                "pipe readers still retain the job socket"
            );
            drop(sink);
            if stalled_host {
                // A partial frame abandoned at the deadline shuts down the stream;
                // it must still reach EOF, rather than retaining a socket clone.
                let mut output = Vec::new();
                host.read_to_end(&mut output).unwrap();
                assert!(!output.is_empty());
                continue;
            }
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            while let Some((kind, bytes)) = protocol::read_frame(&mut host).unwrap() {
                match kind {
                    protocol::STDOUT => stdout.extend(bytes),
                    protocol::STDERR => stderr.extend(bytes),
                    other => panic!("unexpected frame {other}"),
                }
            }
            assert_eq!(stdout, b"stdout");
            assert_eq!(stderr, b"stderr");
        }
    }
}
