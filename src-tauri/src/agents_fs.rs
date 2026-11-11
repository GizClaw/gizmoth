//! Atomic, no-clobber moves between pinned directories. Unlike hard-link
//! publication, these also support writable filesystems without hard links.
use cap_std::fs::Dir;
use std::{
    ffi::OsStr,
    io,
    path::{Component, Path},
};

fn child(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a child filename",
        ));
    }
    Ok(())
}

pub(super) fn rename_new(from: &Dir, source: &OsStr, to: &Dir, target: &OsStr) -> io::Result<()> {
    child(source)?;
    child(target)?;
    platform_rename_new(from, source, to, target)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn platform_rename_new(from: &Dir, source: &OsStr, to: &Dir, target: &OsStr) -> io::Result<()> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };
    let source = CString::new(source.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let target = CString::new(target.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: both directory handles and NUL-terminated child names remain live
    // through the syscall. EXCL/NOREPLACE prevents replacing a concurrent save.
    let result = unsafe {
        #[cfg(target_os = "macos")]
        {
            libc::renameatx_np(
                from.as_raw_fd(),
                source.as_ptr(),
                to.as_raw_fd(),
                target.as_ptr(),
                libc::RENAME_EXCL,
            )
        }
        #[cfg(target_os = "linux")]
        {
            libc::renameat2(
                from.as_raw_fd(),
                source.as_ptr(),
                to.as_raw_fd(),
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        }
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn platform_rename_new(from: &Dir, source: &OsStr, to: &Dir, target: &OsStr) -> io::Result<()> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
    use cap_std::fs::{OpenOptions, OpenOptionsExt};
    use std::{
        ffi::c_void,
        mem::{offset_of, size_of},
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
    };

    #[repr(C)]
    struct RenameInfo {
        flags: u32,
        root: *mut c_void,
        name_bytes: u32,
        name: [u16; 1],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileInformationByHandle(
            file: *mut c_void,
            class: i32,
            info: *const c_void,
            size: u32,
        ) -> i32;
    }
    // DELETE access is required for renaming; operate relative to the pinned
    // handles instead of reconstructing an ambient or UNC path.
    let mut options = OpenOptions::new();
    options
        .read(true)
        .access_mode(0x0001_0000 | 0x0010_0000)
        .follow(FollowSymlinks::No);
    let file = from.open_with(source, &options)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a regular file",
        ));
    }
    let name: Vec<u16> = target.encode_wide().collect();
    let offset = offset_of!(RenameInfo, name);
    let bytes = offset + name.len() * size_of::<u16>();
    let length = u32::try_from(bytes).map_err(|_| io::ErrorKind::InvalidInput)?;
    // A word-aligned allocation provides the alignment required by HANDLE.
    let mut buffer = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<RenameInfo>();
    // SAFETY: the allocation covers the complete variable-length structure,
    // and the source/destination handles outlive the synchronous call.
    let result = unsafe {
        (*info).flags = 0; // ReplaceIfExists = FALSE
        (*info).root = to.as_raw_handle();
        (*info).name_bytes = (name.len() * size_of::<u16>()) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            buffer.as_mut_ptr().cast::<u8>().add(offset).cast::<u16>(),
            name.len(),
        );
        SetFileInformationByHandle(file.as_raw_handle(), 3, info.cast(), length)
    };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn platform_rename_new(_: &Dir, _: &OsStr, _: &Dir, _: &OsStr) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Atomic no-clobber rename is not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn publication_does_not_overwrite_a_new_destination() {
        let root = tempfile::tempdir().unwrap();
        let from = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        fs::write(root.path().join("staged"), b"new").unwrap();
        fs::write(root.path().join("target"), b"editor").unwrap();
        assert!(rename_new(&from, OsStr::new("staged"), &from, OsStr::new("target")).is_err());
        assert_eq!(fs::read(root.path().join("target")).unwrap(), b"editor");
        fs::remove_file(root.path().join("target")).unwrap();
        rename_new(&from, OsStr::new("staged"), &from, OsStr::new("target")).unwrap();
        assert_eq!(fs::read(root.path().join("target")).unwrap(), b"new");
        assert!(!root.path().join("staged").exists());
        assert!(rename_new(&from, OsStr::new("../escape"), &from, OsStr::new("target")).is_err());
    }
}
