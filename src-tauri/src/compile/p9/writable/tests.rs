use super::*;
use std::time::Duration;

#[cfg(target_os = "macos")]
#[test]
fn gui_descriptor_limit_supports_multiple_work_exports() {
    // Changing RLIMIT_NOFILE is process-wide: isolate the low launchd limit
    // from the parallel Rust test harness and leave the hard limit untouched.
    const CHILD: &str = "GIZMOTH_P9_DESCRIPTOR_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "compile::p9::writable::tests::gui_descriptor_limit_supports_multiple_work_exports",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    let hard = limit.rlim_max;
    limit.rlim_cur = 256;
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let directory = tempfile::tempdir().unwrap();
    let mut servers = Vec::new();
    let mut clients = Vec::new();
    for index in 0..3 {
        let root = directory.path().join(format!("work-{index}"));
        fs::create_dir(&root).unwrap();
        let socket = directory.path().join(format!("share-{index}.sock"));
        servers.push(serve_writable(socket.clone(), &root).unwrap());
        clients.push(Client::connect(&socket));
    }
    for client in &mut clients {
        for fid in 1..=128 {
            client.create(0, fid, &format!("file-{fid}"));
            client.write(fid, 0, b"shared work");
        }
        assert_eq!(client.read(128, 0, 32), b"shared work");
    }
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    assert_eq!(
        limit.rlim_max, hard,
        "must not change the inherited hard limit"
    );
    assert!(limit.rlim_cur >= 1024);
}

struct Client {
    stream: UnixStream,
    tag: u16,
}
impl Client {
    fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut client = Self { stream, tag: 0 };
        let mut body = Out::default();
        // The bundled guest requests 512 KiB; the host must negotiate it down.
        body.u32(512 * 1024).bytes(b"9P2000.L");
        let version = client.ok(TVERSION, &body.0);
        let mut version = In { data: &version };
        assert_eq!(version.u32().unwrap(), 64 * 1024);
        assert_eq!(version.bytes().unwrap(), b"9P2000.L");
        let mut body = Out::default();
        body.u32(0).u32(!0).bytes(b"builder").bytes(b"").u32(UID);
        client.ok(TATTACH, &body.0);
        client
    }
    fn call(&mut self, kind: u8, body: &[u8]) -> (u8, Vec<u8>) {
        self.tag += 1;
        let mut frame = Out::default();
        frame.u32(body.len() as u32 + 7).u8(kind).u16(self.tag);
        frame.0.extend_from_slice(body);
        self.stream.write_all(&frame.0).unwrap();
        let mut size = [0; 4];
        self.stream.read_exact(&mut size).unwrap();
        let mut frame = vec![0; u32::from_le_bytes(size) as usize - 4];
        self.stream.read_exact(&mut frame).unwrap();
        assert_eq!(u16::from_le_bytes([frame[1], frame[2]]), self.tag);
        (frame[0], frame[3..].to_vec())
    }
    fn ok(&mut self, kind: u8, body: &[u8]) -> Vec<u8> {
        let (reply, body) = self.call(kind, body);
        assert_eq!(reply, kind + 1, "request {kind}, reply {reply}: {body:?}");
        body
    }
    fn error(&mut self, kind: u8, body: &[u8]) -> u32 {
        let (reply, body) = self.call(kind, body);
        assert_eq!(reply, RLERROR);
        u32::from_le_bytes(body.try_into().unwrap())
    }
    fn walk(&mut self, parent: u32, id: u32, names: &[&str]) {
        let mut body = Out::default();
        body.u32(parent).u32(id).u16(names.len() as u16);
        for name in names {
            body.bytes(name.as_bytes());
        }
        let result = self.ok(TWALK, &body.0);
        assert_eq!(
            u16::from_le_bytes(result[..2].try_into().unwrap()),
            names.len() as u16
        );
    }
    fn create(&mut self, parent: u32, id: u32, name: &str) {
        self.walk(parent, id, &[]);
        let mut body = Out::default();
        body.u32(id)
            .bytes(name.as_bytes())
            .u32(2)
            .u32(0o644)
            .u32(UID);
        self.ok(TLCREATE, &body.0);
    }
    fn open(&mut self, id: u32, flags: u32) {
        let mut body = Out::default();
        body.u32(id).u32(flags);
        self.ok(TLOPEN, &body.0);
    }
    fn write(&mut self, id: u32, offset: u64, bytes: &[u8]) {
        let mut body = Out::default();
        body.u32(id).u64(offset).u32(bytes.len() as u32);
        body.0.extend_from_slice(bytes);
        assert_eq!(self.ok(TWRITE, &body.0), (bytes.len() as u32).to_le_bytes());
    }
    fn read(&mut self, id: u32, offset: u64, count: u32) -> Vec<u8> {
        let mut body = Out::default();
        body.u32(id).u64(offset).u32(count);
        let reply = self.ok(TREAD, &body.0);
        assert_eq!(
            u32::from_le_bytes(reply[..4].try_into().unwrap()) as usize,
            reply.len() - 4
        );
        reply[4..].to_vec()
    }
    fn attrs(&mut self, id: u32, valid: u32, mode: u32, size: u64) {
        let mut body = Out::default();
        body.u32(id)
            .u32(valid)
            .u32(mode)
            .u32(UID)
            .u32(UID)
            .u64(size)
            .u64(1234)
            .u64(5678)
            .u64(2345)
            .u64(6789);
        self.ok(TSETATTR, &body.0);
    }
    fn clunk(&mut self, id: u32) {
        self.ok(TCLUNK, &id.to_le_bytes());
    }
    fn lock(&mut self, id: u32, kind: u8, start: u64, length: u64, pid: u32) -> u8 {
        let mut body = Out::default();
        body.u32(id)
            .u8(kind)
            .u32(0)
            .u64(start)
            .u64(length)
            .u32(pid)
            .bytes(b"vm");
        self.ok(TLOCK, &body.0)[0]
    }
}
fn fixture() -> (tempfile::TempDir, PathBuf, Server, Client) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("work");
    fs::create_dir(&root).unwrap();
    let socket = dir.path().join("9p");
    let server = serve_writable(socket.clone(), &root).unwrap();
    let client = Client::connect(&socket);
    (dir, root, server, client)
}

