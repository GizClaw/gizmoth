//! GizMoth's working directory: where compile data lives. `storage.json` in
//! `<app data>/compile` records a custom location; without it the data stays in
//! `<app data>/compile` itself. Moving renames on the same volume and otherwise
//! copies with holes preserved, so sparse disk images stay small.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// Everything stored in the working directory, moved together.
pub const ITEMS: &[&str] = &[
    "assets",
    "slots",
    "cache",
    "artifacts",
    "packages.json",
    "packages.erofs",
    "packages.erofs.json",
];
pub const MARKER: &str = ".gizmoth-working-directory";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageConfig {
    #[serde(default)]
    pub working_directory: Option<PathBuf>,
}

pub fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("storage.json")
}

/// The configured working directory, or the default `<app data>/compile`.
pub fn configured_root(config_dir: &Path) -> PathBuf {
    fs::read(config_path(config_dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StorageConfig>(&bytes).ok())
        .and_then(|config| config.working_directory)
        .unwrap_or_else(|| config_dir.to_path_buf())
}

pub fn save_root(config_dir: &Path, root: &Path) -> io::Result<()> {
    let path = config_path(config_dir);
    if root == config_dir {
        return match fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let config = StorageConfig {
        working_directory: Some(root.to_path_buf()),
    };
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&config).map_err(io::Error::other)?,
    )?;
    fs::rename(temporary, path)
}

/// Bytes actually allocated below a path (sparse files count their data only).
pub fn allocated(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.blocks() * 512;
    }
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            total += allocated(&entry.path());
        }
    }
    total
}

/// Free space on the volume holding `path` (or its nearest existing ancestor).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
    }
    let c = std::ffi::CString::new(probe.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut stat) } == 0)
        .then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}

fn is_empty_dir(path: &Path) -> io::Result<bool> {
    Ok(fs::read_dir(path)?
        .flatten()
        .all(|entry| entry.file_name() == ".DS_Store"))
}

/// Checks a new working directory and returns its absolute path.
pub fn validate_target(
    current: &Path,
    config_dir: &Path,
    target: &Path,
) -> Result<PathBuf, String> {
    if !target.is_absolute() {
        return Err("The working directory must be an absolute path".into());
    }
    let parent = target
        .parent()
        .ok_or("Choose a folder, not the disk root")?;
    let parent = parent
        .canonicalize()
        .map_err(|_| format!("{} does not exist", parent.display()))?;
    let target = parent.join(target.file_name().ok_or("Choose a folder")?);
    let current = current
        .canonicalize()
        .unwrap_or_else(|_| current.to_path_buf());
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    if target == current {
        return Err("This is already the working directory".into());
    }
    if target.starts_with(&current) || (current.starts_with(&target) && target != config_dir) {
        return Err(
            "The new working directory cannot be inside the current one, or contain it".into(),
        );
    }
    if target.exists() {
        if !target.is_dir() {
            return Err(format!("{} is not a folder", target.display()));
        }
        let reusable = target == config_dir || target.join(MARKER).is_file();
        if !reusable && !is_empty_dir(&target).map_err(|e| e.to_string())? {
            return Err("Choose an empty folder, or create a new one".into());
        }
        if ITEMS.iter().any(|item| target.join(item).exists()) {
            return Err(
                "That folder already holds GizMoth compile data; choose an empty folder".into(),
            );
        }
    }
    let probe = target.join(".gizmoth-write-test");
    fs::create_dir_all(&target)
        .and_then(|_| fs::write(&probe, b""))
        .and_then(|_| fs::remove_file(&probe))
        .map_err(|e| format!("{} is not writable: {e}", target.display()))?;
    Ok(target)
}

fn device(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|meta| meta.dev())
}

/// Copies a file, skipping holes so sparse disk images stay sparse.
pub fn copy_sparse(source: &Path, target: &Path, progress: &mut dyn FnMut(u64)) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let mut input = File::open(source)?;
    let length = input.metadata()?.len();
    let mut output = File::create(target)?;
    let fd = input.as_raw_fd();
    let mut buffer = vec![0u8; 1 << 20];
    let mut offset: i64 = 0;
    while (offset as u64) < length {
        let data = unsafe { libc::lseek(fd, offset, libc::SEEK_DATA) };
        if data < 0 {
            // ENXIO: only a hole remains.
            if io::Error::last_os_error().raw_os_error() == Some(libc::ENXIO) {
                break;
            }
            return Err(io::Error::last_os_error());
        }
        let hole = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
        let end = if hole < 0 { length as i64 } else { hole };
        input.seek(SeekFrom::Start(data as u64))?;
        output.seek(SeekFrom::Start(data as u64))?;
        let mut left = (end - data) as u64;
        while left > 0 {
            let want = left.min(buffer.len() as u64) as usize;
            let n = input.read(&mut buffer[..want])?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
            left -= n as u64;
            progress(n as u64);
        }
        offset = end;
    }
    output.set_len(length)?;
    output.sync_all()?;
    let meta = fs::metadata(source)?;
    let _ = output.set_modified(meta.modified()?);
    fs::set_permissions(target, fs::Permissions::from_mode(meta.mode() & 0o7777))
}

