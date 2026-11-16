//! Read-only 9P2000.L file server that shares a host folder with compile VMs.
//!
//! OpenVMM has no virtio-fs on macOS. Instead the guest agent connects to a
//! vsock port and the guest kernel's v9fs client mounts that connection with
//! `trans=fd`; guest connections to host port P arrive on `<socket>_P`. The
//! server never follows host symlinks (the guest resolves them itself), shows
//! every file as owned by root and refuses every change.
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::{FileExt, MetadataExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    time::Duration,
};

mod writable;
pub use writable::serve_writable;

pub const MAX_MSIZE: u32 = 512 * 1024;
/// Requests of one connection run in parallel, like `make -j` in the guest.
const WORKERS: usize = 8;
/// Bound a stalled transport write so it cannot keep the connection alive forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// size[4] type[1] tag[2] count[4] in front of Rread and Rreaddir data.
const IO_HEADER: u32 = 11;

const RLERROR: u8 = 7;
const TSTATFS: u8 = 8;
const TLOPEN: u8 = 12;
const TLCREATE: u8 = 14;
const TSYMLINK: u8 = 16;
const TMKNOD: u8 = 18;
const TRENAME: u8 = 20;
const TREADLINK: u8 = 22;
const TGETATTR: u8 = 24;
const TSETATTR: u8 = 26;
const TXATTRWALK: u8 = 30;
const TXATTRCREATE: u8 = 32;
const TREADDIR: u8 = 40;
const TFSYNC: u8 = 50;
const TLOCK: u8 = 52;
const TGETLOCK: u8 = 54;
const TLINK: u8 = 70;
const TMKDIR: u8 = 72;
const TRENAMEAT: u8 = 74;
const TUNLINKAT: u8 = 76;
const TVERSION: u8 = 100;
const TATTACH: u8 = 104;
const TFLUSH: u8 = 108;
const TWALK: u8 = 110;
const TREAD: u8 = 116;
const TWRITE: u8 = 118;
const TCLUNK: u8 = 120;
const TREMOVE: u8 = 122;

/// Linux errno values: the guest kernel interprets them, not macOS.
mod errno {
    pub const EPERM: u32 = 1;
    pub const ENOENT: u32 = 2;
    pub const EIO: u32 = 5;
    pub const EBADF: u32 = 9;
    pub const EACCES: u32 = 13;
    pub const ENOTDIR: u32 = 20;
    pub const EISDIR: u32 = 21;
    pub const EINVAL: u32 = 22;
    pub const EMFILE: u32 = 24;
    pub const EROFS: u32 = 30;
    pub const ENAMETOOLONG: u32 = 36;
    pub const ELOOP: u32 = 40;
    pub const ENODATA: u32 = 61;
    pub const EOPNOTSUPP: u32 = 95;
}

fn linux_errno(error: &io::Error) -> u32 {
    match error.raw_os_error() {
        Some(libc::EPERM) => errno::EPERM,
        Some(libc::ENOENT) => errno::ENOENT,
        Some(libc::EACCES) => errno::EACCES,
        Some(libc::ENOTDIR) => errno::ENOTDIR,
        Some(libc::EISDIR) => errno::EISDIR,
        Some(libc::EINVAL) => errno::EINVAL,
        Some(libc::EMFILE | libc::ENFILE) => errno::EMFILE,
        Some(libc::ENAMETOOLONG) => errno::ENAMETOOLONG,
        Some(libc::ELOOP) => errno::ELOOP,
        _ => match error.kind() {
            io::ErrorKind::NotFound => errno::ENOENT,
            io::ErrorKind::PermissionDenied => errno::EACCES,
            _ => errno::EIO,
        },
    }
}

type Reply = Result<Vec<u8>, u32>;

// ---------------------------------------------------------------- wire

struct In<'a> {
    data: &'a [u8],
}

impl<'a> In<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], u32> {
        if self.data.len() < n {
            return Err(errno::EINVAL);
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, u32> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, u32> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, u32> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bytes(&mut self) -> Result<&'a [u8], u32> {
        let n = self.u16()? as usize;
        self.take(n)
    }
}

#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.u16(v.len() as u16);
        self.0.extend_from_slice(v);
        self
    }
    fn qid(&mut self, q: Qid) -> &mut Self {
        self.u8(q.kind).u32(q.version).u64(q.path)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Qid {
    kind: u8,
    version: u32,
    path: u64,
}

fn qid(meta: &fs::Metadata) -> Qid {
    let kind = if meta.is_dir() {
        0x80
    } else if meta.file_type().is_symlink() {
        0x02
    } else {
        0
    };
    Qid {
        kind,
        version: meta.mtime() as u32,
        path: meta.ino(),
    }
}

/// Linux `DT_*` values for readdir entries.
fn dirent_type(meta: &fs::Metadata) -> u8 {
    let kind = meta.file_type();
    if kind.is_dir() {
        4
    } else if kind.is_symlink() {
        10
    } else if kind.is_file() {
        8
    } else {
        0
    }
}

// ---------------------------------------------------------------- server

/// A share's listener; dropping it stops accepting and removes the socket.
/// Mounted connections end when their VM exits.
pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
}