#[test]
fn wire_large_directory_paginates_with_bounded_messages() {
    let (_dir, root, _server, mut c) = fixture();
    let mut expected = std::collections::BTreeSet::from([".".to_owned(), "..".to_owned()]);
    for index in 0..1800 {
        let name = format!("sdk-directory-entry-{index:04}-with-a-long-name");
        fs::write(root.join(&name), b"").unwrap();
        expected.insert(name);
    }
    c.open(0, 0);
    let mut seen = std::collections::BTreeSet::new();
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let mut body = Out::default();
        body.u32(0).u64(offset).u32(512 * 1024);
        let reply = c.ok(TREADDIR, &body.0);
        assert!(reply.len() + 7 <= 64 * 1024);
        let mut entries = In { data: &reply };
        assert_eq!(entries.u32().unwrap() as usize, entries.data.len());
        if entries.data.is_empty() {
            break;
        }
        pages += 1;
        while !entries.data.is_empty() {
            entries.take(13).unwrap(); // qid
            let next = entries.u64().unwrap();
            assert!(next > offset, "directory cookies must advance");
            offset = next;
            entries.take(1).unwrap(); // directory entry type
            let name = String::from_utf8(entries.bytes().unwrap().to_vec()).unwrap();
            assert!(seen.insert(name), "directory entries must not repeat");
        }
        assert!(pages <= 1802, "directory enumeration must finish");
    }
    assert!(
        pages > 1,
        "exercise continuation beyond one negotiated message"
    );
    assert_eq!(seen, expected);
}