/// Only service-owned staging locations are transient. Artifact file names
/// belong to the build and can legitimately look like temporary files.
fn transient(relative: &Path) -> bool {
    let parts: Vec<_> = relative.iter().collect();
    match parts.as_slice() {
        [area, name] if *area == "assets" => name
            .to_str()
            .and_then(|name| name.strip_suffix(".partial"))
            .is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
            }),
        [area, name] if *area == "cache" => {
            *name == ".incoming"
                || name
                    .to_str()
                    .and_then(|name| name.strip_prefix(".trash-"))
                    .and_then(|name| {
                        let split = name.len().checked_sub(37)?;
                        let (namespace, suffix) = (name.get(..split)?, name.get(split..)?);
                        Some((namespace, suffix.strip_prefix('-')?))
                    })
                    .is_some_and(|(namespace, id)| {
                        super::cache::valid_namespace(namespace)
                            && uuid::Uuid::parse_str(id).is_ok()
                    })
        }
        [area, namespace, name] if *area == "cache" => {
            // Older stores placed uploads directly inside each namespace.
            *name == ".incoming"
                && namespace
                    .to_str()
                    .is_some_and(super::cache::valid_namespace)
        }
        [area, name] if *area == "artifacts" => *name == ".staging",
        _ => false,
    }
}

fn copy_tree(
    source: &Path,
    target: &Path,
    relative: &Path,
    progress: &mut dyn FnMut(u64),
    cancel: &dyn Fn() -> bool,
) -> io::Result<()> {
    if cancel() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        return std::os::unix::fs::symlink(fs::read_link(source)?, target);
    }
    if !meta.is_dir() {
        return copy_sparse(source, target, progress);
    }
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let relative = relative.join(&name);
        if transient(&relative) {
            continue;
        }
        copy_tree(
            &entry.path(),
            &target.join(&name),
            &relative,
            progress,
            cancel,
        )?;
    }
    fs::set_permissions(target, fs::Permissions::from_mode(meta.mode() & 0o7777))
}

/// Moves every item from `from` to `to`. On failure nothing has moved and the
/// partial copy is removed. Progress receives (bytes done, bytes total).
pub fn move_items(
    from: &Path,
    to: &Path,
    progress: &dyn Fn(u64, u64),
    cancel: &dyn Fn() -> bool,
) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    fs::write(to.join(MARKER), b"GizMoth working directory\n").map_err(|e| e.to_string())?;
    let present: Vec<&str> = ITEMS
        .iter()
        .copied()
        .filter(|item| fs::symlink_metadata(from.join(item)).is_ok())
        .collect();
    if device(from).is_some() && device(from) == device(to) {
        let mut moved = Vec::new();
        for item in &present {
            if let Err(error) = fs::rename(from.join(item), to.join(item)) {
                for back in moved {
                    let _ = fs::rename(to.join(back), from.join(back));
                }
                return Err(format!("{item}: {error}"));
            }
            moved.push(*item);
        }
        progress(1, 1);
        return Ok(());
    }
    copy_items(from, to, &present, progress, cancel)
}

