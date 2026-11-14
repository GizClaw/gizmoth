//! Installs registry packages and packs them into a read-only erofs
//! filesystem on the VM's second disk. The verified downloads arrive over 9P;
//! unpacking happens here, with Linux semantics, on a scratch disk (the host's
//! file system may be case-insensitive). Tools come from the shared system
//! image, so the host needs no erofs tooling.
use crate::{
    command::Sink,
    protocol::{self, BootConfig, BootMode, PackItem, PackStep},
    session, sys, worker,
};
use std::{
    fs,
    io,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};

const ROOT: &str = "/lower";
/// Downloads, inside ROOT.
const SOURCES: &str = "/mnt";
/// Scratch disk, inside ROOT.
const SCRATCH: &str = "/srv";

/// Runs in PID 1: the system image read-only with `/dev`, `/proc`, `/sys`, a
/// scratch `/tmp` and the downloads at `/mnt`.
pub fn prepare(config: &BootConfig) -> io::Result<()> {
    let root = Path::new(ROOT);
    let system = sys::scsi_disk(0)?;
    sys::mount(&system.to_string_lossy(), root, "erofs", libc::MS_RDONLY, "")?;
    for path in ["dev", "proc", "sys"] {
        sys::bind(&Path::new("/").join(path), &root.join(path))?;
    }
    sys::mount("tmpfs", &root.join("tmp"), "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=1777")?;
    let share = config.shares.first().ok_or_else(|| io::Error::other("pack mode without a share"))?;
    worker::mount_share(root, &protocol::Share { port: share.port, path: SOURCES.into() })
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

fn source(sha256: &str) -> Result<String, String> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("invalid source {sha256}"));
    }
    Ok(format!("{SOURCES}/{sha256}"))
}

fn pack(config: &BootConfig, sink: &Sink, cancel: &AtomicBool) -> Result<(), String> {
    let plan = config.pack.as_ref().ok_or("pack mode without a plan")?;
    let root = Path::new(ROOT);
    let output = sys::scsi_disk(1).map_err(|e| format!("output disk: {e}"))?.to_string_lossy().into_owned();
    let scratch = sys::scsi_disk(2).map_err(|e| format!("scratch disk: {e}"))?.to_string_lossy().into_owned();
    session::phase(sink, "Preparing scratch disk");
    let options = "lazy_itable_init=1,lazy_journal_init=1,nodiscard";
    session::step(sink, cancel, root, &["mke2fs", "-q", "-t", "ext4", "-E", options, "-F", &scratch])?;
    sys::mount(&scratch, &root.join(SCRATCH.trim_start_matches('/')), "ext4", 0, "").map_err(|e| e.to_string())?;
    let packages = format!("{SCRATCH}/packages");
    fs::create_dir_all(root.join(packages.trim_start_matches('/'))).map_err(|e| e.to_string())?;
    for item in &plan.packages {
        install(sink, cancel, root, &packages, item).map_err(|error| format!("{}: {error}", item.dir))?;
    }
    let index = serde_json::to_vec_pretty(&plan.index).map_err(|e| e.to_string())?;
    fs::write(root.join(packages.trim_start_matches('/')).join("index.json"), index).map_err(|e| e.to_string())?;
    session::phase(sink, "Packing");
    // Everything is root-owned and read-only in builds anyway.
    session::step(sink, cancel, root, &["mkfs.erofs", "--quiet", "--all-root", "-zlz4hc", &output, &packages])?;
    sys::umount(&root.join(SCRATCH.trim_start_matches('/')));
    unsafe { libc::sync() };
    Ok(())
}

fn install(sink: &Sink, cancel: &AtomicBool, root: &Path, packages: &str, item: &PackItem) -> Result<(), String> {
    session::phase(sink, &format!("Installing {}", item.dir));
    // Paths as the chrooted tools see them, and as the agent sees them.
    let inside = PathBuf::from(packages).join(relative(&item.dir)?);
    let outside = |path: &Path| root.join(path.strip_prefix("/").unwrap_or(path));
    fs::create_dir_all(outside(&inside)).map_err(|e| e.to_string())?;
    for step in &item.steps {
        match step {
            PackStep::Copy { source: sha256, to, executable } => {
                let target = outside(&inside.join(relative(to)?));
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let from = source(sha256)?;
                fs::copy(outside(Path::new(&from)), &target).map_err(|e| format!("{to}: {e}"))?;
                let mode = if *executable { 0o755 } else { 0o644 };
                fs::set_permissions(&target, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
            }
            PackStep::Extract { source: sha256, to, strip_components } => {
                let target = inside.join(relative(to)?);
                fs::create_dir_all(outside(&target)).map_err(|e| e.to_string())?;
                let (from, target) = (source(sha256)?, target.to_string_lossy().into_owned());
                let strip = format!("--strip-components={strip_components}");
                session::step(sink, cancel, root, &["tar", "-xzf", &from, "-C", &target, &strip, "--no-same-owner"])?;
            }
        }
    }
    for check in &item.checks {
        let path = outside(&inside.join(relative(&check.path)?));
        // Links are resolved inside the system image root, as builds will.
        let meta = fs::metadata(&path).map_err(|_| format!("missing {}", check.path))?;
        if !meta.is_file() || (check.executable && meta.permissions().mode() & 0o111 == 0) {
            return Err(format!("{} is not an{} file", check.path, if check.executable { " executable" } else { "" }));
        }
    }
    Ok(())
}