#[test]
fn wire_mutations_preserve_fids_data_modes_times_and_host_capacity() {
    let (_dir, root, _server, mut c) = fixture();
    c.create(0, 1, "file");
    c.write(1, 0, b"hello");
    c.write(1, 7, b"world");
    assert_eq!(c.read(1, 0, 99), b"hello\0\0world");
    assert!(c.read(1, 99, 20).is_empty());
    c.attrs(1, 8, 0, 3);
    assert_eq!(c.read(1, 0, 99), b"hel");
    c.attrs(1, 8, 0, 8);
    assert_eq!(c.read(1, 0, 99), b"hel\0\0\0\0\0");
    c.attrs(1, 1 | 2 | 4 | 16 | 32 | 128 | 256, 0o755, 0);
    let meta = fs::metadata(root.join("file")).unwrap();
    assert_eq!(meta.mode() & 0o777, 0o755);
    assert_eq!(
        (
            meta.atime(),
            meta.atime_nsec(),
            meta.mtime(),
            meta.mtime_nsec()
        ),
        (1234, 5678, 2345, 6789)
    );
    let mut body = Out::default();
    body.u32(1).u64(0x7ff);
    let attr = c.ok(TGETATTR, &body.0);
    assert_eq!(u32::from_le_bytes(attr[25..29].try_into().unwrap()), UID);
    assert_eq!(u32::from_le_bytes(attr[29..33].try_into().unwrap()), UID);
    let mut body = Out::default();
    body.u32(1).u32(0);
    c.ok(TFSYNC, &body.0);
    let mut body = Out::default();
    body.u32(0).bytes(b"old").u32(0o755).u32(UID);
    c.ok(TMKDIR, &body.0);
    c.walk(0, 2, &["old"]);
    c.create(2, 3, "child");
    c.write(3, 0, b"nested");
    let mut body = Out::default();
    body.u32(2).u32(0).bytes(b"new");
    c.ok(TRENAME, &body.0);
    c.write(3, 0, b"pinned");
    assert_eq!(fs::read(root.join("new/child")).unwrap(), b"pinned");
    c.walk(2, 4, &[".."]);
    c.create(4, 5, "at-root");
    assert!(root.join("at-root").exists());
    // Hard links share content, and the original fid remains usable when its
    // directory entry is renamed and then unlinked.
    let mut body = Out::default();
    body.u32(0).u32(1).bytes(b"alias");
    c.ok(TLINK, &body.0);
    assert_eq!(
        fs::metadata(root.join("file")).unwrap().ino(),
        fs::metadata(root.join("alias")).unwrap().ino()
    );
    let mut body = Out::default();
    body.u32(0).bytes(b"file").u32(0).bytes(b"renamed");
    c.ok(TRENAMEAT, &body.0);
    c.write(1, 0, b"R");
    assert_eq!(c.read(1, 0, 3), b"Rel");
    let mut body = Out::default();
    body.u32(0).bytes(b"renamed").u32(0);
    c.ok(TUNLINKAT, &body.0);
    c.write(1, 1, b"W");
    assert_eq!(c.read(1, 0, 3), b"RWl");
    let mut body = Out::default();
    body.u32(0).bytes(b"symbol").bytes(b"new/child").u32(UID);
    c.ok(TSYMLINK, &body.0);
    c.walk(0, 6, &["symbol"]);
    assert_eq!(&c.ok(TREADLINK, &6u32.to_le_bytes())[2..], b"new/child");
    // Capacity comes from the host filesystem, not a manufactured disk limit.
    let stat = c.ok(TSTATFS, &0u32.to_le_bytes());
    assert!(u64::from_le_bytes(stat[8..16].try_into().unwrap()) > 0);
    c.clunk(3);
    c.walk(0, 7, &["new", "child"]);
    c.ok(TREMOVE, &7u32.to_le_bytes());
    let mut body = Out::default();
    body.u32(0).bytes(b"new").u32(0x200);
    c.ok(TUNLINKAT, &body.0);
    assert!(!root.join("new").exists());
}

