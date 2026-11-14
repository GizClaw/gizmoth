//! Builds the shared system disk from the Ubuntu base rootfs carried in the
//! provisioning initramfs: format, unpack, install packages, add the build user.
//! The host attaches the new image as `/dev/vda` read-write only for this boot.
use crate::{
    command::{self, Sink},
    log,
    protocol::{self, BootConfig, BootMode, GuestMessage, HostMessage, ProvisionSpec},
    sys,
};
use std::{
    fs::{self, File},
    io,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

const BASE: &str = "/base.tar.gz";
const DEFAULT_MIRROR: &str = "http://ports.ubuntu.com/ubuntu-ports";
const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

pub fn serve(config: &BootConfig) -> i32 {
    let Some(spec) = config.provision.clone() else {
        log("provision mode without a specification");
        return 2;
    };
    session(BootMode::Provision, |sink, cancel| provision(&spec, sink, cancel))
}

/// One host connection for a whole one-shot run (provision or pack): hello,
/// streamed output and phases, cancellation, then `Done`.
pub fn session(mode: BootMode, run: impl FnOnce(&Sink, &AtomicBool) -> Result<(), String>) -> i32 {
    let listener = match sys::VsockListener::bind(protocol::CONTROL_PORT) {
        Ok(listener) => listener,
        Err(error) => {
            log(format!("vsock listen: {error}"));
            return 2;
        }
    };
    // The host keeps one connection for the whole provisioning run.
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

fn unpack(destination: &Path) -> Result<(), String> {
    let file = File::open(BASE).map_err(|e| format!("{BASE}: {e}"))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(io::BufReader::new(file)));
    archive.set_preserve_permissions(true);
    archive.set_preserve_ownerships(true);
    archive.set_preserve_mtime(true);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    archive.unpack(destination).map_err(|e| format!("unpack base system: {e}"))
}

fn bind_system(root: &Path) -> Result<(), String> {
    for path in ["dev", "proc", "sys", "run"] {
        sys::bind(&Path::new("/").join(path), &root.join(path)).map_err(|e| e.to_string())?;
    }
    Ok(())
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

fn set_mirror(root: &Path, mirror: &str) -> io::Result<()> {
    for path in ["etc/apt/sources.list.d/ubuntu.sources", "etc/apt/sources.list"] {
        let path = root.join(path);
        if let Ok(text) = fs::read_to_string(&path) {
            fs::write(&path, text.replace(DEFAULT_MIRROR, mirror.trim_end_matches('/')))?;
        }
    }
    Ok(())
}

fn provision(spec: &ProvisionSpec, sink: &Sink, cancel: &AtomicBool) -> Result<(), String> {
    phase(sink, "Preparing installer");
    let installer = Path::new("/base");
    unpack(installer)?;
    bind_system(installer)?;
    phase(sink, "Formatting system disk");
    let disk = sys::scsi_disk(0).map_err(|e| e.to_string())?;
    let disk = disk.to_string_lossy().into_owned();
    let options = "lazy_itable_init=1,lazy_journal_init=1,nodiscard";
    step(sink, cancel, installer, &["mke2fs", "-q", "-t", "ext4", "-L", "gzm-system", "-E", options, "-F", &disk])?;
    let root = Path::new("/sysroot");
    sys::mount(&disk, root, "ext4", 0, "noinit_itable").map_err(|e| e.to_string())?;
    phase(sink, "Unpacking Ubuntu base");
    unpack(root)?;
    bind_system(root)?;
    write_resolv(root).map_err(|e| format!("resolv.conf: {e}"))?;
    if let Some(mirror) = spec.apt_mirror.as_deref().filter(|m| !m.trim().is_empty()) {
        set_mirror(root, mirror).map_err(|e| format!("apt mirror: {e}"))?;
    }
    phase(sink, "Updating package lists");
    step(sink, cancel, root, &["apt-get", "update"])?;
    phase(sink, "Installing packages");
    let mut install = vec!["apt-get", "install", "-y", "--no-install-recommends"];
    install.extend(spec.packages.iter().map(String::as_str));
    step(sink, cancel, root, &install)?;
    phase(sink, "Creating build user");
    let passwd = fs::read_to_string(root.join("etc/passwd")).unwrap_or_default();
    if !passwd.lines().any(|line| line.starts_with("builder:")) {
        step(sink, cancel, root, &["useradd", "-m", "-u", "1000", "-U", "-s", "/bin/bash", "builder"])?;
    }
    if Path::new("/bazelisk").exists() {
        phase(sink, "Installing Bazelisk");
        let target = root.join("usr/local/bin/bazelisk");
        fs::copy("/bazelisk", &target).map_err(|e| format!("bazelisk: {e}"))?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        let link = root.join("usr/local/bin/bazel");
        let _ = fs::remove_file(&link);
        symlink("bazelisk", link).map_err(|e| e.to_string())?;
    }
    phase(sink, "Cleaning up");
    step(sink, cancel, root, &["apt-get", "clean"])?;
    let mut image = spec.image.clone();
    image["agentVersion"] = protocol::AGENT_VERSION.into();
    fs::write(root.join("etc/gizmoth-image.json"), serde_json::to_vec_pretty(&image).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    for path in ["run", "sys", "proc", "dev"] {
        sys::umount(&root.join(path));
    }
    Ok(())
}
