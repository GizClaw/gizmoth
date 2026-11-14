# Compiling in Linux VMs

GizMoth's service runs builds in Linux virtual machines so projects with Linux-only scripts and toolchains build the same way on every Mac. **Compiling** (**编译** in Chinese) is a sidebar group with three entries: **Builds** (slots, queue and logs), **Caches** and **Artifacts**. The Builds and Artifacts entries show the running/queued build and artifact counts. **Settings → Compiling** (**设置 → 编译**) shows the Linux image and development packages, which download and update themselves, plus VM, build-environment and artifact-retention settings. External coding agents submit builds over a WebSocket and share the caches and artifacts.

Phase 1 supports macOS on Apple silicon. Guests are arm64 Ubuntu; x86-only toolchains are not emulated.

## How it works

```text
Agent ── compile_pipe (CLI/MCP) ──► WebSocket /v1/compile/ws ──► GizMoth service (port 7420)
                                                                   ├─ scheduler: N slots + FIFO queue
                                                                   ├─ cache store  <working directory>/cache/
                                                                   ├─ artifacts    <working directory>/artifacts/
                                                                   └─ one OpenVMM process per running slot
                                                                        ├─ system.img  (shared, read-only)
                                                                        ├─ work.img    (this slot's /work)
                                                                        └─ hv_sock: agent control + cache relay
```

- **OpenVMM** (Microsoft, MIT) runs each VM through Hypervisor.framework. GizMoth builds a pinned commit from source and ships it as the `openvmm` sidecar, signed with `com.apple.security.hypervisor`.
- OpenVMM's Hypervisor.framework backend has no PCI MSI delivery, so VMs use Hyper-V vmbus devices: storvsc disks, netvsc networking through OpenVMM's user-mode NAT (consomme, DHCP) and hv_sock. The guest kernel command line blacklists the built-in virtio vsock transport so hv_sock owns guest-to-host vsock.
- The guest kernel is OpenVMM's published arm64 test kernel (Linux 6.18, openvmm-deps 0.3.0-185), downloaded and checked by SHA-256.
- **gizmoth-compile-agent** (`native/compile-agent`, static aarch64 musl) is PID 1 in every VM. GizMoth writes a fresh initramfs per boot containing the agent and `/config.json`. PID 1 only reaps processes; the forked agent serves vsock port 1000 and relays guest `127.0.0.1:7421` to the host cache.

### System image

The image's apt packages are fixed by GizMoth (`IMAGE_PACKAGES`: build-essential, ccache, CMake, Ninja, Git, Python, erofs-utils and other base tools); users do not edit them. Toolchains come from the development packages instead. Building boots an installer VM: it formats a new 24 GiB sparse disk, unpacks Ubuntu 26.04.1 base, runs `apt-get install`, adds the `builder` user (uid 1000) and installs Bazelisk 1.30.0 as `bazel`. All downloads are pinned by SHA-256. The new image replaces the old one only after success; warm slots restart on it before their next build.

Once compiling is in use (an image or packages exist, a build was submitted, or **Prepare build environment** was pressed in Settings → Compiling), the service builds a missing image by itself and rebuilds one a GizMoth update no longer matches as soon as no build is running (failed attempts retry after 30 minutes; **Rebuild image** retries now). The optional Ubuntu package source (for example `http://mirrors.ustc.edu.cn/ubuntu-ports`) replaces `ports.ubuntu.com`; it is only a download source, so changing it does not rebuild the image.

Worker VMs mount the image read-only (`noload`, so several VMs can share it) under a tmpfs overlay: package changes made by a build vanish at power-off.

### Development packages