impl Server {
    /// Signals an unusable reply transport; a clean client disconnect is not a failure.
    pub fn failure_flag(&self) -> Arc<AtomicBool> {
        self.failed.clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.path);
        let _ = fs::remove_file(&self.path);
    }
}

/// Serves `root` read-only on the Unix socket `socket`.
pub fn serve(socket: PathBuf, root: &Path) -> io::Result<Server> {
    let root = Arc::new(root.canonicalize()?);
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    let stop = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let (flag, failure) = (stop.clone(), failed.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if flag.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { break };
            let (root, failed) = (root.clone(), failure.clone());
            std::thread::spawn(move || {
                let _ = connection_with_writable(stream, root, None, failed);
            });
        }
    });
    Ok(Server {
        path: socket,
        stop,
        failed,
    })
}

#[derive(Clone)]
enum Opened {
    File(Arc<File>),
    Dir(Arc<Vec<Entry>>),
    /// The (empty) extended-attribute list of a file.
    NoAttributes,
}

#[derive(Clone)]
struct Fid {
    /// Validated names below the root; never `..` or containing `/`.
    rel: PathBuf,
    open: Option<Opened>,
}

struct Entry {
    name: Vec<u8>,
    qid: Qid,
    kind: u8,
}

struct Request {
    tag: u16,
    completed: AtomicBool,
}

struct Session {
    root: Arc<PathBuf>,
    writable: Option<writable::Session>,
    fids: Mutex<HashMap<u32, Fid>>,
    msize: AtomicU32,
    inflight: Mutex<HashMap<u16, Arc<Request>>>,
    settled: Condvar,
    /// None after a write fails: a partial frame makes the stream unusable.
    writer: Mutex<Option<UnixStream>>,
    failed: Arc<AtomicBool>,
}

fn connection_with_writable(
    stream: UnixStream,
    root: Arc<PathBuf>,
    writable: Option<writable::Session>,
    failed: Arc<AtomicBool>,
) -> io::Result<()> {
    connection_with_write_timeout(stream, root, writable, failed, WRITE_TIMEOUT)
}

fn connection_with_write_timeout(
    stream: UnixStream,
    root: Arc<PathBuf>,
    writable: Option<writable::Session>,
    failed: Arc<AtomicBool>,
    timeout: Duration,
) -> io::Result<()> {
    stream.set_write_timeout(Some(timeout))?;
    let session = Arc::new(Session {
        root,
        writable,
        fids: Mutex::new(HashMap::new()),
        msize: AtomicU32::new(8192),
        inflight: Mutex::new(HashMap::new()),
        settled: Condvar::new(),
        writer: Mutex::new(Some(stream.try_clone()?)),
        failed,
    });
    let (sender, receiver) = mpsc::channel::<(u8, Arc<Request>, Vec<u8>)>();
    let receiver = Arc::new(Mutex::new(receiver));
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let (session, receiver) = (session.clone(), receiver.clone());
            std::thread::spawn(move || loop {
                let next = receiver.lock().unwrap().recv();
                let Ok((kind, request, body)) = next else {
                    return;
                };
                session.dispatch(kind, &request, &body);
            })
        })
        .collect();
    let mut reader = BufReader::with_capacity(MAX_MSIZE as usize, stream);
    loop {
        let mut size = [0u8; 4];
        if reader.read_exact(&mut size).is_err() {
            break;
        }
        let size = u32::from_le_bytes(size);
        if !(7..=MAX_MSIZE + 64).contains(&size) {
            break;
        }
        let mut message = vec![0u8; size as usize - 4];
        if reader.read_exact(&mut message).is_err() {
            break;
        }
        let (kind, tag) = (message[0], u16::from_le_bytes([message[1], message[2]]));
        let body = message.split_off(3);
        match kind {
            TVERSION => {
                // Version resets the session: let every request finish first.
                session.wait_until(|inflight| inflight.is_empty());
                let request = session.begin(tag);
                session.version(&request, &body);
            }
            TFLUSH => {
                // Bind before spawning: the tag may be reused before we run.
                let old = session.flush_target(&body);
                let request = session.begin(tag);
                let session = session.clone();
                std::thread::spawn(move || session.flush(&request, old));
            }
            _ => {
                let request = session.begin(tag);
                if sender.send((kind, request, body)).is_err() {
                    break;
                }
            }
        }
    }
    drop(sender);
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}

impl Session {
    fn begin(&self, tag: u16) -> Arc<Request> {
        let request = Arc::new(Request {
            tag,
            completed: AtomicBool::new(false),
        });
        self.inflight.lock().unwrap().insert(tag, request.clone());
        request
    }

