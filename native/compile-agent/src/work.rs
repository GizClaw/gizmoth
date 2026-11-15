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
            fs::copy(source, target)?;
            if metadata.nlink() > 1 { links.insert(key, target.to_path_buf()); }
        }
    } else {
        log(format!("not migrating an ephemeral IPC node: {}", source.display()));
        return Ok(());
    }
    sys::lchown(target, protocol::BUILDER_UID, protocol::BUILDER_UID)?;
    times(target, &metadata)?;
    fs::set_permissions(target, fs::Permissions::from_mode(metadata.mode() & 0o7777))?;
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
