# Development environments

**Dev Env** in the English sidebar (**开发环境** in Chinese) installs SDKs, toolchains and build tools from the [GizClaw/gizmoth-devenv](https://github.com/GizClaw/gizmoth-devenv) registry. Its [format](https://github.com/GizClaw/gizmoth-devenv/blob/main/docs/format.md) and [lifecycle rules](https://github.com/GizClaw/gizmoth-devenv/blob/main/docs/lifecycle.md) are defined there; this page describes GizMoth's implementation. GizMoth does not use the GizOS `gizos` CLI, Bun or Homebrew.

- **Chips**: one card per chip. Install resolves the chip's SDK, its toolchains and build tools to exact versions for this computer and installs what is missing. Cards that cannot be installed say why: recipe pending, unsupported computer, a missing host program, or a recipe that needs a newer GizMoth.
- **Toolchains**: a table of installed toolchains and the rest of the catalog. Install one directly, or **Add local toolchain** to register a folder you downloaded yourself.

## Registry

Refresh resolves `main` through the GitHub API to a full commit, downloads that commit's archive from `codeload.github.com`, and reads only the indexed JSON files. Unindexed, non-JSON or linked entries under `chips/`, `sdks/` and `toolchains/` reject the whole snapshot, as do unknown fields, unsafe paths, non-HTTPS or credential-bearing URLs, malformed digests and a repository other than `GizClaw/gizmoth-devenv`. A valid snapshot is cached as `<app data>/devenv/registry/<commit>.json`; a failed refresh keeps the previous one. The pages refresh automatically at most once per app session, when there is no snapshot or it is older than 12 hours. Installed environments never depend on the snapshot.

An ordinary browser preview can browse the real catalog without `?demo=1`: it resolves the same GitHub repository to a commit and reads its indexed chip, SDK and toolchain metadata from `raw.githubusercontent.com` at that commit. It caches the last successful catalog for the page session and offers refresh/retry on failure. This path displays metadata only; it never executes recipes, simulates installations or accesses the native bridge. Install and local-toolchain actions remain desktop-only. Explicit `?demo=1` retains the in-memory simulated workflow for UI testing.

## Installation

One job runs at a time and continues while you use other pages. Its phases, byte progress and log appear on both pages. Waiting for HTTP response headers and body chunks has a 60-second timeout and observes cancellation, so an unresponsive download cannot hold the installation slot indefinitely.

1. Every package is resolved from one snapshot. An existing installation with the same ID, version and platform, including a registered local toolchain, is reused.
2. File sources are downloaded over HTTPS (redirects must stay HTTPS) to a temporary file, limited to the declared size, and checked against the declared SHA-256 before entering the cache at `<app cache>/devenv/downloads/sha256/<digest>`. Cached files are rehashed before reuse.
3. Each package gets a new directory, `<app data>/devenv/installs/<sdks|toolchains>/<id>/<version>/<platform>/<installation-id>`. Recipe file checks must pass before its record is written to `records/<installation-id>.json`. A chip's `chips/<chip>.json` is written last.
4. A failed or cancelled package's directory is removed. Packages that completed are kept and reused next time.

This version executes file sources, `copy` steps and `file` checks. Recipes that need `extract`, `checkout`, `run`, Git sources or command checks are refused before any work, with "requires a newer GizMoth". Host programs listed in `requires` must be on `PATH`. The Bazelisk recipe is the only published release today; it installs `bin/bazelisk` and `bin/bazel`, and Bazel itself is downloaded by Bazelisk when a project first uses it.

Composing a chip's process environment and running project builds with it is not implemented yet.

## Install location and reinstall

Installed chip cards show the SDK's full directory. Every installed toolchain has a separate install-location row below its table entry so the complete path stays readable. **Open folder** opens that recorded directory in the system file manager. The native command takes an installation ID, resolves it through local records and verifies managed path containment; the frontend cannot submit an arbitrary path. Registered local toolchain folders can be opened too.

**Reinstall** keeps the exact installed version, creates and verifies a fresh managed directory, and replaces the old installation only after success. Failure or cancellation preserves the old installation. A chip reinstall replaces its SDK and keeps shared/local toolchains. Reinstalling a shared toolchain updates its dependent installation and chip records to the replacement before the displaced directory is removed. Files manually changed in the old managed directory are lost after successful replacement. A matching supported release must exist in the current registry, and its manifest digest must match the original record; unsupported or changed recipes cannot silently reinstall a different payload. The confirmation dialog shows the current path before starting the background job.

Local toolchains are never reinstalled by GizMoth: they can be opened or unregistered, and their files stay untouched. Deleting a managed installation and installing it again from the catalog remains available as a separate flow.

## Removal

Removal uses local records only, so it works offline and after the registry changes. Installed chips and toolchains you installed or registered are roots; everything they reach is kept.

- Removing a chip deletes its record and the installations nothing else still needs.
- Removing a toolchain is refused, with the reason, while a chip or another installation uses it. Otherwise it is deleted together with dependencies nothing else needs.
- A local toolchain is only unregistered. GizMoth never writes to or deletes its folder, and never registers its own managed directories as local.

The dialog previews the exact directories to delete and keep. Confirmation sends that list back; the backend recomputes the plan and refuses if it changed. Each managed directory must resolve inside `installs/` and match its record before it is moved to `trash/` and deleted. Directories without a record, left by an interrupted install, and leftover trash are cleaned on the first page load after launch. The download cache is kept.

## Code and tests

- `src-tauri/src/devenv/registry.rs`: fetching, archive extraction and validation
- `src-tauri/src/devenv/plan.rs`: dependency resolution, host support and supported operations
- `src-tauri/src/devenv/store.rs`: local records, removal planning and execution, recovery
- `src-tauri/src/devenv/install.rs`: jobs, verified downloads, copying and file checks
- `src/pages/DevEnv.tsx`: the Chips and Toolchains pages; `src/demo-devenv.ts` is the browser demo (`?demo=1`)

`cargo test --lib devenv` runs the offline tests. Two opt-in tests use the network: `devenv::registry::tests::live_registry_validates` fetches and validates the published registry, and `devenv::install::tests::live_bazelisk_install_and_removal` installs the real Bazelisk release into a temporary directory and removes it.

```bash
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib devenv -- --ignored
```

Not yet verified in the packaged desktop app: the native page flows, the folder picker, and Windows filesystem behavior.