Every linux-arm64 release published by the [GizClaw/gizmoth-devenv](https://github.com/GizClaw/gizmoth-devenv) registry is a development package; nobody picks them. The service keeps the registry in a Git checkout (`<app data>/compile/registry/`, refreshed every 12 hours or with **Check for updates**; Git must be installed), downloads each release's sources into `<working directory>/assets/<sha256>` with size and SHA-256 checks, forgets releases the registry drops or changes, and lists them in `packages.json`. Today the registry publishes Python 3.11.18/3.12.16 and Bazelisk 1.29.0/1.30.0 for Linux; chip SDKs and the Arm/Xtensa toolchains are still planned there and will be picked up when published. Releases with Git sources, `run` steps or archive formats other than tar.gz are listed as not supported yet.

Packages are unpacked in Linux, not on the Mac: Linux trees can contain names that differ only by case (Python's terminfo database, kernel headers such as `xt_CONNMARK.h`/`xt_connmark.h`), which a case-insensitive APFS volume cannot hold. A packer VM boots the shared system image, reads the downloads over a read-only 9P share (see below), extracts and checks each release on a scratch disk, and writes one lz4hc-compressed erofs filesystem, `packages.erofs`. The pack is rebuilt whenever the downloaded set changes; a release that fails to install leaves the next pack and is retried after 30 minutes.

Every slot attaches the latest pack read-only at `/opt/gizmoth/packages/<toolchains|sdks>/<id>/<version>/`, with `index.json` at its root. The registry's default versions are first on `PATH` and their environment variables are set (`${cache}` becomes `/work/home/.cache`); other versions are available at their paths. Builds also get `GIZMOTH_PACKAGES=/opt/gizmoth/packages` and `PYTHONPYCACHEPREFIX=/work/home/.cache/pycache`, because Python cannot write bytecode next to read-only sources. Idle VMs restart on a new pack before their next build. In a measurement on an M-series Mac, compiling from the pack ran as fast as from a copy on the work disk (100 sequential small `arm-none-eabi-gcc` compiles: 0.49 s from erofs, 0.53 s local, 1.45 s over 9P).

### 9P shares

OpenVMM has no virtio-fs on macOS, so GizMoth serves host folders with its own read-only 9P2000.L server (`p9.rs`): the guest agent connects to a vsock port (guest connections to host port P arrive on `<socket>_P`) and the kernel's v9fs client mounts that connection with `trans=fd`. The server never follows host symlinks, shows every file as owned by root and refuses every change. The packer uses it for the downloads; build slots do not use it today.

### Slots

A slot is one VM and therefore one concurrent build. Each has CPUs, memory and a persistent sparse work disk mounted at `/work`:

| Path | Contents |
| --- | --- |
| `/work/src/<workspace key>` | Synchronized copy of a host workspace |
| `/work/bazel` | Bazel `output_user_root` (output bases, execroot) |
| `/work/bazel-repo-cache` | Bazel repository cache |
| `/work/ccache` | Local ccache tier (size configurable) |
| `/work/home` | `$HOME` of `builder`, including Bazelisk downloads |

A VM boots on demand in about 5 seconds and stays warm for the idle timeout (10 minutes by default), keeping the Bazel server and its analysis cache. Builds prefer warm slots. Resources can change only while a slot is stopped; the disk size only after **Reset disk**, which deletes the work disk.

### Workspace synchronization

The host walks the workspace, compares size, modification time and mode with what the guest recorded after the previous sync, and sends only changed files, symlinks and directories plus deletions. Top-level `bazel-*` links and `.DS_Store` are skipped; settings and requests add exclude globs. Outputs stay in the VM unless collected as artifacts.

### Shared caches

Caches live on the host disk, so every slot and every VM boot shares them, and resetting a work disk does not lose them. Each VM reaches them through its own vsock socket; no network port is opened.

| Cache | Guest configuration | Key format |
| --- | --- | --- |
| Bazel | `/etc/bazel.bazelrc`: `build --remote_cache=http://127.0.0.1:7421/bazel` | Bazel HTTP cache (`/ac/`, `/cas/`; CAS uploads are verified) |
| ccache | `/etc/ccache.conf`: `remote_storage = http://127.0.0.1:7421/ccache`, local tier in `/work/ccache`, `/usr/lib/ccache` first in `PATH` | ccache HTTP storage |
| Custom | The configured environment variable receives `http://127.0.0.1:7421/<name>` | Any `GET`/`PUT`/`HEAD`/`DELETE` keys |

Downloads use the VM's NAT network directly, not host proxy settings. Mirrors are configured through the build environment, for example `BAZELISK_BASE_URL=https://mirrors.huaweicloud.com/bazel`.

Caches are always on. Each has a size limit with least-recently-used eviction to 90%. The page shows size, entries, hits, misses and uploads, and can clear a cache. Cache and boot settings apply when a slot next boots; idle slots restart automatically. ccache's built-in HTTP backend is deprecated upstream; Ubuntu 26.04's ccache still includes it.

## Build WebSocket: `gizmoth.compile.v1`

Discover the URL, headers and request schema (read-only; does not start the service):

```bash
gizmoth --tool compile_pipe '{"client":"coding-agent","title":"Build firmware"}'
```

Connect to `ws://<service>/v1/compile/ws?client=<name>&title=<task>` with `Authorization: Bearer <API key>` (Settings → Service). The server sends `{"type":"hello","protocol":"gizmoth.compile.v1","commands":[...]}`. Send one build request:

```json
{"id":"build-1","method":"build","params":{
  "workspace":"/Users/me/firmware",
  "command":["bazel","build","//app:firmware"],
  "artifacts":["bazel-bin/app/*.update.tar.zlib"],
  "env":{"GIZOS_BOARD":"demo"},
  "timeoutSeconds":3600
}}
```

| Parameter | Meaning |
| --- | --- |
| `workspace` | Absolute directory on the service computer; local connections only |
| `command` | argv run as `builder`; use `["bash","-lc","…"]` for shell syntax |
| `cwd` | Relative directory inside the workspace |
| `env` | Extra environment variables |
| `artifacts` | Relative globs (`*`, `?`, `**`) collected after success |
| `exclude` | Extra workspace globs not synchronized |
| `timeoutSeconds` | Stops the command with SIGINT, SIGTERM, then SIGKILL |
| `collectOnFailure` | Also collect artifacts after a failed command |

The reply is a `result` with the job summary. Events follow as JSON text frames: `queued` (with `position`), `status` (`booting`, `syncing`, `running`, `collecting`), `started` (slot), `synced`, `exit`, `artifact` and finally `finished` with the job and artifact; the server then closes the socket. Binary frames carry the combined log. `lagged` reports skipped events for a slow reader; read the full log with `compile_job`.

**Closing the socket cancels the build**, also while it waits in the queue. `{"method":"cancel"}` does the same; `{"method":"status"}` returns the summary. One build per socket: open several sockets for parallel builds, which run on free slots in FIFO order. Every build gets the environment `GIZMOTH_SLOT`, `GIZMOTH_JOB_ID` and `GIZMOTH_WORKSPACE`.

## Artifacts

Collected files are stored read-only under `<working directory>/artifacts/<id>/files/` with `artifact.json` (job, client, title, workspace, command, SHA-256 per file). Every agent can use them:

```bash
gizmoth --tool compile_artifacts '{"limit":10}'
```

To flash an artifact, send `install` with its ID on a granted device WebSocket. This also works for LAN clients, which cannot send local paths:

```json
{"id":"install-1","method":"install","params":{"artifact":"<artifact id>"}}
```

Add `file` when the artifact contains several `.update.tar.zlib` files. Under Compiling → Artifacts, artifacts can be pinned, revealed in Finder, installed on a serial port or deleted. Retention keeps the newest unpinned artifacts within the configured count and size.

## CLI and MCP tools

| Tool | Result |
| --- | --- |
| `compile_status` | Availability, image, slots, queue, recent builds and cache usage |
| `compile_pipe` | WebSocket URL, headers and request schema |
| `compile_artifacts` | Artifacts with absolute file paths (optional `workspace`, `limit`) |
| `compile_job` | One build's summary and a log page (`after`/`next`) |

## Desktop API

The Compiling pages use the service's app-only routes under `/v1/compile`: `GET /v1/compile` (snapshot), `GET /v1/compile/watch?since=`, `PATCH /v1/compile/settings`, `POST|PATCH|DELETE /v1/compile/slots[/{id}]`, `POST /v1/compile/slots/{id}/stop|reset`, `POST /v1/compile/image/build|cancel`, `GET /v1/compile/image/log`, `DELETE /v1/compile/caches/{name}`, `GET|PATCH|DELETE /v1/compile/artifacts[/{id}]`, `GET /v1/compile/jobs/{id}[/log]`, `POST /v1/compile/jobs/{id}/cancel`, `POST /v1/compile/prepare` (start downloading the image and packages), `POST /v1/compile/packages/refresh` (check the registry now and retry failures) and `POST /v1/compile/storage[/cancel]` (`{"path": "/Volumes/…"}` or `{"default": true}`). Reads are available to Gateway callers; changes are not.

## Working directory

**Settings → Storage** (**设置 → 存储**) chooses GizMoth's working directory, which holds the system image, slot work disks, shared caches, artifacts and pinned downloads. It defaults to `<app data>/compile`; choose an empty folder (for example on an external disk) to move it. The page lists the working directory and, one row each, the folder of every kind of data (system image, slot work disks, shared caches, artifacts, downloads) with its size and a Finder button, plus free space and the progress of a move. Compiling → Caches links to it.

- A move needs idle builds: queued or running builds, image builds and package downloads or packing return 409. Idle VMs stop first, and builds submitted during the move wait in the queue.
- On the same volume every item is renamed. On another volume it is copied with holes preserved (`SEEK_DATA`/`SEEK_HOLE`), so a 64 GiB work disk copies only what it has written; the old copy is deleted after success. A failed or cancelled copy removes the partial copy and keeps the original location.
- `storage.json` in `<app data>/compile` records a custom location; moving back to the default removes it. The new folder gets a `.gizmoth-working-directory` marker.
- If the chosen disk is not connected when the service starts, GizMoth does not recreate the folder on the startup disk: builds fail with a message, and Storage offers to choose another folder (which then starts empty).

Settings, projects and development environments stay in the app data folder.

## Data layout

```text
<app data>/compile/
  settings.json
  storage.json               custom working directory, if any
  registry/                  development package registry checkout and snapshot
<working directory>/          <app data>/compile by default
  assets/<sha256>            pinned and package downloads; <kernel sha>.Image extracted
  packages.json              downloaded development packages
  packages.erofs             packed packages, attached read-only to every slot (+ .json)
  image/system.img           shared system disk, image.json
  slots/<id>/work.img        per-slot disk, console.log, console.vmm.log
  cache/<namespace>/…        shared caches
  artifacts/<id>/            artifact.json and files/
/tmp/gizmoth-<uid>/<slot>.vsock[_7421]   private vsock sockets (path-length limit)
```

## Building and bundling

```bash
rustup target add aarch64-unknown-linux-musl
npm run compile-agent:build
```

```bash
brew install protobuf
npm run openvmm:build
```

The first command stages `src-tauri/resources/compile-agent/gizmoth-compile-agent`; the second clones the pinned OpenVMM commit into `~/.cache/gizmoth/openvmm` (or uses `OPENVMM_SOURCE`), builds it, strips debug info and stages `src-tauri/binaries/openvmm-aarch64-apple-darwin` with the Hypervisor entitlement. Development builds find both there. Release bundles add them with `--config src-tauri/tauri.compile.conf.json` on `aarch64-apple-darwin` (see the release workflow). `GIZMOTH_OPENVMM` and `GIZMOTH_COMPILE_AGENT` override the paths.

## Code

- `src-tauri/src/compile/mod.rs`: settings, slots, scheduler, snapshot and HTTP API
- `jobs.rs`: build execution, log ring, artifact collection; `ws.rs`: the WebSocket protocol
- `vm.rs`: OpenVMM processes; `image.rs`: system image builds; `assets.rs`: pinned downloads and initramfs
- `sync.rs`: workspace scanning and diffing; `cache.rs`: cache store and HTTP server; `artifacts.rs`: artifact store
- `storage.rs`: working-directory choice, validation and sparse-aware moves
- `packages.rs`: registry upkeep, package downloads and packing; `p9.rs`: read-only 9P server
- `src-tauri/src/devenv/registry.rs`: registry parsing, validation and the Git checkout
- `native/compile-agent/src/pack.rs`: the packer VM
- `native/compile-agent/src/protocol.rs`: frame and message definitions shared by host and guest
- `src-tauri/src/mcp/compile.rs`: CLI/MCP tools

## Verification and limits

Verified on an M-series Mac (macOS 27) through an isolated service and the CLI tools: the first build downloading the pinned assets and building the image (about two minutes in total), slot boot in about 5 seconds, incremental sync, `make`, a Bazel 8 build whose rebuild after **Reset disk** reported remote cache hits from the host cache, artifact collection, two parallel slots with a queued third build, cancellation by closing the socket, settings changes restarting idle slots, and VMs stopping with the service. Unit tests cover the protocol, sync diffing, cache server, artifact store, settings, request validation and working-directory moves (rename, sparse copy, cancellation, restart). Installing an artifact on a physical device was not exercised (no H2Loader board attached).

The opt-in native test drives the real desktop app with isolated app data against a real VM: it opens the Compiling pages, adds a slot, follows an agent's build live through its log and artifact, checks cancellation when the agent's socket closes, pins the artifact, visits Caches and Settings → Compiling, moves the working directory and builds again from it. Build the native test app (as `npm run test:tauri` does), then run:

```bash
GIZMOTH_LIVE_COMPILE_TEST=1 node --test --test-concurrency=1 test/integration/compile.test.mjs
```

`GIZMOTH_E2E_COMPILE_SEED=<app data>/compile` reuses another service's downloaded assets and system image through APFS clones instead of building a new image. The fixture's service never listens on the LAN and is shut down with its VMs at the end.

Not yet supported: Windows and Linux hosts, Intel Macs, x86 guests or x86-only toolchains, live sharing of host folders into build slots (only the packer uses 9P), and builds from LAN clients (they cannot name host workspaces). Packaged-installer signing of the sidecar has not been exercised locally.
