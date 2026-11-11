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
- Connect external coding agents from the Agents tab: install the GizMoth Skill, query devices through CLI/MCP, then connect one WebSocket for exclusive device commands. The connection requests access, waits for a grant, carries sends/install/reboot and job events, and releases access when closed. Global instructions stay intact; no MCP setup is required. See the [agent integration guide](docs/agents.md).
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
- `vendor/`: pinned Peetty and Magpie Design System npm archives with source provenance, so installation needs no GitHub Packages credentials; Magpie 0.4.3 is the offline style fallback
- `src/design-system.ts`: automatic CDN style updates at startup, every six hours and after network recovery, with an offline cache of the last successful release
- `src-tauri/`: native desktop and device integration, including the serial service in `src-tauri/src/serial/`
- `native/serial-core/`: Bazel module linking GizOS Host Core for H2Loader serial ports (`npm run serial-core:build`)
- `docs/gizos/`: bundled [GizOS documentation](docs/gizos/README.md)

## Serial service transport

The desktop app and `gizmoth --mcp` use authenticated loopback HTTP. LAN connections must use HTTPS and WSS; never send a service API key over remote HTTP or plain WebSocket. Clients must verify the server certificate chain and the requested hostname or IP address. Use a trusted CA or explicitly trust your private CA; do not disable certificate verification (`curl -k`, `insecure`, or equivalent).

In Settings → Service → LAN TLS, save absolute paths to your PEM certificate chain and private key before selecting the LAN scope. Saving again reloads renewed certificates without changing the listening port. The same paths are stored in `<app data>/gateway.json` alongside the existing settings and API key:

```json
{
  "tlsCertificateChain": "/absolute/path/server-chain.pem",
  "tlsPrivateKey": "/absolute/path/server-key.pem"
}
```

The chain file contains the leaf certificate followed by intermediates. The unencrypted private key may be PKCS#8, PKCS#1 RSA or SEC1 EC. Restrict key-file access to the service user. The certificate must contain a SAN for the DNS name or IP used by clients. GizMoth reads the files locally; it does not return their contents through the service API. Missing, unreadable, malformed or mismatched files fail before a LAN listener is opened. Restart/reconfigure the listeners after replacing files; certificates are not watched for changes.

A client using a private CA connects with certificate verification enabled:

```bash
curl --cacert /path/to/private-ca.pem \
  -H 'Authorization: Bearer <API Key>' \
  https://gizmoth.example:7420/v1/ports
```

WebSocket clients use `wss://` and an `Authorization: Bearer <API Key>` header. Keep credentials out of URLs and query parameters.

The service binds `127.0.0.1` for HTTP/MCP and each detected LAN IPv4 interface separately for TLS on the same port. HTTPS and WSS share the authenticated API. Remote WebSockets accept only header authentication and reject token query parameters. The daemon uses the socket's actual peer and transport for authorization; proxy headers never grant desktop privileges. Local gateway API keys retain gateway permissions, and the desktop's per-launch token is accepted only on the loopback HTTP listener.

Listener changes validate and bind before committing the configuration. A failed update keeps the current listeners and settings. If the configured LAN transport cannot start, the daemon reports the error and starts only a private loopback listener for the desktop and MCP. Disabled or local scope closes LAN listeners. TLS handshakes have a five-second timeout and a separate remote connection budget, preserving local control access.

The native integration is `serial::gateway::Listeners::prepare(&config, current)` followed by `adopt()` in the runtime. `AsyncListeners::accept()` yields the actual peer and transport; the daemon acquires its connection permit before calling `Accepted::handshake()`, then passes the resulting stream to Hyper with WebSocket upgrades enabled. Listener sets retain sockets for same-port scope switches and certificate reloads. The plaintext `gateway::bind` helper refuses LAN configurations as a defense against future accidental exposure.

## Serial port cascading

Another GizMoth's serial ports can be used from this one (Serial ports → Add serial port → Remote GizMoth), over the LAN or a Tailscale network. The owning GizMoth enables LAN access in Settings → Service with TLS as described above; the connecting side enters its address (`host`, `host:port` or `https://host:port`, default port 7420) and API key. Tailscale interfaces (`100.64.0.0/10`) are served alongside LAN interfaces, so the certificate needs a SAN for the Tailscale IP or MagicDNS name used. A certificate trusted by the system (for example from `tailscale cert`) verifies normally; a self-signed certificate is shown as a SHA-256 fingerprint to compare with the one under Settings → Service on the owning side, then pinned exactly.

Nodes speak `gizmoth.gateway.v1` gRPC ([`src-tauri/proto/gizmoth/gateway/v1/gateway.proto`](src-tauri/proto/gizmoth/gateway/v1/gateway.proto)) on the existing service port: HTTP/2 by ALPN over LAN TLS, or prior-knowledge HTTP/2 on loopback, with the same `Authorization: Bearer <API Key>`. The service is `GetNode` (persistent node ID, per-start boot ID), `WatchTopology` (full snapshots on change), `ReadLogs`, `Session` (bidirectional exclusive control sharing the WebSocket queue, grants and job fencing), `UploadPackage` (chunked, SHA-256 checked, installed by package ID) and `GetJob`. Bindings are generated by `build.rs` with protox, so builds need no `protoc`.

The serial service, not the window, maintains connections (`src-tauri/src/serial/remote.rs`; server side in `src-tauri/src/serial/daemon/node_service.rs`). Imported ports appear in the snapshot under `nodes[]` with local IDs `<node id>--<port id>`; `ports[]` stays this computer's own. Reads (status, logs) need no control. Sending, installing and rebooting open a session on the owning node on demand, which that node arbitrates like any other client; it is released after a minute idle. Configuration, approvals and log clearing stay with the owning node. Disconnected nodes keep their last ports, marked stale, and reconnect with backoff. Only a node's own ports are exported, so cascading is one level. `<app data>/serial/remotes.json` (mode 0600) stores addresses, API keys and pinned fingerprints.

## Current limitations

Direct device installs are limited to 512 KiB compressed, and Lua projects do not yet support binary assets. Firmware development is not implemented; serial installation covers devices that already run H2Loader, not ROM recovery of blank boards. Bluetooth serial ports are not yet supported, and serial cascading is one level: a GizMoth does not re-export ports it imports. Device interoperability and end-to-end simulator app execution still need further verification.

Projects and conversations are stored locally. Device keys are kept in private app data, provider keys in the system credential store, and AI requests are sent to the configured provider.

## License

[MIT](LICENSE)
