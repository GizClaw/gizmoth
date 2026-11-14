# Compiling in Linux VMs

GizMoth runs builds in Linux virtual machines on Macs with Apple silicon. **Compiling** in the sidebar contains **Builds** (slots, queue and logs), **Caches** and **Artifacts**. External coding agents submit builds over a WebSocket and share the caches and artifacts. **Settings → Compiling** starts with the shared VM resource settings, followed by boot resources, download management and artifact retention; **Settings → Storage** chooses the working directory.

The app ships only OpenVMM, an ARM64 Linux kernel and a static guest agent. Linux userland, shells, build tools, SDKs and optional compatibility tools come from [GizClaw/gizmoth-devenv](https://github.com/GizClaw/gizmoth-devenv) formulae and are downloaded and managed in GizMoth. There is no bundled Ubuntu distribution or `system.erofs`.

## Boot environment

```text
Agent ── compile_pipe ──► WebSocket /v1/compile/ws ──► GizMoth service
                                                      ├─ FIFO queue + N slots
                                                      ├─ shared cache and artifact stores
                                                      └─ OpenVMM per running slot
                                                           ├─ Linux ARM64 kernel
                                                           ├─ initramfs: static agent + config
                                                           ├─ packages.erofs: downloaded userland/tools, read-only
                                                           ├─ /src: host source, read-only 9P
                                                           └─ work.img: build copy, home, caches, outputs
```

- **OpenVMM** (Microsoft, MIT) runs VMs through Hypervisor.framework. GizMoth builds a pinned revision and ships it as the `openvmm` sidecar, signed with `com.apple.security.hypervisor`.
- The Hypervisor.framework backend uses Hyper-V vmbus devices: storvsc disks, netvsc networking through OpenVMM's user-mode NAT (consomme, DHCP) and hv_sock. The guest command line disables virtio vsock so hv_sock owns the guest-to-host transport.
- **Linux kernel**: OpenVMM's published ARM64 test kernel, currently Linux 6.18 from openvmm-deps 0.3.0-185. `scripts/linux-kernel.json` pins the original archive URL and SHA-256. Staging verifies the archive, extracts only its `Image` member and checks the ARM64 image header.
- **gizmoth-compile-agent** (`native/compile-agent`, static aarch64 musl) boots as PID 1 from a fresh initramfs containing the agent and configuration. It mounts downloaded userland, starts the worker, reaps processes and relays the guest cache endpoint to the host over vsock.

The bundle resources are `linux-kernel/{Image,kernel.json}` and `compile-agent/gizmoth-compile-agent`; the kernel is about 22 MB. The boot environment card reports kernel and agent size, kernel version, staging time and source commit when present. For API compatibility, this metadata still uses the snapshot's `image` key: `base` is `Linux kernel + static agent`, `bazelisk` is null and `packages` is empty.

Earlier versions built Ubuntu images in the working directory. Startup removes only recognized legacy image files and the two pinned old kernel/Ubuntu archives; active formula downloads and packed runtime data are preserved.

## GizMoth formulae and download management

GizMoth checks the registry's main branch on each service startup and downloads all published Linux formulae automatically unless downloads were paused. Metadata refresh remains active while payload downloads are paused or VM boot resources are unavailable; a cached catalog stays usable offline. There is no per-tool selection. A formula declares versions, upstream source downloads and their integrity checks, environment configuration, and installation/build steps. Root formulae supply Linux userland at `/`; other formulae install versioned tools under `/opt/gizmoth/packages/`. The registry identities remain `toolchain` and `sdk`; `guestRoot` marks a formula that contributes to the root filesystem and `bootstrap` identifies bootstrap inputs.

The service keeps the registry checkout under `<app data>/compile/registry/`, checks for updates periodically or through **Check for updates**, verifies source sizes and SHA-256 digests, and stores raw archives under `<working directory>/assets/`. Formula archives are not unpacked into macOS folders: Linux trees can contain names that differ only by case, which a case-insensitive APFS volume cannot represent.

A packer VM boots the static agent and uses the downloaded bootstrap inputs to assemble Linux userland and tools on a scratch disk. It reads archives through a read-only 9P share and writes `packages.erofs`, containing both the root filesystem and versioned tools. This is downloaded runtime data in the working directory, not an application resource. No prebuilt distribution image or producer CI is required. Worker VMs attach this pack read-only and keep the tool directories read-only; runtime writes belong on the slot's work disk or ephemeral mount points.

**Download management** shows the registry revision, formula versions, archive sizes, download progress, installation errors, guest paths and local archive location. Its controls apply to all formulae:

- **Pause downloads** stops payload downloading and preparation while keeping registry metadata current. Existing packed userland can remain usable for builds.
- **Resume downloads** enables upkeep and resumes preparation of all Linux formulae.
- **Check for updates** refreshes the registry and retries failed preparation while upkeep is enabled.
- **Clear downloaded archives** is available when preparation is idle. It deletes raw archives and their download index, then pauses upkeep. It preserves the packed runtime, slot work disks, caches and artifacts. Resuming can fetch the archives again.

The snapshot exposes `packages.active` (upkeep enabled), `packages.paused`, `packages.busy`, `packages.downloads` (`path` and `bytes`), formula states and the packed runtime state. Clearing archives does not imply deleting or invalidating a usable packed runtime.

Default formula versions are first on `PATH` with their declared environment variables. Other versions remain available by path. Builds receive `GIZMOTH_PACKAGES=/opt/gizmoth/packages` and `PYTHONPYCACHEPREFIX=/work/home/.cache/pycache` so Python does not write next to read-only tools. A changed pack is applied when a slot next boots.

The guest kernel remains ARM64. Support for x86 executables belongs to QEMU user-mode emulators and the matching libraries delivered by formulae, rather than a bundled x86 operating system. Availability depends on the actual registry inputs and guest configuration; an ARM64 boot or successful download alone does not verify x86 execution.

## Read-only source and writable builds

OpenVMM has no virtio-fs on macOS. GizMoth's 9P2000.L server (`p9.rs`) serves source and download folders read-only through vsock; the kernel mounts them with `trans=fd`. The server rejects changes and never follows host symlinks to files outside the share.

A build can read the host workspace directly at `/src`. This source mount is read-only. The normal build command runs in a private writable copy under `/work/src/<workspace key>` so in-tree build systems can generate files without changing the host workspace. Source synchronization updates that copy incrementally; outputs remain in the VM until collected as artifacts. Tools also remain read-only, while home, build directories, caches and output locations are writable.

A slot is one VM and one concurrent build, with a persistent sparse work disk mounted at `/work`. **Settings → Compiling** configures one total vCPU budget and one total guest-memory budget shared by every build and preparation VM. These are aggregate limits, not limits repeated for each slot. Defaults are all host CPUs and half of host RAM. Slot creation asks only for a name and disk size; older per-slot CPU/RAM settings are ignored while identity and disks are preserved.

GizMoth automatically assigns worker capacity from the total budget and configured slot count, with at least one vCPU and 1 GiB per worker. The preparation VM requests up to four vCPUs and 2 GiB from that same budget. A VM reserves capacity before boot and holds it while starting, running, warm and stopping; it returns capacity only after teardown. Work remains queued when insufficient capacity is available. Lowering a budget below already allocated capacity is rejected without saving the smaller value. **Stop idle VMs** releases warm capacity without interrupting active builds.

The memory budget caps combined configured guest-RAM capacity. OpenVMM uses sparse backing with `prefetch=off`, so actual resident memory follows guest accesses; VMM process overhead is additional. This is not an exact hard cap on macOS process RSS, ballooning or live resizing. The settings show allocated capacity rather than measured RAM use. macOS schedules vCPU threads, while build-tool settings still control compilation parallelism.

The writable and read-only paths are:

| Path | Purpose | Access |
| --- | --- | --- |
| `/src` | Host workspace source via 9P | Read-only |
| `/opt/gizmoth/packages` | Versioned formula tools | Read-only |
| `/work/src/<workspace key>` | Private synchronized source/build copy | Writable |
| `/work/bazel` | Bazel output bases and execroot | Writable |
| `/work/bazel-repo-cache` | Bazel repository cache | Writable |
| `/work/ccache` | Local ccache tier | Writable |
| `/work/home` | Build user's home, tool downloads and caches | Writable |

The host compares workspace metadata with the previous sync and sends changed files, symlinks, directories and deletions. The guest checks its actual files, including content digests, so edits or interrupted synchronization do not leave modified inputs marked current. Batches are bounded by frame size and entry count. Top-level `bazel-*` links and `.DS_Store` are skipped automatically; a build request may add project-specific exclusion globs.

Slots boot on demand and stay warm for the idle timeout (10 minutes by default), retaining build-server state. **Automatically stop idle VMs** controls this timer (`autoStopIdle`, enabled by default); disabling it preserves warm VMs until explicitly stopped or invalidated by boot-policy changes. Warm VMs still retain guest memory and shared-budget reservations; **Stop idle VMs** releases them without interrupting active builds. Builds prefer warm slots. Boot changes take effect before the next build: busy slots finish, then restart. CPU/RAM capacity is assigned from the shared budget at VM startup; disk capacity changes only after **Clear work disk**, which deletes that slot's disk. Shared caches, downloads and artifacts survive a reset.

## Shared caches

Caches live on the host disk and survive VM boots and work-disk resets. Each VM reaches them through its own vsock socket at guest `127.0.0.1:7421`; no separate host network port is opened.

| Cache | Guest configuration | Key format |
| --- | --- | --- |
| Bazel | `/etc/bazel.bazelrc`: `build --remote_cache=http://127.0.0.1:7421/bazel` | Bazel HTTP AC/CAS; CAS uploads are verified |
| ccache | `/etc/ccache.conf`: HTTP remote storage, local tier in `/work/ccache` | ccache HTTP storage, when supported by the installed formula |
| Custom | A configured environment variable receives `http://127.0.0.1:7421/<name>` | `GET`/`PUT`/`HEAD`/`DELETE` keys |

Caches are always on and use size limits with least-recently-used eviction to 90%. The page shows size, entries, hits, misses and uploads and can clear individual caches. Counts start at service startup. GizMoth generates Bazel runtime/cache configuration, tool activation variables and synchronization policy. These are not global user-editable settings; legacy `bazelrc`, `env` and `excludes` settings are ignored on load and rejected by settings updates. Project flags belong in the build command or project configuration. Build-specific variables and extra exclusions use the request's `env` and `exclude` fields and do not affect other builds. Dependency downloads use the VM's NAT network; request variables such as `BAZELISK_BASE_URL` can select project-specific mirrors.

## Build WebSocket: `gizmoth.compile.v1`

Discover the URL, headers and request schema without starting the service:

```bash
gizmoth --tool compile_pipe '{"client":"coding-agent","title":"Build firmware"}'
```

Connect to `ws://<service>/v1/compile/ws?client=<name>&title=<task>` with `Authorization: Bearer <API key>` from Settings → Service. Authorization is checked again on submission. API-key rotation or disabling Service revokes sockets and cancels unfinished builds. After the `hello` frame, send one build request:

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
| `workspace` | Absolute workspace on the service computer; local connections only |
| `command` | argv run as `builder` in the writable build copy; use `["bash","-lc","…"]` for shell syntax |
| `cwd` | Relative directory in the build copy |
| `env` | Extra environment variables |
| `artifacts` | Relative globs (`*`, `?`, `**`) collected after success |
| `exclude` | Extra workspace globs omitted from synchronization |
| `timeoutSeconds` | Stops the command with SIGINT, SIGTERM, then SIGKILL |
| `collectOnFailure` | Collect outputs after a failed command too |

A `result` returns the job summary. JSON events report `queued`, `status` (`booting`, `syncing`, `running`, `collecting`), `started`, `synced`, `exit`, `artifact`, and finally `finished`; binary frames carry the combined log. The server then closes the socket. `lagged` reports skipped events for a slow reader; use `compile_job` for the full log.

**Closing the socket cancels the build**, including queued work. `{"method":"cancel"}` cancels and `{"method":"status"}` reads the summary. Open one socket per build; free slots take queued jobs in FIFO order. Builds receive `GIZMOTH_SLOT`, `GIZMOTH_JOB_ID` and `GIZMOTH_WORKSPACE`.

## Artifacts and tools

Collected files are read-only under `<working directory>/artifacts/<id>/files/`, with `artifact.json` containing the source job and SHA-256 per file. All agents can query them:

```bash
gizmoth --tool compile_artifacts '{"limit":10}'
```

To install firmware, send `{"method":"install","params":{"artifact":"<id>"}}` on a granted device WebSocket. Add `file` when several firmware packages exist. Artifact IDs also work over LAN, whose callers cannot submit local paths. Compiling → Artifacts can pin, reveal, install or delete outputs. Retention removes the oldest unpinned artifacts beyond the count or size limit.

| CLI/MCP tool | Result |
| --- | --- |
| `compile_status` | Availability, boot metadata, downloads, slots, jobs and caches |
| `compile_pipe` | WebSocket URL, headers and build schema |
| `compile_artifacts` | Artifact IDs and absolute file paths; optional `workspace` and `limit` |
| `compile_job` | Job summary and log page (`after`/`next`) |

## Desktop API

The app uses routes under `/v1/compile`. External Service callers can read; mutations are app-only.

| Route | Purpose |
| --- | --- |
| `GET /v1/compile`, `GET /v1/compile/watch?since=` | Snapshot and long poll |
| `PATCH /v1/compile/settings` | VM, cache and retention settings |
| `POST|PATCH|DELETE /v1/compile/slots[/{id}]` | Manage slots |
| `POST /v1/compile/slots/{id}/stop|reset` | Stop or clear a slot |
| `POST /v1/compile/slots/stop-idle` | Stop warm idle VMs to release shared capacity |
| `DELETE /v1/compile/caches/{name}` | Clear a shared cache |
| `GET|PATCH|DELETE /v1/compile/artifacts[/{id}]` | Manage artifacts |
| `GET /v1/compile/jobs/{id}[/log]`, `POST /v1/compile/jobs/{id}/cancel` | Inspect or cancel a build |
| `POST /v1/compile/prepare` | Start or resume automatic formula downloads |
| `POST /v1/compile/packages/pause` | Pause automatic upkeep |
| `POST /v1/compile/packages/refresh` | Refresh registry and retry failures |
| `DELETE /v1/compile/packages/downloads` | Clear raw downloads/index while idle, preserve packed runtime, pause upkeep |
| `POST /v1/compile/storage[/cancel]` | Move or cancel moving the working directory |

## Storage and bundling

The working directory defaults to `<app data>/compile`. Settings → Storage can move it to an empty folder, including an external disk. Builds and preparation must be idle; idle VMs stop first. A same-volume move renames files. A cross-volume copy preserves sparse holes and keeps the old copy until success. Failure/cancellation removes partial copies. `storage.json` records a custom location. If its disk is absent, GizMoth does not silently recreate it on the startup disk.

```text
GizMoth.app/Contents/MacOS/openvmm
GizMoth.app/Contents/Resources/
  linux-kernel/Image, kernel.json      minimal boot resources
  compile-agent/gizmoth-compile-agent
<app data>/compile/
  settings.json, storage.json
  registry/                           formula registry checkout and snapshot
<working directory>/                  <app data>/compile by default
  assets/<sha256>                     verified formula source downloads
  packages.json                       download index
  packages.erofs, packages.erofs.json  downloaded root userland + versioned tools
  slots/<id>/work.img                 sparse work disk; console logs alongside
  cache/<namespace>/…                 shared caches
  artifacts/<id>/                     artifact.json and files/
/tmp/gizmoth-<uid>/<slot>.vsock[_7421]  private vsock sockets
```

To stage compile resources for a development or ARM app build:

```bash
rustup target add aarch64-unknown-linux-musl
npm run compile-agent:build
brew install protobuf
npm run openvmm:build
npm run linux-kernel:stage
```

The kernel stage runs on macOS or Linux with Node, `curl` and `tar`, without Docker or sudo. It caches the pinned archive in `~/.cache/gizmoth/linux-kernel` (`XDG_CACHE_HOME` respected; `GIZMOTH_KERNEL_CACHE` overrides the whole cache folder) and checks the SHA on every use. It writes only `src-tauri/resources/linux-kernel/{Image,kernel.json}`. OpenVMM builds use the pinned source revision in the existing build script. Development lookup checks environment overrides, application resources, then staged development files: `GIZMOTH_OPENVMM`, `GIZMOTH_COMPILE_AGENT` and `GIZMOTH_LINUX_KERNEL` (a folder).

CI and release ARM app builds stage these resources directly and add `--config src-tauri/tauri.compile.conf.json`. `scripts/verify-macos-release.mjs --compile` checks OpenVMM, agent and kernel resources and rejects legacy Linux system-image resources before checking macOS signing/notarization. The bundle contains no downloaded formula archives or packed userland.

## Code and verification

- `src-tauri/src/compile/mod.rs`: settings, scheduler, snapshots and HTTP API
- `image.rs`: kernel lookup and boot metadata; `assets.rs`: verified downloads and initramfs
- `vm.rs`: OpenVMM; `p9.rs`: read-only shares; `packages.rs`: formula upkeep and packing
- `jobs.rs`, `sync.rs`, `cache.rs`, `artifacts.rs`, `storage.rs`: builds, copies, caches, output retention and storage moves
- `native/compile-agent/src/`: static bootstrap, packer, worker and shared protocol
- `src-tauri/src/devenv/registry.rs`: formula parsing and registry checkout
- `scripts/stage-linux-kernel.mjs`, `scripts/linux-kernel.json`: kernel staging and pins

Verified locally on an Apple silicon Mac using the registered formula snapshot: the VM booted from the kernel and static agent without a system disk, assembled userland from verified downloads, ran Git 2.39.5 and native GCC 12.2, rejected writes to `/src` and `/usr/bin`, and compiled an executable on the writable output disk. The downloaded x86-64 GNU assembler ran transparently through QEMU user-mode/binfmt_misc on the ARM64 kernel and produced a valid x86-64 ELF object. Boot resources measured 23,559,816 bytes; the independent managed userland pack measured 553,345,024 bytes, including the full QEMU package and all registered native/foreign tools. This proves those tools, not universal x86 compatibility or Rosetta support. The opt-in isolated app test is:

```bash
GIZMOTH_LIVE_COMPILE_TEST=1 node --test --test-concurrency=1 test/integration/compile.test.mjs
```

It uses staged boot resources. `GIZMOTH_E2E_COMPILE_SEED=<app data>/compile` can reuse verified downloaded runtime data through APFS clones. The fixture stays off the LAN and shuts down its VMs afterward. Windows/Linux hosts, Intel Mac VM hosts and builds from LAN workspace paths are not supported. Installing a build artifact on physical hardware and signing the packaged OpenVMM sidecar require their own verification.
