//! Default command exports, generated only after all versioned packages exist.
use crate::protocol::{COMMAND_BIN, GIZMOTH_PREFIX};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Component, Path, PathBuf},
};

// Resolve symlinks with the assembled guest's root semantics. A normal host
// metadata()/canonicalize() would resolve an absolute archive link against the
// packer VM's own filesystem instead of the future worker root.
fn resolve(root: &Path, path: &Path, links: u32) -> io::Result<PathBuf> {
    let mut resolved = PathBuf::new();
    let mut parts = path.components();
    while let Some(part) = parts.next() {
        match part {
            Component::RootDir => resolved.clear(),
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    return Err(io::Error::other("command path escapes the guest root"));
                }
            }
            Component::Normal(name) => {
                resolved.push(name);
                if fs::symlink_metadata(root.join(&resolved))?
                    .file_type()
                    .is_symlink()
                {
                    if links == 40 {
                        return Err(io::Error::other("too many command path symlinks"));
                    }
                    let target = fs::read_link(root.join(&resolved))?;
                    resolved.pop();
                    let next = resolved.join(target).join(parts.as_path());
                    return resolve(root, &next, links + 1);
                }
            }
            _ => return Err(io::Error::other("invalid guest command path")),
        }
    }
    Ok(root.join(resolved))
}

// A symlink changes argv[0] and can break copied Python venvs and scripts that
// locate resources beside $0. Exec the original absolute path instead. Quote
// bytes directly, including apostrophes, without assuming UTF-8 filenames.
fn launcher(target: &Path) -> Vec<u8> {
    let mut script = b"#!/bin/sh\nexec '".to_vec();
    for byte in target.as_os_str().as_bytes() {
        if *byte == b'\'' {
            script.extend_from_slice(b"'\\''");
        } else {
            script.push(*byte);
        }
    }
    script.extend_from_slice(b"' \"$@\"\n");
    script
}

