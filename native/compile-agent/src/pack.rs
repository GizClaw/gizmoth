//! Assembles a read-only Linux userland from host-managed formula downloads.
//! The static agent extracts the bootstrap runtime itself; no system disk or
//! preinstalled shell, tar, apt or filesystem utility is needed at startup.
use crate::{
    archive, command::Sink, log,
    protocol::{self, BootConfig, BootMode, PackItem, PackStep},
    session, sys, worker,
};
use std::{fs, io, os::unix::fs::{symlink, PermissionsExt}, path::{Component, Path, PathBuf}, sync::atomic::AtomicBool};

const ROOT: &str = "/lower";
const SOURCES: &str = "/mnt";
const SCRATCH: &str = "/srv";
const PACKAGES: &str = "opt/gizmoth/packages";

pub fn directories(root: &Path) -> io::Result<()> {
    for dir in ["dev", "proc", "sys", "tmp", "run", "etc/ssl/certs", "work", "src", "mnt", "srv", PACKAGES] {
        fs::create_dir_all(root.join(dir))?;
    }
    Ok(())
}

pub fn prepare(config: &BootConfig) -> io::Result<()> {
    let root = Path::new(ROOT);
    sys::mount("tmpfs", root, "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=0755,size=512M")?;
    directories(root)?;
    let share = config.shares.first().ok_or_else(|| io::Error::other("pack mode without downloads"))?;
    worker::mount_share(root, &protocol::Share { port: share.port, path: SOURCES.into() })?;
    let plan = config.pack.as_ref().ok_or_else(|| io::Error::other("pack mode without formulae"))?;
    let bootstrap: Vec<_> = plan.packages.iter().filter(|p| p.bootstrap && p.guest_root).collect();
    if bootstrap.is_empty() {
        return Err(io::Error::other("The registry has no downloaded Linux bootstrap formula; refresh Downloads"));
    }
    for item in bootstrap {
        log(format!("bootstrapping {} from host downloads", item.dir));
        install(None, root, root, item).map_err(io::Error::other)?;
    }
    for path in ["dev", "proc", "sys"] {
        sys::bind(&Path::new("/").join(path), &root.join(path))?;
    }
    Ok(())
}

pub fn serve(config: &BootConfig) -> i32 {
    session::session(BootMode::Pack, |sink, cancel| pack(config, sink, cancel))
}

fn relative(value: &str) -> Result<&Path, String> {
    let path = Path::new(value);
    if value.is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        return Err(format!("unsafe path {value}"));
    }
    Ok(path)
}

fn source(root: &Path, sha256: &str) -> Result<PathBuf, String> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("invalid source digest".into()); }
    Ok(root.join(SOURCES.trim_start_matches('/')).join(sha256))
}

fn pack(config: &BootConfig, sink: &Sink, cancel: &AtomicBool) -> Result<(), String> {
    let plan = config.pack.as_ref().ok_or("pack mode without a plan")?;
    let root = Path::new(ROOT);
    let output = sys::scsi_disk(0).map_err(|e| format!("output disk: {e}"))?.to_string_lossy().into_owned();
    let scratch = sys::scsi_disk(1).map_err(|e| format!("scratch disk: {e}"))?.to_string_lossy().into_owned();
    session::phase(sink, "Preparing writable assembly disk");
    session::step(sink, cancel, root, &["mke2fs", "-q", "-t", "ext4", "-E", "lazy_itable_init=1,lazy_journal_init=1,nodiscard", "-F", &scratch])?;
    sys::mount(&scratch, &root.join(SCRATCH.trim_start_matches('/')), "ext4", 0, "").map_err(|e| e.to_string())?;
    let inside = format!("{SCRATCH}/rootfs");
    let tree = root.join(inside.trim_start_matches('/'));
    directories(&tree).map_err(|e| e.to_string())?;
    for item in &plan.packages {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) { return Err("cancelled".into()); }
        install(Some(sink), root, &tree, item).map_err(|error| format!("{}: {error}", item.dir))?;
    }
    configure(&tree).map_err(|e| e.to_string())?;
    fs::write(tree.join(PACKAGES).join("index.json"), serde_json::to_vec_pretty(&plan.index).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    session::phase(sink, "Packing downloaded userland read-only");
    session::step(sink, cancel, root, &["mkfs.erofs", "--all-root", "-zlz4hc", &output, &inside])?;
    sys::umount(&root.join(SCRATCH.trim_start_matches('/')));
    unsafe { libc::sync() };
    Ok(())
}