/// Removes an entry itself, without following a symlink to a directory.
fn remove_entry(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The cross-volume half of [`move_items`].
fn copy_items(
    from: &Path,
    to: &Path,
    present: &[&str],
    progress: &dyn Fn(u64, u64),
    cancel: &dyn Fn() -> bool,
) -> Result<(), String> {
    let total: u64 = present.iter().map(|item| allocated(&from.join(item))).sum();
    if free_space(to).is_some_and(|free| free < total) {
        return Err(format!(
            "The new location needs {} free",
            crate::compile::jobs::human(total)
        ));
    }
    let mut done = 0u64;
    let mut last = std::time::Instant::now();
    let mut report = |n: u64| {
        done += n;
        if last.elapsed() >= std::time::Duration::from_millis(250) {
            last = std::time::Instant::now();
            progress(done.min(total), total);
        }
    };
    for item in present {
        if let Err(error) = copy_tree(
            &from.join(item),
            &to.join(item),
            Path::new(item),
            &mut report,
            cancel,
        ) {
            let mut message = if error.kind() == io::ErrorKind::Interrupted {
                "cancelled".into()
            } else {
                format!("{item}: {error}")
            };
            for item in present {
                if let Err(error) = remove_entry(&to.join(item)) {
                    message.push_str(&format!("; could not remove {item}: {error}"));
                }
            }
            return Err(message);
        }
    }
    progress(total, total);
    for item in present {
        if let Err(error) = remove_entry(&from.join(item)) {
            // The destination is already complete. Returning an error here
            // would keep the service pointed at a partially removed source.
            eprintln!("Could not remove migrated compile data {item}: {error}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_copy_keeps_holes_and_contents() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("disk.img");
        let mut file = File::create(&source).unwrap();
        file.set_len(64 << 20).unwrap();
        file.seek(SeekFrom::Start(32 << 20)).unwrap();
        file.write_all(b"data in the middle").unwrap();
        drop(file);
        let target = dir.path().join("copy.img");
        let mut copied = 0;
        copy_sparse(&source, &target, &mut |n| copied += n).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().len(), 64 << 20);
        assert!(
            copied < 8 << 20,
            "copied {copied} bytes of a mostly empty image"
        );
        assert!(allocated(&target) < 8 << 20);
        let mut copy = File::open(&target).unwrap();
        copy.seek(SeekFrom::Start(32 << 20)).unwrap();
        let mut text = [0u8; 18];
        copy.read_exact(&mut text).unwrap();
        assert_eq!(&text, b"data in the middle");
    }

    #[test]
    fn copying_skips_transient_entries_and_can_be_cancelled() {
        let base = tempfile::tempdir().unwrap();
        let (from, to) = (base.path().join("from"), base.path().join("to"));
        fs::create_dir_all(from.join("cache/bazel/.incoming")).unwrap();
        fs::write(from.join("cache/bazel/.incoming/partial"), b"p").unwrap();
        fs::write(from.join("cache/bazel/entry"), b"cached").unwrap();
        fs::create_dir_all(from.join("artifacts/a/files")).unwrap();
        fs::write(from.join("artifacts/a/files/app.bin"), b"bin").unwrap();
        let outputs = [
            "artifacts/a/files/app.partial",
            "artifacts/a/files/.incoming/data",
            "artifacts/a/files/.staging/data",
            "artifacts/a/files/.trash-output/data",
            "artifacts/a/files/nested/.incoming/data.partial",
            "cache/bazel/ac/entry.partial",
            "assets/notes.partial",
            "assets/nested/.incoming/data.partial",
        ];
        for output in outputs {
            let path = from.join(output);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, output.as_bytes()).unwrap();
        }
        let asset_partial = format!("assets/{}.partial", "a".repeat(64));
        let cache_trash = format!("cache/.trash-bazel-{}", uuid::Uuid::new_v4());
        let transient_files = [
            "cache/.incoming/partial",
            "artifacts/.staging/a/files/partial",
            asset_partial.as_str(),
            cache_trash.as_str(),
        ];
        for transient in transient_files {
            let path = from.join(transient);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"temporary").unwrap();
        }
        fs::set_permissions(
            from.join("artifacts/a/files/app.bin"),
            fs::Permissions::from_mode(0o444),
        )
        .unwrap();
        fs::create_dir_all(&to).unwrap();
        let items = ["assets", "cache", "artifacts"];
        assert_eq!(
            copy_items(&from, &to, &items, &|_, _| {}, &|| true).unwrap_err(),
            "cancelled"
        );
        assert!(!to.join("cache").exists() && from.join("cache/bazel/entry").exists());
        let last = std::cell::Cell::new((0, 0));
        copy_items(
            &from,
            &to,
            &items,
            &|done, total| last.set((done, total)),
            &|| false,
        )
        .unwrap();
        assert_eq!(last.get().0, last.get().1);
        assert_eq!(fs::read(to.join("cache/bazel/entry")).unwrap(), b"cached");
        assert!(!to.join("cache/bazel/.incoming").exists());
        for transient in transient_files {
            assert!(!to.join(transient).exists(), "{transient} must be skipped");
        }
        for output in outputs {
            assert_eq!(
                fs::read(to.join(output)).unwrap(),
                output.as_bytes(),
                "{output} must be preserved"
            );
        }
        let meta = fs::metadata(to.join("artifacts/a/files/app.bin")).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o444);
        assert!(!from.join("cache").exists() && !from.join("artifacts").exists());
    }

    #[test]
    fn cross_volume_copy_removes_source_files_and_symlinks() {
        let base = tempfile::tempdir().unwrap();
        let (from, to) = (base.path().join("from"), base.path().join("to"));
        let outside = base.path().join("outside");
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), b"untouched").unwrap();
        std::os::unix::fs::symlink(&outside, from.join("assets")).unwrap();
        for item in ["packages.json", "packages.erofs", "packages.erofs.json"] {
            fs::write(from.join(item), item.as_bytes()).unwrap();
        }
        std::os::unix::fs::symlink("missing", from.join("artifacts")).unwrap();
        let items = [
            "assets",
            "packages.json",
            "packages.erofs",
            "packages.erofs.json",
            "artifacts",
        ];
        copy_items(&from, &to, &items, &|_, _| {}, &|| false).unwrap();
        for item in items {
            assert!(
                fs::symlink_metadata(from.join(item)).is_err(),
                "{item} must leave the source"
            );
            assert!(
                fs::symlink_metadata(to.join(item)).is_ok(),
                "{item} must reach the destination"
            );
        }
        assert_eq!(fs::read_link(to.join("assets")).unwrap(), outside);
        assert_eq!(
            fs::read_link(to.join("artifacts")).unwrap(),
            Path::new("missing")
        );
        for item in ["packages.json", "packages.erofs", "packages.erofs.json"] {
            assert_eq!(fs::read(to.join(item)).unwrap(), item.as_bytes());
        }
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"untouched");
    }

    #[test]
    fn cancelled_cross_volume_copy_removes_files_and_links_but_keeps_sources() {
        let base = tempfile::tempdir().unwrap();
        let (from, to) = (base.path().join("from"), base.path().join("to"));
        fs::create_dir_all(from.join("artifacts/a/files")).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(from.join("packages.json"), b"index").unwrap();
        fs::write(from.join("packages.erofs"), b"pack").unwrap();
        let outside = base.path().join("keep");
        fs::write(&outside, b"untouched").unwrap();
        std::os::unix::fs::symlink(&outside, from.join("packages.erofs.json")).unwrap();
        let items = [
            "packages.json",
            "packages.erofs",
            "packages.erofs.json",
            "artifacts",
        ];
        let calls = std::cell::Cell::new(0);
        let cancel = || {
            let n = calls.get() + 1;
            calls.set(n);
            n == 5 // The package files and link, then the artifacts root, were copied.
        };
        assert_eq!(
            copy_items(&from, &to, &items, &|_, _| {}, &cancel).unwrap_err(),
            "cancelled"
        );
        for item in items {
            assert!(
                fs::symlink_metadata(to.join(item)).is_err(),
                "{item} must be cleaned up"
            );
            assert!(
                fs::symlink_metadata(from.join(item)).is_ok(),
                "{item} must stay at the source"
            );
        }
        assert_eq!(fs::read(from.join("packages.json")).unwrap(), b"index");
        assert_eq!(fs::read(from.join("packages.erofs")).unwrap(), b"pack");
        assert_eq!(fs::read(&outside).unwrap(), b"untouched");
    }

    #[test]
    fn targets_are_validated_and_items_move() {
        let base = tempfile::tempdir().unwrap();
        let config = base.path().join("compile");
        fs::create_dir_all(config.join("cache/bazel/ac")).unwrap();
        fs::write(config.join("cache/bazel/ac/k"), b"v").unwrap();
        fs::create_dir_all(config.join("artifacts")).unwrap();
        fs::write(config.join("settings.json"), b"{}").unwrap();
        assert!(validate_target(&config, &config, Path::new("relative")).is_err());
        assert!(validate_target(&config, &config, &config.join("inner")).is_err());
        let busy = base.path().join("busy");
        fs::create_dir_all(&busy).unwrap();
        fs::write(busy.join("notes.txt"), b"x").unwrap();
        assert!(validate_target(&config, &config, &busy).is_err());
        let target = validate_target(&config, &config, &base.path().join("work")).unwrap();
        move_items(&config, &target, &|_, _| {}, &|| false).unwrap();
        assert_eq!(fs::read(target.join("cache/bazel/ac/k")).unwrap(), b"v");
        assert!(!config.join("cache").exists());
        assert!(config.join("settings.json").exists());
        save_root(&config, &target).unwrap();
        assert_eq!(configured_root(&config), target);
        // Moving back to the default location is allowed although it holds settings.
        let back = validate_target(&target, &config, &config).unwrap();
        move_items(&target, &back, &|_, _| {}, &|| false).unwrap();
        save_root(&config, &config).unwrap();
        assert_eq!(configured_root(&config), config);
        assert!(config.join("cache/bazel/ac/k").exists());
    }
}
