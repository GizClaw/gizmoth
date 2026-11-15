//! Writable 9P2000.L export. Namespace operations use a pinned directory fd
//! and exactly one validated component, never a reconstructed host pathname.
//! Fids retain inode descriptors across rename/unlink. The guest resolves
//! symlinks; the server never follows them. IDs are mapped, never host-chowned.
//! As with cap-std, a directory capability follows its inode if the trusted
//! host moves it outside the original namespace. Host fcntl locks are separate.
#[cfg(test)]
mod tests;
use super::*;
use cap_std::fs::Dir;
use std::{
    ffi::CString,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::PermissionsExt,
    },
    sync::{atomic::AtomicU64, OnceLock, Weak},
};

const UID: u32 = 1000;
/// PID1 prepares the share as root before dropping to builder. Both guest
/// identities are squashed to the same virtual owner; neither changes host IDs.
fn mapped_identity(id: u32) -> bool {
    id == 0 || id == UID
}
const EEXIST: u32 = 17;
const EFBIG: u32 = 27;
const ENOSPC: u32 = 28;
const ENOTEMPTY: u32 = 39;
const ESTALE: u32 = 116;
const MAX_FIDS: usize = 65536;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

fn error(e: io::Error) -> u32 {
    match e.raw_os_error() {
        Some(libc::EEXIST) => EEXIST,
        Some(libc::EFBIG) => EFBIG,
        Some(libc::ENOSPC) => ENOSPC,
        Some(libc::ENOTEMPTY) => ENOTEMPTY,
        Some(libc::EBADF) => errno::EBADF,
        Some(libc::EROFS) => errno::EROFS,
        Some(libc::EXDEV) => 18,
        Some(libc::ENOTSUP) => errno::EOPNOTSUPP,
        _ => linux_errno(&e),
    }
}
fn syscall(result: libc::c_int) -> Result<(), u32> {
    if result < 0 {
        Err(error(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}
fn name(bytes: &[u8]) -> Result<CString, u32> {
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(errno::EINVAL);
    }
    if bytes.len() > 255 {
        return Err(errno::ENAMETOOLONG);
    }
    CString::new(bytes).map_err(|_| errno::EINVAL)
}
fn openat(parent: &File, component: &CString, flags: i32, mode: u32) -> Result<File, u32> {
    #[cfg(target_os = "macos")]
    let nofollow = if flags & libc::O_SYMLINK != 0 {
        0
    } else {
        libc::O_NOFOLLOW
    };
    #[cfg(not(target_os = "macos"))]
    let nofollow = libc::O_NOFOLLOW;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            flags | libc::O_CLOEXEC | nofollow,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        Err(error(io::Error::last_os_error()))
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
#[cfg(target_os = "macos")]
fn metadata_flags() -> i32 {
    libc::O_EVTONLY | libc::O_SYMLINK
}
#[cfg(target_os = "linux")]
fn metadata_flags() -> i32 {
    libc::O_PATH
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn metadata_flags() -> i32 {
    libc::O_RDONLY | libc::O_NONBLOCK
}

type Identity = (u64, u64);
fn identity(meta: &fs::Metadata) -> Identity {
    (meta.dev(), meta.ino())
}
#[derive(Clone)]
struct Location {
    parent: Arc<Node>,
    name: CString,
}
struct Node {
    fd: Arc<File>,
    id: Identity,
    location: Mutex<Option<Location>>,
    // A data descriptor captured at walk keeps an unlinked regular file usable.
    data: Option<Data>,
    symlink: Option<Vec<u8>>,
}
#[derive(Clone)]
struct Data {
    file: Arc<File>,
    read: bool,
    write: bool,
    append: bool,
    sync: bool,
}
#[derive(Clone)]
enum Open {
    File(Data),
    Directory(Arc<Vec<Entry>>),
    Attributes,
}
#[derive(Clone)]
struct Handle {
    node: Arc<Node>,
    open: Option<Open>,
}
type NodeKey = (Identity, Identity, Vec<u8>);
struct Export {
    root: Arc<Node>,
    namespace: Mutex<()>,
    nodes: Mutex<HashMap<NodeKey, Weak<Node>>>,
}

/// Exports the host directory directly. Changes are made with the host user's
/// authority; guest IDs 0/1000 map to builder 1000 without host chown. Host-native
/// case sensitivity and capacity apply. Locks coordinate 9P clients in this
/// process, including different exports, but not native host fcntl lock users.
pub fn serve_writable(socket: PathBuf, root: &Path) -> io::Result<Server> {
    let directory = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    let fd = directory.into_std_file();
    let id = identity(&fd.metadata()?);
    let root = Arc::new(Node {
        fd: Arc::new(fd),
        id,
        location: Mutex::new(None),
        data: None,
        symlink: None,
    });
    let export = Arc::new(Export {
        root,
        namespace: Mutex::new(()),
        nodes: Mutex::new(HashMap::new()),
    });
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if flag.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { break };
            let export = export.clone();
            std::thread::spawn(move || {
                let session = Session {
                    export,
                    id: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
                    fids: Mutex::new(HashMap::new()),
                };
                let _ = connection_with_writable(stream, Arc::new(PathBuf::new()), Some(session));
            });
        }
    });
    Ok(Server { path: socket, stop })
}

impl Node {
    fn meta(&self) -> Result<fs::Metadata, u32> {
        self.fd.metadata().map_err(error)
    }
    fn directory(&self) -> Result<(), u32> {
        if self.meta()?.is_dir() {
            Ok(())
        } else {
            Err(errno::ENOTDIR)
        }
    }
    fn dir(&self) -> Result<Dir, u32> {
        self.directory()?;
        Ok(Dir::from_std_file(self.fd.try_clone().map_err(error)?))
    }
    fn parent(&self) -> Option<Arc<Node>> {
        self.location
            .lock()
            .unwrap()
            .as_ref()
            .map(|l| l.parent.clone())
    }
    fn location(&self) -> Result<Location, u32> {
        self.location.lock().unwrap().clone().ok_or(ESTALE)
    }
    fn open(&self, read: bool, write: bool, append: bool) -> Result<Data, u32> {
        if let Some(data) = &self.data {
            if (!read || data.read) && (!write || data.write) && !append {
                return Ok(Data {
                    read,
                    write,
                    ..data.clone()
                });
            }
        }
        let access = if read && write {
            libc::O_RDWR
        } else if write {
            libc::O_WRONLY
        } else {
            libc::O_RDONLY
        };
        let opened = self.location().and_then(|loc| {
            let fd = openat(
                &loc.parent.fd,
                &loc.name,
                access | libc::O_NONBLOCK | if append { libc::O_APPEND } else { 0 },
                0,
            )?;
            if identity(&fd.metadata().map_err(error)?) != self.id {
                return Err(ESTALE);
            }
            Ok(Data {
                file: Arc::new(fd),
                read,
                write,
                append,
                sync: false,
            })
        });
        match opened {
            Ok(data) => Ok(data),
            Err(code) => match &self.data {
                Some(data) if (!read || data.read) && (!write || data.write) && !append => {
                    Ok(Data {
                        read,
                        write,
                        ..data.clone()
                    })
                }
                _ => Err(code),
            },
        }
    }
}

impl Export {
    fn exact(&self, parent: &Node, component: &CString) -> Result<bool, u32> {
        for entry in parent.dir()?.entries().map_err(error)? {
            if entry.map_err(error)?.file_name().as_encoded_bytes() == component.as_bytes() {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn lookup(&self, parent: Arc<Node>, component: CString) -> Result<Arc<Node>, u32> {
        parent.directory()?;
        // On case-insensitive hosts an alias must not masquerade as a separate
        // Linux dentry. Exclusive creators still receive the native EEXIST.
        if !self.exact(&parent, &component)? {
            return Err(errno::ENOENT);
        }
        let fd = openat(&parent.fd, &component, metadata_flags(), 0)?;
        let meta = fd.metadata().map_err(error)?;
        let id = identity(&meta);
        let mut nodes = self.nodes.lock().unwrap();
        let key = (id, parent.id, component.as_bytes().to_vec());
        if let Some(node) = nodes.get(&key).and_then(Weak::upgrade) {
            return Ok(node);
        }
        if nodes.len() >= MAX_FIDS {
            nodes.retain(|_, node| node.strong_count() > 0);
        }
        let symlink = if meta.file_type().is_symlink() {
            let mut bytes = vec![0u8; 65536];
            let n = unsafe {
                libc::readlinkat(
                    parent.fd.as_raw_fd(),
                    component.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if n < 0 {
                return Err(error(io::Error::last_os_error()));
            }
            bytes.truncate(n as usize);
            Some(bytes)
        } else {
            None
        };
        let data = if meta.is_file() {
            [libc::O_RDWR, libc::O_RDONLY, libc::O_WRONLY]
                .into_iter()
                .find_map(|access| {
                    let data = openat(&parent.fd, &component, access | libc::O_NONBLOCK, 0).ok()?;
                    if identity(&data.metadata().ok()?) != id {
                        return None;
                    }
                    Some(Data {
                        file: Arc::new(data),
                        read: access != libc::O_WRONLY,
                        write: access != libc::O_RDONLY,
                        append: false,
                        sync: false,
                    })
                })
        } else {
            None
        };
        let fd = data
            .as_ref()
            .map(|data| data.file.clone())
            .unwrap_or_else(|| Arc::new(fd));
        let node = Arc::new(Node {
            fd,
            id,
            location: Mutex::new(Some(Location {
                parent,
                name: component,
            })),
            data,
            symlink,
        });
        nodes.insert(key, Arc::downgrade(&node));
        Ok(node)
    }
    fn current(&self, node: &Node) -> Result<Location, u32> {
        let loc = node.location()?;
        let fd = openat(&loc.parent.fd, &loc.name, metadata_flags(), 0)?;
        if identity(&fd.metadata().map_err(error)?) != node.id {
            return Err(ESTALE);
        }
        Ok(loc)
    }
    fn detach(&self, parent: Identity, component: &CString) {
        self.nodes.lock().unwrap().retain(|key, weak| {
            if key.1 != parent || key.2 != component.as_bytes() {
                return true;
            }
            if let Some(node) = weak.upgrade() {
                *node.location.lock().unwrap() = None;
            }
            false
        });
    }
    fn rename(&self, source: Arc<Node>, parent: Arc<Node>, component: CString) -> Result<(), u32> {
        parent.directory()?;
        let old = self.current(&source)?;
        let destination = openat(&parent.fd, &component, metadata_flags(), 0);
        let same = destination
            .as_ref()
            .ok()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|m| identity(&m) == source.id);
        // Never overwrite a differently-cased alias of another entry.
        if destination.is_ok() && !self.exact(&parent, &component)? && !same {
            return Err(EEXIST);
        }
        syscall(unsafe {
            libc::renameat(
                old.parent.fd.as_raw_fd(),
                old.name.as_ptr(),
                parent.fd.as_raw_fd(),
                component.as_ptr(),
            )
        })?;
        if same && (old.parent.id != parent.id || self.exact(&old.parent, &old.name)?) {
            return Ok(());
        }
        self.detach(parent.id, &component);
        let mut nodes = self.nodes.lock().unwrap();
        nodes.remove(&(source.id, old.parent.id, old.name.as_bytes().to_vec()));
        *source.location.lock().unwrap() = Some(Location {
            parent: parent.clone(),
            name: component.clone(),
        });
        nodes.insert(
            (source.id, parent.id, component.as_bytes().to_vec()),
            Arc::downgrade(&source),
        );
        Ok(())
    }
}

pub(super) struct Session {
    export: Arc<Export>,
    id: u64,
    fids: Mutex<HashMap<u32, Handle>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.reset();
    }
}
impl Session {
    pub(super) fn reset(&self) {
        self.fids.lock().unwrap().clear();
        locks()
            .lock()
            .unwrap()
            .retain(|l| l.owner.session != self.id);
    }
    fn fid(&self, id: u32) -> Result<Handle, u32> {
        self.fids
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(errno::EBADF)
    }
    fn vacant(&self, id: u32) -> Result<(), u32> {
        let fids = self.fids.lock().unwrap();
        if fids.contains_key(&id) {
            Err(errno::EBADF)
        } else if fids.len() >= MAX_FIDS {
            Err(errno::EMFILE)
        } else {
            Ok(())
        }
    }
    fn clunk(&self, id: u32) -> Result<Handle, u32> {
        let fid = self.fids.lock().unwrap().remove(&id).ok_or(errno::EBADF)?;
        locks()
            .lock()
            .unwrap()
            .retain(|l| !(l.owner.session == self.id && l.fid == id));
        Ok(fid)
    }
    fn list(&self, node: &Node) -> Result<Vec<Entry>, u32> {
        let meta = node.meta()?;
        let parent = node
            .parent()
            .map(|n| n.meta())
            .transpose()?
            .unwrap_or_else(|| meta.clone());
        let mut result = vec![
            Entry {
                name: b".".to_vec(),
                qid: qid(&meta),
                kind: 4,
            },
            Entry {
                name: b"..".to_vec(),
                qid: qid(&parent),
                kind: 4,
            },
        ];
        let mut children = Vec::new();
        for entry in node.dir()?.entries().map_err(error)? {
            let entry = entry.map_err(error)?;
            let component =
                CString::new(entry.file_name().as_encoded_bytes()).map_err(|_| errno::EINVAL)?;
            let Ok(fd) = openat(&node.fd, &component, metadata_flags(), 0) else {
                continue;
            };
            let Ok(meta) = fd.metadata() else { continue };
            children.push(Entry {
                name: component.as_bytes().to_vec(),
                qid: qid(&meta),
                kind: dirent_type(&meta),
            });
        }
        children.sort_by(|a, b| a.name.cmp(&b.name));
        result.extend(children);
        Ok(result)
    }
    fn flags(flags: u32) -> Result<(bool, bool, bool, bool), u32> {
        // Linux O_* values, not the host's. O_DIRECT/O_PATH/O_TMPFILE and
        // unknown semantics are refused; sync writes can be honored below.
        const ALLOWED: u32 = 3
            | 0o100
            | 0o200
            | 0o400
            | 0o1000
            | 0o2000
            | 0o4000
            | 0o10000
            | 0o100000
            | 0o200000
            | 0o400000
            | 0o2000000
            | 0o4000000;
        if flags & !ALLOWED != 0 || flags & 3 == 3 {
            return Err(errno::EOPNOTSUPP);
        }
        let write = flags & 3 != 0;
        let read = flags & 3 != 1;
        if flags & 0o1000 != 0 && !write {
            return Err(errno::EACCES);
        }
        Ok((read, write, flags & 0o2000 != 0, flags & 0o1000 != 0))
    }
    pub(super) fn handle(&self, kind: u8, input: &mut In, msize: u32) -> Reply {
        let body = input.data;
        let result = self.handle_request(kind, input, msize);
        if let Err(code) = &result {
            if std::env::var_os("GIZMOTH_P9_TRACE").is_some() {
                // Do not log write contents. The bounded request prefix carries
                // the wire fid/flags/attribute mask needed to diagnose clients.
                let prefix = &body[..body.len().min(if kind == TWRITE { 16 } else { 64 })];
                eprintln!("9p writable error: type={kind} errno={code} body={prefix:02x?}");
            }
        }
        result
    }
    fn handle_request(&self, kind: u8, input: &mut In, msize: u32) -> Reply {
        // Serializes dentry/fid changes across connections. Every actual host
        // operation still uses directory fds and no-follow final components.
        let _namespace = self.export.namespace.lock().unwrap();
        let limit = msize.saturating_sub(IO_HEADER) as usize;
        let mut out = Out::default();
        match kind {
            TATTACH => {
                let id = input.u32()?;
                let _afid = input.u32()?;
                let _user = input.bytes()?;
                let tree = input.bytes()?;
                let _uid = input.u32()?;
                if !tree.is_empty() {
                    return Err(errno::EINVAL);
                }
                self.vacant(id)?;
                self.fids.lock().unwrap().insert(
                    id,
                    Handle {
                        node: self.export.root.clone(),
                        open: None,
                    },
                );
                out.qid(qid(&self.export.root.meta()?));
            }
            TWALK => {
                let (id, new, count) = (input.u32()?, input.u32()?, input.u16()?);
                let start = self.fid(id)?;
                if start.open.is_some() {
                    return Err(errno::EBADF);
                }
                if new != id {
                    self.vacant(new)?;
                }
                let mut node = start.node;
                let mut qids = Vec::new();
                for _ in 0..count {
                    let component = input.bytes()?;
                    let step = (|| {
                        node.directory()?;
                        match component {
                            b"." => Ok(node.clone()),
                            b".." => Ok(node.parent().unwrap_or_else(|| self.export.root.clone())),
                            _ => self.export.lookup(node.clone(), name(component)?),
                        }
                    })();
                    match step {
                        Ok(next) => {
                            qids.push(qid(&next.meta()?));
                            node = next;
                        }
                        Err(code) if qids.is_empty() => return Err(code),
                        Err(_) => break,
                    }
                }
                if qids.len() == count as usize {
                    self.fids
                        .lock()
                        .unwrap()
                        .insert(new, Handle { node, open: None });
                }
                out.u16(qids.len() as u16);
                for q in qids {
                    out.qid(q);
                }
            }
            TGETATTR => {
                let node = self.fid(input.u32()?)?.node;
                let _mask = input.u64()?;
                let m = node.meta()?;
                out.u64(0x7ff).qid(qid(&m)).u32(m.mode()).u32(UID).u32(UID);
                out.u64(m.nlink())
                    .u64(0)
                    .u64(m.len())
                    .u64(m.blksize())
                    .u64(m.blocks());
                out.u64(m.atime() as u64)
                    .u64(m.atime_nsec() as u64)
                    .u64(m.mtime() as u64)
                    .u64(m.mtime_nsec() as u64);
                out.u64(m.ctime() as u64)
                    .u64(m.ctime_nsec() as u64)
                    .u64(0)
                    .u64(0)
                    .u64(0)
                    .u64(0);
            }
            TLOPEN => {
                let (id, flags) = (input.u32()?, input.u32()?);
                let fid = self.fid(id)?;
                if fid.open.is_some() {
                    return Err(errno::EBADF);
                }
                let (read, write, append, truncate) = Self::flags(flags)?;
                // Linux carries CREATE when reopening an existing fid (GCC
                // uses 0x42, then 0x41 as its writeback-cache fallback). This
                // request never creates a dentry; only Tlcreate can do that.
                // EXCL without CREATE has no effect on supported regular files.
                if flags & (0o100 | 0o200) == (0o100 | 0o200) {
                    return Err(EEXIST);
                }
                let meta = fid.node.meta()?;
                let opened = if meta.is_dir() {
                    if write || truncate {
                        return Err(errno::EISDIR);
                    }
                    Open::Directory(Arc::new(self.list(&fid.node)?))
                } else if meta.is_file() {
                    if flags & 0o200000 != 0 {
                        return Err(errno::ENOTDIR);
                    }
                    let mut data = fid.node.open(read, write, append)?;
                    data.sync = flags & (0o10000 | 0o4000000) != 0;
                    if truncate {
                        data.file.set_len(0).map_err(error)?;
                    }
                    Open::File(data)
                } else if meta.file_type().is_symlink() {
                    return Err(errno::ELOOP);
                } else {
                    return Err(errno::EOPNOTSUPP);
                };
                self.fids
                    .lock()
                    .unwrap()
                    .get_mut(&id)
                    .ok_or(errno::EBADF)?
                    .open = Some(opened);
                out.qid(qid(&fid.node.meta()?)).u32(0);
            }
            TLCREATE => {
                let (id, component, flags, mode, gid) = (
                    input.u32()?,
                    name(input.bytes()?)?,
                    input.u32()?,
                    input.u32()?,
                    input.u32()?,
                );
                let parent = self.fid(id)?;
                if parent.open.is_some() {
                    return Err(errno::EBADF);
                }
                parent.node.directory()?;
                if !mapped_identity(gid) {
                    return Err(errno::EPERM);
                }
                if mode & 0o6000 != 0 {
                    return Err(errno::EOPNOTSUPP);
                }
                let (read, write, append, _) = Self::flags(flags)?;
                let access = if read && write {
                    libc::O_RDWR
                } else if write {
                    libc::O_WRONLY
                } else {
                    libc::O_RDONLY
                };
                let file = openat(
                    &parent.node.fd,
                    &component,
                    access | libc::O_CREAT | libc::O_EXCL | if append { libc::O_APPEND } else { 0 },
                    mode & 0o1777,
                )?;
                file.set_permissions(fs::Permissions::from_mode(mode & 0o1777))
                    .map_err(error)?;
                let node = self.export.lookup(parent.node, component)?;
                self.fids.lock().unwrap().insert(
                    id,
                    Handle {
                        node: node.clone(),
                        open: Some(Open::File(Data {
                            file: Arc::new(file),
                            read,
                            write,
                            append,
                            sync: flags & (0o10000 | 0o4000000) != 0,
                        })),
                    },
                );
                out.qid(qid(&node.meta()?)).u32(0);
            }
            TREAD => {
                let (id, offset, count) = (input.u32()?, input.u64()?, input.u32()?);
                if offset > i64::MAX as u64 {
                    return Err(errno::EINVAL);
                }
                match self.fid(id)?.open {
                    Some(Open::Attributes) => {
                        out.u32(0);
                    }
                    Some(Open::File(data)) if data.read => {
                        let mut bytes = vec![0; (count as usize).min(limit)];
                        let n = data.file.read_at(&mut bytes, offset).map_err(error)?;
                        out.u32(n as u32);
                        out.0.extend_from_slice(&bytes[..n]);
                    }
                    _ => return Err(errno::EBADF),
                }
            }
            TWRITE => {
                let (id, offset, count) = (input.u32()?, input.u64()?, input.u32()?);
                if count as usize > limit
                    || offset
                        .checked_add(count as u64)
                        .is_none_or(|end| end > i64::MAX as u64)
                {
                    return Err(errno::EINVAL);
                }
                let bytes = input.take(count as usize)?;
                let Some(Open::File(data)) = self.fid(id)?.open else {
                    return Err(errno::EBADF);
                };
                if !data.write {
                    return Err(errno::EBADF);
                }
                let n = if data.append {
                    (&*data.file).write(bytes).map_err(error)?
                } else {
                    data.file.write_at(bytes, offset).map_err(error)?
                };
                if data.sync {
                    data.file.sync_all().map_err(error)?;
                }
                out.u32(n as u32);
            }
            TREADDIR => {
                let (id, offset, count) = (input.u32()?, input.u64()?, input.u32()?);
                let fid = self.fid(id)?;
                let Some(Open::Directory(initial)) = fid.open else {
                    return Err(errno::EBADF);
                };
                let entries = if offset == 0 {
                    let next = Arc::new(self.list(&fid.node)?);
                    self.fids
                        .lock()
                        .unwrap()
                        .get_mut(&id)
                        .ok_or(errno::EBADF)?
                        .open = Some(Open::Directory(next.clone()));
                    next
                } else {
                    initial
                };
                let mut bytes = Out::default();
                for (index, entry) in entries
                    .iter()
                    .enumerate()
                    .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                {
                    if bytes.0.len() + 24 + entry.name.len() > (count as usize).min(limit) {
                        break;
                    }
                    bytes
                        .qid(entry.qid)
                        .u64(index as u64 + 1)
                        .u8(entry.kind)
                        .bytes(&entry.name);
                }
                out.u32(bytes.0.len() as u32);
                out.0.extend(bytes.0);
            }
            TMKDIR => {
                let (parent, component, mode, gid) = (
                    self.fid(input.u32()?)?.node,
                    name(input.bytes()?)?,
                    input.u32()?,
                    input.u32()?,
                );
                parent.directory()?;
                if !mapped_identity(gid) {
                    return Err(errno::EPERM);
                }
                if mode & 0o6000 != 0 {
                    return Err(errno::EOPNOTSUPP);
                }
                syscall(unsafe {
                    libc::mkdirat(
                        parent.fd.as_raw_fd(),
                        component.as_ptr(),
                        (mode & 0o1777) as libc::mode_t,
                    )
                })?;
                let node = self.export.lookup(parent, component)?;
                node.fd
                    .set_permissions(fs::Permissions::from_mode(mode & 0o1777))
                    .map_err(error)?;
                out.qid(qid(&node.meta()?));
            }
            TSYMLINK => {
                let (parent, component, target, gid) = (
                    self.fid(input.u32()?)?.node,
                    name(input.bytes()?)?,
                    input.bytes()?.to_vec(),
                    input.u32()?,
                );
                parent.directory()?;
                if !mapped_identity(gid) {
                    return Err(errno::EPERM);
                }
                let target = CString::new(target).map_err(|_| errno::EINVAL)?;
                syscall(unsafe {
                    libc::symlinkat(target.as_ptr(), parent.fd.as_raw_fd(), component.as_ptr())
                })?;
                out.qid(qid(&self.export.lookup(parent, component)?.meta()?));
            }
            TREADLINK => {
                let node = self.fid(input.u32()?)?.node;
                out.bytes(node.symlink.as_ref().ok_or(errno::EINVAL)?);
            }
            TLINK => {
                let (parent, source, component) = (
                    self.fid(input.u32()?)?.node,
                    self.fid(input.u32()?)?.node,
                    name(input.bytes()?)?,
                );
                parent.directory()?;
                if source.meta()?.is_dir() {
                    return Err(errno::EPERM);
                }
                let old = self.export.current(&source)?;
                syscall(unsafe {
                    libc::linkat(
                        old.parent.fd.as_raw_fd(),
                        old.name.as_ptr(),
                        parent.fd.as_raw_fd(),
                        component.as_ptr(),
                        0,
                    )
                })?;
            }
            TRENAME => {
                let (source, parent, component) = (
                    self.fid(input.u32()?)?.node,
                    self.fid(input.u32()?)?.node,
                    name(input.bytes()?)?,
                );
                self.export.rename(source, parent, component)?;
            }
            TRENAMEAT => {
                let (parent, old, destination, component) = (
                    self.fid(input.u32()?)?.node,
                    name(input.bytes()?)?,
                    self.fid(input.u32()?)?.node,
                    name(input.bytes()?)?,
                );
                let source = self.export.lookup(parent, old)?;
                self.export.rename(source, destination, component)?;
            }
            TUNLINKAT => {
                let (parent, component, flags) = (
                    self.fid(input.u32()?)?.node,
                    name(input.bytes()?)?,
                    input.u32()?,
                );
                parent.directory()?;
                if flags & !0x200 != 0 {
                    return Err(errno::EINVAL);
                }
                if !self.export.exact(&parent, &component)? {
                    return Err(errno::ENOENT);
                }
                syscall(unsafe {
                    libc::unlinkat(
                        parent.fd.as_raw_fd(),
                        component.as_ptr(),
                        if flags == 0x200 {
                            libc::AT_REMOVEDIR
                        } else {
                            0
                        },
                    )
                })?;
                self.export.detach(parent.id, &component);
            }
            TREMOVE => {
                let fid = self.clunk(input.u32()?)?;
                let loc = self.export.current(&fid.node)?;
                syscall(unsafe {
                    libc::unlinkat(
                        loc.parent.fd.as_raw_fd(),
                        loc.name.as_ptr(),
                        if fid.node.meta()?.is_dir() {
                            libc::AT_REMOVEDIR
                        } else {
                            0
                        },
                    )
                })?;
                self.export.detach(loc.parent.id, &loc.name);
            }
            TCLUNK => {
                self.clunk(input.u32()?)?;
            }
            TSETATTR => {
                self.setattr(input)?;
            }
            TSTATFS => {
                self.fid(input.u32()?)?;
                let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
                syscall(unsafe { libc::fstatvfs(self.export.root.fd.as_raw_fd(), &mut stat) })?;
                out.u32(0x0102_1997).u32(stat.f_frsize as u32);
                out.u64(stat.f_blocks as u64)
                    .u64(stat.f_bfree as u64)
                    .u64(stat.f_bavail as u64);
                out.u64(stat.f_files as u64)
                    .u64(stat.f_ffree as u64)
                    .u64(stat.f_fsid as u64)
                    .u32(stat.f_namemax as u32);
            }
            TFSYNC => {
                let (fid, data_only) = (self.fid(input.u32()?)?, input.u32()?);
                if data_only > 1 {
                    return Err(errno::EINVAL);
                }
                match fid.open {
                    Some(Open::File(data)) => if data_only == 1 {
                        data.file.sync_data()
                    } else {
                        data.file.sync_all()
                    }
                    .map_err(error)?,
                    Some(Open::Directory(_)) => {
                        let file = openat(
                            &fid.node.fd,
                            &CString::new(".").unwrap(),
                            libc::O_RDONLY | libc::O_DIRECTORY,
                            0,
                        )?;
                        file.sync_all().map_err(error)?;
                    }
                    _ => return Err(errno::EBADF),
                }
            }
            TXATTRWALK => {
                let (fid, new, attribute) = (self.fid(input.u32()?)?, input.u32()?, input.bytes()?);
                self.vacant(new)?;
                if !attribute.is_empty() {
                    return Err(errno::ENODATA);
                }
                self.fids.lock().unwrap().insert(
                    new,
                    Handle {
                        node: fid.node,
                        open: Some(Open::Attributes),
                    },
                );
                out.u64(0);
            }
            TLOCK => {
                return self.lock(input);
            }
            TGETLOCK => {
                return self.getlock(input);
            }
            TMKNOD | TXATTRCREATE => return Err(errno::EOPNOTSUPP),
            _ => return Err(errno::EOPNOTSUPP),
        }
        Ok(out.0)
    }
    fn setattr(&self, input: &mut In) -> Result<(), u32> {
        let (fid, valid, mode, uid, gid, size) = (
            self.fid(input.u32()?)?,
            input.u32()?,
            input.u32()?,
            input.u32()?,
            input.u32()?,
            input.u64()?,
        );
        let (atime, ansec, mtime, mnsec) = (input.u64()?, input.u64()?, input.u64()?, input.u64()?);
        if valid & !0x1ff != 0 {
            return Err(errno::EOPNOTSUPP);
        }
        if valid & 2 != 0 && !mapped_identity(uid) || valid & 4 != 0 && !mapped_identity(gid) {
            return Err(errno::EPERM);
        }
        if valid & 1 != 0 && mode & 0o6000 != 0 {
            return Err(errno::EOPNOTSUPP);
        }
        if size > i64::MAX as u64 && valid & 8 != 0 {
            return Err(EFBIG);
        }
        let node = fid.node;
        if node.meta()?.file_type().is_symlink() {
            // Linux adds CTIME to lchown's UID/GID request (wire valid 0x46).
            // Ownership is virtual, so accept that automatic flag without any
            // host mutation. Explicit link chmod/timestamps remain unsupported.
            if valid & !(2 | 4 | 64) != 0 || valid & 64 != 0 && valid & (2 | 4) == 0 {
                return Err(errno::EOPNOTSUPP);
            }
            return Ok(());
        }
        let time =
            |enabled: bool, explicit: bool, sec: u64, ns: u64| -> Result<libc::timespec, u32> {
                if enabled && explicit && ns >= 1_000_000_000 {
                    return Err(errno::EINVAL);
                }
                Ok(libc::timespec {
                    tv_sec: sec as i64,
                    tv_nsec: if !enabled {
                        libc::UTIME_OMIT
                    } else if explicit {
                        ns as _
                    } else {
                        libc::UTIME_NOW
                    },
                })
            };
        let times = [
            time(valid & 16 != 0, valid & 128 != 0, atime, ansec)?,
            time(valid & 32 != 0, valid & 256 != 0, mtime, mnsec)?,
        ];
        if valid & 8 != 0 {
            let data = match fid.open {
                Some(Open::File(data)) if data.write => data,
                _ => node.open(false, true, false)?,
            };
            data.file.set_len(size).map_err(error)?;
        }
        if valid & 1 != 0 {
            node.fd
                .set_permissions(fs::Permissions::from_mode(mode & 0o1777))
                .map_err(error)?;
        }
        if valid & (16 | 32) != 0 {
            syscall(unsafe { libc::futimens(node.fd.as_raw_fd(), times.as_ptr()) })?;
        }
        // ctime is maintained by the host. Force a metadata update for a
        // ctime-only request; arbitrary ctime values are not in Tsetattr.
        if valid == 64 {
            node.fd
                .set_permissions(node.meta()?.permissions())
                .map_err(error)?;
        }
        Ok(())
    }
}

// POSIX byte-range lock semantics for all 9P connections in this process.
// Unlike fcntl(F_SETLK), owners here are guest clients, not GizMoth's PID.
#[derive(Clone, PartialEq, Eq)]
struct Owner {
    session: u64,
    pid: u32,
    client: Vec<u8>,
}
#[derive(Clone)]
struct RangeLock {
    inode: Identity,
    owner: Owner,
    fid: u32,
    kind: u8,
    start: u64,
    end: u128,
}
static LOCKS: OnceLock<Mutex<Vec<RangeLock>>> = OnceLock::new();
fn locks() -> &'static Mutex<Vec<RangeLock>> {
    LOCKS.get_or_init(|| Mutex::new(Vec::new()))
}
fn end(start: u64, length: u64) -> Result<u128, u32> {
    if length == 0 {
        Ok(u128::MAX)
    } else {
        start
            .checked_add(length)
            .map(|v| v as u128)
            .ok_or(errno::EINVAL)
    }
}
fn conflict(a: &RangeLock, b: &RangeLock) -> bool {
    a.inode == b.inode
        && a.owner != b.owner
        && (a.kind == 1 || b.kind == 1)
        && (a.start as u128) < b.end
        && (b.start as u128) < a.end
}
impl Session {
    fn lock_request(&self, input: &mut In, flags: bool) -> Result<(RangeLock, u32), u32> {
        let (id, kind) = (input.u32()?, input.u8()?);
        let flags = if flags { input.u32()? } else { 0 };
        if flags & !1 != 0 || kind > 2 {
            return Err(errno::EOPNOTSUPP);
        }
        let (start, length, pid, client) = (
            input.u64()?,
            input.u64()?,
            input.u32()?,
            input.bytes()?.to_vec(),
        );
        let fid = self.fid(id)?;
        let Some(Open::File(data)) = fid.open else {
            return Err(errno::EBADF);
        };
        if kind == 1 && !data.write || kind == 0 && !data.read {
            return Err(errno::EBADF);
        }
        Ok((
            RangeLock {
                inode: fid.node.id,
                owner: Owner {
                    session: self.id,
                    pid,
                    client,
                },
                fid: id,
                kind,
                start,
                end: end(start, length)?,
            },
            flags,
        ))
    }
    fn lock(&self, input: &mut In) -> Reply {
        let (request, _) = self.lock_request(input, true)?;
        let mut locks = locks().lock().unwrap();
        if request.kind != 2 && locks.iter().any(|l| conflict(l, &request)) {
            return Ok(vec![1]);
        }
        let mut next = Vec::new();
        for old in locks.drain(..) {
            if old.inode != request.inode
                || old.owner != request.owner
                || old.end <= request.start as u128
                || request.end <= old.start as u128
            {
                next.push(old);
                continue;
            }
            if old.start < request.start {
                next.push(RangeLock {
                    end: request.start as u128,
                    ..old.clone()
                });
            }
            if request.end < old.end {
                next.push(RangeLock {
                    start: request.end as u64,
                    ..old
                });
            }
        }
        if request.kind != 2 {
            next.push(request);
        }
        *locks = next;
        Ok(vec![0])
    }
    fn getlock(&self, input: &mut In) -> Reply {
        let (request, _) = self.lock_request(input, false)?;
        let locks = locks().lock().unwrap();
        let mut out = Out::default();
        if let Some(lock) = locks.iter().find(|l| conflict(l, &request)) {
            out.u8(lock.kind)
                .u64(lock.start)
                .u64(if lock.end == u128::MAX {
                    0
                } else {
                    (lock.end - lock.start as u128) as u64
                })
                .u32(lock.owner.pid)
                .bytes(&lock.owner.client);
        } else {
            out.u8(2)
                .u64(request.start)
                .u64(if request.end == u128::MAX {
                    0
                } else {
                    (request.end - request.start as u128) as u64
                })
                .u32(request.owner.pid)
                .bytes(&request.owner.client);
        }
        Ok(out.0)
    }
}
