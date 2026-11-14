//! Read-only 9P2000.L file server that shares a host folder with compile VMs.
//!
//! OpenVMM has no virtio-fs on macOS. Instead the guest agent connects to a
//! vsock port and the guest kernel's v9fs client mounts that connection with
//! `trans=fd`; guest connections to host port P arrive on `<socket>_P`. The
//! server never follows host symlinks (the guest resolves them itself), shows
//! every file as owned by root and refuses every change.
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    os::unix::{
        fs::{FileExt, MetadataExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
};

pub const MAX_MSIZE: u32 = 512 * 1024;
/// Requests of one connection run in parallel, like `make -j` in the guest.
const WORKERS: usize = 8;
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
    let flag = stop.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if flag.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { break };
            let root = root.clone();
            std::thread::spawn(move || {
                let _ = connection(stream, root);
            });
        }
    });
    Ok(Server { path: socket, stop })
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

struct Session {
    root: Arc<PathBuf>,
    fids: Mutex<HashMap<u32, Fid>>,
    msize: AtomicU32,
    inflight: Mutex<HashSet<u16>>,
    settled: Condvar,
    writer: Mutex<UnixStream>,
}

fn connection(stream: UnixStream, root: Arc<PathBuf>) -> io::Result<()> {
    let session = Arc::new(Session {
        root,
        fids: Mutex::new(HashMap::new()),
        msize: AtomicU32::new(8192),
        inflight: Mutex::new(HashSet::new()),
        settled: Condvar::new(),
        writer: Mutex::new(stream.try_clone()?),
    });
    let (sender, receiver) = mpsc::channel::<(u8, u16, Vec<u8>)>();
    let receiver = Arc::new(Mutex::new(receiver));
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let (session, receiver) = (session.clone(), receiver.clone());
            std::thread::spawn(move || loop {
                let next = receiver.lock().unwrap().recv();
                let Ok((kind, tag, body)) = next else { return };
                session.dispatch(kind, tag, &body);
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
                session.version(tag, &body);
            }
            TFLUSH => {
                // Rflush must follow the flushed request's reply, if any.
                session.begin(tag);
                let session = session.clone();
                std::thread::spawn(move || {
                    if let Ok(old) = (In { data: &body }).u16() {
                        session.wait_until(|inflight| !inflight.contains(&old));
                    }
                    session.reply(tag, TFLUSH + 1, &[]);
                });
            }
            _ => {
                session.begin(tag);
                if sender.send((kind, tag, body)).is_err() {
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
    fn begin(&self, tag: u16) {
        self.inflight.lock().unwrap().insert(tag);
    }

    fn wait_until(&self, done: impl Fn(&HashSet<u16>) -> bool) {
        let mut inflight = self.inflight.lock().unwrap();
        while !done(&inflight) {
            inflight = self.settled.wait(inflight).unwrap();
        }
    }

    fn reply(&self, tag: u16, kind: u8, payload: &[u8]) {
        let mut frame = Vec::with_capacity(7 + payload.len());
        frame.extend_from_slice(&(7 + payload.len() as u32).to_le_bytes());
        frame.push(kind);
        frame.extend_from_slice(&tag.to_le_bytes());
        frame.extend_from_slice(payload);
        let _ = self.writer.lock().unwrap().write_all(&frame);
        self.inflight.lock().unwrap().remove(&tag);
        self.settled.notify_all();
    }

    fn version(&self, tag: u16, body: &[u8]) {
        let mut input = In { data: body };
        let (Ok(msize), Ok(version)) = (input.u32(), input.bytes()) else {
            return self.reply(tag, RLERROR, &errno::EINVAL.to_le_bytes());
        };
        let msize = msize.min(MAX_MSIZE);
        self.msize.store(msize, Ordering::SeqCst);
        self.fids.lock().unwrap().clear();
        let answer: &[u8] = if version == b"9P2000.L" {
            b"9P2000.L"
        } else {
            b"unknown"
        };
        let mut out = Out::default();
        out.u32(msize).bytes(answer);
        self.reply(tag, TVERSION + 1, &out.0);
    }

    fn dispatch(&self, kind: u8, tag: u16, body: &[u8]) {
        match self.handle(kind, &mut In { data: body }) {
            Ok(payload) => self.reply(tag, kind + 1, &payload),
            Err(code) => self.reply(tag, RLERROR, &code.to_le_bytes()),
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
