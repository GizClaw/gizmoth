//! One-shot VM runs (the package packer): a single host connection carries
//! the hello, streamed output, phases, cancellation and the final `Done`.
use crate::{
    command::{self, Sink},
    log,
    protocol::{self, BootMode, GuestMessage, HostMessage},
    sys,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Serves one run: waits for the host's hello, then runs `run` and reports.
pub fn session(mode: BootMode, run: impl FnOnce(&Sink, &AtomicBool) -> Result<(), String>) -> i32 {
    let listener = match sys::VsockListener::bind(protocol::CONTROL_PORT) {
        Ok(listener) => listener,
        Err(error) => {
            log(format!("vsock listen: {error}"));
            return 2;
        }
    };
    // The host keeps one connection for the whole run.
    let mut stream = loop {
        match listener.accept() {
            Ok(mut stream) => match protocol::read_frame(&mut stream) {
                Ok(Some((protocol::JSON, payload))) if matches!(protocol::parse(&payload), Ok(HostMessage::Hello)) => {
                    break stream
                }
                _ => continue,
            },
            Err(error) => {
                log(format!("vsock accept: {error}"));
                return 2;
            }
        }
    };
    let hello = GuestMessage::Hello { version: protocol::AGENT_VERSION, mode, work: None, image: None };
    if protocol::write_json(&mut stream, &hello).is_err() {
        return 2;
    }
    let sink: Sink = Arc::new(Mutex::new(match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return 2,
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        std::thread::spawn(move || loop {
            match protocol::read_frame(&mut stream) {
                Ok(Some((protocol::JSON, payload))) => {
                    if matches!(protocol::parse(&payload), Ok(HostMessage::Cancel)) {
                        cancel.store(true, Ordering::SeqCst);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    cancel.store(true, Ordering::SeqCst);
                    return;
                }
            }
        });
    }
    let result = run(&sink, &cancel);
    unsafe { libc::sync() };
    let (ok, message) = match &result {
        Ok(()) => (true, None),
        Err(error) => (false, Some(error.clone())),
    };
    if let Some(message) = &message {
        command::note(&sink, &format!("failed: {message}"));
    }
    let _ = command::send_json(&sink, &GuestMessage::Done { ok, message });
    if ok {
        0
    } else {
        1
    }
}

pub fn phase(sink: &Sink, phase: &str) {
    let _ = command::send_json(sink, &GuestMessage::Phase { phase: phase.into() });
    command::note(sink, phase);
}

pub fn step(sink: &Sink, cancel: &AtomicBool, root: &Path, argv: &[&str]) -> Result<(), String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    let env = vec![
        ("PATH".to_string(), PATH.to_string()),
        ("HOME".into(), "/root".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("DEBIAN_FRONTEND".into(), "noninteractive".into()),
        ("TERM".into(), "dumb".into()),
    ];
    let spec = command::Spec {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        cwd: PathBuf::from("/"),
        env,
        root: Some(root.to_path_buf()),
        user: None,
    };
    let outcome = command::run(spec, sink, cancel, None).map_err(|e| format!("{}: {e}", argv[0]))?;
    if outcome.success() {
        Ok(())
    } else if outcome.cancelled {
        Err("cancelled".into())
    } else {
        Err(format!("{} exited with {:?}", argv.join(" "), outcome.code.or(outcome.signal)))
    }
}

fn nameservers() -> String {
    let servers: Vec<String> = fs::read_to_string("/proc/net/pnp")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("nameserver "))
        .map(|server| format!("nameserver {}\n", server.trim()))
        .collect();
    if servers.is_empty() {
        "nameserver 1.1.1.1\n".into()
    } else {
        servers.concat()
    }
}

pub fn write_resolv(root: &Path) -> io::Result<()> {
    let path = root.join("etc/resolv.conf");
    let _ = fs::remove_file(&path);
    fs::write(path, nameservers())
}
