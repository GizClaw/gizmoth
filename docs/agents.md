# Connecting external coding agents

Open **Agents** in GizMoth's main sidebar to install the **gizmoth** Skill for an external coding agent. The Skill teaches the agent to discover ports, inspect hardware, read logs and use the resident serial service through GizMoth's command-line tools. The agent loads the detailed instructions when relevant to its task. No MCP setup is required; the existing `gizmoth --mcp` integration remains available separately.

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

Tool results are JSON. Invalid arguments and failed operations return a nonzero exit code. Tool names and arguments are shared with the existing MCP server; the CLI invokes the same service-backed implementation. Read tools do not start a stopped service. A response reporting `service: stopped` is not evidence that the computer has no ports. Open GizMoth and connect a configured port when service startup is needed.

## What device identification means

- `serial_system_ports` / `GET /v1/system-ports` lists discovered paths and available USB product, manufacturer, VID/PID and serial metadata. This may identify a USB adapter rather than the board attached to it.
- `serial_list` / `GET /v1/ports` lists managed ports, connections, detected protocols and device state.
- `serial_status` / `GET /v1/ports/{id}` includes H2Loader board, target, chip, UID, active App/Loader version, Stage and partition information after a successful handshake. Check the connection and status timestamp: cached device information can remain after disconnection.
- A plain serial device does not have a universal identification protocol. Use its documented protocol or logs; do not infer its identity from the adapter name alone.

GizMoth's resident service remains the sole port owner. For raw access, `serial_pipe` returns a WebSocket URL and bearer header. Identify the client and reason, keep the socket open, and wait for `granted` before sending. Closing releases the port. The user manages approval and ownership policy in GizMoth. Firmware installation, reboot and other device-changing operations must follow the user's authorization and identify the exact target; after ambiguous errors, inspect status before retrying. A Skill supplies instructions, not additional operating-system or service permissions.

For direct local or LAN HTTP/WebSocket access, configure **Settings → Service** and use its API key. LAN clients require HTTPS/WSS and certificate verification as described in the [README](../README.md#serial-service-transport).

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
