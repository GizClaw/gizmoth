//! GizMoth compile agent: PID 1 of every compile VM.
//!
//! PID 1 mounts the basics, reads `/config.json` from the initramfs and forks
//! the agent proper. It then only reaps processes (including daemonized Bazel
//! servers re-parented to it) until the agent exits, and powers the VM off.
//! The VM's OpenVMM process exits with it.
mod command;
mod pack;
#[allow(dead_code)]
mod protocol;
mod session;
mod sys;
mod worker;

use protocol::{BootConfig, BootMode};
use std::{fs, path::Path, time::Duration};

pub fn log(message: impl AsRef<str>) {
    eprintln!("gizmoth-agent: {}", message.as_ref());
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("gizmoth-compile-agent {}", protocol::AGENT_VERSION);
        return;
    }
    if std::process::id() != 1 {
        eprintln!("gizmoth-compile-agent runs as PID 1 inside a GizMoth compile VM");
        std::process::exit(2);
    }
    if let Err(error) = early_mounts() {
        log(format!("early mounts failed: {error}"));
    }
    if let Err(error) = sys::loopback_up() {
        log(format!("loopback: {error}"));
    }
    let config = match fs::read("/config.json").map_err(|e| e.to_string()).and_then(|bytes| {
        serde_json::from_slice::<BootConfig>(&bytes).map_err(|e| e.to_string())
    }) {
        Ok(config) => config,
        Err(error) => {
            log(format!("invalid /config.json: {error}"));
            shutdown();
        }
    };
    let prepared = match config.mode {
        BootMode::Worker => worker::prepare(&config),
        BootMode::Pack => pack::prepare(&config),
    };
    if let Err(error) = prepared {
        log(format!("startup failed: {error}"));
        shutdown();
    }
    // Fork before any thread exists. The child is the agent; PID 1 reaps.
    let child = unsafe { libc::fork() };
    if child == 0 {
        let code = match config.mode {
            BootMode::Worker => worker::serve(&config),
            BootMode::Pack => pack::serve(&config),
        };
        std::process::exit(code);
    }
    if child < 0 {
        log("fork failed");
        shutdown();
    }
    reap_until(child);
    shutdown();
}

fn early_mounts() -> std::io::Result<()> {
    let nosuid = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;
    sys::mount("proc", Path::new("/proc"), "proc", nosuid, "")?;
    sys::mount("sysfs", Path::new("/sys"), "sysfs", nosuid, "")?;
    if !Path::new("/dev/null").exists() {
        sys::mount("devtmpfs", Path::new("/dev"), "devtmpfs", libc::MS_NOSUID, "mode=0755")?;
    }
    sys::mount("devpts", Path::new("/dev/pts"), "devpts", libc::MS_NOSUID | libc::MS_NOEXEC, "gid=5,mode=620,ptmxmode=666")?;
    sys::mount("tmpfs", Path::new("/dev/shm"), "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=1777")?;
    sys::mount("tmpfs", Path::new("/run"), "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=0755")?;
    let _ = fs::write("/proc/sys/kernel/hostname", "gizmoth-build");
    Ok(())
}

fn reap_until(agent: i32) {
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
        if pid == agent {
            return;
        }
        if pid < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
            return;
        }
    }
}

/// Stops every remaining process, flushes disks and powers off.
fn shutdown() -> ! {
    unsafe { libc::kill(-1, libc::SIGTERM) };
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let pid = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
        if pid < 0 {
            break;
        }
        if pid == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    unsafe { libc::kill(-1, libc::SIGKILL) };
    while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
    unsafe { libc::sync() };
    for mount in ["/newroot/work", "/lower/srv", "/lower/mnt", "/newroot"] {
        sys::umount(Path::new(mount));
    }
    sys::poweroff()
}
