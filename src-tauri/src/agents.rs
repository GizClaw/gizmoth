//! Explicit installation of GizMoth's personal skill for external coding agents.
//! Status is read-only. SKILL.md and its gizmoth directory must not be symlinks;
//! native skill-root ancestors may resolve through links and are pinned for I/O.
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use serde::Serialize;
use serde_json::json;
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};
use tauri::{AppHandle, Manager};

#[path = "agents_fs.rs"]
mod file_commit;

type R<T> = Result<T, String>;
const OWNERSHIP: &str = "<!-- Managed by GizMoth: gizmoth skill -->";
const DESCRIPTION: &str = "Use GizMoth to discover serial ports, identify connected devices, read logs, install firmware, reboot, request exclusive device communication through GizMoth WebSocket, or build firmware in GizMoth's Linux VMs with shared caches and artifacts.";
const MAX_SKILL: u64 = 16 * 1024 * 1024;
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Debug)]
pub struct Status {
    skill: String,
    targets: Vec<TargetStatus>,
}

#[derive(Serialize, Debug)]
struct TargetStatus {
    id: &'static str,
    name: &'static str,
    path: String,
    installed: bool,
    current: bool,
    error: Option<String>,
}

struct Target {
    id: &'static str,
    name: &'static str,
    path: PathBuf,
    /// An unusable location override: shown beside the target, never mutated.
    error: Option<String>,
}

/// Pi 1.5.2 `normalizeWindowsShellPath` (dist/utils/paths.js): Git Bash, MSYS,
/// Cygwin and WSL drive paths become native drive paths. UNC (`//`) paths and
/// paths already containing `\` are unchanged.
fn windows_shell_path(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        return None;
    }
    let tail = &path[1..];
    // Same order as the case-insensitive `/(?:mnt\/|cygdrive\/)?([a-z])(?:\/(.*))?$/`.
    ["mnt/", "cygdrive/", ""].into_iter().find_map(|prefix| {
        let rest = tail
            .get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &tail[prefix.len()..])?;
        let mut chars = rest.chars();
        let drive = chars.next().filter(char::is_ascii_alphabetic)?;
        let suffix = match chars.as_str() {
            "" => "",
            more => more.strip_prefix('/')?,
        };
        // JavaScript `.` does not match line terminators.
        if suffix.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
            return None;
        }
        Some(format!(
            "{}:\\{}",
            drive.to_ascii_uppercase(),
            suffix.replace('/', "\\")
        ))
    })
}

/// JavaScript `decodeURIComponent`: every `%` starts a hex escape and the
/// decoded bytes must be UTF-8; otherwise it throws.
fn decode_uri_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let escapes_valid = text.match_indices('%').all(|(index, _)| {
        bytes
            .get(index + 1..index + 3)
            .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
    });
    escapes_valid.then_some(())?;
    percent_encoding::percent_decode_str(text)
        .decode_utf8()
        .ok()
        .map(|text| text.into_owned())
}

/// Node's `fileURLToPath`, as Pi calls it. Parsing follows the same WHATWG URL
/// rules; the POSIX and win32 path conversions are reproduced for `windows`.
fn file_url_path(value: &str, windows: bool) -> R<String> {
    let url = url::Url::parse(value).map_err(|error| format!("invalid file URL: {error}"))?;
    let host = match url.host() {
        Some(url::Host::Domain(domain)) => url::quirks::domain_to_unicode(domain),
        Some(_) => url.host_str().unwrap_or_default().to_owned(),
        None => String::new(),
    };
    let mut path = url.path().to_owned();
    // WHATWG normalizes a leading `C|` drive segment to `C:`; url 2.5 keeps `|`.
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b'|'
        && matches!(bytes.get(3), None | Some(b'/'))
    {
        path.replace_range(2..3, ":");
    }
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2f") || (windows && lower.contains("%5c")) {
        return Err("file URL path must not include encoded path separators".into());
    }
    if windows {
        path = path.replace('/', "\\");
    }
    let path = decode_uri_component(&path).ok_or("file URL contains malformed percent-encoding")?;
    if path.contains('\0') {
        return Err("file URL path contains a NUL character".into());
    }
    if !windows {
        return if host.is_empty() {
            Ok(path)
        } else {
            Err("file URL host must be empty or localhost".into())
        };
    }
    if !host.is_empty() {
        return Ok(format!("\\\\{host}{path}"));
    }
    let mut chars = path.chars().skip(1);
    if chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':') {
        Ok(path[1..].to_owned())
    } else {
        Err("file URL path must be absolute".into())
    }
}

/// The directory Pi 1.5.2 `getAgentDir()` derives from a nonempty
/// PI_CODING_AGENT_DIR through `normalizePath()`. `windows` selects Pi's win32
/// branches. Tilde joins are normalized by `resolve_like_node`, as Node's
/// `path.join` keeps home even when the remainder starts with a separator.
fn pi_agent_dir(value: &str, home: &OsStr, windows: bool) -> R<OsString> {
    let shell = windows.then(|| windows_shell_path(value)).flatten();
    let value = shell.as_deref().unwrap_or(value);
    if value == "~" {
        return Ok(home.to_owned());
    }
    let rest = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\").filter(|_| windows));
    if let Some(rest) = rest {
        let mut joined = home.to_owned();
        joined.push(if windows { "\\" } else { "/" });
        joined.push(rest);
        return Ok(joined);
    }
    if value.starts_with("file://") {
        return file_url_path(value, windows).map(OsString::from);
    }
    Ok(value.into())
}

/// Pi resolves the agent directory with Node's `path.resolve`, which folds
/// `.` and `..` lexically. Windows `absolute` already does so; Unix keeps `..`.
fn resolve_like_node(path: PathBuf) -> PathBuf {
    let path = std::path::absolute(&path).unwrap_or(path);
    if cfg!(windows) {
        return path;
    }
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

fn pi_target(home: &Path, value: Option<OsString>) -> Target {
    let skill = "skills/gizmoth/SKILL.md";
    let target = |path: PathBuf, error| Target {
        id: "pi",
        name: "Pi",
        path: path.join(skill),
        error,
    };
    // Like Pi, an empty value selects the default directory.
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return target(home.join(".pi/agent"), None);
    };
    let directory = value
        .to_str()
        .ok_or_else(|| "value is not valid Unicode".to_owned())
        .and_then(|text| pi_agent_dir(text, home.as_os_str(), cfg!(windows)));
    match directory {
        Ok(directory) => target(resolve_like_node(directory.into()), None),
        Err(reason) => target(
            PathBuf::from(value),
            Some(format!(
                "PI_CODING_AGENT_DIR cannot be used: {reason}. Correct it in GizMoth's environment and restart GizMoth."
            )),
        ),
    }
}

