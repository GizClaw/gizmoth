//! Thin Linux syscall helpers for the agent. Only libc is available inside the
//! initramfs, so mounts, vsock and interface setup are done directly.
use std::{
    ffi::CString,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    os::unix::{ffi::OsStrExt, io::FromRawFd, net::UnixStream},
    path::Path,
};

fn cstr(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn mount(source: &str, target: &Path, fstype: &str, flags: libc::c_ulong, data: &str) -> io::Result<()> {
    fs::create_dir_all(target)?;
    let source = CString::new(source)?;
    let target_c = cstr(target)?;
    let fstype = CString::new(fstype)?;
    let data = CString::new(data)?;
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target_c.as_ptr(),
            fstype.as_ptr(),
            flags,
            if data.as_bytes().is_empty() { std::ptr::null() } else { data.as_ptr().cast() },
        )
    };
    check(result).map_err(|error| io::Error::new(error.kind(), format!("mount {}: {error}", target.display())))
}

pub fn bind(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    let (source_c, target_c) = (cstr(source)?, cstr(target)?);
    let result = unsafe {
        libc::mount(source_c.as_ptr(), target_c.as_ptr(), std::ptr::null(), libc::MS_BIND | libc::MS_REC, std::ptr::null())
    };
    check(result).map_err(|error| io::Error::new(error.kind(), format!("bind {}: {error}", target.display())))
}

pub fn umount(target: &Path) {
    if let Ok(target) = cstr(target) {
        unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
    }
}

pub fn chroot(root: &Path) -> io::Result<()> {
    let root_c = cstr(root)?;
    check(unsafe { libc::chroot(root_c.as_ptr()) })?;
    std::env::set_current_dir("/")
}

/// Brings up `lo` so the cache proxy can listen on 127.0.0.1.
pub fn loopback_up() -> io::Result<()> {
    #[repr(C)]
    struct IfReq {
        name: [u8; 16],
        flags: libc::c_short,
        pad: [u8; 22],
    }
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if socket < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut request = IfReq { name: [0; 16], flags: 0, pad: [0; 22] };
    request.name[..2].copy_from_slice(b"lo");
    let result = unsafe {
        let mut result = libc::ioctl(socket, libc::SIOCGIFFLAGS as _, &mut request);
        if result >= 0 {
            request.flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
            result = libc::ioctl(socket, libc::SIOCSIFFLAGS as _, &request);
        }
        libc::close(socket);
        result
    };
    check(result)
}

fn vsock_address(cid: u32, port: u32) -> libc::sockaddr_vm {
    let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    address.svm_cid = cid;
    address.svm_port = port;
    address
}

/// Socket operations are generic; `UnixStream` supplies Read/Write, cloning
/// and shutdown for the vsock descriptor.
pub struct VsockListener(i32);

impl VsockListener {
    pub fn bind(port: u32) -> io::Result<Self> {
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let address = vsock_address(libc::VMADDR_CID_ANY, port);
        let result = unsafe {
            libc::bind(fd, (&address as *const libc::sockaddr_vm).cast(), std::mem::size_of::<libc::sockaddr_vm>() as u32)
        };
        if result < 0 || unsafe { libc::listen(fd, 16) } < 0 {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(error);
        }
        Ok(Self(fd))
    }

    pub fn accept(&self) -> io::Result<UnixStream> {
        loop {
            let fd = unsafe { libc::accept4(self.0, std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_CLOEXEC) };
            if fd >= 0 {
                return Ok(unsafe { UnixStream::from_raw_fd(fd) });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

pub fn vsock_connect(cid: u32, port: u32) -> io::Result<UnixStream> {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let address = vsock_address(cid, port);
    let result = unsafe {
        libc::connect(fd, (&address as *const libc::sockaddr_vm).cast(), std::mem::size_of::<libc::sockaddr_vm>() as u32)
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error);
    }
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

/// Finds the block device of a vmbus SCSI disk by LUN (attachment order on the
/// host). SCSI scanning is asynchronous, so wait briefly for it to appear.
pub fn scsi_disk(lun: u32) -> io::Result<std::path::PathBuf> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Ok(entries) = fs::read_dir("/sys/block") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with("sd") {
                    continue;
                }
                let Ok(target) = fs::read_link(entry.path().join("device")) else { continue };
                let address = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if address.rsplit(':').next() == Some(&lun.to_string()) {
                    let device = Path::new("/dev").join(&name);
                    if device.exists() {
                        return Ok(device);
                    }
                }
            }
        }
        if std::time::Instant::now() > deadline {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("SCSI disk LUN {lun} not found")));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// An ext4 superblock carries 0xEF53 at byte 1080.
pub fn has_ext4(device: &Path) -> io::Result<bool> {
    let mut file = File::open(device)?;
    file.seek(SeekFrom::Start(1080))?;
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic)?;
    Ok(magic == [0x53, 0xEF])
}

pub fn disk_info(path: &Path) -> Option<crate::protocol::DiskInfo> {
    let path = cstr(path).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } < 0 {
        return None;
    }
    let size = stat.f_frsize as u64;
    Some(crate::protocol::DiskInfo { total: stat.f_blocks as u64 * size, free: stat.f_bavail as u64 * size })
}

pub fn lchown(path: &Path, uid: u32, gid: u32) -> io::Result<()> {
    let path = cstr(path)?;
    check(unsafe { libc::lchown(path.as_ptr(), uid, gid) })
}

/// Sets a file's modification time with nanosecond precision, not following links.
pub fn set_mtime(path: &Path, nanos: i64) -> io::Result<()> {
    let path = cstr(path)?;
    let time = libc::timespec { tv_sec: nanos.div_euclid(1_000_000_000), tv_nsec: nanos.rem_euclid(1_000_000_000) };
    let times = [libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT }, time];
    check(unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) })
}

pub fn kill_group(pgid: i32, signal: i32) {
    if pgid > 1 {
        unsafe { libc::kill(-pgid, signal) };
    }
}

pub fn poweroff() -> ! {
    unsafe {
        libc::sync();
        libc::reboot(libc::LINUX_REBOOT_CMD_POWER_OFF);
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
