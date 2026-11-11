//! Bounded extraction into a fresh, owned prefix. No archive-controlled path is
//! handed to tar's unpacker; links are created only after all regular files.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

type R<T> = Result<T, String>;
const MAX_ENTRIES: usize = 200_000;
const MAX_EXPANDED: u64 = 4 * 1024 * 1024 * 1024;
const MAX_FILE: u64 = 1024 * 1024 * 1024;

struct Link {
    path: String,
    target: String,
    resolved: String,
    hard: bool,
}

/// Package archives contain vendor data filenames, including spaces and Unicode.
/// Use portable filesystem rules here; registry manifest paths stay restrictive.
fn safe_component(value: &str) -> bool {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.ends_with(['.', ' '])
        || value.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return false;
    }
    let stem = value
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_lowercase();
    let numbered_device = stem
        .strip_prefix("com")
        .or_else(|| stem.strip_prefix("lpt"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    !numbered_device
        && !matches!(
            stem.as_str(),
            "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
        )
}

fn safe_archive_relative(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && value.split('/').all(safe_component)
}

fn archive_path(value: &str, strip: usize) -> R<Option<String>> {
    let value = value.trim_end_matches('/');
    // A conventional leading ./ is harmless, but dot/parent components inside
    // paths, drive prefixes and Windows separators are refused on every host.
    let value = value.strip_prefix("./").unwrap_or(value);
    if !safe_archive_relative(value) {
        return Err(format!("归档路径不安全：{value}"));
    }
    let parts: Vec<_> = value.split('/').collect();
    Ok((parts.len() > strip).then(|| parts[strip..].join("/")))
}

fn symlink_target(path: &str, target: &str) -> R<String> {
    if target.is_empty() || target.starts_with('/') || target.contains('\\') {
        return Err("归档链接不安全".into());
    }
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "." => (),
            ".." => {
                parts.pop().ok_or("归档链接指向安装目录以外")?;
            }
            part if safe_archive_relative(part) => parts.push(part),
            _ => return Err("归档链接不安全".into()),
        }
    }
    let result = parts.join("/");
    if !safe_archive_relative(&result) {
        return Err("归档链接不安全".into());
    }
    Ok(result)
}

/// Register implicit parents too, so A/file and a/other cannot alias on a
/// case-insensitive host, and a file/link can never become another entry's parent.
fn claim(paths: &mut BTreeMap<String, (String, bool, bool)>, path: &str, directory: bool) -> R<()> {
    let parts: Vec<_> = path.split('/').collect();
    for end in 1..=parts.len() {
        let name = parts[..end].join("/");
        let is_leaf = end == parts.len();
        let is_dir = !is_leaf || directory;
        match paths.get_mut(&name.to_lowercase()) {
            Some((previous, was_dir, explicit)) => {
                if *previous != name || *was_dir != is_dir || (is_leaf && *explicit) {
                    return Err(format!("归档路径冲突：{name}"));
                }
                if is_leaf {
                    *explicit = true;
                }
            }
            None => {
                paths.insert(name.to_lowercase(), (name, is_dir, is_leaf));
            }
        }
    }
    Ok(())
}

/// Scan raw headers first: tar otherwise allocates GNU/PAX metadata before
/// yielding an entry. This bounds those allocations and counts hidden headers.
fn preflight(archive: &Path, cancel: &impl Fn() -> R<()>) -> R<()> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut total = 0u64;
    for (index, entry) in tar
        .entries()
        .map_err(|e| e.to_string())?
        .raw(true)
        .enumerate()
    {
        cancel()?;
        if index >= MAX_ENTRIES {
            return Err("归档条目过多".into());
        }
        let mut entry = entry.map_err(|e| format!("归档无效：{e}"))?;
        let kind = entry.header().entry_type();
        let metadata = kind.is_gnu_longname()
            || kind.is_gnu_longlink()
            || kind.is_pax_local_extensions()
            || kind.is_pax_global_extensions();
        if !(kind.is_file()
            || kind.is_dir()
            || kind.is_symlink()
            || kind.is_hard_link()
            || metadata)
        {
            return Err("归档包含不支持的特殊文件".into());
        }
        let size = entry.size();
        total = total.checked_add(size).ok_or("归档展开大小溢出")?;
        if size > MAX_FILE || total > MAX_EXPANDED || (metadata && size > 64 * 1024) {
            return Err("归档展开文件过大".into());
        }
        let mut read = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            cancel()?;
            let n = entry
                .read(&mut buffer)
                .map_err(|e| format!("归档文件无效：{e}"))?;
            if n == 0 {
                break;
            }
            read += n as u64;
        }
        if read != size {
            return Err("归档文件不完整".into());
        }
    }
    Ok(())
}