#[test]
fn wire_open_handles_survive_replace_and_truncate_reopens() {
    let (_dir, root, _server, mut c) = fixture();
    c.create(0, 1, "old");
    c.write(1, 0, b"old inode");
    c.create(0, 2, "replacement");
    c.write(2, 0, b"new inode");
    let mut body = Out::default();
    body.u32(0).bytes(b"replacement").u32(0).bytes(b"old");
    c.ok(TRENAMEAT, &body.0);
    assert_eq!(c.read(1, 0, 30), b"old inode");
    assert_eq!(c.read(2, 0, 30), b"new inode");
    c.write(1, 0, b"orphan");
    assert_eq!(fs::read(root.join("old")).unwrap(), b"new inode");
    c.walk(0, 3, &["old"]);
    c.open(3, 2 | 0o1000);
    assert!(c.read(3, 0, 40).is_empty());
    c.write(3, 0, b"x");
    assert_eq!(c.read(2, 0, 30), b"x");
    c.walk(0, 4, &["old"]);
    c.open(4, 1 | 0o2000);
    c.write(4, 0, b"y");
    assert_eq!(c.read(3, 0, 30), b"xy");
    let mut write = Out::default();
    write.u32(3).u64(u64::MAX).u32(1).u8(0);
    assert_eq!(c.error(TWRITE, &write.0), errno::EINVAL);
}

#[test]
fn wire_gcc_create_flag_reopens_existing_fid_without_creating_a_dentry() {
    let (_dir, root, _server, mut c) = fixture();
    // GCC's driver first creates its temporary assembly file. cc1 then
    // reopens it using the exact Tlopen flags captured from Linux 6.18.
    c.create(0, 1, "ccJqxLfb.s");
    c.write(1, 0, b"existing");
    c.clunk(1);
    let inode = fs::metadata(root.join("ccJqxLfb.s")).unwrap().ino();
    c.walk(0, 2, &["ccJqxLfb.s"]);
    c.open(2, 0x42);
    assert_eq!(c.read(2, 0, 100), b"existing");
    c.attrs(2, 8, 0, 0);
    c.write(2, 0, b"assembly");
    c.clunk(2);
    c.walk(0, 3, &["ccJqxLfb.s"]);
    c.open(3, 0x41);
    c.write(3, 0, b"A");
    assert_eq!(fs::read(root.join("ccJqxLfb.s")).unwrap(), b"Assembly");
    assert_eq!(fs::metadata(root.join("ccJqxLfb.s")).unwrap().ino(), inode);
    c.walk(0, 4, &["ccJqxLfb.s"]);
    let mut exclusive = Out::default();
    exclusive.u32(4).u32(0xc2 | 0o1000);
    assert_eq!(c.error(TLOPEN, &exclusive.0), EEXIST);
    assert_eq!(fs::read(root.join("ccJqxLfb.s")).unwrap(), b"Assembly");
    // A failed exclusive create leaves the fid usable. EXCL alone follows
    // regular-file open semantics and cannot imply creation or replacement.
    c.open(4, 0x82);
    assert_eq!(c.read(4, 0, 100), b"Assembly");
    c.walk(0, 5, &["ccJqxLfb.s"]);
    let mut unlink = Out::default();
    unlink.u32(0).bytes(b"ccJqxLfb.s").u32(0);
    c.ok(TUNLINKAT, &unlink.0);
    c.open(5, 0x42);
    c.write(5, 0, b"orphan");
    assert!(!root.join("ccJqxLfb.s").exists());
    assert_eq!(c.read(5, 0, 6), b"orphan");
}

