//! Assembles a read-only Linux userland from host-managed formula downloads.
//! The static agent extracts the bootstrap runtime itself; no system disk or
//! preinstalled shell, tar, apt or filesystem utility is needed at startup.
use crate::{
    archive,
    command::{self, Sink},
    git_source, log,
    protocol::{self, BootConfig, BootMode, PackItem, PackStep},
    session, sys, worker,
};
use std::{
    collections::BTreeMap,
    fs, io,
    net::Shutdown,
    os::unix::{
        fs::{symlink, PermissionsExt},
        net::UnixStream,
    },
    path::{Component, Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Duration,
};

const ROOT: &str = "/lower";
const SOURCES: &str = "/mnt";
const SCRATCH: &str = "/srv";
const PACKAGES: &str = "opt/gizmoth/packages";

pub fn directories(root: &Path) -> io::Result<()> {
    for dir in [
        "dev",
        "proc",
        "sys",
        "tmp",
        "run",
        "etc/ssl/certs",
        "work",
        "src",
        "mnt",
        "srv",
        PACKAGES,
    ] {
        fs::create_dir_all(root.join(dir))?;
    }
    Ok(())
}

pub fn prepare(config: &BootConfig) -> io::Result<()> {
    let root = Path::new(ROOT);
    sys::mount(
        "tmpfs",
        root,
        "tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV,
        "mode=0755,size=512M",
    )?;
    directories(root)?;
    let share = config
        .shares
        .first()
        .ok_or_else(|| io::Error::other("pack mode without downloads"))?;
    worker::mount_share(
        root,
        &protocol::Share {
            port: share.port,
            path: SOURCES.into(),
            writable: false,
        },
    )?;
    let plan = config
        .pack
        .as_ref()
        .ok_or_else(|| io::Error::other("pack mode without formulae"))?;
    let bootstrap: Vec<_> = plan
        .packages
        .iter()
        .filter(|p| p.bootstrap && p.guest_root)
        .collect();
    if bootstrap.is_empty() {
        return Err(io::Error::other(
            "The registry has no downloaded Linux bootstrap formula; refresh Downloads",
        ));
    }
    let cancel = AtomicBool::new(false);
    for item in bootstrap {
        log(format!("bootstrapping {} from host downloads", item.dir));
        install(None, &cancel, root, root, item).map_err(io::Error::other)?;
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
    if value.is_empty()
        || !path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("unsafe path {value}"));
    }
    Ok(path)
}

fn source(root: &Path, sha256: &str) -> Result<PathBuf, String> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid source digest".into());
    }
    Ok(root.join(SOURCES.trim_start_matches('/')).join(sha256))
}

