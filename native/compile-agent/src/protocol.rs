//! Host <-> guest protocol of GizMoth's compile VMs.
//!
//! This file is shared verbatim by the Linux guest agent and the host service
//! (`src-tauri/src/compile/guest.rs` includes it with `#[path]`), so both sides
//! always agree on frame layout and message shapes.
//!
//! The host reaches the agent over hybrid vsock: it connects to the VM's Unix
//! socket, writes `CONNECT <CONTROL_PORT>\n` and receives `OK <port>\n`. Guest
//! connections to host port `CACHE_PORT` arrive on `<socket>_<port>`.
//!
//! Every frame is a 4-byte big-endian length (kind byte plus payload), one kind
//! byte and the payload. JSON frames carry [`HostMessage`] / [`GuestMessage`];
//! data frames carry file contents announced by the preceding JSON frame.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Bumped whenever a message or the guest filesystem layout changes
/// incompatibly. The host refuses an image built by another agent version.
pub const AGENT_VERSION: u32 = 5;
pub const GIZMOTH_PREFIX: &str = "/opt/gizmoth";
pub const PACKAGE_ROOT: &str = "/opt/gizmoth/packages";
pub const COMMAND_BIN: &str = "/opt/gizmoth/bin";
pub const CONTROL_PORT: u32 = 1000;
pub const CACHE_PORT: u32 = 7421;
/// Source/download share; always read-only.
pub const SHARE_PORT: u32 = 5640;
/// Host-managed writable build directory.
pub const WORK_SHARE_PORT: u32 = 5641;
pub const WORK_MIGRATION_MARKER: &str = ".gizmoth-migration-complete";
/// 9P message size for shares; matches the host server's maximum.
pub const SHARE_MSIZE: u32 = 512 * 1024;
pub const MAX_FRAME: usize = 1 << 20;
pub const DATA_CHUNK: usize = 512 * 1024;
pub const MANIFEST_CHUNK: usize = 4000;
pub const BUILDER_UID: u32 = 1000;

pub const JSON: u8 = 1;
pub const STDOUT: u8 = 2;
pub const STDERR: u8 = 3;
pub const DATA: u8 = 4;

pub fn write_frame<W: Write + ?Sized>(writer: &mut W, kind: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() + 1 > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame too large",
        ));
    }
    let mut frame = Vec::with_capacity(payload.len() + 5);
    frame.extend_from_slice(&((payload.len() + 1) as u32).to_be_bytes());
    frame.push(kind);
    frame.extend_from_slice(payload);
    writer.write_all(&frame)
}

pub fn write_json<W: Write + ?Sized, T: Serialize>(writer: &mut W, message: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(message).map_err(io::Error::other)?;
    write_frame(writer, JSON, &bytes)
}

/// Splits a JSON array by entry count and serialized frame size, preserving order.
/// `envelope` contains an empty array and the longest other fields used by any
/// chunk (for example, `done: false`). An empty input produces one empty chunk.
pub fn json_chunks<'a, T: Serialize, M: Serialize>(
    items: &'a [T],
    max_count: usize,
    envelope: &M,
) -> io::Result<Vec<&'a [T]>> {
    let base = serde_json::to_vec(envelope)
        .map_err(io::Error::other)?
        .len()
        + 1;
    if max_count == 0 || base > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid chunk envelope or count",
        ));
    }
    let (mut chunks, mut start, mut size) = (Vec::new(), 0, base);
    for (index, item) in items.iter().enumerate() {
        let bytes = serde_json::to_vec(item).map_err(io::Error::other)?.len();
        if bytes > MAX_FRAME - base {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "JSON array entry exceeds frame limit",
            ));
        }
        let comma = usize::from(index > start);
        if index - start == max_count || size + comma + bytes > MAX_FRAME {
            chunks.push(&items[start..index]);
            start = index;
            size = base;
        }
        size += usize::from(index > start) + bytes;
    }
    chunks.push(&items[start..]);
    Ok(chunks)
}

