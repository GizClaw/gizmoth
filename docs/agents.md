# Connecting external coding agents

Open **Agents** in GizMoth's main sidebar to install the **gizmoth** Skill for an external coding agent. The Skill teaches the agent to query ports, cached hardware status and recorded logs through GizMoth's CLI, then run device commands through one granted service WebSocket. The agent loads the detailed instructions when relevant to its task. No MCP setup is required; the existing `gizmoth --mcp` integration provides the same queries separately.

## Installing the Skill

The page shows the user-level `SKILL.md` target for Claude Code, Codex, Gemini CLI and OpenCode. Expand **Preview SKILL.md** to inspect the exact file before installation, or copy it into another agent's Skill directory. Its YAML frontmatter contains the name and task description; the body contains the actual GizMoth executable path, examples and device-identification guidance. It contains no API keys.

Choose **Install Skill**, **Update Skill** or **Uninstall Skill** for the corresponding agent. Updating also refreshes executable paths after moving the application. Installation never modifies global `CLAUDE.md`, `AGENTS.md` or `GEMINI.md` files. An existing same-name Skill that is not managed by GizMoth is a conflict and is preserved. Uninstall removes only the managed `SKILL.md` and removes its directory only if empty; sibling assets and other Skills remain intact.

| Agent | User-level target | Override |
| --- | --- | --- |
| Claude Code | `~/.claude/skills/gizmoth/SKILL.md` | `CLAUDE_CONFIG_DIR` replaces `~/.claude` |
| Codex | `~/.agents/skills/gizmoth/SKILL.md` | — |
| Gemini CLI | `~/.gemini/skills/gizmoth/SKILL.md` | `GEMINI_CLI_HOME` replaces the base home (`~`), before `.gemini` |
| OpenCode | `~/.config/opencode/skills/gizmoth/SKILL.md` | `XDG_CONFIG_HOME` replaces `~/.config` |

Overrides come from GizMoth's process environment. Codex uses its current documented user-level `.agents/skills` directory rather than its global instruction-file location. Some agents also discover compatible directories: for example, Gemini CLI reads `~/.agents/skills`, and OpenCode reads `~/.agents/skills` and `~/.claude/skills`. The page reports installation at each displayed path, not whether a running agent has loaded or enabled the Skill. A compatible installation can therefore be available to more than one agent; uninstalling one path does not remove other copies.

If an installation or update is not discovered, reload Skills or restart the agent. Gemini CLI provides `/skills reload`. Skills can still be disabled or require activation approval in the target agent. Project-level or other higher-priority Skills may override a user-level copy with the same name.

Unreadable files, symlinks at the managed destination and conflicting ownership prevent mutation and display a per-agent error. Fix permissions or the conflicting installation, refresh the integration status and retry. Existing managed files are backed up privately before replacement. Updates and uninstalls also retain the actual displaced file in a private `.gizmoth-recovery/` directory beside the user-level Skill directories, on the same filesystem. This preserves later writes through an editor's already-open file handle. If another file appears during publication, GizMoth does not overwrite it and reports the restore/recovery location. These recovery files are retained for manual recovery, not automatically pruned. Browser demo installation is simulated and never changes local files.

Windows UNC destinations remain limited by the pinned filesystem dependency's directory-path handling and are not supported by this version; use a local directory. The native no-clobber publication path does not require hard links, including on local exFAT volumes. Windows filesystem execution still requires platform verification.

## Calling tools

The Skill uses the absolute executable path. The examples below use `gizmoth` for brevity; use the full path shown in `SKILL.md` when it is not on your PATH.

List tool names, parameter schemas and descriptions:

```bash
gizmoth --list-tools
```

Discover local ports, including ports not yet added to GizMoth:

```bash
gizmoth --tool serial_system_ports '{}'
```

Inspect managed ports and a particular device:

```bash
gizmoth --tool serial_list '{}'
```

```bash
gizmoth --tool serial_status '{"port":"<port-id>"}'
```

Tool results are JSON. Invalid arguments and failed queries return a nonzero exit code. The CLI and MCP share the read-only registry: `gizmoth_status`, `serial_list`, `serial_system_ports`, `serial_status`, `serial_logs`, `serial_job` and `serial_pipe`. Queries never start a stopped service. A response reporting `service: stopped` is not evidence that the computer has no ports. Use GizMoth's app to add/configure/connect ports or enable Service. Legacy mutation tools return an error directing callers to the device WebSocket.

## What device identification means

- `serial_system_ports` / `GET /v1/system-ports` lists discovered paths and available USB product, manufacturer, VID/PID and serial metadata. This may identify a USB adapter rather than the board attached to it.
- `serial_list` / `GET /v1/ports` lists managed ports, connections, detected protocols and device state.
- `serial_status` / `GET /v1/ports/{id}` includes H2Loader board, target, chip, UID, active App/Loader version, Stage and partition information after a successful handshake. Check the connection and status timestamp: cached device information can remain after disconnection.
- A plain serial device does not have a universal identification protocol. Use its documented protocol or logs; do not infer its identity from the adapter name alone.

## Device WebSocket sessions

GizMoth's resident service is always the sole process opening and managing the physical serial port and running H2Loader. Agents connect to the service, never to the device path or a separate flashing utility. Status, log and job queries need no ownership grant. All device operations use the same granted WebSocket.

Discover the URL and authentication headers:

```bash
gizmoth --tool serial_pipe '{"port":"<port-id>","client":"coding-agent","title":"Inspect device and refresh status"}'
```

