<p align="center"><img src="branding/preview-mono-512.png" width="128" alt="GizMoth icon"></p>

# GizMoth

English · [简体中文](README.zh-CN.md)

GizMoth is a desktop app for managing GizOS devices and building Lua apps with an AI coding assistant. It is built with Tauri 2, React and TypeScript.

## Features

- Pair devices over Bluetooth or with a credential link, and manage supported controls.
- Create, edit, validate and export Lua projects, or install them on a paired device.
- Use an AI assistant with a configurable model provider and offline GizOS documentation.
- Let the assistant run projects in local simulators, test physical buttons, and inspect startup/install status and screen captures, subject to the project's run permission.
- Create independent simulators, select firmware and send application run requests.
- Manage local serial ports through a resident serial service: live status and logs per port, H2Loader detection and firmware installation, and GizMoth's global MCP server (`gizmoth --mcp`) for other tools.
- Manage chip and general development environments from the [GizClaw/gizmoth-devenv](https://github.com/GizClaw/gizmoth-devenv) registry, including a GizOS base environment with Bazel/Python dependencies. Install multiple toolchain and Python versions side by side, reuse exact dependencies, inspect/open install folders, and remove or reinstall managed versions. Local toolchain folders can be registered without modifying their files. See the [development-environment guide](docs/devenv.md).
- Connect external coding agents from the Agents tab: preview, install, update or uninstall the GizMoth Skill, query devices through CLI/MCP, then connect one WebSocket for exclusive device commands. Each connection identifies its client and task title, waits for a grant, carries sends/install/reboot and job events, and releases access when closed. Device loss or reboot reports availability events while retaining the connection and exclusive owner; operators can disconnect a selected session. Global instructions stay intact; no MCP setup is required. See the [agent integration guide](docs/agents.md).
- Switch between English and Simplified Chinese, with light and dark themes.
- Keep a small [Peetty](https://github.com/idy/peetty) companion on the page: it walks, jumps between safe edges and can be dragged. Hover for actions, or focus it and press Enter. Hide or restore it in Settings → General → Page companion.

## Install

Find macOS (`.dmg`) and Windows (`.exe`) installers on [Releases](https://github.com/GizClaw/gizmoth/releases) when available. If no release is available, build from source using the instructions below.

On macOS, use v0.1.1 or later: the initial v0.1.0 bundles have an incomplete resource signature and are rejected by Gatekeeper.

## Development

Install [Node.js 22.19+](https://nodejs.org/), [Rust 1.90+](https://www.rust-lang.org/tools/install) and the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/), then run:

```bash
npm ci
npm run app
```

To build an installer without an updater signing key:

```bash
npm run app:build -- --config src-tauri/tauri.unsigned.conf.json
```

For a browser-only preview, run `npm run dev` and open `http://127.0.0.1:1420/?demo=1`. See the [maintenance guide](docs/maintenance.md) for tests, releases and protocol references.

- `src/`: React interface and coding assistant, including `PageCompanion.tsx` for the local page companion
- `vendor/`: pinned Peetty and Magpie Design System npm archives with source provenance, so installation needs no GitHub Packages credentials; Magpie 0.4.4 is the offline style and font fallback
- `src/design-system.ts`: automatic CDN style and typography updates at startup, every six hours and after network recovery, with fonts included in the offline cache of the last successful release
- `src-tauri/`: native desktop and device integration, including the serial service in `src-tauri/src/serial/` and development environments in `src-tauri/src/devenv/`
- `native/serial-core/`: Bazel module linking GizOS Host Core for H2Loader serial ports (`npm run serial-core:build`)
- `docs/gizos/`: bundled [GizOS documentation](docs/gizos/README.md)

## Serial service transport

The desktop app, `gizmoth --mcp`, LAN clients and other GizMoth nodes share port 7420. The service speaks HTTP/1.1, WebSocket (`ws://`) and native gRPC over HTTP/2 prior knowledge (h2c). Enable LAN scope in Settings → Service, then connect with the displayed address and API key. No HTTPS, certificates, private keys or fingerprint confirmation are required. Ordinary LAN traffic is plaintext; Tailscale encrypts traffic between its nodes.

```bash
curl -H 'Authorization: Bearer <API Key>' http://192.168.1.20:7420/v1/ports
```

The service binds loopback and each detected LAN or Tailscale IPv4 interface separately. External callers authenticate through the bearer header; remote WebSockets reject token query parameters. The desktop's per-launch token works only through the dedicated loopback listener. A connection to a LAN listener remains a Gateway caller even when it originates from the same computer, and proxy headers never grant desktop privileges.

Listener changes bind before committing the configuration and keep the current listeners on failure. Interface changes are reconciled automatically; failed binds are retried, with private loopback control retained. Local scope or disabling Service closes LAN listeners. Local and remote callers have separate connection budgets. The native integration is `gateway::Listeners::prepare(&config, current)` followed by `adopt()`; the connection carries its listener identity into HTTP and gRPC authorization.

## Serial port cascading

Another GizMoth's serial ports can be used from this one (Serial ports → Add serial port → Remote GizMoth), over the LAN or a Tailscale network. The owning GizMoth enables LAN access in Settings → Service; the connecting side enters its address (`host`, `host:port` or `http://host:port`, default port 7420) and API key. Tailscale interfaces (`100.64.0.0/10`) are served alongside LAN interfaces. The connection flow is address/key → Connect → Add, without certificates.

Nodes speak `gizmoth.gateway.v1` gRPC ([`src-tauri/proto/gizmoth/gateway/v1/gateway.proto`](src-tauri/proto/gizmoth/gateway/v1/gateway.proto)) on the existing service port: HTTP/2 prior knowledge (h2c) on loopback, LAN and Tailscale, with the same `Authorization: Bearer <API Key>`. The service is `GetNode` (persistent node ID, per-start boot ID), `WatchTopology` (full snapshots on change), `ReadLogs`, `Session` (bidirectional exclusive control sharing the WebSocket queue, grants and job fencing), `UploadPackage` (chunked, SHA-256 checked, installed by package ID) and `GetJob`. Bindings are generated by `build.rs` with protox, so builds need no `protoc`.

The serial service, not the window, maintains connections (`src-tauri/src/serial/remote.rs`; server side in `src-tauri/src/serial/daemon/node_service.rs`). Imported ports appear in the snapshot under `nodes[]` with local IDs `<node id>--<port id>`; `ports[]` stays this computer's own. Reads (status, logs) need no control. Sending, installing and rebooting open a session on the owning node on demand, which that node arbitrates like any other client; it is released after a minute idle. Configuration, approvals and log clearing stay with the owning node. Disconnected nodes keep their last ports, marked stale, and reconnect with backoff. Only a node's own ports are exported, so cascading is one level. `<app data>/serial/remotes.json` (mode 0600) stores addresses and API keys.

## Current limitations

Direct device installs are limited to 512 KiB compressed, and Lua projects do not yet support binary assets. Firmware project builds are not implemented. The development-environment registry publishes Bazelisk, managed Python versions and the GizOS base host-tools environment; chip SDK recipes remain pending. Serial installation covers devices that already run H2Loader, not ROM recovery of blank boards. Bluetooth serial ports are not yet supported, and serial cascading is one level: a GizMoth does not re-export ports it imports. Device interoperability and end-to-end simulator app execution still need further verification.

Projects and conversations are stored locally. Device keys are kept in private app data, provider keys in the system credential store, and AI requests are sent to the configured provider.

## License

[MIT](LICENSE)
