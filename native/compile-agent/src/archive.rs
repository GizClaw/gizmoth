//! Shell-free extraction of downloaded formula sources. No distro, tar, ar or
//! decompressor executable is needed to bootstrap a VM's userland.
use flate2::read::GzDecoder;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn relative(path: &Path, strip: u32) -> io::Result<PathBuf> {
    if path.components().any(|p| !matches!(p, Component::Normal(_) | Component::CurDir)) {
        return Err(invalid(format!("unsafe archive path {}", path.display())));
    }
    Ok(path.components().filter(|p| matches!(p, Component::Normal(_))).skip(strip as usize).collect())
}

fn contained_link(path: &Path, link: &Path) -> io::Result<PathBuf> {
    let mut depth = path.parent().map_or(0, |p| p.components().count());
    for part in link.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {},
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return Err(invalid("archive link escapes its extraction root")),
        }
    }
    Ok(link.to_path_buf())
}

fn replace_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Err(invalid("archive file replaces a directory")),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn parents(root: &Path, path: &Path) -> io::Result<()> {
    let mut current = root.to_path_buf();
    if let Some(parent) = path.parent() {
        for part in parent.components() {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if !meta.is_dir() => return Err(invalid("archive ancestor is not a directory")),
                Ok(_) => {},
                Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn unpack_tar(reader: impl Read, target: &Path, strip: u32) -> io::Result<()> {
    use std::os::unix::fs::{symlink, PermissionsExt};
    fs::create_dir_all(target)?;
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = relative(&entry.path()?, strip)?;
        if path.as_os_str().is_empty() { continue; }
        parents(target, &path)?;
        let destination = target.join(&path);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            if fs::symlink_metadata(&destination).is_ok_and(|m| !m.is_dir()) {
                return Err(invalid("archive directory replaces a non-directory"));
            }
            fs::create_dir_all(&destination)?;
        } else {
            replace_file(&destination)?;
            if kind.is_file() {
                entry.unpack(&destination)?;
            } else if kind.is_symlink() {
                let link = entry.link_name()?.ok_or_else(|| invalid("missing symlink target"))?;
                // An absolute Linux link must stay in this formula's root,
                // including a foreign ABI installed below /opt/gizmoth/x86_64.
                let link = if link.is_absolute() {
                    let target = relative(link.strip_prefix("/").map_err(|_| invalid("invalid link"))?, 0)?;
                    let mut relative = PathBuf::new();
                    for _ in 0..path.parent().map_or(0, |p| p.components().count()) { relative.push(".."); }
                    relative.push(target); relative
                } else { link.into_owned() };
                symlink(contained_link(&path, &link)?, &destination)?;
            } else if kind.is_hard_link() {
                let link = entry.link_name()?.ok_or_else(|| invalid("missing hardlink target"))?;
                let link = relative(&link, strip)?;
                parents(target, &link)?;
                let source = target.join(link);
                if !fs::symlink_metadata(&source)?.is_file() { return Err(invalid("unsafe hardlink target")); }
                fs::hard_link(source, &destination)?;
            } else {
                return Err(invalid(format!("unsupported archive entry {}", path.display())));
            }
        }
        if !kind.is_symlink() {
            fs::set_permissions(&destination, fs::Permissions::from_mode(entry.header().mode()? & 0o777))?;
        }
    }
    Ok(())
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
}

fn unpack_xz(reader: impl Read, target: &Path, strip: u32) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(target)?;
    let path = target.join(format!(".gizmoth-extract-{}-{}.tar", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let temporary = Temporary(path.clone());
    let mut output = OpenOptions::new().write(true).create_new(true).open(&path)?;
    lzma_rs::xz_decompress(&mut BufReader::new(reader), &mut output)
        .map_err(|error| invalid(format!("xz: {error}")))?;
    drop(output);
    // The expanded tar stays on the assembly disk, not in a Vec proportional
    // to a multi-gigabyte SDK. The tar reader streams each file from there.
    let result = unpack_tar(File::open(&path)?, target, strip);
    drop(temporary);
    result
}

fn unpack_deb(source: &Path, target: &Path) -> io::Result<()> {
    let mut input = File::open(source)?;
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    if &magic != b"!<arch>\n" { return Err(invalid("not a Debian ar archive")); }
    loop {
        let mut header = [0; 60];
        match input.read(&mut header[..1])? {
            0 => return Err(invalid("Debian archive has no supported data member")),
            _ => input.read_exact(&mut header[1..])?,
        }
        if &header[58..] != b"`\n" { return Err(invalid("invalid ar header")); }
        let name = std::str::from_utf8(&header[..16]).map_err(|_| invalid("invalid ar member"))?.trim().trim_end_matches('/');
        let size: u64 = std::str::from_utf8(&header[48..58]).map_err(|_| invalid("invalid ar size"))?
            .trim().parse().map_err(|_| invalid("invalid ar size"))?;
        let start = input.stream_position()?;
        if size > input.metadata()?.len().saturating_sub(start) { return Err(invalid("truncated ar member")); }
        match name {
            "data.tar.xz" => return unpack_xz((&mut input).take(size), target, 0),
            "data.tar.gz" => return unpack_tar(GzDecoder::new((&mut input).take(size)), target, 0),
            "data.tar" => return unpack_tar((&mut input).take(size), target, 0),
            name if name.starts_with("data.tar") => return Err(invalid(format!("unsupported Debian payload {name}"))),
            _ => { input.seek(SeekFrom::Start(start.checked_add(size).and_then(|n| n.checked_add(size % 2)).ok_or_else(|| invalid("ar offset overflow"))?))?; }
        }
    }
}

fn unpack_zip(source: &Path, target: &Path, strip: u32) -> io::Result<()> {
    use std::os::unix::fs::{symlink, PermissionsExt};
    fs::create_dir_all(target)?;
    let mut archive = zip::ZipArchive::new(File::open(source)?)
        .map_err(|error| invalid(format!("zip: {error}")))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)
            .map_err(|error| invalid(format!("zip entry: {error}")))?;
        let name = entry.name().map_err(|error| invalid(format!("ZIP filename: {error}")))?;
        let path = relative(Path::new(name.as_ref()), strip)?;
        if path.as_os_str().is_empty() { continue; }
        parents(target, &path)?;
        let destination = target.join(&path);
        let mode = entry.unix_mode().unwrap_or(if entry.is_dir() { 0o40755 } else { 0o100644 });
        let kind = mode & 0o170000;
        if !matches!(kind, 0 | 0o100000 | 0o040000 | 0o120000) {
            return Err(invalid("unsupported ZIP entry type"));
        }
        if entry.is_dir() || kind == 0o040000 {
            if fs::symlink_metadata(&destination).is_ok_and(|m| !m.is_dir()) {
                return Err(invalid("archive directory replaces a non-directory"));
            }
            fs::create_dir_all(&destination)?;
        } else {
            replace_file(&destination)?;
            if kind == 0o120000 {
                if entry.size() > 4096 { return Err(invalid("ZIP symlink target is too long")); }
                let mut link = String::new();
                entry.take(4097).read_to_string(&mut link)?;
                if link.len() > 4096 { return Err(invalid("ZIP symlink target is too long")); }
                symlink(contained_link(&path, Path::new(&link))?, &destination)?;
            } else {
                // ZipArchive seeks through metadata, but entry payloads stream
                // straight to disk, including large SDK and compiler files.
                io::copy(&mut entry, &mut File::create(&destination)?)?;
            }
        }
        if kind != 0o120000 {
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

pub fn extract(source: &Path, target: &Path, format: &str, strip: u32) -> io::Result<()> {
    if fs::symlink_metadata(target).is_ok_and(|meta| !meta.is_dir()) {
        return Err(invalid("archive extraction root is not a directory"));
    }
    match format {
        "tar.gz" => unpack_tar(GzDecoder::new(File::open(source)?), target, strip),
        "tar.xz" => unpack_xz(File::open(source)?, target, strip),
        "tar.bz2" => unpack_tar(bzip2::read::BzDecoder::new(File::open(source)?), target, strip),
        "zip" => unpack_zip(source, target, strip),
        "deb" if strip == 0 => unpack_deb(source, target),
        _ => Err(invalid(format!("unsupported extraction format {format}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("gzm-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        root
    }

    fn tar_fixture() -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, data, mode) in [
            ("prefix/.git/HEAD", b"ref: refs/heads/main\n".as_slice(), 0o644),
            ("prefix/bin/compiler", b"compiler".as_slice(), 0o755),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64); header.set_mode(mode); header.set_cksum();
            tar.append_data(&mut header, path, data).unwrap();
        }
        tar.into_inner().unwrap()
    }

    #[test]
    fn tar_stripping_handles_long_paths_and_refuses_link_ancestors() {
        use flate2::{write::GzEncoder, Compression};
        let root = std::env::temp_dir().join(format!("gzm-tar-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let name = format!("prefix/lib/{}/module.py", "long-name".repeat(20));
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644); header.set_size(4); header.set_cksum();
        builder.append_data(&mut header, &name, &b"data"[..]).unwrap();
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&builder.into_inner().unwrap()).unwrap();
        let source = root.join("package.gz");
        fs::write(&source, gzip.finish().unwrap()).unwrap();
        extract(&source, &root.join("out"), "tar.gz", 1).unwrap();
        assert_eq!(fs::read(root.join("out").join(name.strip_prefix("prefix/").unwrap())).unwrap(), b"data");
        let mut builder = tar::Builder::new(Vec::new());
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink); link.set_size(0); link.set_mode(0o777);
        link.set_link_name("../../outside").unwrap(); link.set_cksum();
        builder.append_data(&mut link, "usr", io::empty()).unwrap();
        builder.append_data(&mut header, "usr/escape", &b"data"[..]).unwrap();
        assert!(unpack_tar(builder.into_inner().unwrap().as_slice(), &root.join("bad"), 0).is_err());
        assert!(!root.join("outside/escape").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn xz_and_bzip2_preserve_sdk_metadata_modes_and_component_stripping() {
        use std::os::unix::fs::PermissionsExt;
        let root = test_root("sdk-codecs");
        let tar = tar_fixture();
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut BufReader::new(tar.as_slice()), &mut xz).unwrap();
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
        bz.write_all(&tar).unwrap();
        for (format, bytes) in [("tar.xz", xz), ("tar.bz2", bz.finish().unwrap())] {
            let source = root.join(format!("input.{format}"));
            let output = root.join(format!("out.{format}"));
            fs::write(&source, bytes).unwrap();
            extract(&source, &output, format, 1).unwrap();
            assert_eq!(fs::read(output.join(".git/HEAD")).unwrap(), b"ref: refs/heads/main\n");
            assert_eq!(fs::read(output.join("bin/compiler")).unwrap(), b"compiler");
            assert_ne!(fs::metadata(output.join("bin/compiler")).unwrap().permissions().mode() & 0o111, 0);
            assert_eq!(fs::read_dir(&output).unwrap().count(), 2, "remove the xz spool");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_streams_files_and_preserves_contained_links_and_executables() {
        use std::os::unix::fs::PermissionsExt;
        use zip::write::SimpleFileOptions;
        let root = test_root("zip");
        let mut archive = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().unix_permissions(0o755);
        archive.start_file("prefix/bin/compiler", options).unwrap();
        archive.write_all(b"compiler").unwrap();
        archive.add_symlink("prefix/bin/cc", "compiler", options).unwrap();
        let source = root.join("input.zip");
        fs::write(&source, archive.finish().unwrap().into_inner()).unwrap();
        extract(&source, &root.join("out"), "zip", 1).unwrap();
        assert_eq!(fs::read(root.join("out/bin/cc")).unwrap(), b"compiler");
        assert_eq!(fs::read_link(root.join("out/bin/cc")).unwrap(), Path::new("compiler"));
        assert_ne!(fs::metadata(root.join("out/bin/compiler")).unwrap().permissions().mode() & 0o111, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_refuses_traversal_escaping_links_and_a_symlink_extraction_root() {
        use zip::write::SimpleFileOptions;
        use std::os::unix::fs::symlink;
        let root = test_root("zip-containment");
        for (name, link) in [("prefix/../escape", None), ("prefix/link", Some("../../escape"))] {
            let mut archive = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
            let options = SimpleFileOptions::default();
            if let Some(link) = link { archive.add_symlink(name, link, options).unwrap(); }
            else { archive.start_file(name, options).unwrap(); archive.write_all(b"bad").unwrap(); }
            let source = root.join("unsafe.zip");
            fs::write(&source, archive.finish().unwrap().into_inner()).unwrap();
            assert!(extract(&source, &root.join("out"), "zip", 1).is_err());
            assert!(!root.join("escape").exists());
        }
        let mut archive = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        archive.start_file("prefix/file", SimpleFileOptions::default()).unwrap();
        archive.write_all(b"data").unwrap();
        let source = root.join("safe.zip");
        fs::write(&source, archive.finish().unwrap().into_inner()).unwrap();
        fs::create_dir(root.join("outside")).unwrap();
        symlink(root.join("outside"), root.join("alias")).unwrap();
        assert!(extract(&source, &root.join("alias"), "zip", 1).is_err());
        assert_eq!(fs::read_dir(root.join("outside")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tar_rebases_absolute_links_and_rejects_standalone_escaping_links() {
        let root = test_root("tar-links");
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink); header.set_mode(0o777); header.set_size(0);
        header.set_link_name("/usr/lib/loader").unwrap(); header.set_cksum();
        tar.append_data(&mut header, "prefix/lib/loader", io::empty()).unwrap();
        unpack_tar(tar.into_inner().unwrap().as_slice(), &root.join("safe"), 1).unwrap();
        assert_eq!(fs::read_link(root.join("safe/lib/loader")).unwrap(), Path::new("../usr/lib/loader"));
        let mut tar = tar::Builder::new(Vec::new());
        header.set_link_name("../../outside").unwrap(); header.set_cksum();
        tar.append_data(&mut header, "prefix/link", io::empty()).unwrap();
        assert!(unpack_tar(tar.into_inner().unwrap().as_slice(), &root.join("unsafe"), 1).is_err());
        assert!(!root.join("unsafe/link").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn debian_payload_is_extracted_without_external_programs() {
        let path = std::env::temp_dir().join(format!("gzm-deb-test-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755); header.set_size(4); header.set_cksum();
        tar.append_data(&mut header, "usr/bin/demo", &b"test"[..]).unwrap();
        let tar = tar.into_inner().unwrap();
        let mut payload = Vec::new();
        lzma_rs::xz_compress(&mut BufReader::new(tar.as_slice()), &mut payload).unwrap();
        let deb = path.join("test.deb");
        let mut output = File::create(&deb).unwrap();
        output.write_all(b"!<arch>\n").unwrap();
        write!(output, "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", "data.tar.xz/", 0, 0, 0, "100644", payload.len()).unwrap();
        output.write_all(&payload).unwrap(); drop(output);
        extract(&deb, &path.join("root"), "deb", 0).unwrap();
        assert_eq!(fs::read(path.join("root/usr/bin/demo")).unwrap(), b"test");
        fs::remove_dir_all(path).unwrap();
    }
}
