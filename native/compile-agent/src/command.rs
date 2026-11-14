//! Runs one command in its own session, streaming stdout/stderr frames to the
//! host. Cancellation and timeouts escalate SIGINT, SIGTERM, SIGKILL so Bazel
//! can stop a build cleanly while its server stays warm.
use crate::{protocol, sys};
use std::{
    io::{self, Read},
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
    let _ = send(sink, protocol::STDERR, format!("\x1b[2m[gizmoth] {text}\x1b[0m\n").as_bytes());
}

pub fn run(spec: Spec, sink: &Sink, cancel: &AtomicBool, timeout: Option<Duration>) -> io::Result<Outcome> {
    let program = spec.argv.first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
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
                if libc::setgroups(0, std::ptr::null()) < 0 || libc::setgid(id) < 0 || libc::setuid(id) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let pgid = child.id() as i32;
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let mut readers = 0;
    for (pipe, kind) in [
        (child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), protocol::STDOUT),
        (child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>), protocol::STDERR),
    ] {
        let Some(mut pipe) = pipe else { continue };
        let (sink, done) = (sink.clone(), done_tx.clone());
        readers += 1;
        std::thread::spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if send(&sink, kind, &buffer[..n]).is_err() {
                            // Keep draining so the command never blocks on a full pipe.
                            continue;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = done.send(());
        });
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
    for _ in 0..readers {
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
