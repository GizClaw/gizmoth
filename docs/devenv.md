# Development environments

**Dev Env** (**开发环境** in Chinese) manages the [GizClaw/gizmoth-devenv](https://github.com/GizClaw/gizmoth-devenv) registry through GizMoth's own native engine. No GizOS `gizos` CLI, Bun or Homebrew is required. The registry owns its [format](https://github.com/GizClaw/gizmoth-devenv/blob/main/docs/format.md) and [lifecycle contract](https://github.com/GizClaw/gizmoth-devenv/blob/main/docs/lifecycle.md).

- **Chips**: one card per chip. Install resolves the SDK, its target dependencies and build tools for this computer. Unfinished SDKs and unsupported hosts remain explicitly unavailable.
- **Environments**: generic tool collections, such as GizOS base host development. A card declares exact dependency versions, installs what is missing and shares matching installations. GizOS currently needs Bazelisk 1.30.0 and Python 3.12.16; a firmware build also needs its target SDK/compiler and platform prerequisites.
- **Toolchains**: compilers, runtimes and build tools. Choose a published version to install. Each installed version has its own row, directory, users, folder action and removal/reinstall controls. **Add local toolchain** registers a folder the user downloaded themselves.

## Versions and dependency ownership

Identity is `(kind, ID, version, platform)`, not just the tool name. Python 3.11.18 and 3.12.16 coexist; two environments can need different versions, and one environment may explicitly require several versions of the same tool. Only identical dependencies are deduplicated. A cycle is rejected using exact version identities.

An installed environment records exact installation UUIDs in `<app data>/devenv/environments/<id>.json`; it may retain display metadata for offline use. The registry's later defaults do not change that lock. Reinstall rewrites environment, chip and installation references to the new payload before retiring the old one.

Installing tools does not overwrite system Python, mutate the user's global PATH or activate every installed version at once. Project-command environment composition and build integration are not implemented yet. Bazelisk installs as `bazelisk` and `bazel`; the consuming project's `.bazelversion` selects the actual Bazel version on first use.

## Registry

The desktop keeps a persistent Git clone of `GizClaw/gizmoth-devenv` at `<app data>/devenv/registry/repository/`. Git must be installed and available on the app's PATH. The first refresh clones `main` with shallow, single-branch history; later refreshes reuse that checkout and run `git pull --ff-only`. Refresh checks the origin, branch and clean working tree, and refuses local edits or diverged history without overwriting them. Git runs without interactive prompts or repository hooks, with bounded output and a two-minute deadline per command.

The index, environments, chips, SDKs and toolchain releases are read from one immutable local commit through a bounded Git archive. Validation rejects unindexed entries, unsafe paths, invalid source pins, unknown protocol fields and an unexpected repository origin. Only a fully validated first clone is published. The last validated JSON snapshot remains in `<app data>/devenv/registry/` for fast startup and offline use, including migration from earlier versions; failed pulls or invalid updates preserve that snapshot across restarts. Automatic refresh runs at most once per app session if the snapshot is missing or older than 12 hours; the titlebar refresh updates the local clone explicitly.

An ordinary browser can browse real registry metadata without `?demo=1`, using the same pinned commit on `raw.githubusercontent.com`. It shows all published tool versions and environment dependencies, retains the last successful page-session catalog, and supports refresh/retry. Native installation and local-folder operations stay desktop-only. Explicit `?demo=1` uses a separate simulated fixture for interactive UI tests.

## Installation

One background job runs at a time and survives page navigation. Phases, byte progress and logs are available on each Dev Env page. HTTP header and body waits have bounded timeouts and respond to cancellation.

1. Resolve all exact dependencies from one snapshot. Reuse matching healthy managed or registered local installations. Never substitute another version because its executable has the same name.
2. Download file sources into private temporary files. Check expected size and SHA-256 before publishing to `<app cache>/devenv/downloads/sha256/<digest>`; rehash cached sources before reuse.
3. Create a fresh, owned `<app data>/devenv/installs/<sdks|toolchains>/<id>/<version>/<platform>/<installation-id>` directory. Reject linked ancestors or a directory belonging to another installation.
4. Materialize payloads and run file checks. Publish each ready installation record under `records/<UUID>.json`, then commit the chip/environment's exact root references last.
5. Failure or cancellation removes the unfinished payload. Successfully completed dependencies remain available for later reuse.

The engine executes verified file sources, `copy`, bounded `tar.gz` extraction and file checks. Extraction preserves executable permissions and valid contained links while rejecting absolute/traversal paths, escaping links, special devices and file/case collisions. Compressed metadata, member count, per-file size and total expanded bytes are bounded. Normal Unicode and space-containing vendor filenames remain supported. Other archive formats, Git checkout, arbitrary installer commands and command checks are refused before mutation.

Python uses pinned [python-build-standalone](https://github.com/astral-sh/python-build-standalone) release 20261110 distributions. Versions 3.11.18 and 3.12.16 have independent managed prefixes; the registry includes macOS, Linux and Windows x86_64/arm64 metadata. Native installation qualification here is macOS arm64 only.

## Install location and reinstall

Installed chip cards show their SDK's full path. Each installed toolchain version has a full-width location row and **Open folder**. Native folder opening takes an installation UUID, resolves a local record and checks managed path ownership before invoking the system file manager. Registered local toolchains can be opened too.

**Reinstall** uses the exact recorded version and original release-manifest digest. It checks a fresh directory before updating references and removing the previous payload; failure or cancellation leaves the old installation intact. A chip repair reinstalls its SDK and keeps shared toolchains. Manual edits in the old managed directory are removed after successful replacement. If a matching supported recipe is absent or changed, the same-version repair is refused instead of silently installing another release.

Local toolchains are never reinstalled or deleted by GizMoth. Their registration can be removed without changing their files.

## Removal and recovery

Installed chips, generic environments and explicitly requested installations are retention roots. Everything they reach is kept. Removing a chip or environment releases only managed dependencies nothing else still needs. Different versions are independent: retaining Python 3.12 does not retain or delete Python 3.11 automatically. An in-use toolchain cannot be removed; the preview names its users.

Every removal previews exact paths and retained components. Confirmation sends the installation IDs back; the backend recomputes the plan under admission and refuses a changed result. Replacement/removal transactions replay before graph validation after an interruption, so partial record deletion does not strand the store. Retired payloads are moved into owned trash and cleaned; unfinished unrecorded installations are recovered after launch. Ordinary removal preserves the download cache and never removes user projects or local toolchain folders.

## Code and validation

- `src-tauri/src/devenv/registry.rs`: pinned registry parsing and validation; `registry/git.rs`: persistent clone and fast-forward updates
- `plan.rs`: exact-version dependency graphs and host/operation support
- `store.rs`: local locks, retention roots, safe paths and replayable transactions
- `install.rs`: jobs, verified downloads and publication
- `extract.rs`: bounded, contained tar.gz extraction
- `src/pages/DevEnv.tsx`: Chips, Environments and Toolchains UI
- `src/devenv-browser.ts`: read-only public catalog; `src/demo-devenv.ts`: explicit simulated fixture

Offline coverage runs with the frontend suite and `cargo test --lib devenv`. The ignored live tests fetch the real registry, install/reinstall Bazelisk in a temporary store, install both Python versions, run each interpreter in isolated mode with a fixed version/SSL probe, install GizOS by exact dependency reuse and check removal retention.

```bash
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib devenv -- --ignored
```

The native macOS catalog flow was also exercised in a real isolated app: first clone, refresh using the same repository, failed refresh with an unreachable per-repository proxy, and restart with the cached catalog. After building the native fixture, rerun it with:

```bash
GIZMOTH_LIVE_DEVENV_TEST=1 node --test --test-concurrency=1 test/integration/devenv.test.mjs
```

Packaged installer UI, native file-manager/folder-picker flows, and Windows runtime/filesystem behavior were not exercised locally.