fn pack(config: &BootConfig, sink: &Sink, cancel: &AtomicBool) -> Result<(), String> {
    let plan = config.pack.as_ref().ok_or("pack mode without a plan")?;
    let root = Path::new(ROOT);
    let output = sys::scsi_disk(0)
        .map_err(|e| format!("output disk: {e}"))?
        .to_string_lossy()
        .into_owned();
    let scratch = sys::scsi_disk(1)
        .map_err(|e| format!("scratch disk: {e}"))?
        .to_string_lossy()
        .into_owned();
    session::phase(sink, "Preparing writable assembly disk");
    session::step(
        sink,
        cancel,
        root,
        &[
            "mke2fs",
            "-q",
            "-t",
            "ext4",
            "-E",
            "lazy_itable_init=1,lazy_journal_init=1,nodiscard",
            "-F",
            &scratch,
        ],
    )?;
    sys::mount(
        &scratch,
        &root.join(SCRATCH.trim_start_matches('/')),
        "ext4",
        0,
        "",
    )
    .map_err(|e| e.to_string())?;
    let inside = format!("{SCRATCH}/rootfs");
    let tree = root.join(inside.trim_start_matches('/'));
    directories(&tree).map_err(|e| e.to_string())?;
    let mut mounts = AssemblyMounts(Vec::new());
    for path in ["dev", "proc", "sys"] {
        mounts.bind(&Path::new("/").join(path), &tree.join(path))?;
    }
    mounts.bind(&root.join("mnt"), &tree.join("mnt"))?;
    for (path, options) in [
        ("tmp", "mode=1777"),
        ("run", "mode=0755"),
        ("work", "mode=0755"),
    ] {
        let target = tree.join(path);
        sys::mount(
            "tmpfs",
            &target,
            "tmpfs",
            libc::MS_NOSUID | libc::MS_NODEV,
            options,
        )
        .map_err(|e| e.to_string())?;
        mounts.0.push(target);
    }
    for path in ["home", "tmp", "cache", "build"] {
        let path = tree.join("work").join(path);
        fs::create_dir_all(&path)
            .and_then(|()| sys::lchown(&path, protocol::BUILDER_UID, protocol::BUILDER_UID))
            .map_err(|e| e.to_string())?;
    }
    // System payloads are already ordered before SDK setup and tool commands.
    for item in plan.packages.iter().filter(|p| p.guest_root) {
        install(Some(sink), cancel, root, &tree, item)
            .map_err(|error| format!("{}: {error}", item.dir))?;
    }
    configure(&tree).map_err(|e| e.to_string())?;
    let translated =
        worker::translation(&tree).map_err(|e| format!("x86 translation setup: {e}"))?;
    for item in plan.packages.iter().filter(|p| !p.guest_root) {
        install(Some(sink), cancel, root, &tree, item)
            .map_err(|error| format!("{}: {error}", item.dir))?;
    }
    crate::activation::commands(&tree, &plan.command_paths)
        .map_err(|e| format!("command activation: {e}"))?;
    configure_git(&tree).map_err(|e| e.to_string())?;
    // Setup ran as root; qualification gets a fresh builder-owned HOME/cache,
    // just as a worker boot does, with no installer-owned cache artifacts.
    sys::umount(&tree.join("work"));
    sys::mount(
        "tmpfs",
        &tree.join("work"),
        "tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV,
        "mode=0755",
    )
    .map_err(|e| e.to_string())?;
    for path in ["home", "tmp", "cache", "build"] {
        let path = tree.join("work").join(path);
        fs::create_dir_all(&path)
            .and_then(|()| sys::lchown(&path, protocol::BUILDER_UID, protocol::BUILDER_UID))
            .map_err(|e| e.to_string())?;
    }
    // Qualification runs under the same read-only programs and builder identity
    // as real jobs, with only ephemeral HOME/cache/output directories writable.
    for path in ["usr", "bin", "sbin", "lib", "lib64", "opt"] {
        let target = tree.join(path);
        if fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir()) {
            sys::bind_readonly(&target, &target).map_err(|e| e.to_string())?;
            mounts.0.push(target);
        }
    }
    for item in &plan.packages {
        for check in &item.command_checks {
            session::phase(sink, &format!("Checking {}", item.dir));
            let mut environment = item.environment.clone();
            if translated {
                environment.insert("GIZMOTH_X86_64_TRANSLATION".into(), "qemu-binfmt-v1".into());
            }
            let spec = formula_spec(
                &tree,
                &check.program,
                &check.args,
                "/work/build",
                &environment,
                Some(protocol::BUILDER_UID),
            )?;
            run(
                sink,
                cancel,
                spec,
                check.timeout_seconds,
                check.stdout_contains.as_deref(),
            )
            .map_err(|error| format!("{}: command check: {error}", item.dir))?;
        }
    }
    drop(mounts);
    fs::write(
        tree.join(PACKAGES).join("index.json"),
        serde_json::to_vec_pretty(&plan.index).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    session::phase(sink, "Packing downloaded userland read-only");
    session::step(
        sink,
        cancel,
        root,
        &["mkfs.erofs", "--all-root", "-zlz4hc", &output, &inside],
    )?;
    sys::umount(&root.join(SCRATCH.trim_start_matches('/')));
    unsafe { libc::sync() };
    Ok(())
}

