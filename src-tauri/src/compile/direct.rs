//! Direct builds keep only two capabilities into the caller's workspace. Pin both
//! before queueing. Never resolve a guest-supplied path in the ambient host namespace.
use super::{artifacts::Staging, guest::protocol, p9};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, MetadataExt, OpenOptions, OpenOptionsExt};
use std::{
    collections::HashSet,
    io,
    os::{fd::AsRawFd, unix::fs::MetadataExt as _},
    path::{Component, Path, PathBuf},
};

#[cfg(test)]
mod tests;

type Id = (u64, u64);
fn id(dir: &Dir) -> io::Result<Id> {
    let meta = dir.dir_metadata()?;
    Ok((meta.dev(), meta.ino()))
}

/// The request path has already been canonicalized. Resolve each component
/// without following links in case an ancestor was replaced since admission.
fn open_root(path: &Path) -> io::Result<Dir> {
    let mut dir = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for component in path.components() {
        match component {
            Component::Normal(name) => dir = dir.open_dir_nofollow(name)?,
            Component::RootDir => {}
            _ => return Err(io::Error::other("expected a canonical workspace path")),
        }
    }
    Ok(dir)
}

#[derive(Clone)]
pub(super) struct WriterKey {
    path: PathBuf,
    source: Id,
    out: Id,
}

impl WriterKey {
    pub fn conflicts(&self, other: &Self) -> bool {
        self.source == other.source
            || self.out == other.out
            || self.path == other.path
            || self.path.starts_with(other.path.join("out"))
            || other.path.starts_with(self.path.join("out"))
    }
}

pub(super) struct Workspace {
    pub key: WriterKey,
    pub source: Dir,
    pub out: Dir,
    _descriptors: p9::DescriptorPermit,
}

impl Workspace {
    pub fn check_storage(path: &Path, protected: &Path) -> Result<(), String> {
        let protected = protected
            .canonicalize()
            .map_err(|e| format!("{}: {e}", protected.display()))?;
        let out = path.join("out");
        if out.starts_with(&protected) || protected.starts_with(out) {
            return Err("Direct workspace out/ overlaps GizMoth service storage".into());
        }
        Ok(())
    }

    pub fn prepare(path: &Path) -> Result<Self, String> {
        let open = || -> io::Result<Self> {
            let descriptors = p9::reserve_descriptors(2)?;
            let source = open_root(path)?;
            match source.create_dir("out") {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            let out = source.open_dir_nofollow("out")?;
            let key = WriterKey {
                path: path.into(),
                source: id(&source)?,
                out: id(&out)?,
            };
            if key.out.0 != key.source.0 {
                return Err(io::Error::other("out/ must be on the workspace volume"));
            }
            Ok(Self {
                key,
                source,
                out,
                _descriptors: descriptors,
            })
        };
        open().map_err(|e| {
            format!(
                "Direct build needs a real out directory under {}: {e}",
                path.display()
            )
        })
    }

    pub fn verify(&self) -> Result<(), String> {
        let current = open_root(&self.key.path)
            .and_then(|root| Ok((id(&root)?, id(&root.open_dir_nofollow("out")?)?)));
        if current.ok() != Some((self.key.source, self.key.out)) {
            return Err("The accepted workspace or its out directory was replaced".into());
        }
        Ok(())
    }

    pub fn lock(&self) -> Result<Lease<'_>, String> {
        // The scheduler queues contending jobs locally. Other daemon instances
        // must not silently open the same output capability for writing either.
        self.verify()?;
        if unsafe { libc::flock(self.out.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "Cannot exclusively lock workspace out/: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Lease(&self.out))
    }

    pub fn collect(
        &self,
        patterns: &[String],
        staging: &mut Staging,
        max_files: u32,
        max_bytes: u64,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<String>, String> {
        let mut search = Search {
            staging,
            cancel,
            work: 1_000_000,
            seen: HashSet::new(),
            max_files,
            bytes_left: max_bytes,
            device: self.key.out.0,
        };
        let mut missing = Vec::new();
        for pattern in patterns {
            let parts: Vec<_> = pattern.split('/').collect();
            if !search
                .walk(&self.out, Path::new(""), &parts, 0)
                .map_err(|e| format!("collect /out/{pattern}: {e}"))?
            {
                missing.push(pattern.clone());
            }
        }
        Ok(missing)
    }
}

pub(super) struct Lease<'a>(&'a Dir);
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Walk only the output subtrees selected by the patterns. Every directory and
/// final file is opened relative to its pinned parent, with links refused.
struct Search<'a> {
    staging: &'a mut Staging,
    cancel: &'a dyn Fn() -> bool,
    work: usize,
    seen: HashSet<String>,
    max_files: u32,
    bytes_left: u64,
    device: u64,
}
impl Search<'_> {
    fn step(&mut self) -> io::Result<()> {
        if (self.cancel)() {
            return Err(io::Error::other("cancelled"));
        }
        self.work = self
            .work
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("artifact search exceeds the traversal limit"))?;
        Ok(())
    }
    fn walk(&mut self, dir: &Dir, prefix: &Path, parts: &[&str], depth: usize) -> io::Result<bool> {
        self.step()?;
        let Some((&first, rest)) = parts.split_first() else {
            return Ok(false);
        };
        if depth > 48 {
            return Ok(false);
        }
        // Match even literal components against directory entries. A host lookup
        // can accept spelling aliases which the Linux export does not expose.
        let literal = !first.contains(['*', '?']);
        let recursive = first == "**";
        let mut matched = recursive && self.walk(dir, prefix, rest, depth + 1)?;
        for entry in dir.entries()? {
            self.step()?;
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if literal {
                if name == first {
                    return self.entry(dir, prefix, name, rest, depth);
                }
            } else if recursive && entry.file_type()?.is_dir() {
                matched |= self.entry(dir, prefix, name, parts, depth)?;
            } else if (!recursive || rest.is_empty()) && protocol::component_matches(first, name) {
                matched |= self.entry(dir, prefix, name, rest, depth)?;
            }
        }
        Ok(matched)
    }
    fn entry(
        &mut self,
        dir: &Dir,
        prefix: &Path,
        name: &str,
        rest: &[&str],
        depth: usize,
    ) -> io::Result<bool> {
        let meta = match dir.symlink_metadata(name) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            result => result?,
        };
        if meta.dev() != self.device {
            return Ok(false);
        }
        let path = prefix.join(name);
        if !rest.is_empty() && meta.is_dir() {
            let child = dir.open_dir_nofollow(name)?;
            if id(&child)?.0 != self.device {
                return Ok(false);
            }
            return self.walk(&child, &path, rest, depth + 1);
        }
        if !rest.is_empty() || !meta.is_file() {
            return Ok(false);
        }
        let relative = path
            .to_str()
            .ok_or_else(|| io::Error::other("invalid output name"))?;
        if self.seen.contains(relative) {
            return Ok(true);
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .follow(FollowSymlinks::No)
            .custom_flags(libc::O_NONBLOCK);
        let file = dir.open_with(name, &options)?.into_std();
        let meta = file.metadata()?;
        if !meta.is_file() || meta.dev() != self.device {
            return Err(io::Error::other("output changed during collection"));
        }
        if self.seen.len() >= self.max_files as usize || meta.len() > self.bytes_left {
            return Err(io::Error::other("artifacts exceed the file or byte limit"));
        }
        self.staging
            .snapshot(relative, file, &meta, self.cancel)
            .map_err(io::Error::other)?;
        self.bytes_left -= meta.len();
        self.seen.insert(relative.to_owned());
        Ok(true)
    }
}
