<p align="center"><img src="branding/preview-mono-512.png" width="128" alt="GizMoth icon"></p>

# GizMoth

A desktop app for GizOS devices: pair a device over Bluetooth, adjust it, and build Lua apps with an embedded coding agent, then push them to the device.

Built with Tauri 2, React and TypeScript. The coding agent is [pi-agent-core](https://www.npmjs.com/package/@mariozechner/pi-agent-core) running inside the app and talking to whichever model provider you configure.

## Features

- **Pairing**: GizOS BLE Binding v1 over native Bluetooth, or a device credential link. Credentials are stored in the OS credential store.
- **Simple control**: online status, firmware, battery and Wi-Fi; volume, display and light brightness for hardware the device declares; list and run installed Lua apps.
- **Projects**: local Lua projects (`app.json`, `<app_id>.lua`, assets under `<app_id>/`) with an editor, autosave and package checks against the firmware's rules.
- **Coding agent**: the agent lists, reads, writes and edits project files, runs the package check, and can push to the selected device after you approve. The conversation for each project is saved locally.
- **Providers**: magpie (local gateway), Anthropic, OpenAI, Google Gemini, OpenRouter, DeepSeek, or any OpenAI- or Anthropic-compatible endpoint. Model lists can be read from the provider's API or entered by hand.
- **Push**: package (format-1 manifest, SHA-256, deterministic USTAR + zlib) and install directly to the device, then start it. Packages can also be exported as `.lua-app.tar.zlib`.

## Install

Download the latest installer from [Releases](https://github.com/GizClaw/gizmoth/releases): the `.dmg` for macOS (Apple silicon or Intel) or the `.exe` for Windows.

## How it fits together

| Layer | Responsibility | Key files |
| --- | --- | --- |
| Rust | BLE binding, OS credential store, project files, packaging, device HTTP, tray | `src-tauri/src/*.rs` |
| Bridge | Narrow typed IPC commands; no generic filesystem or shell access | `src/bridge.ts` |
| Agent | pi-agent-core with project tools and the GizOS system prompt | `src/agent.ts` |
| Providers | Presets, model lists, mapping to pi-ai models | `src/providers.ts` |
| HTTP | Routes provider requests through Tauri's Rust HTTP client (no CORS) | `src/http.ts` |
| UI | Sidebar shell, projects, devices, providers, settings | `src/App.tsx`, `src/pages/` |

Only the OpenAI, Anthropic and Google provider APIs of pi-ai are bundled; other SDKs are stubbed out in `vite.config.ts`.

## Local data

- Device credentials and provider API keys: macOS Keychain, Windows Credential Manager or Linux Secret Service.
- Device list, provider configuration (without keys), projects and per-project agent conversations: the Tauri app data directory (`projects/<id>/`, `chats/<id>.json`, `devices.json`, `providers.json`).

Provider requests may go to any HTTPS URL, or HTTP on `localhost`/`127.0.0.1`. Device requests are HTTPS only, with redirects disabled so credentials are not forwarded. Mutating device requests are never retried automatically; after a timeout, refresh before trying again.

## Protocol references

- BLE Binding v1 and credential URL rules follow GizOS `libs/gizclaw/include/h2_gizclaw_ble_binding.h`.
- Package rules: GizOS [`libs/lua/app_package.py`](https://github.com/GizClaw/gizos/blob/becc10c99a0b66d7d9430411c148c63c4500dc73/libs/lua/app_package.py) and [`h2_lua_apps.c`](https://github.com/GizClaw/gizos/blob/becc10c99a0b66d7d9430411c148c63c4500dc73/libs/lua/apps/src/h2_lua_apps.c).
- Device HTTP: GizClaw [`api/http/peer.json`](https://github.com/GizClaw/gizclaw/blob/666ce64d8b1bb3e127de85e331c6a2c298ba1b65/api/http/peer.json): `lua-app/install`, `tool/v0/invoke` (`lua.app.list`, `lua.app.run`), `mhs/v0/{manifest,read,write}`, `runtime`, `status`.

## Limitations

- Direct install is limited to 512 KiB compressed; larger packages need a device-reachable URL, which GizMoth does not host.
- Project payloads are UTF-8 text only; binary assets are not supported yet.
- The agent only knows standard Lua; GizOS device APIs for Lua are not described to it yet.
- Real-device interoperability (BLE binding, install, run, controls) still needs hardware verification on each platform.

## License

[MIT](LICENSE)