/// Export executable files from default formula prependPath directories.
/// First declaration wins, preserving the old PATH's collision behavior.
/// Exact-version setup/check environments never use this global activation.
pub fn commands(root: &Path, paths: &[String]) -> io::Result<()> {
    // These are application-owned paths. Never follow payload-provided links
    // while creating the activation directory, or overwrite payload files.
    let mut prefix = root.to_path_buf();
    for part in Path::new(GIZMOTH_PREFIX).components() {
        if let Component::Normal(part) = part {
            prefix.push(part);
            match fs::symlink_metadata(&prefix) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(io::Error::other(
                        "managed prefix must contain real directories",
                    ))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&prefix)?,
                Err(error) => return Err(error),
            }
        }
    }
    let bin = root.join(COMMAND_BIN.trim_start_matches('/'));
    fs::create_dir(&bin)?;
    let mut exports = BTreeMap::new();
    for path in paths {
        let path = Path::new(path);
        if !path.is_absolute()
            || path.components().any(|p| {
                !matches!(
                    p,
                    Component::RootDir | Component::Normal(_) | Component::CurDir
                )
            })
        {
            return Err(io::Error::other("invalid formula command directory"));
        }
        let directory = match resolve(root, path, 0) {
            Ok(directory) => directory,
            // As with PATH, an absent optional command directory is harmless.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let target = path.join(entry.file_name());
            let resolved = match resolve(root, &target, 0) {
                Ok(resolved) => resolved,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let meta = fs::metadata(resolved)?;
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                exports.entry(entry.file_name()).or_insert(target);
            }
        }
    }
    for (name, target) in exports {
        let destination = bin.join(name);
        fs::write(&destination, launcher(&target))?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::symlink,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Tree(PathBuf);
    impl Tree {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "gzm-activation-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, path: &str, executable: bool) {
            let target = self.0.join(path.trim_start_matches('/'));
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, b"fixture").unwrap();
            fs::set_permissions(
                target,
                fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
            )
            .unwrap();
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exports_executables_and_aliases_with_first_path_precedence() {
        let tree = Tree::new();
        tree.file("opt/gizmoth/packages/python/3/bin/python3", true);
        tree.file("opt/gizmoth/packages/python/3/bin/data", false);
        tree.file("opt/gizmoth/packages/other/bin/python3", true);
        tree.file("opt/gizmoth/packages/other/bin/ninja", true);
        let python = tree.0.join("opt/gizmoth/packages/python/3/bin");
        symlink("python3", python.join("python")).unwrap();
        symlink(
            "/opt/gizmoth/packages/python/3/bin/python3",
            python.join("absolute-python"),
        )
        .unwrap();
        symlink("absent", python.join("dangling")).unwrap();
        fs::create_dir(python.join("subdirectory")).unwrap();
        commands(
            &tree.0,
            &[
                "/opt/gizmoth/packages/python/3/bin".into(),
                "/opt/gizmoth/packages/other/bin".into(),
                "/missing".into(),
            ],
        )
        .unwrap();
        let bin = tree.0.join("opt/gizmoth/bin");
        assert_eq!(
            fs::read(bin.join("python3")).unwrap(),
            b"#!/bin/sh\nexec '/opt/gizmoth/packages/python/3/bin/python3' \"$@\"\n"
        );
        assert_eq!(
            fs::read(bin.join("python")).unwrap(),
            b"#!/bin/sh\nexec '/opt/gizmoth/packages/python/3/bin/python' \"$@\"\n"
        );
        assert_eq!(
            fs::read(bin.join("ninja")).unwrap(),
            b"#!/bin/sh\nexec '/opt/gizmoth/packages/other/bin/ninja' \"$@\"\n"
        );
        assert_eq!(
            fs::metadata(bin.join("python3"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(fs::symlink_metadata(bin.join("absolute-python")).is_ok());
        for name in ["data", "dangling", "subdirectory"] {
            assert!(fs::symlink_metadata(bin.join(name)).is_err());
        }
    }

    #[test]
    fn absolute_directory_links_resolve_inside_the_guest() {
        let tree = Tree::new();
        tree.file("usr/bin/guest-tool", true);
        symlink("/usr/bin", tree.0.join("bin")).unwrap();
        commands(&tree.0, &["/bin".into()]).unwrap();
        assert_eq!(
            fs::read(tree.0.join("opt/gizmoth/bin/guest-tool")).unwrap(),
            b"#!/bin/sh\nexec '/bin/guest-tool' \"$@\"\n"
        );
    }

    #[test]
    fn launchers_preserve_original_script_location_and_literal_arguments() {
        let tree = Tree::new();
        let target = tree.0.join("tool's $location");
        fs::write(&target, b"#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\"\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let command = tree.0.join("public-command");
        fs::write(&command, launcher(&target)).unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(command)
            .args(["space argument", "'$(exit 55)'", ""])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\nspace argument\n'$(exit 55)'\n\n", target.display())
        );
    }

    #[test]
    fn rejects_symlink_loops_root_escapes_and_reserved_bin_payloads() {
        let tree = Tree::new();
        tree.file("tools/ok", true);
        symlink("loop", tree.0.join("tools/loop")).unwrap();
        assert!(commands(&tree.0, &["/tools".into()]).is_err());
        fs::remove_dir(tree.0.join("opt/gizmoth/bin")).unwrap();
        fs::remove_file(tree.0.join("tools/loop")).unwrap();
        symlink("../../outside", tree.0.join("tools/escape")).unwrap();
        assert!(commands(&tree.0, &["/tools".into()]).is_err());
        assert!(
            commands(&tree.0, &[]).is_err(),
            "never overwrite an existing activation directory"
        );
        let tree = Tree::new();
        let outside = Tree::new();
        fs::create_dir(tree.0.join("opt")).unwrap();
        symlink(&outside.0, tree.0.join("opt/gizmoth")).unwrap();
        assert!(commands(&tree.0, &[]).is_err());
        assert!(!outside.0.join("bin").exists());
    }
}
