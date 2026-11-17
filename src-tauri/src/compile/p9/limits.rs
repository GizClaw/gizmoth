//! Process-wide admission for 9P resources. Leave descriptor capacity for the
//! daemon's other services; an inherited shell limit is not a usage measurement.
use std::{
    fs, io,
    ops::Deref,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
};

pub(super) struct Budget {
    limit: usize,
    used: AtomicUsize,
}
impl Budget {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
        })
    }
    pub(super) fn reserve(self: &Arc<Self>, count: usize) -> io::Result<Permit> {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let next = used
                .checked_add(count)
                .filter(|next| *next <= self.limit)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EMFILE))?;
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(current) => used = current,
            }
        }
        Ok(Permit {
            budget: self.clone(),
            count,
        })
    }
}
pub(super) struct Permit {
    budget: Arc<Budget>,
    count: usize,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.count, Ordering::AcqRel);
    }
}

static DESCRIPTORS: OnceLock<Arc<Budget>> = OnceLock::new();
static FIDS: OnceLock<Arc<Budget>> = OnceLock::new();
static CONNECTIONS: OnceLock<Arc<Budget>> = OnceLock::new();
pub(super) const CONNECTIONS_PER_EXPORT: usize = 8;
const MAX_CONNECTIONS: usize = 32;

pub(super) fn prepare() -> io::Result<()> {
    if DESCRIPTORS.get().is_some() {
        return Ok(());
    }
    raise_soft_limit()?;
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let soft = usize::try_from(limit.rlim_cur).unwrap_or(usize::MAX);
    // Cap 9P's share even on hosts with a very large rlimit. For a low hard
    // limit retain at least 64 slots, and otherwise reserve a quarter for the
    // daemon. Other services can still exhaust the OS limit independently.
    let capacity = soft.saturating_sub((soft / 4).max(64)).min(32_768);
    DESCRIPTORS.get_or_init(|| Budget::new(capacity));
    Ok(())
}
pub(super) fn descriptors(count: usize) -> io::Result<Permit> {
    prepare()?;
    DESCRIPTORS.get().unwrap().reserve(count)
}
pub(super) fn fid() -> Result<Arc<Permit>, u32> {
    FIDS.get_or_init(|| Budget::new(65_536))
        .reserve(1)
        .map(Arc::new)
        .map_err(|_| super::errno::EMFILE)
}

#[cfg(test)]
pub(super) fn set_test_limits(descriptors: usize, fids: usize, connections: usize) {
    assert!(DESCRIPTORS.set(Budget::new(descriptors)).is_ok());
    assert!(FIDS.set(Budget::new(fids)).is_ok());
    assert!(CONNECTIONS.set(Budget::new(connections)).is_ok());
}

#[cfg(test)]
pub(super) fn fill_test_descriptors(spare: usize) -> Permit {
    let budget = DESCRIPTORS.get().unwrap();
    budget
        .reserve(budget.limit - budget.used.load(Ordering::Acquire) - spare)
        .unwrap()
}

/// The guard covers the accepted socket and its writer clone, and stays with
/// the connection until all workers and their session descriptors have exited.
pub(super) struct Connection {
    _local: Permit,
    _global: Permit,
    _descriptors: Permit,
}
pub(super) fn connection(local: &Arc<Budget>) -> io::Result<Connection> {
    Ok(Connection {
        _local: local.reserve(1)?,
        _global: CONNECTIONS
            .get_or_init(|| Budget::new(MAX_CONNECTIONS))
            .reserve(1)?,
        _descriptors: descriptors(2)?,
    })
}

/// A permit belongs to the descriptor, not its fid table entry: outstanding
/// requests and writable ancestor nodes may keep this object alive after clunk.
pub(super) struct File {
    file: fs::File,
    _permit: Permit,
}
impl File {
    pub(super) fn open(open: impl FnOnce() -> io::Result<fs::File>) -> io::Result<Self> {
        let permit = descriptors(1)?;
        Ok(Self {
            file: open()?,
            _permit: permit,
        })
    }
    pub(super) fn entries(&self) -> io::Result<Entries> {
        // cap-std temporarily needs a capability clone plus the iterator's
        // independent directory descriptor and stream. Conservatively keep all
        // three slots reserved until the iterator is closed.
        let permit = descriptors(3)?;
        let dir = cap_std::fs::Dir::from_std_file(self.file.try_clone()?);
        Ok(Entries {
            entries: dir.entries()?,
            _permit: permit,
        })
    }
}
impl Deref for File {
    type Target = fs::File;
    fn deref(&self) -> &Self::Target {
        &self.file
    }
}
pub(super) struct Entries {
    entries: cap_std::fs::ReadDir,
    _permit: Permit,
}
impl Iterator for Entries {
    type Item = io::Result<cap_std::fs::DirEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        self.entries.next()
    }
}

#[cfg(target_os = "macos")]
fn raise_soft_limit() -> io::Result<()> {
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
fn raise_soft_limit() -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_admission_enforces_export_and_process_budgets() {
        if super::super::isolated_test(
            "compile::p9::limits::tests::connection_admission_enforces_export_and_process_budgets",
        ) {
            return;
        }
        set_test_limits(32, 1024, 2);
        let first_export = Budget::new(1);
        let second_export = Budget::new(2);
        let first = connection(&first_export).unwrap();
        assert!(connection(&first_export).is_err());
        let second = connection(&second_export).unwrap();
        assert!(connection(&second_export).is_err());
        drop(first);
        assert!(connection(&second_export).is_ok());
        drop(second);
        assert!(connection(&first_export).is_ok());
    }

    #[test]
    fn permits_are_shared_and_released_only_by_the_last_owner() {
        let budget = Budget::new(2);
        let first = Arc::new(budget.reserve(1).unwrap());
        let inflight = first.clone();
        let second = budget.reserve(1).unwrap();
        drop(first);
        assert!(budget.reserve(1).is_err());
        drop(inflight);
        let replacement = budget.reserve(1).unwrap();
        assert!(budget.reserve(1).is_err());
        drop((second, replacement));
        assert!(budget.reserve(2).is_ok());
    }
}
