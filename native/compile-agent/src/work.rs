//! One-time migration of an old slot disk into its new host-backed directory.
//! The host supplies a private clone for journal replay and publishes the new
//! directory only after this copy and its completion marker have succeeded.
use crate::{log, protocol, sys};
use std::{
    collections::HashMap,
    fs,
    io,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

fn exact_existing(path: &Path) -> io::Result<Option<fs::Metadata>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        let exact = fs::read_dir(parent)?.any(|entry| entry.is_ok_and(|entry| entry.file_name() == name));
        if !exact {
            return Err(io::Error::other(format!(
                "The host filesystem cannot represent the legacy filename distinctly: {}", path.display()
            )));
        }
    }
    Ok(Some(metadata))
}

fn times(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let name = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let values = [
        libc::timespec { tv_sec: metadata.atime(), tv_nsec: metadata.atime_nsec() },
        libc::timespec { tv_sec: metadata.mtime(), tv_nsec: metadata.mtime_nsec() },
    ];
    if unsafe { libc::utimensat(libc::AT_FDCWD, name.as_ptr(), values.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn copy_contents(source: &Path, target: &Path) -> io::Result<u64> {
    // fs::copy also applies the source mode, including setuid/setgid bits that
    // writable 9P cannot represent. Copy only data; apply supported modes below.
    io::copy(&mut fs::File::open(source)?, &mut fs::File::create(target)?)
}

fn copy(source: &Path, target: &Path, links: &mut HashMap<(u64, u64), PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    let existing = exact_existing(target)?;
    if metadata.is_dir() {
        if existing.as_ref().is_some_and(|m| !m.is_dir()) {
            return Err(io::Error::other(format!("Conflicting migration target: {}", target.display())));
        }
        if existing.is_none() { fs::create_dir(target)?; }
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy(&entry.path(), &target.join(entry.file_name()), links)?;
        }
    } else if metadata.file_type().is_symlink() {
        if existing.is_some() { return Err(io::Error::other("Conflicting migration symlink")); }
        symlink(fs::read_link(source)?, target)?;
        return Ok(());
    } else if metadata.is_file() {
        if existing.is_some() { return Err(io::Error::other("Conflicting migration file")); }
        let key = (metadata.dev(), metadata.ino());
        if let Some(first) = links.get(&key) {
            fs::hard_link(first, target)?;
        } else {
            copy_contents(source, target)?;
            if metadata.nlink() > 1 { links.insert(key, target.to_path_buf()); }
        }
    } else {
        log(format!("not migrating an ephemeral IPC node: {}", source.display()));
        return Ok(());
    }
    sys::lchown(target, protocol::BUILDER_UID, protocol::BUILDER_UID)?;
    times(target, &metadata)?;
    fs::set_permissions(target, fs::Permissions::from_mode(metadata.mode() & 0o1777))?;
    Ok(())
}

pub fn migrate(root: &Path, fingerprint: &str) -> io::Result<()> {
    if fingerprint.is_empty() { return Ok(()); }
    let old = root.join("run/legacy-work");
    fs::create_dir_all(&old)?;
    let device = sys::scsi_disk(1)?;
    if !sys::has_ext4(&device)? { return Err(io::Error::other("Legacy work disk is not ext4; original retained")); }
    // The host attaches a disposable clone, so ext4 may replay its journal
    // without changing the original disk that remains available as a backup.
    sys::mount(&device.to_string_lossy(), &old, "ext4", libc::MS_NOATIME, "noinit_itable")?;
    let target = root.join("work");
    let mut links = HashMap::new();
    log("migrating legacy build data into the host work directory");
    for entry in fs::read_dir(&old)? {
        let entry = entry?;
        if entry.file_name() == protocol::WORK_MIGRATION_MARKER {
            return Err(io::Error::other("Reserved migration marker already exists in the old work disk"));
        }
        copy(&entry.path(), &target.join(entry.file_name()), &mut links)?;
    }
    sys::umount(&old);
    fs::write(target.join(protocol::WORK_MIGRATION_MARKER), fingerprint)?;
    unsafe { libc::sync() };
    log("legacy build data copied successfully; original disk retained");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct Workspace {
        source: PathBuf,
        target: PathBuf,
    }

    impl Workspace {
        fn new() -> Self {
            // Worker jobs set TMPDIR=/work/tmp, which is writable 9P and cannot
            // represent legacy setuid/setgid modes. Guest /tmp is native tmpfs;
            // MS_NOSUID prevents execution with those bits, not storing them.
            let source_root = std::env::var_os("GIZMOTH_MIGRATION_TEST_SOURCE_ROOT")
                .map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
            let target_root = std::env::var_os("GIZMOTH_MIGRATION_TEST_TARGET_ROOT")
                .map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let name = format!("gzm-migration-test-{}-{nonce}", std::process::id());
            let workspace = Self {
                source: source_root.join(format!("{name}-source")),
                target: target_root.join(format!("{name}-target")),
            };
            fs::create_dir(&workspace.source).expect("create native migration source fixture");
            fs::create_dir(&workspace.target).expect("create migration target fixture");
            if std::env::var_os("GIZMOTH_MIGRATION_TEST_REQUIRE_9P").is_some() {
                #[cfg(target_os = "linux")]
                {
                    fn filesystem(path: &Path) -> libc::c_long {
                        use std::{ffi::CString, os::unix::ffi::OsStrExt};
                        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
                        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
                        assert_eq!(unsafe { libc::statfs(name.as_ptr(), &mut stat) }, 0,
                            "statfs {}: {}", path.display(), io::Error::last_os_error());
                        stat.f_type as libc::c_long
                    }
                    assert_eq!(filesystem(&workspace.source), 0x0102_1994, "source must be native tmpfs");
                    assert_eq!(filesystem(&workspace.target), 0x0102_1997, "target must be actual Linux 9P");
                    assert_ne!(fs::metadata(&workspace.source).unwrap().dev(), fs::metadata(&workspace.target).unwrap().dev());
                    assert_eq!(unsafe { libc::geteuid() }, protocol::BUILDER_UID, "run this regression as the real worker builder user");
                    assert_eq!(unsafe { libc::getegid() }, protocol::BUILDER_UID, "run this regression as the real worker builder group");
                    eprintln!("migration regression: source={} (tmpfs), target={} (9P), euid={}, egid={}",
                        workspace.source.display(), workspace.target.display(),
                        unsafe { libc::geteuid() }, unsafe { libc::getegid() });
                }
                #[cfg(not(target_os = "linux"))]
                panic!("the writable 9P regression requires Linux");
            }
            workspace
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.source);
            let _ = fs::remove_dir_all(&self.target);
        }
    }

    #[test]
    fn copy_contents_never_applies_source_setuid_or_setgid() {
        let workspace = Workspace::new();
        let source = workspace.source.join("source");
        let target = workspace.target.join("target");
        let data = b"legacy build data\0with binary contents\n";
        fs::write(&source, data).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o7751)).unwrap();
        assert_eq!(fs::metadata(&source).unwrap().mode() & 0o7777, 0o7751);

        assert_eq!(copy_contents(&source, &target).unwrap(), data.len() as u64);
        // Check before the final chmod: masking there alone cannot fix fs::copy.
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o6000, 0);
        assert_eq!(fs::read(&target).unwrap(), data);
        assert_eq!(fs::metadata(&source).unwrap().mode() & 0o7777, 0o7751);
        assert_eq!(fs::read(&source).unwrap(), data);
    }

    #[test]
    fn migration_normalizes_special_modes_and_preserves_data_and_metadata() {
        // Root is unnecessary here: worker jobs run as UID/GID 1000 and may
        // lchown their own new target to the same builder UID/current group.
        // Exercise that real syscall; do not bypass it or substitute ownership.
        let workspace = Workspace::new();
        let source = workspace.source.join("source");
        let target = workspace.target.join("target");
        fs::create_dir(&source).unwrap();
        let file = source.join("file");
        let data = b"keep the original build output\0\n";
        fs::write(&file, data).unwrap();
        fs::hard_link(&file, source.join("alias")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o7751)).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o3771)).unwrap();
        // Linux 9P declares one-second timestamp granularity and truncates
        // fractional times before sending Tsetattr. Use representable fixture
        // times, retaining exact equality checks at the destination's precision.
        let stamp = fs::FileTimes::new()
            .set_accessed(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_001));
        fs::File::open(&file).unwrap().set_times(stamp).unwrap();
        fs::File::open(&source).unwrap().set_times(stamp).unwrap();
        let original_dir = fs::metadata(&source).unwrap();
        let original_file = fs::metadata(&file).unwrap();
        assert_eq!(original_dir.mode() & 0o7777, 0o3771);
        assert_eq!(original_file.mode() & 0o7777, 0o7751);

        copy(&source, &target, &mut HashMap::new()).unwrap();

        let migrated_dir = fs::metadata(&target).unwrap();
        let migrated_file = fs::metadata(target.join("file")).unwrap();
        let alias = fs::metadata(target.join("alias")).unwrap();
        assert_eq!(migrated_dir.mode() & 0o7777, 0o1771);
        assert_eq!(migrated_file.mode() & 0o7777, 0o1751);
        for (migrated, original) in [(&migrated_dir, &original_dir), (&migrated_file, &original_file)] {
            assert_eq!((migrated.uid(), migrated.gid()), (protocol::BUILDER_UID, protocol::BUILDER_UID));
            assert_eq!((migrated.mtime(), migrated.mtime_nsec()), (original.mtime(), original.mtime_nsec()));
        }
        assert_eq!((migrated_dir.atime(), migrated_dir.atime_nsec()), (original_dir.atime(), original_dir.atime_nsec()));
        assert_eq!((migrated_file.dev(), migrated_file.ino()), (alias.dev(), alias.ino()));
        assert_eq!(migrated_file.nlink(), 2);
        assert_ne!((migrated_file.dev(), migrated_file.ino()), (original_file.dev(), original_file.ino()));
        assert_eq!(fs::read(target.join("file")).unwrap(), data);
        assert_eq!(fs::read(target.join("alias")).unwrap(), data);
        for (path, original) in [(&source, &original_dir), (&file, &original_file)] {
            let retained = fs::metadata(path).unwrap();
            assert_eq!(retained.mode(), original.mode());
            assert_eq!((retained.uid(), retained.gid()), (original.uid(), original.gid()));
            assert_eq!((retained.mtime(), retained.mtime_nsec()), (original.mtime(), original.mtime_nsec()));
        }
        assert_eq!(fs::read(&file).unwrap(), data);
    }
}