fn install(
    sink: Option<&Sink>,
    cancel: &AtomicBool,
    source_root: &Path,
    tree: &Path,
    item: &PackItem,
) -> Result<(), String> {
    if let Some(sink) = sink {
        session::phase(sink, &format!("Installing {}", item.dir));
    }
    let base = if item.guest_root {
        tree.to_path_buf()
    } else {
        tree.join(PACKAGES).join(relative(&item.dir)?)
    };
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    for step in &item.steps {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        match step {
            PackStep::Copy {
                source: digest,
                to,
                executable,
            } => {
                let target = base.join(relative(to)?);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::copy(source(source_root, digest)?, &target).map_err(|e| e.to_string())?;
                fs::set_permissions(
                    target,
                    fs::Permissions::from_mode(if *executable { 0o755 } else { 0o644 }),
                )
                .map_err(|e| e.to_string())?;
            }
            PackStep::Extract {
                source: digest,
                to,
                format,
                strip_components,
            } => {
                archive::extract(
                    &source(source_root, digest)?,
                    &base.join(relative(to)?),
                    format,
                    *strip_components,
                )
                .map_err(|e| e.to_string())?;
            }
            PackStep::Checkout { source: digest, to } => {
                let sink = sink.ok_or(
                    "Bootstrap formulae cannot checkout Git sources before userland exists",
                )?;
                let path = item
                    .environment
                    .get("PATH")
                    .map(String::as_str)
                    .unwrap_or("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
                git_source::checkout(
                    &source(source_root, digest)?,
                    digest,
                    tree,
                    &base.join(relative(to)?),
                    cancel,
                    path,
                    |invocation| checkout_command(tree, sink, cancel, invocation),
                )?;
            }
            PackStep::Run {
                program,
                args,
                cwd,
                environment,
                timeout_seconds,
            } => {
                let sink =
                    sink.ok_or("Bootstrap formulae cannot run commands before userland exists")?;
                // Runtime aliases are policy, not Debian archive members. A
                // root recipe may need them immediately after extracting tools.
                if item.guest_root {
                    configure(tree).map_err(|e| e.to_string())?;
                }
                let mut env = item.environment.clone();
                env.extend(environment.clone());
                let spec = formula_spec(tree, program, args, cwd, &env, None)?;
                run(sink, cancel, spec, *timeout_seconds, None)?;
            }
        }
    }
    for check in &item.checks {
        let path = base.join(relative(&check.path)?);
        let guest_path = path.strip_prefix(tree).map_err(|e| e.to_string())?;
        let path = crate::activation::resolve(tree, guest_path)
            .map_err(|e| format!("{}: {e}", check.path))?;
        let meta = fs::metadata(path).map_err(|e| format!("{}: {e}", check.path))?;
        if !meta.is_file() || (check.executable && meta.permissions().mode() & 0o111 == 0) {
            return Err(format!("{} is not an executable/file", check.path));
        }
    }
    Ok(())
}

/// Git metadata can be proportional to the SDK file count. Spool captured
/// stdout to the private assembly directory instead of retaining it in RAM.
fn checkout_command(
    tree: &Path,
    sink: &Sink,
    cancel: &AtomicBool,
    invocation: git_source::GitCommand,
) -> Result<(), String> {
    use std::io::Write;
    let spec = command::Spec {
        argv: invocation.argv,
        cwd: invocation.cwd,
        env: invocation.env,
        root: Some(tree.to_path_buf()),
        user: None,
    };
    let Some(path) = invocation.stdout else {
        return run(sink, cancel, spec, 1800, None);
    };
    let mut file = fs::File::create(path).map_err(|e| e.to_string())?;
    let (mut reader, writer) = UnixStream::pair().map_err(|e| e.to_string())?;
    let target = sink.clone();
    let capture = std::thread::spawn(move || -> io::Result<()> {
        let mut failed = None;
        while let Some((kind, payload)) = protocol::read_frame(&mut reader)? {
            if kind == protocol::STDOUT {
                if failed.is_none() {
                    if let Err(error) = file.write_all(&payload) {
                        failed = Some(error);
                    }
                }
            } else {
                let _ = command::send(&target, kind, &payload);
            }
        }
        if let Some(error) = failed {
            return Err(error);
        }
        file.flush()
    });
    let output = Arc::new(Mutex::new(writer));
    let outcome = command::run(spec, &output, cancel, Some(Duration::from_secs(1800)));
    let _ = output.lock().unwrap().shutdown(Shutdown::Write);
    capture
        .join()
        .map_err(|_| "Git output reader failed")?
        .map_err(|e| e.to_string())?;
    let outcome = outcome.map_err(|e| format!("git: {e}"))?;
    if outcome.cancelled {
        return Err("cancelled".into());
    }
    if outcome.timed_out {
        return Err("git timed out after 1800 seconds".into());
    }
    if !outcome.success() {
        return Err(format!(
            "git exited with {:?}",
            outcome.code.or(outcome.signal)
        ));
    }
    Ok(())
}

struct AssemblyMounts(Vec<PathBuf>);
impl AssemblyMounts {
    fn bind(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        sys::bind(from, to).map_err(|e| e.to_string())?;
        self.0.push(to.to_path_buf());
        Ok(())
    }
}
impl Drop for AssemblyMounts {
    fn drop(&mut self) {
        for path in self.0.iter().rev() {
            sys::umount(path);
        }
    }
}

fn formula_spec(
    tree: &Path,
    program: &str,
    args: &[String],
    cwd: &str,
    environment: &BTreeMap<String, String>,
    user: Option<u32>,
) -> Result<command::Spec, String> {
    let cwd = PathBuf::from(cwd);
    if !cwd.is_absolute()
        || cwd
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err("Invalid formula working directory".into());
    }
    fs::create_dir_all(tree.join(cwd.strip_prefix("/").unwrap())).map_err(|e| e.to_string())?;
    let mut env = BTreeMap::from([
        (
            "PATH".into(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
        ),
        ("HOME".into(), "/work/home".into()),
        ("TMPDIR".into(), "/work/tmp".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("TERM".into(), "dumb".into()),
        (
            "USER".into(),
            if user.is_some() { "builder" } else { "root" }.into(),
        ),
        ("PYTHONPYCACHEPREFIX".into(), "/work/cache/pycache".into()),
        ("QEMU_LD_PREFIX".into(), "/opt/gizmoth/x86_64".into()),
    ]);
    env.extend(environment.clone());
    Ok(command::Spec {
        argv: std::iter::once(program.to_owned())
            .chain(args.iter().cloned())
            .collect(),
        cwd,
        env: env.into_iter().collect(),
        root: Some(tree.to_path_buf()),
        user,
    })
}

/// Reuse command session cancellation/deadlines and stream output to the host.
/// A check's expected stdout is matched across frame boundaries without keeping
/// the command's potentially unbounded output in memory.
fn run(
    sink: &Sink,
    cancel: &AtomicBool,
    spec: command::Spec,
    seconds: u64,
    expected: Option<&str>,
) -> Result<(), String> {
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    let program = spec.argv.first().cloned().ok_or("Empty formula command")?;
    let (output, capture) = if let Some(expected) = expected {
        let (mut reader, writer) = UnixStream::pair().map_err(|e| e.to_string())?;
        let (target, expected) = (sink.clone(), expected.as_bytes().to_vec());
        let capture = std::thread::spawn(move || -> io::Result<bool> {
            let mut tail = Vec::new();
            let mut found = expected.is_empty();
            while let Some((kind, payload)) = protocol::read_frame(&mut reader)? {
                if kind == protocol::STDOUT && !found {
                    tail.extend_from_slice(&payload);
                    found = tail.windows(expected.len()).any(|v| v == expected);
                    let discard = tail.len().saturating_sub(expected.len().saturating_sub(1));
                    tail.drain(..discard);
                }
                command::send(&target, kind, &payload)?;
            }
            Ok(found)
        });
        (Arc::new(Mutex::new(writer)), Some(capture))
    } else {
        (sink.clone(), None)
    };
    let outcome = command::run(spec, &output, cancel, Some(Duration::from_secs(seconds)));
    let found = if let Some(capture) = capture {
        let _ = output.lock().unwrap().shutdown(Shutdown::Write);
        capture
            .join()
            .map_err(|_| "Formula output reader failed")?
            .map_err(|e| e.to_string())?
    } else {
        true
    };
    let outcome = outcome.map_err(|e| format!("{program}: {e}"))?;
    if outcome.cancelled {
        return Err("cancelled".into());
    }
    if outcome.timed_out {
        return Err(format!("{program} timed out after {seconds} seconds"));
    }
    if !outcome.success() {
        return Err(format!(
            "{program} exited with {:?}",
            outcome.code.or(outcome.signal)
        ));
    }
    if !found {
        return Err(format!(
            "{program} stdout did not contain the required text"
        ));
    }
    Ok(())
}

fn configure_git(root: &Path) -> io::Result<()> {
    fn repositories(root: &Path, dir: &Path, config: &mut String) -> io::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                let path = format!(
                    "/{}",
                    dir.strip_prefix(root).map_err(io::Error::other)?.display()
                );
                config.push_str(&format!(
                    "\tdirectory = \"{}\"\n",
                    path.replace('\\', "\\\\").replace('"', "\\\"")
                ));
            } else if entry.file_type()?.is_dir() {
                repositories(root, &entry.path(), config)?;
            }
        }
        Ok(())
    }
    let mut config = fs::read_to_string(root.join("etc/gitconfig")).unwrap_or_default();
    config.push_str("\n[safe]\n");
    repositories(root, &root.join(PACKAGES).join("sdks"), &mut config)?;
    fs::write(root.join("etc/gitconfig"), config)
}

fn configure(root: &Path) -> io::Result<()> {
    fs::write(root.join("etc/passwd"), "root:x:0:0:root:/root:/bin/sh\nbuilder:x:1000:1000:Builder:/work/home:/bin/bash\nnobody:x:65534:65534:nobody:/nonexistent:/bin/false\n")?;
    fs::write(
        root.join("etc/group"),
        "root:x:0:\nbuilder:x:1000:\nnogroup:x:65534:\n",
    )?;
    fs::write(
        root.join("etc/nsswitch.conf"),
        "passwd: files\ngroup: files\nhosts: files dns\n",
    )?;
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
        if !path.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                certificates(&entry.path(), output)?;
            } else if entry.path().extension().is_some_and(|e| e == "crt") {
                output.extend(fs::read(entry.path())?);
                output.push(b'\n');
            }
        }
        Ok(())
    }
    let mut bundle = Vec::new();
    certificates(&root.join("usr/share/ca-certificates"), &mut bundle)?;
    if !bundle.is_empty() {
        fs::write(root.join("etc/ssl/certs/ca-certificates.crt"), bundle)?;
    }
    Ok(())
}
