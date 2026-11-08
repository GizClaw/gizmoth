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
- Switch between English and Simplified Chinese, with light and dark themes.

## Install

Find macOS (`.dmg`) and Windows (`.exe`) installers on [Releases](https://github.com/GizClaw/gizmoth/releases) when available. If no release is available, build from source using the instructions below.

On macOS, use v0.1.1 or later: the initial v0.1.0 bundles have an incomplete resource signature and are rejected by Gatekeeper.

## Development

Install [Node.js 22.13+](https://nodejs.org/), [Rust 1.90+](https://www.rust-lang.org/tools/install) and the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/), then run:

```bash
npm ci
npm run app
```

To build an installer without an updater signing key:

```bash
npm run app:build -- --config src-tauri/tauri.unsigned.conf.json
```

For a browser-only preview, run `npm run dev` and open `http://127.0.0.1:1420/?demo=1`. See the [maintenance guide](docs/maintenance.md) for tests, releases and protocol references.

- `src/`: React interface and coding assistant
- `src-tauri/`: native desktop and device integration
- `docs/gizos/`: bundled [GizOS documentation](docs/gizos/README.md)

## Current limitations

Direct device installs are limited to 512 KiB compressed, and Lua projects do not yet support binary assets. Firmware development and flashing are not implemented. Device interoperability and end-to-end simulator app execution still need further verification.

Projects and conversations are stored locally. Device keys are kept in private app data, provider keys in the system credential store, and AI requests are sent to the configured provider.

## License

[MIT](LICENSE)
