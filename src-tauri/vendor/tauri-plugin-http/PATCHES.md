# Local HTTP response cancellation patch

## Upstream origin

This directory vendors the published **tauri-plugin-http 2.8.3** Rust crate from
[crates.io](https://crates.io/crates/tauri-plugin-http/2.8.3). Its packaged
`.cargo_vcs_info.json` identifies upstream commit
[`44293a5722efcaf27060504e63b17aeba721b40f`](https://github.com/tauri-apps/plugins-workspace/tree/44293a5722efcaf27060504e63b17aeba721b40f/plugins/http)
in `tauri-apps/plugins-workspace`, directory `plugins/http`.

The published normalized `Cargo.toml`, original manifest, source, build script,
global API script, permissions, and original MIT/Apache-2.0 licenses are retained.
The application selects this copy through `[patch.crates-io]` in
`src-tauri/Cargo.toml`; the JavaScript package remains unchanged.

## Local change

Only `src/commands.rs` differs from the published Rust source:

- Each response owns a mutex and a sticky Tokio watch cancellation flag.
- Body reads select cancellation against acquiring the response and awaiting the
  next chunk. This also cancels a completely idle SSE read without another chunk.
- `fetch_cancel_body` signals cancellation before removing its resource. The
  resource's `close` hook also signals readers retaining an `Arc`.
- A mutex replaces the upstream unsafe mutable response access.
- Unit tests cover cancellation before subscription, active and queued reads,
  simultaneous ready data/cancellation, normal chunk delivery and EOF, resource
  cleanup, and a silent loopback connection that receives no further data.

Command names, response framing, HTTP scope checks, permissions, TLS behavior,
and public APIs are unchanged. No request/read timeout or heartbeat is added, and
no new dependency is required.

## Validation and removal

From the repository root, run:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked -p tauri-plugin-http --lib
cargo check --manifest-path src-tauri/Cargo.toml --locked --lib
```

The application's native integration suite separately verifies that Stop and
project unmount disconnect a fully silent provider stream. Remove this override
only after an upstream release passes those cancellation checks.