`serial_pipe` requires `client` and a `title` describing the work (up to 120 characters); `reason` is optional additional context. GizMoth shows the client and task title in the device list, holder details and waiting queue, including while the device is offline. It returns `url`, `headers`, `protocol: gizmoth.device.v2` and `commands`, each with `name`, `description` and `inputSchema`. Discovery rejects older service protocols rather than promising lease retention on a service that would close it on device loss. **Obtaining this URL does not request ownership. Opening the WebSocket is the take request; closing it is release.** There is no separate take/release tool or API. Use a WebSocket client that supports authentication headers, keep credentials private and keep the socket open throughout the operation.

The endpoint remains `/v1/ports/{id}/pipe?client=&title=`. Automatic approval defaults to true, including while GizMoth controls the port: requests wait for existing work and receive FIFO grants without a manual mode switch. With `autoGrant` disabled, the user accepts or rejects requests in the app. Waiting sockets receive `{"type":"queued","state":"pending","position":1,"holder":null}` and stay connected until `{"type":"granted"}` arrives on that same socket. A command sent early receives a failed `result` with `state: pending`, preserving both the connection and queue position. A shared client name or API key cannot share a socket's grant.

After `granted`, send structured JSON text frames with a unique request `id`, a `method` and a `params` object. The discovery schemas describe these methods:

| Method | Parameters | Behavior |
| --- | --- | --- |
| `send` | `text`, optional `lineEnding` (`lf`, `crlf`, `cr`, `none`) | Send text to a plain serial device |
| `install` | Absolute local `package`, optional `stageOnly`, `jobId` UUID | Run GizMoth's native H2Loader installation; local connections only |
| `reboot` | `target` (`app`, `loader`), optional `jobId` UUID | Reboot through H2Loader and verify reconnection |
| `refresh` | Optional `jobId` UUID | Request fresh H2Loader status |
| `cancel` | `jobId` UUID | Cancel this socket's job at a safe I/O boundary |

For example, these are frames sent on a granted socket, with replies matched by `id`:

```json
{"id":"send-1","method":"send","params":{"text":"help","lineEnding":"lf"}}
{"id":"refresh-1","method":"refresh","params":{}}
```

Successful replies are `{"type":"result","id":"request-id","ok":true,"result":{...}}`; failures are `{"type":"result","id":"request-id","ok":false,"error":{"code":...,"message":"..."}}`. Job progress and completion arrive as `{"type":"job","job":{...}}` on the same socket. Track the returned job ID through its terminal state; command acceptance does not establish success. `serial_status` and `serial_job` remain available for recovery after a lost reply. Send install, reboot, refresh and cancel through the granted socket too. Client binary frames and raw text are rejected; server binary frames contain device logs. Physical serial access and H2Loader protocol handling remain inside GizMoth throughout the session.

### Device availability does not release ownership

A physical serial loss, USB re-enumeration, H2Loader reboot, protocol reconnect or temporary port disable keeps the same WebSocket and its grant. The holder and its task remain visible, and queued clients continue waiting. The service pushes a `device` event for availability changes:

```json
{"type":"device","sequence":2,"at":1794418725195,"state":"disconnected","connection":"waiting","ready":false,"reason":"Device unplugged"}
```

`state` is `disconnected`, `connecting`, `reconnecting` or `connected`; `connection` uses the existing port status values. `ready` describes physical I/O readiness, not permission to take the device. A pending client still waits for `granted`; an already-granted client does not need another grant after recovery. Brief loss/recovery transitions are delivered separately, even between snapshot updates. `missedEvents` on a resynchronization event means a slow receiver missed availability events; inspect current status and logs before assuming what happened.

While unavailable, physical commands fail with HTTP-equivalent error code `503`; they are not buffered or replayed. The holder can still query cached status/logs/jobs or cancel its own existing job. Let the agent's task decide whether to wait or close. If waiting, continue on the same socket after `ready:true`, using a new request ID and checking prior job results before retrying ambiguous work. A reboot or transfer interrupted by device loss can fail or cancel that job without releasing the socket.

An operator can disconnect a particular holder or waiter by its connection ID, including offline or during work. Stale IDs cannot disconnect a replacement holder. Explicit port removal, service shutdown, revoked authorization, socket closure and transport/backpressure failures can still end the session.

Firmware installation, reboot and other device changes must follow the user's authorization for the exact target and package. Installation requires an absolute `.update.tar.zlib` path on the service computer and an actual local connection; LAN clients cannot submit local paths. `stageOnly` transfers without rebooting. After an ambiguous failure, inspect status and recent job history before retrying. A Skill supplies instructions, not additional permissions.

Closing the socket releases its communication rights and cancels session-owned work at safe I/O boundaries. The next queued socket receives `granted` only after completion or cancellation settles. After the automatic queue drains, GizMoth control resumes. Explicitly reclaiming control in the app also pauses automatic approval so waiters cannot immediately take it back.

For local or LAN access, configure **Settings → Service** and use its API key in the bearer header. External HTTP is for queries; device mutations use the granted WebSocket. LAN clients require HTTPS/WSS and certificate verification as described in the [README](../README.md#serial-service-transport).

## Maintaining the demo fixture

The native `skill()` renderer is the source of truth. A Unix Rust test compares its fixed example-path output with `src/demo-agent-skill.json`, so changed guidance cannot silently leave the browser demo stale. Regenerate the committed fixture through the same renderer when needed:

```bash
GIZMOTH_UPDATE_SKILL_FIXTURE=1 cargo test --locked --manifest-path src-tauri/Cargo.toml --lib agents::tests::demo_skill_fixture_matches_renderer
```

## Agent documentation

- [Claude Code Skills](https://code.claude.com/docs/en/skills)
- [Codex Skills](https://developers.openai.com/codex/skills)
- [Gemini CLI Skills](https://geminicli.com/docs/cli/skills/)
- [OpenCode Skills](https://opencode.ai/docs/skills/)