/// `Ok(None)` at a clean end of stream between frames.
pub fn read_frame<R: Read + ?Sized>(reader: &mut R) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut length = [0u8; 4];
    let mut filled = 0;
    while filled < 4 {
        match reader.read(&mut length[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut kind = [0u8; 1];
    reader.read_exact(&mut kind)?;
    let mut payload = vec![0u8; length - 1];
    reader.read_exact(&mut payload)?;
    Ok(Some((kind[0], payload)))
}

pub fn parse<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> io::Result<T> {
    serde_json::from_slice(payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Link,
}

/// One synchronized workspace path. `mtime` is the host's modification time in
/// nanoseconds; together with size and mode it decides whether to resend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mtime: i64,
    #[serde(default)]
    pub mode: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl Entry {
    pub fn same_content(&self, other: &Entry) -> bool {
        self.kind == other.kind
            && match self.kind {
                EntryKind::File => {
                    self.size == other.size
                        && self.mtime == other.mtime
                        && self.mode & 0o777 == other.mode & 0o777
                }
                EntryKind::Dir => true,
                EntryKind::Link => self.target == other.target,
            }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestJob {
    pub id: String,
    /// Stable key of the host workspace: `/work/src/<workspace>` in the guest.
    pub workspace: String,
    pub argv: Vec<String>,
    /// Relative directory within the workspace.
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEnv {
    /// Namespaces served by the host cache through the guest proxy.
    pub bazel: bool,
    pub ccache: bool,
    /// Local ccache tier on the slot disk, e.g. `5G`.
    pub ccache_local_size: String,
    /// Additional namespaces: name and environment variable receiving its URL.
    #[serde(default)]
    pub custom: Vec<(String, String)>,
}

/// `/config.json` inside the initramfs, written by the host for every boot.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootConfig {
    pub mode: BootMode,
    pub slot: String,
    #[serde(default)]
    pub caches: CacheEnv,
    /// Extra lines appended to `/etc/bazel.bazelrc`.
    #[serde(default)]
    pub bazelrc: Vec<String>,
    /// Extra environment for every command.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Directories put in front of `PATH` for every command (package toolchains).
    #[serde(default)]
    pub path: Vec<String>,
    /// Host folder mounts with explicit per-share access policy.
    #[serde(default)]
    pub shares: Vec<Share>,
    /// Optional legacy work data to copy before the first host-directory build.
    #[serde(default)]
    pub legacy_work: String,
    /// Read-only erofs disks attached after the runtime disk.
    #[serde(default)]
    pub images: Vec<ImageMount>,
    /// What a pack boot installs and packs.
    #[serde(default)]
    pub pack: Option<PackPlan>,
}

/// Registry packages to unpack (with Linux semantics: the host's file system
/// may be case-insensitive) and pack into one erofs filesystem. Sources are
/// verified downloads named by SHA-256 in the shared folder.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PackPlan {
    pub packages: Vec<PackItem>,
    /// Default formula command directories, in PATH precedence order. The
    /// packer exports their executable files into COMMAND_BIN after setup.
    #[serde(default)]
    pub command_paths: Vec<String>,
    /// Written as `index.json` at the root of the pack.
    pub index: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PackItem {
    /// Relative directory, for example `toolchains/python/3.12.16`.
    pub dir: String,
    #[serde(default)]
    pub guest_root: bool,
    #[serde(default)]
    pub bootstrap: bool,
    pub steps: Vec<PackStep>,
    pub checks: Vec<PackCheck>,
    #[serde(default)]
    pub command_checks: Vec<PackCommandCheck>,
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PackStep {
    Copy {
        source: String,
        to: String,
        executable: bool,
    },
    Extract {
        source: String,
        to: String,
        #[serde(default = "default_archive_format")]
        format: String,
        strip_components: u32,
    },
    Checkout {
        source: String,
        to: String,
    },
    Run {
        program: String,
        args: Vec<String>,
        cwd: String,
        environment: std::collections::BTreeMap<String, String>,
        #[serde(rename = "timeoutSeconds")]
        timeout_seconds: u64,
    },
}

fn default_archive_format() -> String {
    "tar.gz".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PackCheck {
    pub path: String,
    pub executable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PackCommandCheck {
    pub program: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub stdout_contains: Option<String>,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageMount {
    /// vmbus SCSI LUN: the disk's position in the host's attachment order.
    pub lun: u32,
    /// Absolute mount point in the guest.
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Share {
    /// Host vsock port of the share's 9P server.
    pub port: u32,
    /// Absolute mount point in the guest.
    pub path: String,
    /// Only the private work export is writable; source/tools remain read-only.
    #[serde(default)]
    pub writable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BootMode {
    Worker,
    /// Builds userland from `shares[0]` on output LUN 0, using scratch LUN 1.
    Pack,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub total: u64,
    pub free: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostMessage {
    Hello,
    Job {
        job: GuestJob,
    },
    /// Workspace paths to remove, deepest first.
    Delete {
        paths: Vec<String>,
    },
    /// A file is followed by DATA frames totalling `entry.size` bytes.
    Put {
        entry: Entry,
    },
    SyncDone,
    Cancel,
    Collect {
        patterns: Vec<String>,
        max_files: u32,
        max_bytes: u64,
    },
    Finish,
    Poweroff,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum GuestMessage {
    Hello {
        version: u32,
        mode: BootMode,
        #[serde(default)]
        work: Option<DiskInfo>,
        #[serde(default)]
        image: Option<serde_json::Value>,
    },
    /// Recorded host identities validated against the current guest tree.
    /// Altered or missing entries use a target-less link as a mismatch marker,
    /// retaining the path so the host can either resend or delete it.
    Manifest {
        entries: Vec<Entry>,
        done: bool,
    },
    Phase {
        phase: String,
    },
    Synced {
        files: u64,
        bytes: u64,
    },
    Exit {
        code: Option<i32>,
        signal: Option<i32>,
        timed_out: bool,
        cancelled: bool,
    },
    /// Followed by DATA frames totalling `size` bytes.
    File {
        path: String,
        size: u64,
        mode: u32,
    },
    Collected {
        files: u32,
        bytes: u64,
        missing: Vec<String>,
    },
    Error {
        message: String,
    },
    /// End of a one-shot run (the package packer).
    Done {
        ok: bool,
        message: Option<String>,
    },
}

/// A safe workspace-relative path: nonempty normal components only.
pub fn valid_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Shell-style matching of one path component: `*` and `?`.
pub fn component_matches(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni, mut star, mut mark) = (0usize, 0usize, None, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Validates an artifact pattern: relative, `**` only as a whole component.
pub fn valid_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern.len() <= 1024
        && !pattern.starts_with('/')
        && !pattern.contains('\0')
        && pattern.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && (part == "**" || !part.contains("**"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_reject_oversize() {
        let mut buffer = Vec::new();
        write_json(
            &mut buffer,
            &HostMessage::Collect {
                patterns: vec!["out/*.bin".into()],
                max_files: 3,
                max_bytes: 9,
            },
        )
        .unwrap();
        write_frame(&mut buffer, DATA, b"abc").unwrap();
        let mut reader = buffer.as_slice();
        let (kind, payload) = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(kind, JSON);
        assert!(matches!(
            parse::<HostMessage>(&payload).unwrap(),
            HostMessage::Collect { max_files: 3, .. }
        ));
        assert_eq!(
            read_frame(&mut reader).unwrap(),
            Some((DATA, b"abc".to_vec()))
        );
        assert_eq!(read_frame(&mut reader).unwrap(), None);
        assert!(write_frame(&mut Vec::new(), DATA, &vec![0; MAX_FRAME]).is_err());
        assert!(read_frame(&mut [0u8, 0, 0, 0].as_slice()).is_err());
    }

    #[test]
    fn manifest_chunks_fit_frames_with_long_escaped_paths_and_targets() {
        let entries: Vec<Entry> = (0..MANIFEST_CHUNK + 1)
            .map(|index| Entry {
                path: format!("{index}/{}", "long\\\"path/".repeat(35)),
                kind: EntryKind::Link,
                size: 0,
                mtime: 0,
                mode: 0o777,
                target: Some("../target\\\"\\\\/".repeat(40)),
            })
            .collect();
        let envelope = GuestMessage::Manifest {
            entries: vec![],
            done: false,
        };
        assert!(write_json(
            &mut Vec::new(),
            &GuestMessage::Manifest {
                entries: entries[..MANIFEST_CHUNK].to_vec(),
                done: false,
            }
        )
        .is_err());
        let chunks = json_chunks(&entries, MANIFEST_CHUNK, &envelope).unwrap();
        assert!(chunks.len() > 2);
        let mut received = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            assert!(chunk.len() <= MANIFEST_CHUNK);
            let mut frame = Vec::new();
            write_json(
                &mut frame,
                &GuestMessage::Manifest {
                    entries: chunk.to_vec(),
                    done: index + 1 == chunks.len(),
                },
            )
            .unwrap();
            let (_, payload) = read_frame(&mut frame.as_slice()).unwrap().unwrap();
            match parse::<GuestMessage>(&payload).unwrap() {
                GuestMessage::Manifest { entries, done } => {
                    assert_eq!(done, index + 1 == chunks.len());
                    received.extend(entries);
                }
                _ => panic!("expected manifest"),
            }
        }
        assert_eq!(received, entries);
        let empty: Vec<Entry> = Vec::new();
        assert_eq!(
            json_chunks(&empty, MANIFEST_CHUNK, &envelope).unwrap(),
            vec![&[][..]]
        );
    }

    #[test]
    fn json_chunks_enforce_count_exact_byte_boundary_and_individual_limit() {
        let envelope = HostMessage::Delete { paths: vec![] };
        let paths = vec!["x".to_string(); 4001];
        let chunks = json_chunks(&paths, 2000, &envelope).unwrap();
        assert_eq!(
            chunks.iter().map(|chunk| chunk.len()).collect::<Vec<_>>(),
            [2000, 2000, 1]
        );
        let overhead = serde_json::to_vec(&envelope).unwrap().len() + 1;
        // JSON quotes add two bytes to this single string. The kind byte is
        // already included in overhead, so this fills exactly MAX_FRAME.
        let exact = vec!["x".repeat(MAX_FRAME - overhead - 2)];
        let chunks = json_chunks(&exact, 2000, &envelope).unwrap();
        let mut frame = Vec::new();
        write_json(
            &mut frame,
            &HostMessage::Delete {
                paths: chunks[0].to_vec(),
            },
        )
        .unwrap();
        assert_eq!(frame.len(), MAX_FRAME + 4);
        assert!(json_chunks(&[format!("{}x", exact[0])], 2000, &envelope).is_err());
        assert!(json_chunks(&paths, 0, &envelope).is_err());
    }

    #[test]
    fn paths_and_patterns_stay_inside_the_workspace() {
        assert!(valid_relative("a/b.c"));
        for bad in ["", "/a", "a/../b", "a//b", "./a", "a/\0"] {
            assert!(!valid_relative(bad), "{bad:?}");
        }
        assert!(valid_pattern("bazel-bin/**/*.update.tar.zlib"));
        assert!(!valid_pattern("a/**b"));
        assert!(!valid_pattern("../x"));
        assert!(component_matches(
            "*.update.tar.zlib",
            "app.update.tar.zlib"
        ));
        assert!(component_matches("fw-?.bin", "fw-1.bin"));
        assert!(!component_matches("*.bin", "fw.binx"));
        assert!(valid_key("ab12-cd"));
        assert!(!valid_key("AB"));
    }
}