fn targets(home: &Path, env: impl Fn(&str) -> Option<OsString>) -> Vec<Target> {
    let override_dir = |key: &str, fallback: PathBuf| {
        env(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|path| std::path::absolute(&path).unwrap_or(path))
            .unwrap_or(fallback)
    };
    vec![
        Target {
            id: "claude-code",
            name: "Claude Code",
            path: override_dir("CLAUDE_CONFIG_DIR", home.join(".claude"))
                .join("skills/gizmoth/SKILL.md"),
            error: None,
        },
        Target {
            id: "codex",
            name: "Codex",
            path: home.join(".agents/skills/gizmoth/SKILL.md"),
            error: None,
        },
        Target {
            id: "gemini-cli",
            name: "Gemini CLI",
            path: override_dir("GEMINI_CLI_HOME", home.to_path_buf())
                .join(".gemini/skills/gizmoth/SKILL.md"),
            error: None,
        },
        Target {
            id: "opencode",
            name: "OpenCode",
            path: override_dir("XDG_CONFIG_HOME", home.join(".config"))
                .join("opencode/skills/gizmoth/SKILL.md"),
            error: None,
        },
        pi_target(home, env("PI_CODING_AGENT_DIR")),
    ]
}

#[derive(Clone, Copy)]
enum Shell {
    Posix,
    PowerShell,
}

fn quote(text: &str, shell: Shell) -> String {
    match shell {
        Shell::Posix => format!("'{}'", text.replace('\'', "'\"'\"'")),
        Shell::PowerShell => {
            let escaped: String = text
                .chars()
                .flat_map(|c| {
                    let quote =
                        matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}');
                    std::iter::once(c).chain(quote.then_some(c))
                })
                .collect();
            format!("'{escaped}'")
        }
    }
}

fn skill(executable: &Path, data_dir: &Path, shell: Shell) -> R<String> {
    if !executable.is_absolute() || !data_dir.is_absolute() {
        return Err("GizMoth executable and data directory must be absolute paths".into());
    }
    let executable = executable.to_str().ok_or("Executable path is not UTF-8")?;
    let data_dir = data_dir.to_str().ok_or("Data directory is not UTF-8")?;
    if executable
        .chars()
        .chain(data_dir.chars())
        .any(char::is_control)
    {
        return Err("GizMoth paths must not contain control characters".into());
    }
    let prefix = format!(
        "{}{} --data-dir {}",
        if matches!(shell, Shell::PowerShell) {
            "& "
        } else {
            ""
        },
        quote(executable, shell),
        quote(data_dir, shell),
    );
    let invocation = |arguments: &str| {
        let line = format!("{prefix} {arguments}");
        if matches!(shell, Shell::PowerShell) {
            // Windows PowerShell 5 and PowerShell 7's Legacy mode both use
            // native command-line quoting. Scope the setting to this invocation.
            // A downstream pipeline command makes PowerShell wait even for the
            // Windows GUI-subsystem executable. Fail without exiting the host.
            format!("& {{ $PSNativeCommandArgumentPassing = 'Legacy'; {line} | Out-String -Stream; if ($LASTEXITCODE -ne 0) {{ $ErrorActionPreference = 'Stop'; throw \"GizMoth exited with code $LASTEXITCODE\" }} }}")
        } else {
            line
        }
    };
    let command = |name: &str, args: serde_json::Value| {
        let json = args.to_string();
        let json = if matches!(shell, Shell::PowerShell) {
            json.replace('"', "\\\"")
        } else {
            json
        };
        invocation(&format!("--tool {name} {}", quote(&json, shell)))
    };
    // All examples are shell-quoted for this platform, including paths and JSON.
    // Credentials are resolved by the native transport and never embedded here.
    Ok(format!(
        r#"---
name: gizmoth
description: {description}
---
{OWNERSHIP}

## GizMoth device queries

Use GizMoth to inspect devices and communicate through its resident service. Commands below use {shell_name}; replace examples with exact returned IDs and real paths. Each invocation prints JSON; errors go to stderr with a nonzero exit code. CLI/MCP tools are queries only. List the current tools, arguments and descriptions, then inspect ports and recorded logs:

```{language}
{list_tools}
{status}
{system_ports}
{list}
{port_status}
{logs}
```

Queries do not start a stopped service. Its empty discovery result does not prove no devices are attached. Use GizMoth's app to add/configure/connect ports or enable Service. serial_list lists configured ports; serial_system_ports lists the running service's discovered paths and USB metadata. USB vendor/product IDs or a USB serial number identify an interface, not the attached board or firmware. Never guess an unknown device's identity. Prefer exact port IDs or paths; names may be ambiguous.

serial_status returns connection, activity, current/recent jobs, and cached H2Loader details in h2loader with timestamp h2loaderAt (Unix milliseconds). Check connection, present, detected and h2loaderAt: cached details can be stale or remain after disconnection. Fresh H2Loader status requires the refresh command on a granted WebSocket. h2loader.commands reports device-supported operations; do not assume every board supports everything.

The first serial_logs call returns a tail. Follow it by passing next as after AND the same instanceId and generation. If reset is true, discard the previous cursor and adopt the returned identity and next; truncated means some lines were omitted. Substitute actual returned values:

```{language}
{logs_next}
```

## Discover, connect and wait

GizMoth alone opens and manages the physical serial port and runs the H2Loader protocol. Never open a device path with a serial library, terminal, flashing utility or another process, or implement H2Loader in the client. Status, log and job queries need no ownership grant. All device-changing operations use one granted WebSocket.

Discover the service WebSocket:

```{language}
{pipe}
```

serial_pipe requires client and title (a brief task name, up to 120 characters); reason is optional additional context. The device list shows both the client and task title, including while the device is offline. serial_pipe is read-only: it returns url, headers, protocol: gizmoth.device.v2 and commands (name, description, inputSchema). Obtaining the URL does not request ownership. Keep credentials private and use the authentication headers, never URL tokens. Use a WebSocket client that supports headers.

Connecting to /v1/ports/PORT_ID/pipe?client=&title= IS the take request. Keep that socket open. Automatic approval defaults to true: existing work finishes, then requests receive FIFO grants without a manual mode switch. With autoGrant off, the user approves in GizMoth. Waiting sockets receive {{"type":"queued","state":"pending","position":1,"holder":null}} and remain connected. Wait until {{"type":"granted"}} on that same socket before commands. A premature command receives a result error with state: pending; keep waiting without reconnecting or losing your queue position. Stop on rejected/revoked or an ended session. A shared client name or API key never shares a grant between connections.

## Commands on the granted socket

Send JSON text frames with a unique request id, method and params. Follow commands[].inputSchema from discovery. These are WebSocket frames, not CLI/MCP calls; send only operations authorized by the user's task:

```json
{send_frame}
{refresh_frame}
{install_frame}
{reboot_frame}
{cancel_frame}
```

send is for plain-serial text; lineEnding is lf, crlf, cr or none. install, reboot and refresh execute GizMoth's native H2Loader operations and return jobs. cancel takes the UUID of this socket's job and requests cancellation at a safe I/O boundary. Never send binary client frames or unstructured raw text; they are rejected. Binary server frames contain device logs.

Each command receives {{"type":"result","id":"request-id","ok":true,"result":{{...}}}} or {{"type":"result","id":"request-id","ok":false,"error":{{"code":...,"message":"..."}}}}. Match replies by id. Job progress and completion arrive as {{"type":"job","job":{{...}}}} on that same socket. Keep receiving until the returned job.id reaches a terminal job.state; accepted is not completed. Send refresh or cancel through this socket too. UI sends and other connections cannot run device operations under its grant; cached queries remain available.

Physical device loss, unplugging, re-enumeration and H2Loader reboot do not release the WebSocket or its grant. Keep the same connection open and let the agent's task decide whether to wait or close. Device events have type: device, sequence, at, state (disconnected/connecting/reconnecting/connected), connection, ready and reason. A granted socket stays granted while ready is false; never wait for a second granted event after recovery. Physical commands fail with 503 while unavailable and are not buffered or replayed; cancellation of that socket's existing job remains available. Wait for a device event with ready:true before submitting a fresh command ID, and inspect the prior job before retrying ambiguous work. If missedEvents is present, some availability transitions were missed and the event resynchronizes the current state. The user can force-disconnect one holder or waiter in GizMoth. Port removal, server shutdown, revoked authorization or a failed/slow socket can still end a session.

Firmware installation and reboot change a device. Follow the user's authorization for the exact target/board, compatible package and intended reboot; existing explicit authorization applies. Installation accepts an absolute .update.tar.zlib path on the service computer over an actual local connection, or a GizMoth build artifact ID. LAN path installs are denied even with a valid API key. stageOnly transfers without rebooting; normal install transfers, reboots to upgrade and verifies reconnection. Optional client-generated jobId UUIDs on install/reboot/refresh help recover an ambiguous response. Preserve request/job IDs; after a lost response, inspect serial_status and recent history before retrying to avoid duplicate execution. You may query jobs without a grant:

```{language}
{job}
```

serial_job waitSeconds is bounded at 120 and can return while still running. Check job.id and job.state and report failures or cancellation honestly.

Closing the WebSocket IS release; there is no separate take/release tool or API. Close after the intended work finishes. Disconnecting cancels session-owned work at safe I/O boundaries, and the next queued socket cannot receive granted until completion/cancellation settles. After the automatic queue drains, GizMoth control resumes. GizMoth retains the physical serial port throughout.
{compile}"#,
        description = serde_json::to_string(DESCRIPTION).map_err(|error| error.to_string())?,
        shell_name = if matches!(shell, Shell::PowerShell) {
            "Windows PowerShell"
        } else {
            "a POSIX shell"
        },
        language = if matches!(shell, Shell::PowerShell) {
            "powershell"
        } else {
            "sh"
        },
        list_tools = invocation("--list-tools"),
        status = command("gizmoth_status", json!({})),
        system_ports = command("serial_system_ports", json!({})),
        list = command("serial_list", json!({})),
        port_status = command("serial_status", json!({"port":"PORT_ID"})),
        logs = command("serial_logs", json!({"port":"PORT_ID","limit":100})),
        logs_next = command(
            "serial_logs",
            json!({"port":"PORT_ID","after":42,"instanceId":"RETURNED_INSTANCE_ID","generation":0,"limit":100})
        ),
        pipe = command(
            "serial_pipe",
            json!({"port":"PORT_ID","client":"coding-agent","title":"Inspect device and refresh status"})
        ),
        send_frame =
            json!({"id":"send-1","method":"send","params":{"text":"help","lineEnding":"lf"}}),
        refresh_frame = json!({"id":"refresh-1","method":"refresh","params":{}}),
        install_frame = json!({"id":"install-1","method":"install","params":{
            "package": if matches!(shell, Shell::PowerShell) { r"C:\firmware\device.update.tar.zlib" } else { "/absolute/path/device.update.tar.zlib" },
            "jobId":"e6c8988a-00a7-4f75-b4df-926194810d70"
        }}),
        reboot_frame = json!({"id":"reboot-1","method":"reboot","params":{"target":"app"}}),
        cancel_frame = json!({"id":"cancel-1","method":"cancel","params":{"jobId":"e6c8988a-00a7-4f75-b4df-926194810d70"}}),
        job = command("serial_job", json!({"port":"PORT_ID","waitSeconds":30})),
        compile = if matches!(shell, Shell::PowerShell) {
            String::new()
        } else {
            compile_section(&command)
        },
    ))
}