#[test]
fn wire_range_locks_conflict_split_unlock_and_release_across_exports() {
    let (dir, root, _server, mut a) = fixture();
    a.create(0, 1, "locked");
    let socket = dir.path().join("other");
    let _other = serve_writable(socket.clone(), &root).unwrap();
    let mut b = Client::connect(&socket);
    b.walk(0, 1, &["locked"]);
    b.open(1, 2);
    assert_eq!(a.lock(1, 0, 0, 10, 11), 0);
    assert_eq!(b.lock(1, 0, 0, 10, 22), 0);
    assert_eq!(b.lock(1, 1, 0, 10, 22), 1);
    let mut query = Out::default();
    query.u32(1).u8(1).u64(0).u64(10).u32(22).bytes(b"vm");
    let locked = b.ok(TGETLOCK, &query.0);
    assert_eq!(locked[0], 0);
    assert_eq!(u32::from_le_bytes(locked[17..21].try_into().unwrap()), 11);
    assert_eq!(a.lock(1, 2, 4, 2, 11), 0);
    assert_eq!(b.lock(1, 1, 4, 2, 22), 0);
    assert_eq!(a.lock(1, 1, 0, 10, 11), 1);
    assert_eq!(b.lock(1, 2, 0, 0, 22), 0);
    assert_eq!(a.lock(1, 1, 0, 10, 11), 0);
    assert_eq!(b.lock(1, 1, 9, 1, 22), 1);
    assert_eq!(b.lock(1, 1, 10, 1, 22), 0);
    a.clunk(1);
    assert_eq!(b.lock(1, 1, 0, 0, 22), 0);
    b.clunk(1);
    a.walk(0, 2, &["locked"]);
    a.open(2, 2);
    assert_eq!(a.lock(2, 1, 0, 0, 33), 0);
    let mut version = Out::default();
    version.u32(MAX_MSIZE).bytes(b"9P2000.L");
    a.ok(TVERSION, &version.0);
    b.walk(0, 2, &["locked"]);
    b.open(2, 2);
    assert_eq!(b.lock(2, 1, 0, 0, 44), 0);
    drop(b);
    let mut c = Client::connect(&socket);
    c.walk(0, 1, &["locked"]);
    c.open(1, 2);
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while c.lock(1, 1, 0, 0, 55) == 1 {
        assert!(std::time::Instant::now() < until);
        std::thread::yield_now();
    }
}

#[test]
fn wire_symlinks_dotdot_and_concurrent_swaps_cannot_escape() {
    let (dir, root, _server, mut c) = fixture();
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"safe").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    c.walk(0, 1, &["escape"]);
    let mut open = Out::default();
    open.u32(1).u32(2);
    assert_eq!(c.error(TLOPEN, &open.0), errno::ELOOP);
    let mut attrs = Out::default();
    attrs
        .u32(1)
        .u32(8)
        .u32(0)
        .u32(UID)
        .u32(UID)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(0);
    assert_eq!(c.error(TSETATTR, &attrs.0), errno::EOPNOTSUPP);
    c.walk(0, 2, &["..", ".."]);
    c.create(2, 3, "inside");
    assert!(root.join("inside").exists());
    assert!(!dir.path().join("inside").exists());
    let mut mkdir = Out::default();
    mkdir.u32(0).bytes(b"../outside").u32(0o755).u32(UID);
    assert_eq!(c.error(TMKDIR, &mkdir.0), errno::EINVAL);
    fs::create_dir(root.join("safe")).unwrap();
    c.walk(0, 4, &["safe"]);
    fs::rename(root.join("safe"), root.join("held")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("safe")).unwrap();
    c.create(4, 5, "pinned-parent");
    assert!(root.join("held/pinned-parent").exists());
    assert!(!outside.join("pinned-parent").exists());
    // Repeated last-component substitution never turns an accepted write-open
    // into an open of the symlink target. All checks occur on the actual fd.
    fs::write(root.join("race"), b"inside").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let race = root.clone();
    let target = outside.join("secret");
    let swapper = std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            let _ = fs::remove_file(race.join("race"));
            let _ = std::os::unix::fs::symlink(&target, race.join("race"));
            let _ = fs::remove_file(race.join("race"));
            let _ = fs::write(race.join("race"), b"inside");
        }
    });
    for id in 10..110 {
        let mut walk = Out::default();
        walk.u32(0).u32(id).u16(1).bytes(b"race");
        let (kind, _) = c.call(TWALK, &walk.0);
        if kind == TWALK + 1 {
            let mut open = Out::default();
            open.u32(id).u32(2 | 0o1000);
            let (kind, _) = c.call(TLOPEN, &open.0);
            if kind == TLOPEN + 1 {
                c.write(id, 0, b"guest");
            }
            c.clunk(id);
        }
    }
    stop.store(true, Ordering::Relaxed);
    swapper.join().unwrap();
    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"safe");
}