pub fn tar_gz(archive: &Path, destination: &Path, strip: u32, cancel: impl Fn() -> R<()>) -> R<()> {
    cancel()?;
    preflight(archive, &cancel)?;
    // The caller creates its final installation prefix before extraction.
    if std::fs::symlink_metadata(destination).is_ok() {
        return Err("解压路径已存在".into());
    }
    std::fs::create_dir_all(destination).map_err(|e| format!("无法创建解压目录：{e}"))?;
    let root = std::fs::canonicalize(destination).map_err(|e| e.to_string())?;
    let file = std::fs::File::open(archive).map_err(|e| format!("无法读取归档：{e}"))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut paths = BTreeMap::new();
    let mut links = Vec::new();
    let mut total = 0u64;
    let mut count = 0usize;
    for entry in tar.entries().map_err(|e| format!("归档无效：{e}"))? {
        cancel()?;
        let mut entry = entry.map_err(|e| format!("归档无效：{e}"))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err("归档条目过多".into());
        }
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink() || kind.is_hard_link()) {
            return Err("归档包含不支持的特殊文件".into());
        }
        let raw = entry.path().map_err(|e| e.to_string())?;
        let raw = raw.to_str().ok_or("归档路径不是 UTF-8")?;
        let Some(path) = archive_path(raw, strip as usize)? else {
            if !kind.is_dir() {
                return Err("归档条目被完全剥离".into());
            }
            continue;
        };
        claim(&mut paths, &path, kind.is_dir())?;
        let size = entry.size();
        total = total.checked_add(size).ok_or("归档展开大小溢出")?;
        if size > MAX_FILE || total > MAX_EXPANDED {
            return Err("归档展开文件过大".into());
        }
        let target = root.join(&path);
        if kind.is_symlink() || kind.is_hard_link() {
            std::fs::create_dir_all(target.parent().ok_or("归档链接路径无效")?)
                .map_err(|e| e.to_string())?;
            if size != 0 {
                return Err("归档链接包含数据".into());
            }
            let name = entry
                .link_name()
                .map_err(|e| e.to_string())?
                .ok_or("归档链接缺少目标")?;
            let name = name.to_str().ok_or("归档链接不是 UTF-8")?.to_owned();
            let resolved = if kind.is_hard_link() {
                archive_path(&name, strip as usize)?.ok_or("归档硬链接目标被剥离")?
            } else {
                symlink_target(&path, &name)?
            };
            links.push(Link {
                path,
                target: name,
                resolved,
                hard: kind.is_hard_link(),
            });
        } else if kind.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        } else {
            std::fs::create_dir_all(target.parent().ok_or("解压路径无效")?)
                .map_err(|e| e.to_string())?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| format!("无法写入归档文件：{e}"))?;
            let mut copied = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                cancel()?;
                let n = entry
                    .read(&mut buffer)
                    .map_err(|e| format!("归档文件无效：{e}"))?;
                if n == 0 {
                    break;
                }
                copied += n as u64;
                if copied > size {
                    return Err("归档文件大小不匹配".into());
                }
                file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
            }
            if copied != size {
                return Err("归档文件不完整".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Strip suid/sgid/sticky and writable group/other bits.
                let mode = entry.header().mode().map_err(|e| e.to_string())?;
                std::fs::set_permissions(
                    &target,
                    std::fs::Permissions::from_mode(0o644 | (mode & 0o111)),
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    // Resolve chains in dependency order. Only a target already present within
    // root may be linked; cycles and dangling links fail without following out.
    while !links.is_empty() {
        let before = links.len();
        let mut pending = Vec::new();
        for link in links {
            cancel()?;
            let source = if link.hard {
                root.join(&link.resolved)
            } else {
                root.join(&link.path)
                    .parent()
                    .ok_or("归档链接路径无效")?
                    .join(&link.target)
            };
            let real = match std::fs::canonicalize(&source) {
                Ok(real) => real,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    pending.push(link);
                    continue;
                }
                Err(e) => return Err(format!("归档链接无效：{e}")),
            };
            if !real.starts_with(&root) {
                return Err("归档链接指向安装目录以外".into());
            }
            let destination = root.join(&link.path);
            std::fs::create_dir_all(destination.parent().ok_or("归档链接路径无效")?)
                .map_err(|e| e.to_string())?;
            if link.hard {
                if !real.is_file() {
                    return Err("归档硬链接目标不是文件".into());
                }
                std::fs::hard_link(real, destination)
                    .map_err(|e| format!("无法创建硬链接：{e}"))?;
            } else {
                create_symlink(Path::new(&link.target), &destination, real.is_dir())?;
            }
        }
        if pending.len() == before {
            return Err("归档包含循环或失效链接".into());
        }
        links = pending;
    }
    cancel()
}

fn create_symlink(target: &Path, destination: &Path, directory: bool) -> R<()> {
    #[cfg(unix)]
    let result = {
        let _ = directory;
        std::os::unix::fs::symlink(target, destination)
    };
    #[cfg(windows)]
    let result = if directory {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    };
    #[cfg(not(any(unix, windows)))]
    let result: std::io::Result<()> = {
        let _ = (target, destination, directory);
        Err(std::io::Error::other("symlink unsupported"))
    };
    result.map_err(|e| format!("无法创建归档链接：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(root: &Path, entries: &[(&str, tar::EntryType, &str, u32)]) -> PathBuf {
        let path = root.join("source.tar.gz");
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        ));
        for (name, kind, target, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_mode(*mode);
            let data = if kind.is_file() {
                b"python".as_slice()
            } else {
                &[]
            };
            header.set_size(data.len() as u64);
            if kind.is_symlink() || kind.is_hard_link() {
                header.set_link_name(target).unwrap();
            }
            // Set bytes directly so tests can include paths tar's writer refuses.
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder.append(&header, data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn extracts_normal_space_and_unicode_filenames() {
        let root = tempfile::tempdir().unwrap();
        let names = [
            "lib/python3.11/site-packages/setuptools/_vendor/jaraco/text/Lorem ipsum.txt",
            "lib/示例 目录/你好世界.txt",
            "lib/café/data file (copy).txt",
        ];
        let entries: Vec<_> = names
            .iter()
            .map(|name| (format!("python/{name}"), tar::EntryType::Regular, "", 0o644))
            .collect();
        let entries: Vec<_> = entries
            .iter()
            .map(|(name, kind, target, mode)| (name.as_str(), *kind, *target, *mode))
            .collect();
        let source = archive(root.path(), &entries);
        let out = root.path().join("out");
        tar_gz(&source, &out, 1, || Ok(())).unwrap();
        for name in names {
            assert_eq!(std::fs::read(out.join(name)).unwrap(), b"python");
        }
    }

    #[test]
    fn portable_archive_paths_still_reject_unsafe_components() {
        for path in [
            "../escape",
            "/absolute",
            "C:/drive",
            "C:drive",
            "\\\\server\\share",
            "bin\\tool",
            "bin/../escape",
            "bin/./file",
            "bin//file",
            "bin/trailing.",
            "bin/trailing ",
            "bin/new\nline",
            "bin/null\0byte",
            "bin/tab\tname",
            "bin/escape\u{1b}",
            "bin/CON",
            "bin/nul.txt",
            "bin/COM1.log",
            "bin/LPT9",
            "bin/COM¹.txt",
            "bin/con .txt",
            "bin/CONOUT$",
            "bin/file:stream",
            "bin/less<name",
            "bin/greater>name",
            "bin/quote\"name",
            "bin/pipe|name",
            "bin/question?name",
            "bin/star*name",
        ] {
            assert!(archive_path(path, 0).is_err(), "{path:?}");
        }
        assert_eq!(
            symlink_target("bin/工具", "../lib/示例 目录/tool file").unwrap(),
            "lib/示例 目录/tool file"
        );
        assert!(symlink_target("bin/工具", "../../outside file").is_err());
        assert!(symlink_target("bin/工具", "C:/outside file").is_err());
        let mut claimed = BTreeMap::new();
        claim(&mut claimed, "lib/Éxample/data", false).unwrap();
        assert!(claim(&mut claimed, "lib/éxample/other", false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unicode_and_space_link_targets_remain_contained() {
        let root = tempfile::tempdir().unwrap();
        let source = archive(
            root.path(),
            &[
                (
                    "python/bin/工具 link",
                    tar::EntryType::Symlink,
                    "../lib/示例 目录/tool file",
                    0o777,
                ),
                (
                    "python/lib/示例 目录/tool file",
                    tar::EntryType::Regular,
                    "",
                    0o755,
                ),
            ],
        );
        let out = root.path().join("out");
        tar_gz(&source, &out, 1, || Ok(())).unwrap();
        assert!(std::fs::symlink_metadata(out.join("bin/工具 link"))
            .unwrap()
            .is_symlink());
        assert_eq!(std::fs::read(out.join("bin/工具 link")).unwrap(), b"python");
    }

    #[test]
    fn rejects_traversal_special_entries_collisions_and_outside_links() {
        let file = tar::EntryType::Regular;
        for entries in [
            vec![("python/../outside", file, "", 0o644)],
            vec![("/python/outside", file, "", 0o644)],
            vec![("python/C:/outside", file, "", 0o644)],
            vec![("python/dev", tar::EntryType::Char, "", 0o644)],
            vec![
                ("python/Bin/a", file, "", 0o644),
                ("python/bin/b", file, "", 0o644),
            ],
            vec![(
                "python/bin/python",
                tar::EntryType::Symlink,
                "../../outside",
                0o777,
            )],
            vec![(
                "python/bin/python",
                tar::EntryType::Symlink,
                "/tmp/outside",
                0o777,
            )],
            vec![
                ("python/a", tar::EntryType::Symlink, "b", 0o777),
                ("python/a/child", file, "", 0o644),
            ],
            vec![
                ("python/a", tar::EntryType::Symlink, "b", 0o777),
                ("python/b", tar::EntryType::Symlink, "a", 0o777),
            ],
        ] {
            let root = tempfile::tempdir().unwrap();
            let source = archive(root.path(), &entries);
            assert!(
                tar_gz(&source, &root.path().join("out"), 1, || Ok(())).is_err(),
                "{entries:?}"
            );
            assert!(!root.path().join("outside").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn preserves_executables_and_forward_in_tree_links() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = archive(
            root.path(),
            &[
                (
                    "python/bin/python3",
                    tar::EntryType::Symlink,
                    "python3.12",
                    0o777,
                ),
                (
                    "python/bin/python3.12",
                    tar::EntryType::Symlink,
                    "../lib/python",
                    0o777,
                ),
                (
                    "python/bin/hard",
                    tar::EntryType::Link,
                    "python/lib/python",
                    0o777,
                ),
                ("python/lib/python", tar::EntryType::Regular, "", 0o6755),
            ],
        );
        let out = root.path().join("out");
        tar_gz(&source, &out, 1, || Ok(())).unwrap();
        assert!(std::fs::symlink_metadata(out.join("bin/python3"))
            .unwrap()
            .is_symlink());
        assert_eq!(std::fs::read(out.join("bin/python3")).unwrap(), b"python");
        assert_eq!(
            std::fs::metadata(out.join("bin/hard"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o755
        );
    }

    #[test]
    fn cancellation_and_expanded_bounds_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        let source = archive(
            root.path(),
            &[("python/a", tar::EntryType::Regular, "", 0o644)],
        );
        assert!(tar_gz(
            &source,
            &root.path().join("out"),
            1,
            || Err("已取消".into())
        )
        .unwrap_err()
        .contains("已取消"));
        assert!(!root.path().join("out").exists());
        let mut header = tar::Header::new_gnu();
        header.set_size(MAX_FILE + 1);
        header.set_mode(0o644);
        header.set_path("python/large").unwrap();
        header.set_cksum();
        let file = root.path().join("huge.tar.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&file).unwrap(),
            flate2::Compression::fast(),
        );
        encoder.write_all(header.as_bytes()).unwrap();
        encoder.finish().unwrap();
        assert!(tar_gz(&file, &root.path().join("large"), 1, || Ok(()))
            .unwrap_err()
            .contains("过大"));
    }
}