    fn flush_target(&self, body: &[u8]) -> Option<Arc<Request>> {
        let old = (In { data: body }).u16().ok()?;
        self.inflight.lock().unwrap().get(&old).cloned()
    }

    fn flush(&self, request: &Arc<Request>, old: Option<Arc<Request>>) {
        if let Some(old) = old {
            // Rflush follows this request instance's reply, even if its numeric
            // tag has since been assigned to a different request.
            self.wait_until(|_| old.completed.load(Ordering::SeqCst));
        }
        self.reply(request, TFLUSH + 1, &[]);
    }

    fn wait_until(&self, done: impl Fn(&HashMap<u16, Arc<Request>>) -> bool) {
        let mut inflight = self.inflight.lock().unwrap();
        while !done(&inflight) {
            inflight = self.settled.wait(inflight).unwrap();
        }
    }

    fn reply(&self, request: &Arc<Request>, kind: u8, payload: &[u8]) {
        let mut frame = Vec::with_capacity(7 + payload.len());
        frame.extend_from_slice(&(7 + payload.len() as u32).to_le_bytes());
        frame.push(kind);
        frame.extend_from_slice(&request.tag.to_le_bytes());
        frame.extend_from_slice(payload);
        {
            let mut writer = self.writer.lock().unwrap();
            if let Some(stream) = writer.as_mut() {
                let result = stream.write_all(&frame);
                if result.is_err() {
                    self.failed.store(true, Ordering::SeqCst);
                    // Wake the request reader too. Keep the failed state under
                    // the writer lock: no worker may append another frame after
                    // a partially transmitted reply.
                    let _ = stream.shutdown(Shutdown::Both);
                    *writer = None;
                }
            }
        }
        self.finish(request);
    }

    fn finish(&self, request: &Arc<Request>) {
        let mut inflight = self.inflight.lock().unwrap();
        request.completed.store(true, Ordering::SeqCst);
        // The client can reuse a tag as soon as it receives the reply, before
        // this worker gets here. Never remove that replacement's entry.
        if inflight
            .get(&request.tag)
            .is_some_and(|current| Arc::ptr_eq(current, request))
        {
            inflight.remove(&request.tag);
        }
        self.settled.notify_all();
    }

    fn version(&self, request: &Arc<Request>, body: &[u8]) {
        let mut input = In { data: body };
        let (Ok(msize), Ok(version)) = (input.u32(), input.bytes()) else {
            return self.reply(request, RLERROR, &errno::EINVAL.to_le_bytes());
        };
        let msize = msize.min(MAX_MSIZE);
        self.msize.store(msize, Ordering::SeqCst);
        self.fids.lock().unwrap().clear();
        if let Some(writable) = &self.writable {
            writable.reset();
        }
        let answer: &[u8] = if version == b"9P2000.L" {
            b"9P2000.L"
        } else {
            b"unknown"
        };
        let mut out = Out::default();
        out.u32(msize).bytes(answer);
        self.reply(request, TVERSION + 1, &out.0);
    }

    fn dispatch(&self, kind: u8, request: &Arc<Request>, body: &[u8]) {
        let result = self.handle(kind, &mut In { data: body });
        match result {
            Ok(payload) => self.reply(request, kind + 1, &payload),
            Err(code) => self.reply(request, RLERROR, &code.to_le_bytes()),
        }
    }

    fn fid(&self, id: u32) -> Result<Fid, u32> {
        self.fids
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(errno::EBADF)
    }

    fn full(&self, rel: &Path) -> PathBuf {
        self.root.join(rel)
    }

    fn lstat(&self, rel: &Path) -> Result<fs::Metadata, u32> {
        fs::symlink_metadata(self.full(rel)).map_err(|e| linux_errno(&e))
    }

    fn io_limit(&self, count: u32) -> usize {
        count.min(self.msize.load(Ordering::SeqCst).saturating_sub(IO_HEADER)) as usize
    }