#[test]
fn wire_linux_lchown_symlink_ctime_flag_maps_ownership_without_following() {
    let (dir, root, _server, mut c) = fixture();
    fs::write(root.join("target"), b"inside").unwrap();
    fs::create_dir(root.join("directory")).unwrap();
    let outside = dir.path().join("outside");
    fs::write(&outside, b"outside").unwrap();
    let outside_before = fs::metadata(&outside).unwrap();
    let targets = [
        ("file-link", PathBuf::from("target")),
        ("dir-link", PathBuf::from("directory")),
        ("dangling", PathBuf::from("missing")),
        ("outside-link", outside.clone()),
    ];
    for (index, (component, target)) in targets.iter().enumerate() {
        std::os::unix::fs::symlink(target, root.join(component)).unwrap();
        let fid = index as u32 + 1;
        c.walk(0, fid, &[component]);
        let before = fs::symlink_metadata(root.join(component)).unwrap();
        // Exact kernel request from source sync's lchown: UID|GID|CTIME,
        // mode=0, uid=gid=1000, size and both timestamps zero.
        let mut request = Out::default();
        request
            .u32(fid)
            .u32(0x46)
            .u32(0)
            .u32(1000)
            .u32(1000)
            .u64(0)
            .u64(0)
            .u64(0)
            .u64(0)
            .u64(0);
        c.ok(TSETATTR, &request.0);
        let after = fs::symlink_metadata(root.join(component)).unwrap();
        assert_eq!(
            (
                after.uid(),
                after.gid(),
                after.mode(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            ),
            (
                before.uid(),
                before.gid(),
                before.mode(),
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec()
            )
        );
        assert_eq!(fs::read_link(root.join(component)).unwrap(), *target);
        for valid in [1, 16, 32, 64, 0x46 | 16] {
            let mut unsupported = Out::default();
            unsupported
                .u32(fid)
                .u32(valid)
                .u32(0o777)
                .u32(UID)
                .u32(UID)
                .u64(0)
                .u64(0)
                .u64(0)
                .u64(0)
                .u64(0);
            assert_eq!(c.error(TSETATTR, &unsupported.0), errno::EOPNOTSUPP);
        }
    }
    let outside_after = fs::metadata(&outside).unwrap();
    assert_eq!(
        (
            outside_after.uid(),
            outside_after.gid(),
            outside_after.mode(),
            outside_after.mtime(),
            outside_after.mtime_nsec(),
            outside_after.ctime(),
            outside_after.ctime_nsec()
        ),
        (
            outside_before.uid(),
            outside_before.gid(),
            outside_before.mode(),
            outside_before.mtime(),
            outside_before.mtime_nsec(),
            outside_before.ctime(),
            outside_before.ctime_nsec()
        )
    );
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
}

#[test]
fn wire_access_modes_and_unsupported_requests_are_not_silently_accepted() {
    let (_dir, root, _server, mut c) = fixture();
    c.create(0, 1, "mode");
    c.write(1, 0, b"abc");
    let owner = fs::metadata(root.join("mode")).unwrap().uid();
    c.attrs(1, 1, 0, 0);
    c.attrs(1, 8, 0, 2);
    c.attrs(1, 1, 0o644, 0);
    assert_eq!(c.read(1, 0, 10), b"ab");
    c.walk(0, 2, &["mode"]);
    c.open(2, 0);
    let mut write = Out::default();
    write.u32(2).u64(0).u32(1).u8(0);
    assert_eq!(c.error(TWRITE, &write.0), errno::EBADF);
    let mut attrs = Out::default();
    attrs
        .u32(1)
        .u32(2)
        .u32(0)
        .u32(1234)
        .u32(UID)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(0);
    assert_eq!(c.error(TSETATTR, &attrs.0), errno::EPERM);
    assert_eq!(fs::metadata(root.join("mode")).unwrap().uid(), owner);
    assert_eq!(c.error(TMKNOD, &[]), errno::EOPNOTSUPP);
    assert_eq!(c.error(TXATTRCREATE, &[]), errno::EOPNOTSUPP);
    c.walk(0, 3, &["mode"]);
    let mut open = Out::default();
    open.u32(3).u32(0o40000);
    assert_eq!(c.error(TLOPEN, &open.0), errno::EOPNOTSUPP);
    let mut lock = Out::default();
    lock.u32(1)
        .u8(1)
        .u32(2)
        .u64(0)
        .u64(0)
        .u32(1)
        .bytes(b"reclaim");
    assert_eq!(c.error(TLOCK, &lock.0), errno::EOPNOTSUPP);
}

#[test]
fn wire_directory_fids_follow_reparenting_and_refresh_readdir() {
    let (_dir, root, _server, mut c) = fixture();
    for component in ["first", "second"] {
        let mut body = Out::default();
        body.u32(0).bytes(component.as_bytes()).u32(0o755).u32(UID);
        c.ok(TMKDIR, &body.0);
    }
    c.walk(0, 1, &["first"]);
    c.walk(0, 2, &["second"]);
    c.walk(1, 3, &[]);
    c.open(3, 0);
    let mut body = Out::default();
    body.u32(3).u64(0).u32(8192);
    let initial = c.ok(TREADDIR, &body.0);
    c.create(1, 4, "new-entry");
    let refreshed = c.ok(TREADDIR, &body.0);
    assert!(refreshed.len() > initial.len());
    let mut body = Out::default();
    body.u32(1).u32(2).bytes(b"moved");
    c.ok(TRENAME, &body.0);
    c.walk(1, 5, &[".."]);
    c.create(5, 6, "new-parent");
    assert!(root.join("second/new-parent").exists());
    c.write(4, 0, b"stable");
    assert_eq!(
        fs::read(root.join("second/moved/new-entry")).unwrap(),
        b"stable"
    );
}

#[test]
fn wire_root_initialization_maps_ids_without_host_chown_and_survives_root_rename() {
    let (dir, root, _server, mut c) = fixture();
    c.walk(0, 1, &[]);
    let mut create = Out::default();
    create
        .u32(1)
        .bytes(b"root-created")
        .u32(2)
        .u32(0o644)
        .u32(0);
    c.ok(TLCREATE, &create.0);
    c.write(1, 0, b"staged");
    let mut mkdir = Out::default();
    mkdir.u32(0).bytes(b"home").u32(0o755).u32(0);
    c.ok(TMKDIR, &mkdir.0);
    let mut symlink = Out::default();
    symlink
        .u32(0)
        .bytes(b"root-link")
        .bytes(b"root-created")
        .u32(0);
    c.ok(TSYMLINK, &symlink.0);
    c.walk(0, 2, &["home"]);
    c.walk(0, 3, &["root-link"]);
    for (fid, path) in [
        (0, root.clone()),
        (1, root.join("root-created")),
        (2, root.join("home")),
        (3, root.join("root-link")),
    ] {
        let before = fs::symlink_metadata(&path).unwrap();
        let mut attrs = Out::default();
        attrs
            .u32(fid)
            .u32(2 | 4)
            .u32(0)
            .u32(0)
            .u32(0)
            .u64(0)
            .u64(0)
            .u64(0)
            .u64(0)
            .u64(0);
        c.ok(TSETATTR, &attrs.0);
        let after = fs::symlink_metadata(&path).unwrap();
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
        let mut stat = Out::default();
        stat.u32(fid).u64(0x7ff);
        let stat = c.ok(TGETATTR, &stat.0);
        assert_eq!(u32::from_le_bytes(stat[25..29].try_into().unwrap()), UID);
        assert_eq!(u32::from_le_bytes(stat[29..33].try_into().unwrap()), UID);
    }
    let final_root = dir.path().join("final-work");
    fs::rename(&root, &final_root).unwrap();
    // Root itself moves after Hello; all existing fids and future attaches
    // retain the original directory capability, never the old pathname.
    c.write(1, 0, b"active");
    c.create(2, 4, "after-move");
    c.write(4, 0, b"new");
    c.walk(0, 5, &["..", ".."]);
    c.create(5, 6, "at-final-root");
    let mut next = Client::connect(&dir.path().join("9p"));
    next.walk(0, 1, &["root-created"]);
    next.open(1, 2);
    assert_eq!(next.read(1, 0, 20), b"active");
    assert_eq!(
        fs::read(final_root.join("home/after-move")).unwrap(),
        b"new"
    );
    assert!(final_root.join("at-final-root").exists());
    assert!(!root.exists());
    next.walk(0, 2, &[]);
    let mut invalid = Out::default();
    invalid
        .u32(2)
        .bytes(b"bad-owner")
        .u32(2)
        .u32(0o644)
        .u32(1234);
    assert_eq!(next.error(TLCREATE, &invalid.0), errno::EPERM);
    let mut invalid = Out::default();
    invalid.u32(0).bytes(b"bad-group").u32(0o755).u32(1234);
    assert_eq!(next.error(TMKDIR, &invalid.0), errno::EPERM);
    let mut invalid = Out::default();
    invalid
        .u32(0)
        .bytes(b"bad-link")
        .bytes(b"root-created")
        .u32(1234);
    assert_eq!(next.error(TSYMLINK, &invalid.0), errno::EPERM);
    let mut invalid = Out::default();
    invalid
        .u32(1)
        .u32(4)
        .u32(0)
        .u32(UID)
        .u32(1234)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(0);
    assert_eq!(next.error(TSETATTR, &invalid.0), errno::EPERM);
}

#[test]
fn wire_case_collisions_follow_host_rules_without_overwriting_aliases() {
    let (_dir, root, _server, mut c) = fixture();
    c.create(0, 1, "MixedCase");
    c.write(1, 0, b"original");
    let insensitive = root.join("mixedcase").exists();
    c.walk(0, 2, &[]);
    let mut create = Out::default();
    create.u32(2).bytes(b"mixedcase").u32(2).u32(0o644).u32(UID);
    if insensitive {
        assert_eq!(c.error(TLCREATE, &create.0), EEXIST);
        c.create(0, 3, "other");
        c.write(3, 0, b"other");
        let mut rename = Out::default();
        rename.u32(3).u32(0).bytes(b"mixedcase");
        assert_eq!(c.error(TRENAME, &rename.0), EEXIST);
        assert_eq!(fs::read(root.join("MixedCase")).unwrap(), b"original");
        let mut rename = Out::default();
        rename.u32(1).u32(0).bytes(b"mixedcase");
        c.ok(TRENAME, &rename.0);
        c.write(1, 0, b"renamed!");
        assert_eq!(fs::read(root.join("mixedcase")).unwrap(), b"renamed!");
    } else {
        c.ok(TLCREATE, &create.0);
        assert!(root.join("MixedCase").exists() && root.join("mixedcase").exists());
    }
}