/// Linux build VMs are available on macOS (Apple silicon) services only.
fn compile_section(command: &dyn Fn(&str, serde_json::Value) -> String) -> String {
    let build = json!({"id":"build-1","method":"build","params":{
        "workspace":"/absolute/path/project",
        "command":["bazel","build","//app:firmware"],
        "artifacts":["bazel-bin/app/*.update.tar.zlib"]
    }});
    let install = json!({"id":"install-2","method":"install","params":{"artifact":"ARTIFACT_ID"}});
    format!(
        r#"
## Builds in GizMoth's Linux VMs

On macOS with Apple silicon, GizMoth runs builds in Linux VMs (Ubuntu, arm64) with a shared Bazel remote cache and ccache, and keeps their outputs as shared artifacts. Prefer it over host builds for Linux-only toolchains. Check slots, the queue, caches and existing artifacts:

```sh
{status}
{artifacts}
```

Discover the compile WebSocket, then connect with the returned headers and send exactly one build request on it:

```sh
{pipe}
```

```json
{build}
```

workspace is an absolute project directory on this computer. It is synchronized incrementally into the slot's VM; top-level bazel-* links are skipped and exclude takes extra glob patterns. command is an argv run as user builder in the workspace (cwd selects a subdirectory); use ["bash","-lc","..."] for shell syntax. artifacts are relative glob patterns (*, ?, **) collected after success. The socket receives JSON frames queued (position), status, started, synced, exit, artifact and finished; binary frames carry the build log. Keep the socket open until finished: closing it cancels the build, including while queued. Open one socket per build; GizMoth runs as many builds in parallel as it has slots and queues the rest. The first build may spend a few minutes preparing the Linux system image.

Artifacts are shared by every agent: compile_artifacts lists artifact IDs, the source job and each file's absolutePath. To flash one, send install with the artifact ID on a granted device WebSocket instead of a path; this also works over LAN. Add file when the artifact holds several .update.tar.zlib files. Read a build's status and log afterwards with compile_job:

```json
{install}
```

```sh
{job}
```
"#,
        status = command("compile_status", json!({})),
        artifacts = command("compile_artifacts", json!({"limit":10})),
        pipe = command(
            "compile_pipe",
            json!({"client":"coding-agent","title":"Build firmware"})
        ),
        job = command("compile_job", json!({"job":"JOB_ID"})),
    )
}

/// Only our canonical frontmatter and exact ownership comment establish
/// ownership. A same-name third-party skill is a collision, never an update.
/// Older owned bodies/descriptions remain installed and can be refreshed.
fn require_owned(bytes: &[u8]) -> R<()> {
    let collision = "Existing SKILL.md is not an intact GizMoth-managed skill; move it aside or restore it before retrying";
    let text = std::str::from_utf8(bytes).map_err(|_| collision)?;
    let normalized = text.replace("\r\n", "\n");
    let mut lines = normalized.lines();
    if lines.next() != Some("---") || lines.next() != Some("name: gizmoth") {
        return Err(collision.into());
    }
    let description = lines
        .next()
        .and_then(|line| line.strip_prefix("description: "))
        .and_then(|value| serde_json::from_str::<String>(value).ok());
    if !description.is_some_and(|value| !value.trim().is_empty())
        || lines.next() != Some("---")
        || lines.next() != Some(OWNERSHIP)
        || normalized.matches(OWNERSHIP).count() != 1
        || normalized.matches("<!-- Managed by GizMoth").count() != 1
    {
        return Err(collision.into());
    }
    Ok(())
}

