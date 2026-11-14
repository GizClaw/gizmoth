//! Persistent registry checkout. Git only transports metadata; recipes still pass
//! the same bounded archive and schema validation before publication.
use super::{extract, parse_snapshot, require, valid_commit, BRANCH, MAX_ARCHIVE, R};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(120);
const MAX_OUTPUT: usize = 16 * 1024;

fn command(directory: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .args([
            "-c",
            "core.hooksPath=",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "credential.interactive=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // GUI launches can inherit Git's environment from a parent shell.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
    ] {
        command.env_remove(key);
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    #[cfg(unix)]
    command.process_group(0);
    command
}

// A Git pull owns fetch/transport subprocesses too. Kill the whole group on an
// error or dropped future before allowing another refresh into the checkout.
#[cfg(unix)]
struct ProcessTree(Option<i32>);
#[cfg(unix)]
impl ProcessTree {
    fn attach(child: &tokio::process::Child) -> R<Self> {
        Ok(Self(Some(child.id().ok_or("Git 进程已退出")? as i32)))
    }
    fn stop(&mut self) {
        if let Some(group) = self.0.take() {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
    fn finish(&mut self) {
        self.0 = None;
    }
}
#[cfg(unix)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
struct ProcessTree(std::os::windows::io::OwnedHandle);
#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &tokio::process::Child) -> R<Self> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows::Win32::{Foundation::HANDLE, System::JobObjects::*};
        let process = HANDLE(child.raw_handle().ok_or("Git 进程已退出")?);
        let create = || -> windows::core::Result<Self> {
            let handle = unsafe { CreateJobObjectW(None, None)? };
            let job = unsafe { OwnedHandle::from_raw_handle(handle.0) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of_val(&limits) as u32,
                )?;
                AssignProcessToJobObject(handle, process)?;
            }
            Ok(Self(job))
        };
        create().map_err(|e| format!("无法管理 Git 子进程：{e}"))
    }
    fn stop(&mut self) {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{Foundation::HANDLE, System::JobObjects::TerminateJobObject};
        unsafe {
            let _ = TerminateJobObject(HANDLE(self.0.as_raw_handle()), 1);
        }
    }
    fn finish(&mut self) {}
    // OwnedHandle closes the job on drop, terminating remaining descendants.
}

async fn read_limited(reader: impl AsyncRead + Unpin, limit: usize) -> R<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| format!("无法读取 Git 输出：{e}"))?;
    require(bytes.len() <= limit, || "Git 目录输出过大".into())?;
    Ok(bytes)
}

async fn output(command: &mut Command, limit: usize, timeout: Duration) -> R<Vec<u8>> {
    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "未找到 Git，请先安装 Git 后检查开发包更新".into()
        } else {
            format!("无法启动 Git：{e}")
        }
    })?;
    let mut tree = ProcessTree::attach(&child)?;
    let stdout = child.stdout.take().ok_or("无法读取 Git 输出")?;
    let stderr = child.stderr.take().ok_or("无法读取 Git 错误")?;
    let result = tokio::time::timeout(timeout, async {
        let (status, stdout, stderr) = tokio::try_join!(
            async { child.wait().await.map_err(|e| format!("Git 运行失败：{e}")) },
            read_limited(stdout, limit),
            read_limited(stderr, MAX_OUTPUT),
        )?;
        require(status.success(), || {
            format!(
                "Git 目录更新失败：{}",
                String::from_utf8_lossy(&stderr).trim()
            )
        })?;
        Ok(stdout)
    })
    .await;
    match result {
        Ok(Ok(bytes)) => {
            tree.finish();
            Ok(bytes)
        }
        result => {
            tree.stop();
            let _ = child.kill().await;
            match result {
                Ok(Err(error)) => Err(error),
                _ => Err("Git 目录更新超时，请稍后重试".into()),
            }
        }
    }
}

async fn git(directory: &Path, args: &[&OsStr], limit: usize) -> R<Vec<u8>> {
    output(command(directory).args(args), limit, TIMEOUT).await
}

async fn text(directory: &Path, args: &[&str]) -> R<String> {
    let args: Vec<_> = args.iter().map(OsStr::new).collect();
    String::from_utf8(git(directory, &args, MAX_OUTPUT).await?)
        .map(|s| s.trim().to_owned())
        .map_err(|_| "Git 目录输出不是 UTF-8".into())
}

async fn check_checkout(repository: &Path, remote: &str) -> R<()> {
    for path in [repository.to_path_buf(), repository.join(".git")] {
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|e| format!("无法读取本地 Git 目录：{e}"))?;
        require(metadata.is_dir() && !metadata.is_symlink(), || {
            "本地目录不是 GizMoth 管理的 Git 仓库".into()
        })?;
    }
    require(
        text(repository, &["remote", "get-url", "--all", "origin"]).await? == remote,
        || "本地 Git 目录来源不匹配".into(),
    )?;
    require(
        text(repository, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await? == BRANCH,
        || "本地 Git 目录分支不是 main，请先恢复分支后重试".into(),
    )?;
    require(
        text(
            repository,
            &["status", "--porcelain", "--untracked-files=normal"],
        )
        .await?
        .is_empty(),
        || "本地 Git 目录有修改，请先保存或撤销修改后重试".into(),
    )
}

