# Maintenance

## Validation

Run `npm run build`, `npm test` and `cargo check --manifest-path src-tauri/Cargo.toml --locked` for frontend and Rust checks. `npm run test:tauri` builds and runs the isolated native integration suite; `npm run test:tauri:run` reuses its existing test build. The suite uses temporary app data and a local mock model, not real devices or provider credentials. CI runs native integration tests on macOS and Windows; see the [CI workflow](../.github/workflows/ci.yml) for the full matrix.

## Releases

The [Release workflow](../.github/workflows/release.yml) accepts an existing stable `vX.Y.Z` tag, which must match the versions in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`. Configure `TAURI_SIGNING_PRIVATE_KEY` and, if the key is encrypted, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The private key must match the public key pinned in `tauri.conf.json`; do not replace it for each release or commit it. Optional macOS and Windows code-signing credentials are listed in the workflow.

The workflow builds signed updater artifacts for macOS ARM/Intel and Windows, verifies their signatures, and uploads `latest.json` last to a draft GitHub release. Publication is a separate manual step. Existing releases and moved tags are rejected. Ordinary CI builds do not produce signed updater artifacts.

## GizOS documentation

The [bundled guides](gizos/README.md) are shared by the offline reader and the built-in documentation MCP. Run `npm run docs:gizos:bundle` after editing them. The guide also describes how to update the pinned sources. Other MCP clients can run the same server over stdio using `node --experimental-strip-types` with the absolute path to `scripts/gizos-docs-mcp.mjs`.

## Protocol references

- BLE binding and credential links: GizOS `libs/gizclaw/include/h2_gizclaw_ble_binding.h`.
- Lua packages: GizOS [`app_package.py`](https://github.com/GizClaw/gizos/blob/becc10c99a0b66d7d9430411c148c63c4500dc73/libs/lua/app_package.py) and [`h2_lua_apps.c`](https://github.com/GizClaw/gizos/blob/becc10c99a0b66d7d9430411c148c63c4500dc73/libs/lua/apps/src/h2_lua_apps.c).
- Device HTTP: GizClaw [`api/http/peer.json`](https://github.com/GizClaw/gizclaw/blob/666ce64d8b1bb3e127de85e331c6a2c298ba1b65/api/http/peer.json).