fn install(sink: Option<&Sink>, source_root: &Path, tree: &Path, item: &PackItem) -> Result<(), String> {
    if let Some(sink) = sink { session::phase(sink, &format!("Installing {}", item.dir)); }
    let base = if item.guest_root { tree.to_path_buf() } else { tree.join(PACKAGES).join(relative(&item.dir)?) };
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    for step in &item.steps {
        match step {
            PackStep::Copy { source: digest, to, executable } => {
                let target = base.join(relative(to)?);
                if let Some(parent) = target.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
                fs::copy(source(source_root, digest)?, &target).map_err(|e| e.to_string())?;
                fs::set_permissions(target, fs::Permissions::from_mode(if *executable { 0o755 } else { 0o644 })).map_err(|e| e.to_string())?;
            }
            PackStep::Extract { source: digest, to, format, strip_components } => {
                archive::extract(&source(source_root, digest)?, &base.join(relative(to)?), format, *strip_components).map_err(|e| e.to_string())?;
            }
        }
    }
    for check in &item.checks {
        let path = base.join(relative(&check.path)?);
        let meta = fs::metadata(&path).map_err(|e| format!("{}: {e}", check.path))?;
        if !meta.is_file() || (check.executable && meta.permissions().mode() & 0o111 == 0) {
            return Err(format!("{} is not an executable/file", check.path));
        }
    }
    Ok(())
}

fn configure(root: &Path) -> io::Result<()> {
    fs::write(root.join("etc/passwd"), "root:x:0:0:root:/root:/bin/sh\nbuilder:x:1000:1000:Builder:/work/home:/bin/bash\nnobody:x:65534:65534:nobody:/nonexistent:/bin/false\n")?;
    fs::write(root.join("etc/group"), "root:x:0:\nbuilder:x:1000:\nnogroup:x:65534:\n")?;
    fs::write(root.join("etc/nsswitch.conf"), "passwd: files\ngroup: files\nhosts: files dns\n")?;
    if !root.join("bin/sh").exists() && root.join("bin/dash").is_file() {
        let _ = fs::remove_file(root.join("bin/sh"));
        symlink("dash", root.join("bin/sh"))?;
    }
    fs::write(root.join("etc/gizmoth-image.json"), serde_json::to_vec(&serde_json::json!({"agentVersion": protocol::AGENT_VERSION, "source": "downloaded formulae", "format": "erofs"})).map_err(io::Error::other)?)?;
    if root.join("usr/bin/ccache").is_file() {
        let wrappers = root.join("usr/lib/ccache");
        fs::create_dir_all(&wrappers)?;
        for name in ["gcc", "g++", "cc", "c++"] {
            if fs::symlink_metadata(wrappers.join(name)).is_err() {
                symlink("../../bin/ccache", wrappers.join(name))?;
            }
        }
    }
    // Debian alternatives are maintainer-script policy. Set the minimal stable
    // aliases directly instead of invoking a package manager inside the VM.
    for (name, target) in [("cc", "gcc"), ("c++", "g++"), ("awk", "mawk")] {
        let path = root.join("usr/bin").join(name);
        if !path.exists() && root.join("usr/bin").join(target).exists() {
            let _ = fs::remove_file(&path);
            symlink(target, path)?;
        }
    }
    // Certificates are data from the formula; no post-install shell is needed.
    fn certificates(path: &Path, output: &mut Vec<u8>) -> io::Result<()> {
        if !path.is_dir() { return Ok(()); }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() { certificates(&entry.path(), output)?; }
            else if entry.path().extension().is_some_and(|e| e == "crt") {
                output.extend(fs::read(entry.path())?); output.push(b'\n');
            }
        }
        Ok(())
    }
    let mut bundle = Vec::new();
    certificates(&root.join("usr/share/ca-certificates"), &mut bundle)?;
    if !bundle.is_empty() { fs::write(root.join("etc/ssl/certs/ca-certificates.crt"), bundle)?; }
    Ok(())
}