    fn handle(&self, kind: u8, input: &mut In) -> Reply {
        if let Some(writable) = &self.writable {
            return writable.handle(kind, input, self.msize.load(Ordering::SeqCst));
        }
        let mut out = Out::default();
        match kind {
            TATTACH => {
                let fid = input.u32()?;
                let meta = self.lstat(Path::new(""))?;
                self.fids.lock().unwrap().insert(
                    fid,
                    Fid {
                        rel: PathBuf::new(),
                        open: None,
                    },
                );
                out.qid(qid(&meta));
            }
            TWALK => {
                let (fid, newfid, count) = (input.u32()?, input.u32()?, input.u16()?);
                let start = self.fid(fid)?;
                if newfid != fid && self.fids.lock().unwrap().contains_key(&newfid) {
                    return Err(errno::EBADF);
                }
                let mut rel = start.rel.clone();
                let mut is_dir = count == 0 || self.lstat(&rel)?.is_dir();
                let mut qids = Vec::new();
                for _ in 0..count {
                    let name = input.bytes()?;
                    let step = (|| {
                        if !is_dir {
                            return Err(errno::ENOTDIR);
                        }
                        let next = match name {
                            b"" => return Err(errno::EINVAL),
                            b"." => rel.clone(),
                            // `..` above the root stays at the root.
                            b".." => rel.parent().map(Path::to_path_buf).unwrap_or_default(),
                            _ if name.contains(&b'/') || name.contains(&0) => {
                                return Err(errno::EINVAL)
                            }
                            _ => rel.join(std::str::from_utf8(name).map_err(|_| errno::ENOENT)?),
                        };
                        let meta = self.lstat(&next)?;
                        Ok((next, meta))
                    })();
                    match step {
                        Ok((next, meta)) => {
                            qids.push(qid(&meta));
                            is_dir = meta.is_dir();
                            rel = next;
                        }
                        Err(code) if qids.is_empty() => return Err(code),
                        Err(_) => break,
                    }
                }
                if qids.len() == count as usize {
                    self.fids
                        .lock()
                        .unwrap()
                        .insert(newfid, Fid { rel, open: None });
                }
                out.u16(qids.len() as u16);
                for q in qids {
                    out.qid(q);
                }
            }
            TGETATTR => {
                let fid = self.fid(input.u32()?)?;
                let meta = self.lstat(&fid.rel)?;
                let time = |s: i64, ns: i64| (s as u64, ns as u64);
                let (atime, mtime, ctime) = (
                    time(meta.atime(), meta.atime_nsec()),
                    time(meta.mtime(), meta.mtime_nsec()),
                    time(meta.ctime(), meta.ctime_nsec()),
                );
                let mode = (meta.mode() & 0o170000) | (meta.mode() & 0o777);
                // P9_GETATTR_BASIC: mode, nlink, uid, gid, rdev, times, ino, size, blocks.
                out.u64(0x7ff).qid(qid(&meta)).u32(mode).u32(0).u32(0);
                out.u64(meta.nlink())
                    .u64(0)
                    .u64(meta.size())
                    .u64(4096)
                    .u64(meta.blocks());
                out.u64(atime.0).u64(atime.1).u64(mtime.0).u64(mtime.1);
                out.u64(ctime.0).u64(ctime.1).u64(0).u64(0).u64(0).u64(0);
            }
            TLOPEN => {
                let (id, flags) = (input.u32()?, input.u32()?);
                // O_WRONLY, O_RDWR or O_TRUNC (Linux values).
                if flags & 3 != 0 || flags & 0o1000 != 0 {
                    return Err(errno::EROFS);
                }
                let fid = self.fid(id)?;
                let meta = self.lstat(&fid.rel)?;
                let opened = if meta.is_dir() {
                    Opened::Dir(Arc::new(self.list(&fid.rel, &meta)?))
                } else if meta.is_file() {
                    let file = fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(self.full(&fid.rel))
                        .map_err(|e| linux_errno(&e))?;
                    Opened::File(Arc::new(file))
                } else if meta.file_type().is_symlink() {
                    return Err(errno::ELOOP);
                } else {
                    return Err(errno::EACCES);
                };
                if let Some(entry) = self.fids.lock().unwrap().get_mut(&id) {
                    entry.open = Some(opened);
                }
                out.qid(qid(&meta)).u32(0);
            }
            TREAD => {
                let (fid, offset, count) = (input.u32()?, input.u64()?, input.u32()?);
                let file = match self.fid(fid)?.open {
                    Some(Opened::File(file)) => file,
                    Some(Opened::NoAttributes) => return Ok(0u32.to_le_bytes().to_vec()),
                    _ => return Err(errno::EBADF),
                };
                let mut buffer = vec![0u8; self.io_limit(count)];
                let n = file
                    .read_at(&mut buffer, offset)
                    .map_err(|e| linux_errno(&e))?;
                out.u32(n as u32);
                out.0.extend_from_slice(&buffer[..n]);
            }
            TREADDIR => {
                let (fid, offset, count) = (input.u32()?, input.u64()?, input.u32()?);
                let Some(Opened::Dir(entries)) = self.fid(fid)?.open else {
                    return Err(errno::EBADF);
                };
                let limit = self.io_limit(count);
                let mut data = Out::default();
                for (index, entry) in entries.iter().enumerate().skip(offset as usize) {
                    if data.0.len() + 24 + entry.name.len() > limit {
                        break;
                    }
                    data.qid(entry.qid)
                        .u64(index as u64 + 1)
                        .u8(entry.kind)
                        .bytes(&entry.name);
                }
                out.u32(data.0.len() as u32);
                out.0.extend_from_slice(&data.0);
            }
            TREADLINK => {
                let fid = self.fid(input.u32()?)?;
                let target = fs::read_link(self.full(&fid.rel)).map_err(|e| linux_errno(&e))?;
                out.bytes(target.as_os_str().as_encoded_bytes());
            }
            TSTATFS => {
                self.fid(input.u32()?)?;
                let path = std::ffi::CString::new(self.root.as_os_str().as_encoded_bytes())
                    .map_err(|_| errno::EINVAL)?;
                let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
                if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
                    return Err(errno::EIO);
                }
                out.u32(0x0102_1997).u32(stat.f_frsize as u32);
                out.u64(stat.f_blocks as u64)
                    .u64(stat.f_bfree as u64)
                    .u64(stat.f_bavail as u64);
                out.u64(stat.f_files as u64)
                    .u64(stat.f_ffree as u64)
                    .u64(stat.f_fsid as u64);
                out.u32(255);
            }
            TCLUNK => {
                let fid = input.u32()?;
                self.fids.lock().unwrap().remove(&fid).ok_or(errno::EBADF)?;
            }
            TREMOVE => {
                // Remove always clunks, even when it fails.
                self.fids.lock().unwrap().remove(&input.u32()?);
                return Err(errno::EROFS);
            }
            TXATTRWALK => {
                let (fid, newfid) = (input.u32()?, input.u32()?);
                let name = input.bytes()?;
                let start = self.fid(fid)?;
                // Files have no extended attributes: listing them yields an
                // empty list (tools like `cp -a` require that), reading one
                // reports that it does not exist.
                if !name.is_empty() {
                    return Err(errno::ENODATA);
                }
                self.fids.lock().unwrap().insert(
                    newfid,
                    Fid {
                        rel: start.rel,
                        open: Some(Opened::NoAttributes),
                    },
                );
                out.u64(0);
            }
            TFSYNC => {}
            TLOCK => {
                out.u8(0);
            }
            TGETLOCK => {
                let _fid = input.u32()?;
                let (_kind, start, length, proc_id, client) = (
                    input.u8()?,
                    input.u64()?,
                    input.u64()?,
                    input.u32()?,
                    input.bytes()?,
                );
                // Nothing is ever locked: F_UNLCK.
                out.u8(2).u64(start).u64(length).u32(proc_id).bytes(client);
            }
            TLCREATE | TSYMLINK | TMKNOD | TRENAME | TSETATTR | TXATTRCREATE | TLINK | TMKDIR
            | TRENAMEAT | TUNLINKAT | TWRITE => return Err(errno::EROFS),
            _ => return Err(errno::EOPNOTSUPP),
        }
        Ok(out.0)
    }

    /// A directory's entries with `.` and `..`, read once at open.
    fn list(&self, rel: &Path, meta: &fs::Metadata) -> Result<Vec<Entry>, u32> {
        let parent = self.lstat(rel.parent().unwrap_or(Path::new("")))?;
        let mut entries = vec![
            Entry {
                name: b".".to_vec(),
                qid: qid(meta),
                kind: 4,
            },
            Entry {
                name: b"..".to_vec(),
                qid: qid(&parent),
                kind: 4,
            },
        ];
        let mut children = Vec::new();
        for entry in fs::read_dir(self.full(rel)).map_err(|e| linux_errno(&e))? {
            let entry = entry.map_err(|e| linux_errno(&e))?;
            // `DirEntry::metadata` does not follow symlinks.
            let Ok(meta) = entry.metadata() else { continue };
            children.push(Entry {
                name: entry.file_name().as_encoded_bytes().to_vec(),
                qid: qid(&meta),
                kind: dirent_type(&meta),
            });
        }
        children.sort_by(|a, b| a.name.cmp(&b.name));
        entries.extend(children);
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Client {
        stream: UnixStream,
        tag: u16,
    }

    impl Client {
        fn call(&mut self, kind: u8, body: &[u8]) -> (u8, Vec<u8>) {
            self.tag += 1;
            let mut frame = Vec::new();
            frame.extend_from_slice(&(7 + body.len() as u32).to_le_bytes());
            frame.push(kind);
            frame.extend_from_slice(&self.tag.to_le_bytes());
            frame.extend_from_slice(body);
            self.stream.write_all(&frame).unwrap();
            let mut size = [0u8; 4];
            self.stream.read_exact(&mut size).unwrap();
            let mut reply = vec![0u8; u32::from_le_bytes(size) as usize - 4];
            self.stream.read_exact(&mut reply).unwrap();
            assert_eq!(u16::from_le_bytes([reply[1], reply[2]]), self.tag);
            (reply[0], reply[3..].to_vec())
        }

        fn ok(&mut self, kind: u8, body: &[u8]) -> Vec<u8> {
            let (reply, data) = self.call(kind, body);
            assert_eq!(reply, kind + 1, "{kind}: error {:?}", data);
            data
        }

        fn error(&mut self, kind: u8, body: &[u8]) -> u32 {
            let (reply, data) = self.call(kind, body);
            assert_eq!(reply, RLERROR, "{kind} should fail");
            u32::from_le_bytes(data[..4].try_into().unwrap())
        }

        fn walk(&mut self, fid: u32, newfid: u32, names: &[&str]) -> (u8, Vec<u8>) {
            let mut body = Out::default();
            body.u32(fid).u32(newfid).u16(names.len() as u16);
            for name in names {
                body.bytes(name.as_bytes());
            }
            self.call(TWALK, &body.0)
        }
    }

    fn names(readdir: &[u8]) -> Vec<String> {
        let mut input = In {
            data: &readdir[4..],
        };
        let mut names = Vec::new();
        while !input.data.is_empty() {
            input.take(13 + 8 + 1).unwrap();
            names.push(String::from_utf8(input.bytes().unwrap().to_vec()).unwrap());
        }
        names
    }

    fn request_session() -> (Arc<Session>, UnixStream) {
        let (writer, peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        (
            Arc::new(Session {
                root: Arc::new(PathBuf::new()),
                writable: None,
                fids: Mutex::new(HashMap::new()),
                msize: AtomicU32::new(MAX_MSIZE),
                inflight: Mutex::new(HashMap::new()),
                settled: Condvar::new(),
                writer: Mutex::new(Some(writer)),
                failed: Arc::new(AtomicBool::new(false)),
            }),
            peer,
        )
    }

    #[test]
    fn late_completion_preserves_reused_tag_and_its_flush_target() {
        let (session, _peer) = request_session();
        let original = session.begin(1);
        // Pause the old reply after transmission but before finish(). The guest
        // has received it and is entitled to reuse the same numeric tag.
        let replacement = session.begin(1);
        session.finish(&original);
        let target = session.flush_target(&1u16.to_le_bytes()).unwrap();
        assert!(Arc::ptr_eq(&target, &replacement));
        assert!(original.completed.load(Ordering::SeqCst));
        assert!(!target.completed.load(Ordering::SeqCst));
        session.finish(&replacement);
        assert!(session.inflight.lock().unwrap().is_empty());
    }

    #[test]
    fn delayed_flush_does_not_follow_reused_tag_into_a_flush_cycle() {
        let (session, mut peer) = request_session();
        let original = session.begin(1);
        // The reader captures F's target, then F's thread is delayed.
        let target = session.flush_target(&1u16.to_le_bytes());
        let first_flush = session.begin(2);
        session.finish(&original);
        // Reuse tag 1 for G, a flush of F. Waiting on numeric tags would now
        // create F -> G -> F, despite the original request having completed.
        let second_target = session.flush_target(&2u16.to_le_bytes());
        let second_flush = session.begin(1);
        let (done, completion) = mpsc::channel();
        let workers: Vec<_> = [
            (first_flush.clone(), target),
            (second_flush.clone(), second_target),
        ]
        .into_iter()
        .map(|(request, old)| {
            let (session, done) = (session.clone(), done.clone());
            std::thread::spawn(move || {
                session.flush(&request, old);
                done.send(()).unwrap();
            })
        })
        .collect();
        let finished = (0..2).all(|_| completion.recv_timeout(Duration::from_secs(5)).is_ok());
        if !finished {
            // Release waiters even when regressing, so the test cannot leak
            // deadlocked threads into the rest of the test process.
            session.finish(&first_flush);
            session.finish(&second_flush);
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(finished, "flushes followed reused tags into a cycle");
        for tag in [2u16, 1] {
            let mut reply = [0; 7];
            peer.read_exact(&mut reply).unwrap();
            assert_eq!(u32::from_le_bytes(reply[..4].try_into().unwrap()), 7);
            assert_eq!(reply[4], TFLUSH + 1);
            assert_eq!(u16::from_le_bytes(reply[5..].try_into().unwrap()), tag);
        }
        assert!(session.inflight.lock().unwrap().is_empty());
    }

    /// A real connection, including its reader and worker pool. Keep the peer
    /// open while waiting for completion to catch writers that fail to wake it.
    fn large_read_connection(
        timeout: Duration,
    ) -> (
        tempfile::TempDir,
        Client,
        Arc<AtomicBool>,
        mpsc::Receiver<io::Result<()>>,
    ) {
        use std::os::fd::AsRawFd;

        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("large"), vec![0x5a; MAX_MSIZE as usize]).unwrap();
        let (server, stream) = UnixStream::pair().unwrap();
        let capacity: libc::c_int = 4096;
        // Make backpressure deterministic even on hosts with large defaults.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    server.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&capacity as *const libc::c_int).cast(),
                    std::mem::size_of_val(&capacity) as libc::socklen_t,
                )
            },
            0
        );
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let failed = Arc::new(AtomicBool::new(false));
        let (done, completion) = mpsc::channel();
        let (path, failure) = (Arc::new(root.path().to_owned()), failed.clone());
        std::thread::spawn(move || {
            let result = connection_with_write_timeout(server, path, None, failure, timeout);
            let _ = done.send(result);
        });
        let mut client = Client { stream, tag: 0 };
        let mut version = Out::default();
        version.u32(MAX_MSIZE).bytes(b"9P2000.L");
        client.ok(TVERSION, &version.0);
        let mut attach = Out::default();
        attach.u32(0).u32(!0).bytes(b"root").bytes(b"").u32(0);
        client.ok(TATTACH, &attach.0);
        client.walk(0, 1, &["large"]);
        let mut open = Out::default();
        open.u32(1).u32(0);
        client.ok(TLOPEN, &open.0);
        (root, client, failed, completion)
    }

    #[test]
    fn stalled_reply_closes_reader_and_workers_without_appending_frames() {
        let (_root, mut client, failed, completion) =
            large_read_connection(Duration::from_millis(50));
        let mut read = Out::default();
        read.u32(1).u64(0).u32(MAX_MSIZE);
        let first_tag = client.tag + 1;
        // Queue more replies than workers, then deliberately read nothing.
        for _ in 0..WORKERS + 2 {
            client.tag += 1;
            let mut frame = Out::default();
            frame.u32(7 + read.0.len() as u32).u8(TREAD).u16(client.tag);
            frame.0.extend_from_slice(&read.0);
            client.stream.write_all(&frame.0).unwrap();
        }
        completion
            .recv_timeout(Duration::from_secs(5))
            .expect("connection did not exit")
            .unwrap();
        assert!(failed.load(Ordering::SeqCst));
        let mut received = Vec::new();
        client.stream.read_to_end(&mut received).unwrap();
        assert!(
            received.len() >= IO_HEADER as usize,
            "expected a partial Rread"
        );
        assert_eq!(
            u32::from_le_bytes(received[..4].try_into().unwrap()),
            MAX_MSIZE
        );
        assert_eq!(received[4], TREAD + 1);
        // Parallel requests may finish in any order, but only one frame may start.
        let tag = u16::from_le_bytes(received[5..7].try_into().unwrap());
        assert!((first_tag..=client.tag).contains(&tag));
        assert!(
            received.len() < MAX_MSIZE as usize,
            "peer unexpectedly received a full reply"
        );
        assert!(
            received[IO_HEADER as usize..]
                .iter()
                .all(|&byte| byte == 0x5a),
            "a later frame was appended after the partial reply"
        );
    }

    #[test]
    fn large_replies_and_clean_disconnect_do_not_signal_failure() {
        let (_root, mut client, failed, completion) = large_read_connection(Duration::from_secs(1));
        let mut read = Out::default();
        read.u32(1).u64(0).u32(MAX_MSIZE);
        let payload = client.ok(TREAD, &read.0);
        assert_eq!(
            u32::from_le_bytes(payload[..4].try_into().unwrap()),
            MAX_MSIZE - IO_HEADER
        );
        assert!(payload[4..].iter().all(|&byte| byte == 0x5a));
        assert!(!failed.load(Ordering::SeqCst));
        drop(client);
        completion
            .recv_timeout(Duration::from_secs(5))
            .expect("connection did not exit")
            .unwrap();
        assert!(!failed.load(Ordering::SeqCst));
    }

    #[test]
    fn serves_a_folder_read_only_without_leaving_it() {
        let dir = tempfile::tempdir().unwrap();
        let share = dir.path().join("share");
        fs::create_dir_all(share.join("bin")).unwrap();
        fs::write(share.join("bin/tool"), b"#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(
            share.join("bin/tool"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        fs::write(dir.path().join("secret"), b"outside").unwrap();
        std::os::unix::fs::symlink("bin/tool", share.join("link")).unwrap();
        std::os::unix::fs::symlink("..", share.join("up")).unwrap();
        let socket = dir.path().join("s");
        let server = serve(socket.clone(), &share).unwrap();
        let mut client = Client {
            stream: UnixStream::connect(&socket).unwrap(),
            tag: 0,
        };

        let mut version = Out::default();
        version.u32(1 << 20).bytes(b"9P2000.L");
        let reply = client.ok(TVERSION, &version.0);
        assert_eq!(
            u32::from_le_bytes(reply[..4].try_into().unwrap()),
            MAX_MSIZE
        );
        let mut attach = Out::default();
        attach.u32(0).u32(!0).bytes(b"root").bytes(b"").u32(0);
        assert_eq!(client.ok(TATTACH, &attach.0)[0], 0x80);

        // Walk, stat, open and read a file.
        let (kind, data) = client.walk(0, 1, &["bin", "tool"]);
        assert_eq!(
            (kind, u16::from_le_bytes([data[0], data[1]])),
            (TWALK + 1, 2)
        );
        let mut getattr = Out::default();
        getattr.u32(1).u64(0x7ff);
        let attr = client.ok(TGETATTR, &getattr.0);
        let mode = u32::from_le_bytes(attr[21..25].try_into().unwrap());
        assert_eq!(mode, 0o100755);
        assert_eq!(&attr[25..33], &[0; 8], "uid and gid are root");
        let mut open = Out::default();
        open.u32(1).u32(0);
        client.ok(TLOPEN, &open.0);
        let mut read = Out::default();
        read.u32(1).u64(10).u32(100);
        let data = client.ok(TREAD, &read.0);
        assert_eq!(&data[4..], b"echo hi\n");

        // Writes, creation and write opens are refused.
        let mut write_open = Out::default();
        write_open.u32(1).u32(2);
        assert_eq!(client.error(TLOPEN, &write_open.0), errno::EROFS);
        let mut mkdir = Out::default();
        mkdir.u32(0).bytes(b"new").u32(0o755).u32(0);
        assert_eq!(client.error(TMKDIR, &mkdir.0), errno::EROFS);

        // Directory listing with `.` and `..`, symlinks reported, not followed.
        client.walk(0, 2, &[]);
        let mut open = Out::default();
        open.u32(2).u32(0);
        client.ok(TLOPEN, &open.0);
        let mut readdir = Out::default();
        readdir.u32(2).u64(0).u32(8192);
        assert_eq!(
            names(&client.ok(TREADDIR, &readdir.0)),
            [".", "..", "bin", "link", "up"]
        );
        client.walk(0, 3, &["link"]);
        let mut readlink = Out::default();
        readlink.u32(3);
        assert_eq!(&client.ok(TREADLINK, &readlink.0)[2..], b"bin/tool");

        // `..` stays at the root, walking through a symlink fails, and a
        // missing first name is an error while a later one is a partial walk.
        let (_, data) = client.walk(0, 4, &["..", "..", "bin"]);
        assert_eq!(u16::from_le_bytes([data[0], data[1]]), 3);
        let (kind, _) = client.walk(0, 5, &["up", "secret"]);
        assert_eq!(kind, TWALK + 1);
        let (kind, data) = client.walk(0, 6, &["missing"]);
        assert_eq!(
            (kind, &data[..4]),
            (RLERROR, &errno::ENOENT.to_le_bytes()[..])
        );
        let (_, data) = client.walk(0, 7, &["bin", "missing"]);
        assert_eq!(u16::from_le_bytes([data[0], data[1]]), 1);
        let mut getattr = Out::default();
        getattr.u32(7).u64(0x7ff);
        assert_eq!(client.error(TGETATTR, &getattr.0), errno::EBADF);
        let mut clunk = Out::default();
        clunk.u32(1);
        client.ok(TCLUNK, &clunk.0);
        assert_eq!(client.error(TCLUNK, &clunk.0), errno::EBADF);

        // Extended attributes: an empty list, and no named attribute.
        let mut xattr = Out::default();
        xattr.u32(0).u32(8).bytes(b"");
        assert_eq!(client.ok(TXATTRWALK, &xattr.0), 0u64.to_le_bytes());
        let mut read = Out::default();
        read.u32(8).u64(0).u32(100);
        assert_eq!(client.ok(TREAD, &read.0), 0u32.to_le_bytes());
        let mut named = Out::default();
        named.u32(0).u32(9).bytes(b"system.posix_acl_access");
        assert_eq!(client.error(TXATTRWALK, &named.0), errno::ENODATA);

        drop(server);
        assert!(!socket.exists());
    }

    #[test]
    fn walking_past_a_symlink_never_reaches_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let share = dir.path().join("share");
        fs::create_dir_all(&share).unwrap();
        fs::create_dir_all(dir.path().join("outside")).unwrap();
        fs::write(dir.path().join("outside/secret"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside"), share.join("escape")).unwrap();
        let socket = dir.path().join("s");
        let _server = serve(socket.clone(), &share).unwrap();
        let mut client = Client {
            stream: UnixStream::connect(&socket).unwrap(),
            tag: 0,
        };
        let mut version = Out::default();
        version.u32(65536).bytes(b"9P2000.L");
        client.ok(TVERSION, &version.0);
        let mut attach = Out::default();
        attach.u32(0).u32(!0).bytes(b"").bytes(b"").u32(0);
        client.ok(TATTACH, &attach.0);
        let (_, data) = client.walk(0, 1, &["escape", "secret"]);
        // Only the symlink itself is walked; `secret` is never looked up.
        assert_eq!(u16::from_le_bytes([data[0], data[1]]), 1);
        let (kind, data) = client.walk(0, 2, &["escape"]);
        assert_eq!((kind, data[2]), (TWALK + 1, 0x02));
        let mut open = Out::default();
        open.u32(2).u32(0);
        assert_eq!(client.error(TLOPEN, &open.0), errno::ELOOP);
    }
}