/// Follow native root ancestors intentionally, then reject links at the actual
/// gizmoth skill directory. Every file operation uses the pinned directory.
fn skill_directory(target: &Target, create: bool) -> R<Option<(Dir, Dir)>> {
    let directory = target.path.parent().ok_or("Skill has no directory")?;
    let root_path = directory.parent().ok_or("Skill has no root")?;
    if create {
        fs::create_dir_all(root_path).map_err(|error| error.to_string())?;
    }
    let root = match Dir::open_ambient_dir(root_path, cap_std::ambient_authority()) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if create {
        match root.create_dir("gizmoth") {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    match root.symlink_metadata("gizmoth") {
        Ok(metadata) if !metadata.is_dir() => {
            return Err(
                "GizMoth skill directory must be a directory; symlinks are not supported".into(),
            );
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        _ => {}
    }
    let directory = root
        .open_dir_nofollow("gizmoth")
        .map_err(|error| error.to_string())?;
    Ok(Some((root, directory)))
}

struct Snapshot {
    bytes: Vec<u8>,
    metadata: fs::Metadata,
}

fn read_skill(parent: &Dir, name: &std::ffi::OsStr) -> R<Option<Snapshot>> {
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(meta) if !meta.is_file() => {
            return Err("SKILL.md must be a regular file; symlinks are not supported".into())
        }
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_fs_ext::OpenOptionsSyncExt;
        options.nonblock(true);
    }
    let mut file = parent
        .open_with(name, &options)
        .map_err(|error| error.to_string())?
        .into_std();
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_SKILL {
        return Err("SKILL.md is not a regular file or exceeds 16 MiB".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_SKILL + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_SKILL {
        return Err("SKILL.md exceeds 16 MiB".into());
    }
    Ok(Some(Snapshot { bytes, metadata }))
}

fn unchanged(before: Option<&Snapshot>, after: Option<&Snapshot>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if before.metadata.dev() != after.metadata.dev()
                    || before.metadata.ino() != after.metadata.ino()
                    || before.metadata.mode() != after.metadata.mode()
                {
                    return false;
                }
            }
            before.bytes == after.bytes
                && before.metadata.modified().ok() == after.metadata.modified().ok()
                && before.metadata.permissions().readonly()
                    == after.metadata.permissions().readonly()
        }
        _ => false,
    }
}

fn private_child(parent: &Dir, name: &str) -> R<Dir> {
    let builder = cap_std::fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use cap_std::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    match parent.create_dir_with(name, &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let child = parent
        .open_dir_nofollow(name)
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        if child
            .dir_metadata()
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("Agent backup directory must be private (mode 0700)".into());
        }
    }
    Ok(child)
}

fn private_create_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn backup(data_dir: &Path, id: &str, bytes: &[u8]) -> R<()> {
    fs::create_dir_all(data_dir).map_err(|error| error.to_string())?;
    let root = Dir::open_ambient_dir(data_dir, cap_std::ambient_authority())
        .map_err(|error| error.to_string())?;
    let root = private_child(&root, "agents")?;
    let root = private_child(&root, "backups")?;
    let root = private_child(&root, id)?;
    let mut file = root
        .open_with(
            format!("{}.bak", uuid::Uuid::new_v4()),
            &private_create_options(),
        )
        .map_err(|error| error.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())
}

fn verify_snapshot(parent: &Dir, original: Option<&Snapshot>) -> R<()> {
    if unchanged(
        original,
        read_skill(parent, std::ffi::OsStr::new("SKILL.md"))?.as_ref(),
    ) {
        Ok(())
    } else {
        Err("GizMoth skill changed during the update; retry to preserve those edits".into())
    }
}

/// Retain the actual displaced inode on the same filesystem. An editor with an
/// already-open descriptor can finish writing without losing its recovery copy.
fn commit_existing(
    parent: &Dir,
    recovery: &Dir,
    original: &Snapshot,
    staged: Option<&str>,
    before_publish: impl FnOnce(),
) -> R<()> {
    let name = std::ffi::OsStr::new("SKILL.md");
    let retained = format!("{}.bak", uuid::Uuid::new_v4());
    file_commit::rename_new(parent, name, recovery, retained.as_ref())
        .map_err(|error| error.to_string())?;
    let restore = || {
        if file_commit::rename_new(recovery, retained.as_ref(), parent, name).is_ok() {
            "The displaced file was restored.".to_owned()
        } else {
            format!("The displaced file is retained in .gizmoth-recovery/{retained} beside the skill directories.")
        }
    };
    match read_skill(recovery, retained.as_ref()) {
        Ok(captured) if unchanged(Some(original), captured.as_ref()) => {}
        _ => {
            return Err(format!(
                "GizMoth skill changed during the update. {}",
                restore()
            ))
        }
    }
    before_publish();
    if let Some(staged) = staged {
        if let Err(error) = file_commit::rename_new(parent, staged.as_ref(), parent, name) {
            return Err(format!(
                "Unable to publish GizMoth skill: {error}. {}",
                restore()
            ));
        }
    } else {
        match parent.symlink_metadata(name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => {
                return Err(format!(
                    "Another Skill appeared during uninstall. {}",
                    restore()
                ))
            }
        }
    }
    // Never unlink the retained inode: a non-cooperating editor can still hold
    // it open and write after validation. The private recovery directory owns it.
    Ok(())
}

