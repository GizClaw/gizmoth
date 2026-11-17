//! Writable 9P fids pin host descriptors. GUI-launched macOS processes commonly
//! inherit a 256-descriptor soft limit, shared by every compile slot.
use std::io;

#[cfg(target_os = "macos")]
pub(super) fn prepare() -> io::Result<()> {
    const DESCRIPTORS: libc::rlim_t = 65_536;
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if limit.rlim_cur >= DESCRIPTORS {
        return Ok(());
    }
    // Darwin rejects values above this ceiling even with an unlimited hard
    // rlimit. Raise only our soft limit; preserve both host policy ceilings.
    let mut maximum: libc::c_int = 0;
    let mut size = std::mem::size_of_val(&maximum);
    if unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            (&mut maximum as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if maximum <= 0 {
        return Err(io::Error::other("Invalid macOS file descriptor ceiling"));
    }
    let target = DESCRIPTORS.min(limit.rlim_max).min(maximum as libc::rlim_t);
    if target > limit.rlim_cur {
        limit.rlim_cur = target;
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(super) fn prepare() -> io::Result<()> {
    Ok(())
}