/// Read committed objects, never a mixture of mutable working-tree files.
async fn read(repository: &Path) -> R<(String, BTreeMap<String, String>)> {
    let commit = text(repository, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    require(valid_commit(&commit), || "本地 Git 目录提交无效".into())?;
    let archive = git(
        repository,
        &[
            OsStr::new("archive"),
            OsStr::new("--format=tar.gz"),
            OsStr::new("--prefix=registry/"),
            OsStr::new(&commit),
        ],
        MAX_ARCHIVE,
    )
    .await?;
    tauri::async_runtime::spawn_blocking(move || {
        let files = extract(&archive)?;
        parse_snapshot(&commit, 0, &files)?;
        Ok((commit, files))
    })
    .await
    .map_err(|_| "解析本地 Git 目录失败".to_owned())?
}

/// Own only a uniquely allocated staging directory, including after cancellation.
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) async fn sync(repository: &Path, remote: &str) -> R<(String, BTreeMap<String, String>)> {
    match std::fs::symlink_metadata(repository) {
        Ok(_) => {
            check_checkout(repository, remote).await?;
            text(
                repository,
                &[
                    "pull",
                    "--ff-only",
                    "--no-rebase",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "origin",
                    BRANCH,
                ],
            )
            .await?;
            require(
                text(repository, &["rev-parse", "--verify", "HEAD^{commit}"]).await?
                    == text(
                        repository,
                        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
                    )
                    .await?,
                || "本地 Git 目录包含上游之外的提交，请先保存并恢复 main 后重试".into(),
            )?;
            read(repository).await
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = repository.parent().ok_or("本地 Git 目录路径无效")?;
            std::fs::create_dir_all(parent).map_err(|e| format!("无法创建本地 Git 目录：{e}"))?;
            let staging = Staging(parent.join(format!(".clone-{}", uuid::Uuid::new_v4())));
            std::fs::create_dir(&staging.0).map_err(|e| format!("无法创建本地 Git 目录：{e}"))?;
            git(
                parent,
                &[
                    OsStr::new("clone"),
                    OsStr::new("--depth=1"),
                    OsStr::new("--single-branch"),
                    OsStr::new("--branch"),
                    OsStr::new(BRANCH),
                    OsStr::new("--no-tags"),
                    OsStr::new("--"),
                    OsStr::new(remote),
                    staging.0.as_os_str(),
                ],
                MAX_OUTPUT,
            )
            .await?;
            check_checkout(&staging.0, remote).await?;
            let snapshot = read(&staging.0).await?;
            std::fs::rename(&staging.0, repository)
                .map_err(|e| format!("无法保存本地 Git 目录：{e}"))?;
            Ok(snapshot)
        }
        Err(e) => Err(format!("无法读取本地 Git 目录：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture(root: &Path) -> PathBuf {
        let remote = root.join("upstream");
        std::fs::create_dir(&remote).unwrap();
        text(&remote, &["init", "--initial-branch=main"])
            .await
            .unwrap();
        for (name, content) in super::super::tests::files() {
            let path = remote.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        commit(&remote, "Initial registry").await;
        remote
    }

    async fn commit(remote: &Path, message: &str) -> String {
        text(remote, &["add", "."]).await.unwrap();
        text(
            remote,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                message,
            ],
        )
        .await
        .unwrap();
        text(remote, &["rev-parse", "HEAD"]).await.unwrap()
    }

    #[tokio::test]
    async fn clones_then_pulls_incrementally_and_reads_offline() {
        let root = tempfile::tempdir().unwrap();
        let remote = fixture(root.path()).await;
        let repository = root.path().join("registry/repository");
        let remote_url = remote.to_str().unwrap();
        let (first, files) = sync(&repository, remote_url).await.unwrap();
        assert_eq!(files, super::super::tests::files());
        assert!(repository.join(".git").is_dir());
        // A custom local Git setting survives refresh, proving the clone is reused.
        text(&repository, &["config", "gizmoth.test", "retained"])
            .await
            .unwrap();
        std::fs::write(remote.join("README.md"), "Updated documentation").unwrap();
        let next = commit(&remote, "Update registry").await;
        assert_ne!(first, next);
        let (updated, _) = sync(&repository, remote_url).await.unwrap();
        assert_eq!(updated, next);
        assert_eq!(
            text(&repository, &["config", "gizmoth.test"])
                .await
                .unwrap(),
            "retained"
        );
        std::fs::rename(&remote, root.path().join("offline")).unwrap();
        assert!(sync(&repository, remote_url).await.is_err());
        assert_eq!(read(&repository).await.unwrap().0, next);
    }

    #[tokio::test]
    async fn refuses_dirty_wrong_origin_wrong_branch_and_diverged_checkouts() {
        let root = tempfile::tempdir().unwrap();
        let remote = fixture(root.path()).await;
        let repository = root.path().join("repository");
        let remote_url = remote.to_str().unwrap();
        let (first, _) = sync(&repository, remote_url).await.unwrap();
        let registry = repository.join("registry.json");
        let original = std::fs::read(&registry).unwrap();
        std::fs::write(&registry, "user edits").unwrap();
        assert!(sync(&repository, remote_url)
            .await
            .unwrap_err()
            .contains("有修改"));
        assert_eq!(std::fs::read_to_string(&registry).unwrap(), "user edits");
        std::fs::write(&registry, original).unwrap();
        assert!(sync(&repository, "https://example.com/foreign")
            .await
            .unwrap_err()
            .contains("来源不匹配"));
        text(&repository, &["checkout", "-b", "custom"])
            .await
            .unwrap();
        assert!(sync(&repository, remote_url)
            .await
            .unwrap_err()
            .contains("分支不是 main"));
        text(&repository, &["checkout", "main"]).await.unwrap();
        std::fs::write(repository.join("local.txt"), "local commit").unwrap();
        let local = commit(&repository, "Local commit").await;
        assert!(sync(&repository, remote_url)
            .await
            .unwrap_err()
            .contains("上游之外的提交"));
        assert_eq!(read(&repository).await.unwrap().0, local);
        std::fs::write(remote.join("upstream.txt"), "upstream commit").unwrap();
        commit(&remote, "Upstream commit").await;
        assert!(sync(&repository, remote_url).await.is_err());
        assert_eq!(read(&repository).await.unwrap().0, local);
        assert_ne!(first, local);
    }

    #[tokio::test]
    async fn invalid_or_failed_clone_does_not_publish_a_repository() {
        let root = tempfile::tempdir().unwrap();
        let remote = fixture(root.path()).await;
        std::fs::write(remote.join("registry.json"), "{}").unwrap();
        commit(&remote, "Invalid registry").await;
        let repository = root.path().join("registry/repository");
        assert!(sync(&repository, remote.to_str().unwrap()).await.is_err());
        assert!(!repository.exists());
        assert_eq!(
            std::fs::read_dir(repository.parent().unwrap())
                .unwrap()
                .count(),
            0
        );
        assert!(
            sync(&repository, root.path().join("missing").to_str().unwrap())
                .await
                .is_err()
        );
        assert!(!repository.exists());
        assert_eq!(
            std::fs::read_dir(repository.parent().unwrap())
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn invalid_update_is_refused_and_the_last_commit_stays_valid() {
        let root = tempfile::tempdir().unwrap();
        let remote = fixture(root.path()).await;
        let repository = root.path().join("repository");
        let (commit, files) = sync(&repository, remote.to_str().unwrap()).await.unwrap();
        std::fs::write(remote.join("registry.json"), "{}").unwrap();
        self::commit(&remote, "Invalid update").await;
        assert!(sync(&repository, remote.to_str().unwrap()).await.is_err());
        // The caller keeps publishing the last valid snapshot.
        assert_eq!(parse_snapshot(&commit, 1, &files).unwrap().commit, commit);
    }

    #[tokio::test]
    async fn command_deadline_and_output_limits_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        let remote = fixture(root.path()).await;
        let mut list = command(&remote);
        list.args(["ls-files"]);
        assert!(output(&mut list, 1, TIMEOUT)
            .await
            .unwrap_err()
            .contains("过大"));

        // A stalled HTTP request must close when the future times out or is
        // dropped. Killing only Git leaves its transport helper/socket alive.
        for cancel in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (ready, accepted) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                assert!(stream.read(&mut bytes).await.unwrap() > 0);
                ready.send(()).unwrap();
                match stream.read(&mut bytes).await {
                    Ok(0) => (),
                    // Windows reports a forcibly terminated transport as RST.
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => (),
                    result => panic!("Git transport remained open after termination: {result:?}"),
                }
            });
            let mut request = command(&remote);
            request.args(["ls-remote", &format!("http://{address}/registry")]);
            let pending =
                tokio::spawn(async move { output(&mut request, MAX_OUTPUT, TIMEOUT).await });
            tokio::time::timeout(Duration::from_secs(15), accepted)
                .await
                .unwrap()
                .unwrap();
            if cancel {
                pending.abort();
                assert!(pending.await.unwrap_err().is_cancelled());
            } else {
                // Windows Git startup can exceed a short wall-clock deadline.
                // Advance only after its real transport has reached the server,
                // then resume real time for OS process/socket teardown.
                tokio::time::pause();
                tokio::time::advance(TIMEOUT).await;
                tokio::time::resume();
                let error = pending.await.unwrap().unwrap_err();
                assert!(error.contains("超时"), "{error}");
            }
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
        }
    }
}