fn set_file(target: &Target, data_dir: &Path, skill: &str, installed: bool) -> R<()> {
    if let Some(error) = &target.error {
        return Err(error.clone());
    }
    let _guard = WRITE_LOCK.lock().map_err(|_| "Agent skill is busy")?;
    let Some((root, parent)) = skill_directory(target, installed)? else {
        return Ok(());
    };
    let name = std::ffi::OsStr::new("SKILL.md");
    let original = read_skill(&parent, name)?;
    if let Some(original) = &original {
        require_owned(&original.bytes)?;
        if installed && original.bytes == skill.as_bytes() {
            return Ok(());
        }
    } else if !installed {
        // A missing uninstall never creates a lock, backup or directory.
        return Ok(());
    }
    // The lock lives in the skill root, allowing an empty gizmoth directory to
    // be pruned without unlinking a lock used by another GizMoth instance.
    let mut lock_options = OpenOptions::new();
    lock_options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_fs_ext::OpenOptionsSyncExt;
        lock_options.nonblock(true);
    }
    let lock = root
        .open_with(".gizmoth-agents.lock", &lock_options)
        .map_err(|error| error.to_string())?
        .into_std();
    if !lock
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("Agent skill lock is not a regular file".into());
    }
    lock.try_lock()
        .map_err(|error| format!("Agent skill is busy: {error}"))?;
    verify_snapshot(&parent, original.as_ref())?;
    if original
        .as_ref()
        .is_some_and(|snapshot| snapshot.metadata.permissions().readonly())
    {
        return Err("Agent skill is read-only".into());
    }
    if let Some(original) = &original {
        backup(data_dir, target.id, &original.bytes)
            .map_err(|error| format!("Unable to back up agent skill: {error}"))?;
    }
    if !installed {
        let recovery = private_child(&root, ".gizmoth-recovery")?;
        commit_existing(
            &parent,
            &recovery,
            original.as_ref().ok_or("Skill is missing")?,
            None,
            || {},
        )?;
        drop(parent);
        // remove_dir is deliberately non-recursive: scripts, references and
        // any other sibling assets always survive uninstall.
        let _ = root.remove_dir("gizmoth");
        return Ok(());
    }
    let temporary = format!(".gizmoth-skill-{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut file = parent
            .open_with(&temporary, &private_create_options())
            .map_err(|error| error.to_string())?
            .into_std();
        file.write_all(skill.as_bytes())
            .map_err(|error| error.to_string())?;
        if let Some(original) = &original {
            file.set_permissions(original.metadata.permissions())
                .map_err(|error| error.to_string())?;
        }
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        verify_snapshot(&parent, original.as_ref())?;
        if let Some(original) = &original {
            let recovery = private_child(&root, ".gizmoth-recovery")?;
            commit_existing(&parent, &recovery, original, Some(&temporary), || {})?;
        } else {
            file_commit::rename_new(&parent, temporary.as_ref(), &parent, name)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    let _ = parent.remove_file(&temporary);
    result
}

fn target_status(target: &Target, skill: &str) -> TargetStatus {
    let mut status = TargetStatus {
        id: target.id,
        name: target.name,
        path: target.path.to_string_lossy().into_owned(),
        installed: false,
        current: false,
        error: target.error.clone(),
    };
    if status.error.is_some() {
        return status;
    }
    let mut inspect = || -> R<()> {
        let Some((_, parent)) = skill_directory(target, false)? else {
            return Ok(());
        };
        if let Some(snapshot) = read_skill(&parent, std::ffi::OsStr::new("SKILL.md"))? {
            require_owned(&snapshot.bytes)?;
            status.installed = true;
            status.current = snapshot.bytes == skill.as_bytes();
        }
        Ok(())
    };
    if let Err(error) = inspect() {
        status.error = Some(error);
    }
    status
}

fn context(app: &AppHandle) -> R<(Vec<Target>, PathBuf, String)> {
    let home = app.path().home_dir().map_err(|error| error.to_string())?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let shell = if cfg!(windows) {
        Shell::PowerShell
    } else {
        Shell::Posix
    };
    let skill = skill(&executable, &data_dir, shell)?;
    Ok((targets(&home, |key| std::env::var_os(key)), data_dir, skill))
}

fn status_with(targets: &[Target], skill: String) -> Status {
    let statuses = targets
        .iter()
        .map(|target| target_status(target, &skill))
        .collect();
    Status {
        skill,
        targets: statuses,
    }
}

pub fn status(app: &AppHandle) -> R<Status> {
    let (targets, _, skill) = context(app)?;
    Ok(status_with(&targets, skill))
}

pub fn set(app: &AppHandle, id: &str, installed: bool) -> R<Status> {
    let (targets, data_dir, skill) = context(app)?;
    let target = targets
        .iter()
        .find(|target| target.id == id)
        .ok_or("Unknown agent target")?;
    // Return mutation errors beside the target with the full refreshed status.
    let error = set_file(target, &data_dir, &skill, installed).err();
    let mut status = status_with(&targets, skill);
    if let Some(error) = error {
        status
            .targets
            .iter_mut()
            .find(|target| target.id == id)
            .unwrap()
            .error = Some(error);
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(root: &Path) -> Target {
        Target {
            id: "codex",
            name: "Codex",
            path: root.join("skills/gizmoth/SKILL.md"),
            error: None,
        }
    }

    fn text(root: &Path, executable: &str) -> String {
        skill(&root.join(executable), &root.join("data"), Shell::Posix).unwrap()
    }

    #[test]
    fn frontmatter_is_first_and_has_standard_name_description_and_ownership() {
        let root = tempfile::tempdir().unwrap();
        let skill = text(root.path(), "gizmoth");
        let lines = skill.lines().take(6).collect::<Vec<_>>();
        assert_eq!(&lines[..2], ["---", "name: gizmoth"]);
        assert_eq!(
            serde_json::from_str::<String>(lines[2].strip_prefix("description: ").unwrap())
                .unwrap(),
            DESCRIPTION
        );
        assert_eq!(&lines[3..5], ["---", OWNERSHIP]);
        require_owned(skill.as_bytes()).unwrap();
        require_owned(skill.replace('\n', "\r\n").as_bytes()).unwrap();
        let status = status_with(&targets(root.path(), |_| None), skill);
        let value = serde_json::to_value(status).unwrap();
        assert!(value["skill"].as_str().unwrap().starts_with("---\n"));
        assert!(value.get("prompt").is_none());
    }

    #[test]
    fn unowned_or_malformed_skills_are_never_changed_or_removed() {
        let root = tempfile::tempdir().unwrap();
        let target = target(root.path());
        let valid = text(root.path(), "gizmoth");
        fs::create_dir_all(target.path.parent().unwrap()).unwrap();
        for original in [
            String::new(),
            "third party instructions".into(),
            valid.replace(OWNERSHIP, ""),
            valid.replace(OWNERSHIP, "<!-- Managed by GizMoth: broken -->"),
            valid.replacen("name: gizmoth", "name: other", 1),
            valid.replacen("---\n", "", 1),
            format!("\u{feff}{valid}"),
            format!("{valid}\n{OWNERSHIP}"),
            valid.replace("description: ", "description: \"unterminated"),
        ] {
            fs::write(&target.path, &original).unwrap();
            for installed in [true, false] {
                assert!(set_file(&target, &root.path().join("data"), &valid, installed).is_err());
                assert_eq!(fs::read(&target.path).unwrap(), original.as_bytes());
            }
            let status = target_status(&target, &valid);
            assert!(target.path.exists());
            assert!(!status.installed && !status.current && status.error.is_some());
        }
        assert!(!root.path().join("data").exists());
        assert!(!root.path().join("skills/.gizmoth-agents.lock").exists());
    }

    #[test]
    fn status_and_missing_uninstall_are_read_only() {
        let root = tempfile::tempdir().unwrap();
        let target = target(root.path());
        let data = root.path().join("data");
        let skill = text(root.path(), "gizmoth");
        let missing = target_status(&target, &skill);
        assert!(!target.path.exists());
        assert!(!missing.installed && !missing.current && missing.error.is_none());
        set_file(&target, &data, &skill, false).unwrap();
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        fs::create_dir_all(target.path.parent().unwrap()).unwrap();
        set_file(&target, &data, &skill, false).unwrap();
        assert_eq!(fs::read_dir(root.path().join("skills")).unwrap().count(), 1);
        assert_eq!(
            fs::read_dir(target.path.parent().unwrap()).unwrap().count(),
            0
        );
        assert!(!data.exists());
    }

    #[test]
    fn all_targets_install_update_and_remove_preserving_globals_and_assets() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let old = text(root.path(), "old/gizmoth");
        let new = text(root.path(), "new/gizmoth");
        let globals = [
            ".claude/CLAUDE.md",
            ".codex/AGENTS.md",
            ".gemini/GEMINI.md",
            ".config/opencode/AGENTS.md",
            ".pi/agent/AGENTS.md",
        ];
        for path in globals {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"\xef\xbb\xbfuser rules\r\n").unwrap();
        }
        for target in targets(root.path(), |_| None) {
            set_file(&target, &data, &old, true).unwrap();
            assert!(target_status(&target, &old).current);
            let stale = target_status(&target, &new);
            assert!(stale.installed && !stale.current);
            let asset = target.path.parent().unwrap().join("references/notes.txt");
            fs::create_dir_all(asset.parent().unwrap()).unwrap();
            fs::write(&asset, b"user asset").unwrap();
            set_file(&target, &data, &new, true).unwrap();
            let backups = data.join("agents/backups").join(target.id);
            let files = fs::read_dir(&backups)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(fs::read(files[0].path()).unwrap(), old.as_bytes());
            set_file(&target, &data, &new, true).unwrap();
            assert_eq!(fs::read_dir(&backups).unwrap().count(), 1);
            assert!(target_status(&target, &new).current);
            set_file(&target, &data, &new, false).unwrap();
            assert!(!target.path.exists());
            assert!(target.path.parent().unwrap().exists());
            assert_eq!(fs::read(&asset).unwrap(), b"user asset");
            assert_eq!(fs::read_dir(&backups).unwrap().count(), 2);
            fs::remove_file(&asset).unwrap();
            fs::remove_dir(asset.parent().unwrap()).unwrap();
            set_file(&target, &data, &new, true).unwrap();
            set_file(&target, &data, &new, false).unwrap();
            assert!(!target.path.parent().unwrap().exists());
        }
        for path in globals {
            assert_eq!(
                fs::read(root.path().join(path)).unwrap(),
                b"\xef\xbb\xbfuser rules\r\n"
            );
        }
    }

    #[test]
    fn native_skill_targets_honor_only_documented_overrides() {
        let root = tempfile::tempdir().unwrap();
        let paths = targets(root.path(), |key| {
            Some(root.path().join(key).into_os_string())
        });
        assert_eq!(
            paths[0].path,
            root.path()
                .join("CLAUDE_CONFIG_DIR/skills/gizmoth/SKILL.md")
        );
        assert_eq!(
            paths[1].path,
            root.path().join(".agents/skills/gizmoth/SKILL.md")
        );
        assert_eq!(
            paths[2].path,
            root.path()
                .join("GEMINI_CLI_HOME/.gemini/skills/gizmoth/SKILL.md")
        );
        assert_eq!(
            paths[3].path,
            root.path()
                .join("XDG_CONFIG_HOME/opencode/skills/gizmoth/SKILL.md")
        );
        assert_eq!(
            paths[4].path,
            root.path()
                .join("PI_CODING_AGENT_DIR/skills/gizmoth/SKILL.md")
        );
        let pi = |value: &str| {
            targets(root.path(), |key| {
                (key == "PI_CODING_AGENT_DIR").then(|| OsString::from(value))
            })
            .swap_remove(4)
        };
        let pi_dir = |value: &str| pi(value).path;
        let pi_agent = root.path().join("pi-agent/skills/gizmoth/SKILL.md");
        assert_eq!(pi_dir("~/pi-agent"), pi_agent);
        assert_eq!(pi_dir("~"), root.path().join("skills/gizmoth/SKILL.md"));
        assert!(pi_dir("~other/agent").ends_with("~other/agent/skills/gizmoth/SKILL.md"));
        // Node's path.join keeps home after a repeated separator; resolve folds `..`.
        assert_eq!(pi_dir("~//pi-agent"), pi_agent);
        assert_eq!(pi_dir("~/a/../pi-agent/."), pi_agent);
        let content = text(root.path(), "gizmoth");
        let data = root.path().join("data");
        let url = url::Url::from_directory_path(root.path().join("pi agent")).unwrap();
        assert!(url.as_str().contains("pi%20agent"));
        let decoded = pi(url.as_str());
        let decoded_path = root.path().join("pi agent/skills/gizmoth/SKILL.md");
        assert_eq!(decoded.path, decoded_path);
        set_file(&decoded, &data, &content, true).unwrap();
        assert!(decoded_path.is_file());
        // Pi itself cannot start with this value; it must never become a path.
        let invalid = pi("file:///%ZZ/agent");
        assert!(set_file(&invalid, &data, &content, true)
            .unwrap_err()
            .contains("PI_CODING_AGENT_DIR"));
        let status = target_status(&invalid, &content);
        assert!(!status.installed && status.error.is_some());
        if cfg!(unix) {
            assert!(!Path::new("file:").exists());
        }
        let defaults = targets(root.path(), |_| None);
        assert_eq!(
            defaults[0].path,
            root.path().join(".claude/skills/gizmoth/SKILL.md")
        );
        assert_eq!(defaults[1].path, paths[1].path);
        assert_eq!(
            defaults[2].path,
            root.path().join(".gemini/skills/gizmoth/SKILL.md")
        );
        assert_eq!(
            defaults[3].path,
            root.path().join(".config/opencode/skills/gizmoth/SKILL.md")
        );
        assert_eq!(
            defaults[4].path,
            root.path().join(".pi/agent/skills/gizmoth/SKILL.md")
        );
        let empty = targets(root.path(), |_| Some(OsString::new()));
        assert_eq!(empty[0].path, defaults[0].path);
        assert_eq!(empty[2].path, defaults[2].path);
        assert_eq!(empty[3].path, defaults[3].path);
        assert_eq!(empty[4].path, defaults[4].path);
        let overridden = &paths[2];
        set_file(overridden, &data, &content, true).unwrap();
        assert!(target_status(overridden, &content).current);
        assert!(!defaults[2].path.exists());
    }

    #[test]
    fn pi_agent_dir_follows_pi_normalize_path_on_both_platforms() {
        // Expected values come from Pi 1.5.2 normalizePath and Node fileURLToPath.
        let posix = |value| pi_agent_dir(value, OsStr::new("/home/me"), false);
        let windows = |value| pi_agent_dir(value, OsStr::new(r"C:\Users\me"), true);
        for (value, expected) in [
            ("file:///tmp/pi%20agent", "/tmp/pi agent"),
            ("file://localhost/tmp/pi", "/tmp/pi"),
            ("file:///tmp/a%23b?query#frag", "/tmp/a#b"),
            ("file:///tmp/%E2%9C%93", "/tmp/\u{2713}"),
            ("file:///tmp/a/../b/./c", "/tmp/b/c"),
            ("file:///C|/Users/me/pi", "/C:/Users/me/pi"),
            ("file:///a%5Cb", r"/a\b"),
            ("/c/Users/test/pi-agent", "/c/Users/test/pi-agent"),
            (r"~\pi", r"~\pi"),
            ("FILE:///tmp/x", "FILE:///tmp/x"),
        ] {
            assert_eq!(posix(value).unwrap(), OsStr::new(expected), "{value}");
        }
        for (value, expected) in [
            ("/c/Users/test/pi-agent", r"C:\Users\test\pi-agent"),
            ("/mnt/c/Users/test/pi-agent", r"C:\Users\test\pi-agent"),
            ("/cygdrive/c/Users/test/pi-agent", r"C:\Users\test\pi-agent"),
            ("/MNT/D/x", r"D:\x"),
            ("/c", r"C:\"),
            ("//server/share/pi", "//server/share/pi"),
            (r"/c\x", r"/c\x"),
            ("/mnt/cd/x", "/mnt/cd/x"),
            (r"~\pi", r"C:\Users\me\pi"),
            ("~/pi", r"C:\Users\me\pi"),
            ("file:///C:/Users/me/pi%20agent", r"C:\Users\me\pi agent"),
            ("file:///C|/Users/me/pi", r"C:\Users\me\pi"),
            ("file://server/share/pi", r"\\server\share\pi"),
            (
                "file://xn--nxasmq6b/share",
                "\\\\\u{3b2}\u{3cc}\u{3bb}\u{3bf}\u{3c3}\\share",
            ),
        ] {
            assert_eq!(windows(value).unwrap(), OsStr::new(expected), "{value}");
        }
        for value in [
            "file://server/share/pi",
            "file:///a%2Fb",
            "file:///%ZZ",
            "file:///a%",
            "file:///%FF",
            "file:///a%00b",
            "file://exa mple/x",
        ] {
            assert!(posix(value).is_err(), "{value}");
        }
        for value in [
            "file:///tmp/x",
            "file:///C:/a%5Cb",
            "file:///C:/a%2fb",
            "file:///%ZZ",
        ] {
            assert!(windows(value).is_err(), "{value}");
        }
    }

    #[test]
    fn prompt_tracks_paths_and_quotes_both_shells() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("GizMoth's app/$test`/gizmoth");
        let data = root.path().join("app's data");
        let posix = skill(&executable, &data, Shell::Posix).unwrap();
        let powershell = skill(&executable, &data, Shell::PowerShell).unwrap();
        assert!(posix.contains("'\"'\"'"));
        assert!(powershell.contains("& '") && powershell.contains("GizMoth''s"));
        for text in [&posix, &powershell] {
            for expected in [
                "--list-tools",
                "gizmoth_status",
                "serial_system_ports",
                "serial_status",
                "serial_logs",
                "instanceId",
                "generation",
                "h2loaderAt",
                "serial_pipe",
                "serial_job",
                "gizmoth.device.v2",
                "granted",
                "Connecting to /v1/ports/PORT_ID/pipe?client=&title= IS the take request",
                "Closing the WebSocket IS release",
                "safe I/O boundaries",
            ] {
                assert!(text.contains(expected), "{expected}");
            }
            for removed in [
                "serial_add",
                "serial_connect",
                "serial_send",
                "serial_install",
                "serial_reboot",
                "serial_refresh",
                "serial_cancel",
            ] {
                assert!(!text.contains(removed), "{removed}");
            }
            let frames: Vec<serde_json::Value> = text
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|value| value["method"].is_string())
                .collect();
            let methods: Vec<_> = frames
                .iter()
                .map(|frame| frame["method"].as_str().unwrap())
                .collect();
            let device = ["send", "refresh", "install", "reboot", "cancel"];
            if std::ptr::eq(text, &posix) {
                // Linux build VMs exist on macOS services: build, then install by artifact.
                assert_eq!(methods[..5], device);
                assert_eq!(methods[5..], ["build", "install"]);
                for expected in [
                    "compile_status",
                    "compile_pipe",
                    "compile_artifacts",
                    "compile_job",
                    "\"artifact\":\"ARTIFACT_ID\"",
                ] {
                    assert!(text.contains(expected), "{expected}");
                }
            } else {
                assert_eq!(methods, device);
                assert!(!text.contains("compile_pipe"));
            }
            let ids: std::collections::BTreeSet<_> = frames
                .iter()
                .map(|frame| frame["id"].as_str().unwrap())
                .collect();
            assert_eq!(ids.len(), frames.len());
            assert!(text.split_whitespace().count() < 2000);
        }
        let changed = skill(&root.path().join("new/gizmoth"), &data, Shell::Posix).unwrap();
        assert_ne!(posix, changed);
        assert!(skill(Path::new("relative"), &data, Shell::Posix).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn generated_posix_command_preserves_literal_paths_and_json() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("GizMoth's app/$test`/gizmoth");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let data = root.path().join("app's data/$literal`");
        let text = skill(&executable, &data, Shell::Posix).unwrap();
        let command = text
            .lines()
            .find(|line| line.contains(" --tool serial_status "))
            .unwrap();
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", command])
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        let args = output.lines().collect::<Vec<_>>();
        assert_eq!(
            &args[..4],
            [
                "--data-dir",
                data.to_str().unwrap(),
                "--tool",
                "serial_status"
            ]
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(args[4]).unwrap(),
            json!({"port":"PORT_ID"})
        );
    }

    #[test]
    fn concurrent_changes_are_detected_before_commit() {
        let root = tempfile::tempdir().unwrap();
        let parent = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        let name = std::ffi::OsStr::new("SKILL.md");
        let before = read_skill(&parent, name).unwrap();
        fs::write(root.path().join(name), b"editor contents").unwrap();
        assert!(verify_snapshot(&parent, before.as_ref()).is_err());
        let after = read_skill(&parent, name).unwrap();
        fs::write(root.path().join(name), b"changed").unwrap();
        assert!(verify_snapshot(&parent, after.as_ref()).is_err());
        fs::write(root.path().join("temporary"), b"replacement").unwrap();
        assert!(file_commit::rename_new(&parent, "temporary".as_ref(), &parent, name).is_err());
        assert_eq!(fs::read(root.path().join(name)).unwrap(), b"changed");
    }

    #[test]
    fn held_lock_or_failed_backup_never_replaces_the_skill() {
        let root = tempfile::tempdir().unwrap();
        let target = target(root.path());
        let old = text(root.path(), "old/gizmoth");
        let new = text(root.path(), "new/gizmoth");
        let data = root.path().join("data");
        set_file(&target, &data, &old, true).unwrap();
        let lock = fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("skills/.gizmoth-agents.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        assert!(set_file(&target, &data, &new, true)
            .unwrap_err()
            .contains("busy"));
        assert_eq!(fs::read(&target.path).unwrap(), old.as_bytes());
        drop(lock);
        fs::write(&data, b"not a directory").unwrap();
        assert!(set_file(&target, &data, &new, true)
            .unwrap_err()
            .contains("back up"));
        assert_eq!(fs::read(&target.path).unwrap(), old.as_bytes());
        assert_eq!(
            fs::read_dir(target.path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_keep_permissions_and_reject_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let target = target(root.path());
        let data = root.path().join("data");
        fs::create_dir_all(target.path.parent().unwrap()).unwrap();
        let old = text(root.path(), "old/gizmoth");
        let new = text(root.path(), "new/gizmoth");
        fs::write(&target.path, &old).unwrap();
        fs::set_permissions(&target.path, fs::Permissions::from_mode(0o640)).unwrap();
        set_file(&target, &data, &new, true).unwrap();
        assert_eq!(
            fs::metadata(&target.path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let backups = data.join("agents/backups/codex");
        assert_eq!(
            fs::metadata(&backups).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let backup = fs::read_dir(backups)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_file(&target.path).unwrap();
        let external = root.path().join("external");
        fs::write(&external, b"external contents").unwrap();
        symlink(&external, &target.path).unwrap();
        assert!(target_status(&target, &new).error.is_some());
        for installed in [true, false] {
            assert!(set_file(&target, &data, &new, installed)
                .unwrap_err()
                .contains("symlinks"));
        }
        assert_eq!(fs::read(external).unwrap(), b"external contents");
        assert!(fs::symlink_metadata(&target.path)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(&target.path).unwrap();
        fs::remove_dir(target.path.parent().unwrap()).unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("SKILL.md"), &old).unwrap();
        symlink(&outside, target.path.parent().unwrap()).unwrap();
        assert!(target_status(&target, &new).error.is_some());
        for installed in [true, false] {
            assert!(set_file(&target, &data, &new, installed).is_err());
        }
        assert_eq!(fs::read(outside.join("SKILL.md")).unwrap(), old.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn demo_skill_fixture_matches_renderer() {
        let rendered = skill(
            Path::new("/Applications/GizMoth.app/Contents/MacOS/gizmoth"),
            Path::new("/Users/example/Library/Application Support/com.gizclaw.gizmoth"),
            Shell::Posix,
        )
        .unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/demo-agent-skill.json");
        if std::env::var_os("GIZMOTH_UPDATE_SKILL_FIXTURE").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            fs::write(
                &fixture,
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&json!({"skill": rendered})).unwrap()
                ),
            )
            .unwrap();
        }
        let actual: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
        assert_eq!(
            actual["skill"], rendered,
            "Regenerate with GIZMOTH_UPDATE_SKILL_FIXTURE=1 and this test"
        );
    }

    #[test]
    fn powershell_quotes_escape_every_single_quote_delimiter() {
        for delimiter in ['\'', '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'] {
            let input = format!("O{delimiter}Brien");
            assert_eq!(
                quote(&input, Shell::PowerShell),
                format!("'O{delimiter}{delimiter}Brien'")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn powershell_invocations_wait_and_fail_without_exiting_the_host() {
        use std::os::unix::fs::PermissionsExt;
        let Some(pwsh) = std::env::var_os("GIZMOTH_TEST_PWSH") else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("O'‘’‚‛Brien");
        let invocation = |executable: &Path| {
            skill(executable, &data, Shell::PowerShell)
                .unwrap()
                .lines()
                .find(|line| line.contains(" --list-tools "))
                .unwrap()
                .to_owned()
        };
        let run = |command: &str| {
            std::process::Command::new(&pwsh)
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    command,
                ])
                .output()
                .unwrap()
        };
        let failed = invocation(Path::new("/usr/bin/false"));
        assert!(!run(&failed).status.success());
        assert!(!run(&format!(
            "$ErrorActionPreference='SilentlyContinue'; {failed}"
        ))
        .status
        .success());
        let success = invocation(Path::new("/usr/bin/true"));
        assert!(run(&format!("$LASTEXITCODE=37; {success}"))
            .status
            .success());
        let host = run(&format!(
            "try {{ {failed} }} catch {{ Write-Output 'caught' }}; Write-Output 'HOST_STILL_ALIVE'"
        ));
        assert!(String::from_utf8_lossy(&host.stdout).contains("HOST_STILL_ALIVE"));
        let helper = root.path().join("helper'‘’‚‛.sh");
        fs::write(&helper, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let output = run(&invocation(&helper));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            stdout.lines().collect::<Vec<_>>(),
            ["--data-dir", data.to_str().unwrap(), "--list-tools"]
        );
        fs::write(&helper, b"#!/bin/sh\nprintf '{\"ok\":true}\\n'\n").unwrap();
        let output = run(&invocation(&helper));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            json!({"ok":true})
        );
    }

    #[test]
    fn retained_commit_preserves_intervening_saves_and_open_writers() {
        use std::io::{Seek, SeekFrom};
        for update in [true, false] {
            for atomic_save in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let parent =
                    Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
                let recovery = private_child(&parent, ".gizmoth-recovery").unwrap();
                let name = root.path().join("SKILL.md");
                fs::write(&name, b"original").unwrap();
                fs::write(root.path().join("staged"), b"replacement").unwrap();
                let original = read_skill(&parent, "SKILL.md".as_ref()).unwrap().unwrap();
                if atomic_save {
                    fs::write(root.path().join("editor"), b"intervening editor").unwrap();
                    fs::rename(root.path().join("editor"), &name).unwrap();
                } else {
                    fs::write(&name, b"intervening editor").unwrap();
                }
                assert!(commit_existing(
                    &parent,
                    &recovery,
                    &original,
                    update.then_some("staged"),
                    || {}
                )
                .is_err());
                assert_eq!(fs::read(&name).unwrap(), b"intervening editor");
                let original = read_skill(&parent, "SKILL.md".as_ref()).unwrap().unwrap();
                let mut writer = fs::OpenOptions::new().write(true).open(&name).unwrap();
                commit_existing(
                    &parent,
                    &recovery,
                    &original,
                    update.then_some("staged"),
                    || {},
                )
                .unwrap();
                writer.seek(SeekFrom::Start(0)).unwrap();
                writer.set_len(0).unwrap();
                writer.write_all(b"late descriptor save").unwrap();
                writer.sync_all().unwrap();
                let copies = fs::read_dir(root.path().join(".gizmoth-recovery"))
                    .unwrap()
                    .map(|e| fs::read(e.unwrap().path()).unwrap())
                    .collect::<Vec<_>>();
                assert!(copies.iter().any(|bytes| bytes == b"late descriptor save"));
                if update {
                    assert_eq!(fs::read(&name).unwrap(), b"replacement");
                } else {
                    assert!(!name.exists());
                }
            }
        }
    }

    #[test]
    fn a_new_destination_during_commit_is_never_overwritten() {
        for update in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let parent = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
            let recovery = private_child(&parent, ".gizmoth-recovery").unwrap();
            let name = root.path().join("SKILL.md");
            fs::write(&name, b"original").unwrap();
            fs::write(root.path().join("staged"), b"replacement").unwrap();
            let original = read_skill(&parent, "SKILL.md".as_ref()).unwrap().unwrap();
            let result = commit_existing(
                &parent,
                &recovery,
                &original,
                update.then_some("staged"),
                || {
                    fs::write(&name, b"new unowned Skill").unwrap();
                },
            );
            assert!(result.is_err());
            assert_eq!(fs::read(&name).unwrap(), b"new unowned Skill");
            let files = fs::read_dir(root.path().join(".gizmoth-recovery"))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(fs::read(files[0].path()).unwrap(), b"original");
        }
    }

    #[cfg(unix)]
    #[test]
    fn fifo_lock_is_rejected_without_blocking_other_mutations() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("skills")).unwrap();
        let path = root.path().join("skills/.gizmoth-agents.lock");
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: creates only a FIFO inside the test-owned temporary directory.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let target = target(root.path());
        let skill = text(root.path(), "gizmoth");
        assert!(set_file(&target, &root.path().join("data"), &skill, true)
            .unwrap_err()
            .contains("regular file"));
        fs::remove_file(path).unwrap();
        set_file(&target, &root.path().join("data"), &skill, true).unwrap();
        assert!(target_status(&target, &skill).current);
    }

    #[cfg(unix)]
    #[test]
    fn native_skill_root_ancestors_may_be_symlinked() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("native-root");
        fs::create_dir(&native).unwrap();
        symlink(&native, root.path().join("skills")).unwrap();
        let target = target(root.path());
        let skill = text(root.path(), "gizmoth");
        set_file(&target, &root.path().join("data"), &skill, true).unwrap();
        assert_eq!(
            fs::read(native.join("gizmoth/SKILL.md")).unwrap(),
            skill.as_bytes()
        );
        assert!(target_status(&target, &skill).current);
        set_file(&target, &root.path().join("data"), &skill, false).unwrap();
        assert!(!native.join("gizmoth").exists());
        assert!(fs::symlink_metadata(root.path().join("skills"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
