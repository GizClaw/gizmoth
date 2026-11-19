# Compiling in Linux VMs

GizMoth runs builds in Linux virtual machines on Macs with Apple silicon. **Compiling** in the sidebar contains **Builds** (slots, queue and logs), **Caches** and **Artifacts**. External coding agents submit [JSON execution requests](execution.md) through `gizmoth request`; the resident service retains detached tasks. The existing build WebSocket remains supported. Both share the same slots, caches and artifacts. **Settings → Compiling** contains the shared VM resource budget, download pause/resume/update controls and artifact retention. **Settings → Storage** chooses the working directory and provides confirmed cleanup of downloaded archives and unpinned artifacts.

The app ships only OpenVMM, an ARM64 Linux kernel and a static guest agent. Linux userland, shells, build tools, SDKs and optional compatibility tools come from [GizClaw/gizmoth-devenv](https://github.com/GizClaw/gizmoth-devenv) formulae and are downloaded and managed in GizMoth. There is no bundled Ubuntu distribution or `system.erofs`.

## Boot environment

```text
Agent ── gizmoth request ──► JSON request ─────────────► GizMoth service
                                                      ├─ FIFO queue + N slots
                                                      ├─ shared cache and artifact stores
                                                      └─ OpenVMM per running slot
                                                           ├─ Linux ARM64 kernel
                                                           ├─ initramfs: static agent + config
                                                           ├─ packages.erofs: downloaded userland/tools, read-only
                                                           ├─ /src: host source, read-only 9P
                                                           └─ /work: host slots/<id>/work, writable 9P
```

- **OpenVMM** (Microsoft, MIT) runs VMs through Hypervisor.framework. GizMoth builds a pinned revision and ships it as the `openvmm` sidecar, signed with `com.apple.security.hypervisor`.
- The Hypervisor.framework backend uses Hyper-V vmbus devices: storvsc disks, netvsc networking through OpenVMM's user-mode NAT (consomme, DHCP) and hv_sock. The guest command line disables virtio vsock so hv_sock owns the guest-to-host transport.
- **Linux kernel**: OpenVMM's published ARM64 test kernel, currently Linux 6.18 from openvmm-deps 0.3.0-185. `scripts/linux-kernel.json` pins the original archive URL and SHA-256. Staging verifies the archive, extracts only its `Image` member and checks the ARM64 image header.
- **gizmoth-compile-agent** (`native/compile-agent`, static aarch64 musl) boots as PID 1 from a fresh initramfs containing the agent and configuration. It mounts downloaded userland, starts the worker, reaps processes and relays the guest cache endpoint to the host over vsock.

The bundle resources are `linux-kernel/{Image,kernel.json}` and `compile-agent/gizmoth-compile-agent`; the kernel is about 22 MB. These bundled components are internal application resources and have no separate status card or user preparation controls. Dependency downloads and runtime assembly are presented only by download management. Diagnostic API metadata still uses the snapshot's `image` key: `base` is `Linux kernel + static agent`, `bazelisk` is null and `packages` is empty.

Earlier versions built Ubuntu images in the working directory. Startup removes only recognized legacy image files and the two pinned old kernel/Ubuntu archives; active formula downloads and packed runtime data are preserved.

## GizMoth formulae and download management

GizMoth checks the registry's main branch on each service startup and downloads supported Linux formulae automatically unless downloads were paused or an entry was explicitly removed. Metadata refresh remains active while payload downloads are paused or VM boot resources are unavailable; a cached catalog stays usable offline. The package list shows recipes selected for the current Linux build environment, with remove and reinstall actions where available. Other platform variants stay in catalog metadata without appearing as separate UI tabs or unsupported preview rows. Titles use upstream package/repository names from registry metadata; multi-source bundles use their canonical formula identity. A formula declares versions, upstream source downloads and their integrity checks, environment configuration, and installation/build steps. Root formulae supply Linux userland at `/`; other formulae install versioned tools under `/opt/gizmoth/packages/`. The registry identities remain `toolchain` and `sdk`; `guestRoot` marks a formula that contributes to the root filesystem and `bootstrap` identifies bootstrap inputs.

For a temporary local recipe test, set the repository-local Git setting `gizmoth.registryBranch` in `<app data>/compile/registry/repository`, explicitly fetch and switch to that branch, then use **Check for updates**. Only this local setting overrides `main`; a different checked-out branch without it is rejected. To return to normal updates, unset the local setting and switch the checkout back to `main` before refreshing. Fresh installations always clone `main`.

The service keeps the registry checkout under `<app data>/compile/registry/`, checks for updates periodically or through **Check for updates**, verifies source sizes and SHA-256 digests, and stores raw archives under `<working directory>/assets/`. Formula archives are not unpacked into macOS folders: Linux trees can contain names that differ only by case, which a case-insensitive APFS volume cannot represent.

A packer VM boots the static agent and uses the downloaded bootstrap inputs to assemble Linux userland and tools on a scratch disk. It reads archives through a read-only 9P share and writes `packages.erofs`, containing both the root filesystem and versioned tools. This is downloaded runtime data in the working directory, not an application resource. No prebuilt distribution image or producer CI is required. Worker VMs attach this pack read-only and keep the tool directories read-only; runtime writes belong in the slot's host work directory or ephemeral mount points.

Installation supports verified file copies and `tar.gz`, `tar.xz`, `tar.bz2`, ZIP and Debian archives. XZ decompression streams into a temporary tar file on the assembly disk with a 256 MiB decoder memory limit, so a large expanded XZ block does not exhaust the preparation VM's 2 GiB reservation. Failed decoding removes the temporary file before extraction. For a Git source, GizMoth fetches the exact commit into private bare repositories on the host, follows the pinned recursive submodule graph when requested, and acquires LFS objects with size and SHA-256 verification. A local source container holds self-contained Git bundles and verified LFS objects under its actual SHA-256 in `assets/`; its identity is persisted with the cached package and reverified before reuse. Unknown Git transfer totals do not display a percentage. The packer copies each source container once into a private file on its assembly disk while verifying the bytes written against the container SHA-256. Archive validation and extraction read that same verified local file. It restores these repositories offline inside Linux, preserves tags and Git metadata, checks every requested gitlink and LFS pointer, and rejects incomplete SDK trees. Source containers retain the acquisition's submodule policy: recursive sources must contain the full pinned graph, while explicitly nonrecursive sources preserve uninitialized gitlinks without initializing or fetching children. Older containers retain recursive semantics; legacy nonrecursive acquisition memos are regenerated so omission is never mistaken for an incomplete recursive download. Host filesystem case rules never affect the SDK checkout. Containers are local cache data; the registry contains JSON configuration and original upstream URLs only.

Declarative `run` steps execute inside the Linux assembly root with deadlines and cancellation. Root payloads install first, followed by tools and SDKs in exact dependency order. `${prefix}`, `${cache}`, `${work}` and declared `${toolchain:<id>}` references resolve to guest paths; GizMoth constructs `PATH` from the package and its pinned dependencies. SDK setup must install its Python environment and other declared inputs offline. File checks resolve both absolute and relative symlinks inside the assembled guest root, including links from SDK tool layouts to other managed packages; missing targets, loops and paths escaping that root fail validation. A formula's metadata, complete source acquisition and command qualification are separate from successful firmware compilation; target usability requires an actual build using the installed SDK.

After setup, command checks run as `builder` with programs and SDKs mounted read-only and a fresh writable HOME/cache. Nonzero exits, timeouts and missing `stdoutContains` text reject the new pack, preserving the previously published runtime. SDK Git roots and submodules receive exact system `safe.directory` entries. A cached recipe from an older app is replanned from its verified registry manifest so previously omitted command checks are retained.

**Download management** in Settings → Compiling shows the registry revision, formula versions, archive sizes, download progress, installation errors and guest paths. Platform and execution policy stay internal; the UI shows download destinations and Linux installation locations. All source downloads are stored on the host. ARM64 recipes can include x86-only programs with QEMU user-mode and their exact foreign-runtime dependencies; a separate x86_64 target recipe is not required for those programs. Legacy nondefault root-layer releases and tools pinned to them remain view-only, including transitive dependencies, because shared root versions cannot coexist. When a default release moves into a versioned prefix, the existing runtime remains usable until replacement defaults have downloaded. Guest-root entries show representative absolute Linux file paths from the formula's file checks, such as `/usr/bin/git` or `/opt/gizmoth/x86_64/lib/x86_64-linux-gnu/libgcc_s.so.1`. Package rows default to a single line with name, colored labels, version, archive size, state and management menu. The unified list has counted filters for All and the available architecture/ecosystem labels, including ARM64, x86_64, ESP32, Jieli and Beken. Labels describe payload execution architecture and ecosystem independently of the recipe platform or compiler output target: the ARM64-environment Jieli tools carry x86_64/Jieli, while the ARM64 QEMU executable carries ARM64. Host-independent ESP ROM data carries only ESP32. These display-only labels also remain available from cached and removed entries; filtering does not change downloads, package identity or runtime assembly. A keyboard-accessible disclosure expands host and Linux paths as a two-column list with labels on the left and paths on the right, separated by fine rules; collapsing restores the compact row. Host paths identify the original downloads under `assets/<SHA256>` and the shared `packages.erofs` image containing an installed release. Files at download destinations may be absent after cleanup or before download; these paths are not host extraction directories. Default versioned tools also show `/opt/gizmoth/bin` as their command directory on the Linux worker PATH. These are checked locations rather than a complete file inventory. Offline installed entries use their cached checks. Formulae without file checks fall back to the merged installation destination; versioned tools show their ordinary package directory. Its global controls are:

- **Pause downloads** stops payload downloading and preparation while keeping registry metadata current. Existing packed userland can remain usable for builds.
- **Resume downloads** enables upkeep and resumes preparation of all Linux formulae.
- **Check for updates** refreshes the registry and retries failed preparation while upkeep is enabled.

Each managed row has contextual **Delete** and **Reinstall** actions. Confirmed deletion persists an exclusion, removes only unshared cached inputs and rebuilds the aggregate read-only runtime. Linux system/bootstrap layers and packages still required by retained installations or selected pending installations cannot be removed individually. A retained release can still be deliberately removed when its replacement recipe is unsupported; reinstalling that unsupported recipe remains unavailable. A removed entry stays visible and does not return during automatic upkeep; its **Install** action restores it. Reinstall revalidates sources, fetches damaged/missing inputs and changes a persisted repair nonce, forcing fresh extraction and qualification even when the version and manifest are unchanged. Explicit operations preserve the global pause setting and finish without enabling unrelated automatic downloads. Jobs, VM startup/shutdown, storage moves and ongoing preparation serialize with these operations.

A failed download, qualification or publication preserves the previous runtime and metadata. Publication uses a rollback journal so an interrupted image/metadata replacement recovers on restart. Existing VMs retain their old read-only disk inode; changed boot generations retire them before reuse. Package repair does not delete work directories, build caches, artifacts or projects.

**Clear downloaded archives** lives on the downloads row in **Settings → Storage** and requires confirmation. It is available when downloads and runtime preparation are idle; pause downloads and wait for ongoing work to finish if necessary. It deletes raw archives and their download index, then pauses upkeep. The prepared runtime, slot work directories, caches and artifacts remain available. A durable cleanup journal restores uncommitted removals and completes committed removals after restart. Cleanup failures remain visible, count toward download storage and block storage moves until recovery succeeds. Resuming downloads can fetch the archives again.

The snapshot exposes `packages.active` (upkeep enabled), `packages.paused`, `packages.busy`, `packages.platforms`, `packages.downloads` (`path`, `bytes`, `cleanupBytes` and `error`), formula states and the packed runtime state. Rows include `canRemove`, `canReinstall`, their unavailable reasons, `repairing`, display-only `installPath`, `linuxPaths`, `hostPaths` (download destinations and matching runtime image) and `commandPath` (the default version's Linux command directory, when exported). `linuxPaths` is an ordered, deduplicated array of Linux locations; unmanaged platform previews have an empty array. A manual repair can progress while global upkeep remains paused; `removed` entries represent persisted exclusions. Clearing archives does not imply deleting or invalidating a usable packed runtime. Download allocation statistics are cached and refreshed after changes and periodically; snapshots and controls do not wait for recursive disk scans. Removed-package exclusions apply to their exact recipe platform, so an old fallback exclusion does not block a newly selected native recipe.

`/opt/gizmoth` is the managed Linux prefix. Versioned tools remain under `/opt/gizmoth/packages/{toolchains,sdks}/<id>/<version>`. After installation, the packer generates `/opt/gizmoth/bin` with small launchers that `exec` executable files in the default formula versions' `prependPath` directories. Only declared command directories are exported; nondefault versions remain available by their exact paths. If defaults export the same command name, the first formula in the existing kind/ID and `prependPath` order wins. Switching defaults, deleting or reinstalling a package rebuilds this activation directory together with the read-only runtime.

Worker `PATH` starts with `/opt/gizmoth/bin`, followed by ccache wrappers when enabled and the standard Linux paths. Formula installation and qualification retain separate paths for their exact declared dependency versions, so the global defaults cannot replace a pinned SDK interpreter or compiler. Builds receive `GIZMOTH_PREFIX=/opt/gizmoth`, `GIZMOTH_PACKAGES=/opt/gizmoth/packages` and `PYTHONPYCACHEPREFIX=/work/home/.cache/pycache`. The host macOS PATH is unaffected. Linux shells, ELF loaders and root userland keep their required standard paths; foreign x86 libraries stay under `/opt/gizmoth/x86_64`. A changed pack is applied when a slot next boots.

The guest kernel remains ARM64. Support for x86 executables belongs to QEMU user-mode emulators and the matching libraries delivered by formulae, rather than a bundled x86 operating system. Availability depends on the actual registry inputs and guest configuration; an ARM64 boot or successful download alone does not verify x86 execution.

## Read-only source and writable builds

The bundled OpenVMM macOS backend has no native virtio-fs or virtio-9p resolver. GizMoth implements its own 9P2000.L layer (`p9.rs` and `p9/writable.rs`) over hv_sock. The guest agent connects to a host vsock port and the guest kernel mounts the connection with `trans=fd`. Source and archive exports are read-only; the slot work export is writable. Symlink entries are exposed for the guest to resolve within its own mount namespace. Replies have a 60-second socket write timeout. A failed or partially transmitted reply closes that connection permanently and marks the export failed; the host then stops and reaps its VM, returning the resource reservation even if the guest is blocked in filesystem I/O. A clean disconnect does not mark the export failed. In-flight requests retain their own completion identity when Linux reuses a numeric tag. Flushes wait for the request captured on receipt, so tag reuse cannot erase a later request or redirect a delayed flush to it.

The macOS OpenVMM hv_sock relay also rechecks blocked ring readiness once per second on its existing async driver. The timer is only a readiness hint: normal ring checks still decide whether bytes can be read or written, and real notifications and closure take precedence. This prevents a missed host notification from being the only way to resume a ready relay; it neither changes framing nor injects guest interrupts. Fault-injection tests suppress host notifications and exercise bidirectional 131,083-byte transfers over 0x6000-byte rings with fragmented reads, alongside closure and bounded idle polling. This recovery test does not identify the origin of the intermittent notification failure seen during SDK installation.

A build reads the original host workspace at `/src`, which stays read-only. The normal command runs in a private synchronized copy under `/work/src/<workspace key>`, allowing in-tree build systems to generate files without modifying the original workspace. `/work` directly exports `<working directory>/slots/<id>/work/` from the host. Home, temporary files, build directories, local caches and outputs are ordinary files there; artifact collection copies selected outputs into the separate artifact store.

A slot runs one VM and one concurrent build. The Builds page connects this computer to compact slot workstations, showing each name, status and current task. Two wrench glyphs and a screw animate only while the slot is busy; idle/stopped slots remain still, and reduced-motion preferences suppress the animation. One **Slot count** control adjusts the total from 0 to 16; individual slots have no editing or management menu. Increasing the count creates named slots automatically. Decreasing it removes idle slots from the end, preserving remaining slot identities, active jobs and detached work directories. If too few idle slots are available, the whole change is rejected; reducing to zero also rejects queued work. There is no per-slot disk image, capacity setting or virtual disk quota: builds use the host work directory's filesystem capacity, reported by `statvfs`. Old per-slot CPU, memory and disk-size settings are ignored on load; slot IDs and names remain. The snapshot retains the `disk` object for compatibility, with the host work `path`, allocated usage and null virtual `size`/`free` fields.

Selecting a task or an occupied slot opens its dedicated build-log page. A read-only terminal fills the available space, using a black background, monospace text and single line spacing. It interprets ANSI colors (including 256-color and RGB), carriage returns and cursor/erase sequences, so progress updates can overwrite earlier output. This is an output viewer; it does not allocate a guest PTY or accept command input. Programs must emit color themselves, for example Bazel with `--color=yes`. Case-insensitive filtering preserves the parsed colors, copying produces plain text, and following tracks live output. Scrolling away from the end pauses following. The UI retains the newest 512 KiB of raw log text and up to 20,000 terminal scrollback rows. Task details and the full multiline command are collapsed by default; the breadcrumb or **Back to builds** returns to the task list.

**Settings → Compiling** configures one total vCPU budget and one total guest-memory budget shared by every build and preparation VM. These are aggregate limits. Defaults are all host CPUs and half of host RAM.

GizMoth automatically assigns worker capacity from the total budget and configured slot count, with at least one vCPU and 1 GiB per worker. The preparation VM requests up to four vCPUs and 2 GiB from that same budget. A VM reserves capacity before boot and holds it while starting, running, warm and stopping; it returns capacity only after teardown. Work remains queued when insufficient capacity is available. Lowering a budget below already allocated capacity is rejected without saving the smaller value. **Stop idle VMs** releases warm capacity without interrupting active builds.

The memory budget caps combined configured guest-RAM capacity. OpenVMM uses sparse backing with `prefetch=off`, so actual resident memory follows guest accesses; VMM process overhead is additional. This is not an exact hard cap on macOS process RSS, ballooning or live resizing. The settings show allocated capacity rather than measured RAM use. macOS schedules vCPU threads, while build-tool settings still control compilation parallelism.

Before serving host folders on macOS, GizMoth raises its own open-file soft limit toward 65,536, bounded by the inherited hard limit and `kern.maxfilesperproc`. This does not change system settings or the hard limit. Writable 9P handles pin host descriptors across filesystem operations, and all slots share the resident service's descriptor budget. GUI-launched processes commonly inherit a soft limit of 256; leaving it unchanged can make Bazel toolchain analysis fail with `Too many open files`, even though the guest has a 65,536-descriptor limit.

Both 9P exports negotiate messages up to 64 KiB, including when the guest requests 512 KiB. Linux v9fs allocates a contiguous buffer based on this size for each open directory; the smaller ceiling reduces allocation pressure during large SDK directory walks. Reads, writes and directory enumeration continue across multiple messages. Read-only and writable directory snapshots share a 128 MiB host-memory budget, including construction and in-flight references after clunk. Byte-range lock records and retained client identities have global, session and fid limits; resource exhaustion returns an explicit error before changing existing locks.

The writable and read-only paths are:

| Path | Purpose | Access |
| --- | --- | --- |
| `/src` | Host workspace source via 9P | Read-only |
| `/opt/gizmoth/packages` | Versioned formula tools | Read-only |
| `/opt/gizmoth/bin` | Active default command launchers | Read-only |
| `/out` | Direct mode only. The accepted workspace's own `out/` | Writable |
| `/work/src/<workspace key>` | Sync mode. Private synchronized source/build copy | Writable |
| `/work/bazel` | Bazel output bases and execroot | Writable |
| `/work/bazel-repo-cache` | Bazel repository cache | Writable |
| `/work/home` | Build user's home, tool downloads and caches | Writable |
| `/work/tmp`, `/work/output` | Temporary files and build outputs | Writable |

The writable export uses descriptor-bound fids and single-component operations relative to opened directories. Guest IDs 0 and 1000 map to virtual builder UID/GID 1000 without changing host ownership. Directory capabilities keep referring to their inode if the trusted host renames them, including publication of a migrated root. Host filesystem case behavior applies: a case-insensitive volume cannot hold two names that differ only by case. Byte-range locks coordinate 9P clients in the GizMoth process, including separate export connections, but do not coordinate with native host `fcntl` users. Unsupported special nodes, xattr writes, direct I/O, lock reclaim and symlink chmod/timestamp updates return explicit errors.

For a root Git worktree whose `.git` file points outside the workspace, synchronization reads its Git directory and common metadata on the host and assembles a portable `.git` directory in the private copy. The source HEAD, index, staged-only objects, refs and split-index files are retained; source configuration selects the guest working tree. Mutable metadata is snapshotted and identified by content so equal-size edits with preserved timestamps still invalidate the manifest. The host repository is never modified, and hooks and other worktrees are omitted. Metadata-root hooks and worktree pointers are omitted without dropping branch refs that share those names. Excluding a `.git/…` directory prunes its descendants before traversal. Git metadata symlinks, borrowed object alternates and reftable storage are rejected explicitly rather than creating an incomplete private repository.

The host compares workspace metadata with the previous sync and sends changed files, symlinks, directories and deletions. The guest checks its actual files, including content digests, so edits or interrupted synchronization do not leave modified inputs marked current. Batches are bounded by frame size and entry count. Top-level `bazel-*` links and `.DS_Store` are skipped automatically; a build request may add project-specific exclusion globs.

Slots boot on demand and stay warm for the idle timeout (10 minutes by default), retaining build-server state. **Automatically stop idle VMs** controls this timer (`autoStopIdle`, enabled by default); disabling it preserves warm VMs until explicitly stopped or invalidated by boot-policy changes. Warm VMs still retain guest memory and shared-budget reservations; **Stop idle VMs** releases them without interrupting active builds. Builds prefer warm slots. Boot changes take effect before the next build: busy slots finish, then restart. CPU/RAM capacity is assigned from the shared budget at VM startup. The compatibility reset API deletes a slot's host work directory and retained legacy work data, allowing a fresh directory to be created on the next build. Shared caches, downloads and artifacts survive a reset. Changing the total slot count preserves detached work directories.

### Direct workspace builds

Set `spec.workspaceMode` to `"direct"` in a `gizmoth request` compile request or a `CompileTask`. GizMoth mounts the canonical host `workspace` read-only at `/src` and opens or creates its `out/` directory as an independent writable mount at `/out`. `/src/out` stays part of the read-only source mount. A symlink in place of `out` is rejected. The guest sees ordinary source files, including untracked files. A linked worktree's `.git` pointer keeps its host contents. Projects that need portable Git metadata, source patches or in-tree writes can use `"sync"`, the default.

The builder starts in `/src` or the relative `cwd` below it. `GIZMOTH_SOURCE` and `GIZMOTH_WORKSPACE` default to `/src`. `GIZMOTH_BUILD_DIR` and `GIZMOTH_OUTPUT` default to `/out`. `HOME=/work/home` and `TMPDIR=/work/tmp` are guest tmpfs scratch in direct mode. The `env` map can override command defaults. Configure native build state explicitly, for example `cmake -S . -B /out/debug && cmake --build /out/debug`. Use separate directories such as `/out/debug` and `/out/release` for incompatible configurations. Shared compiler caches still work. Slot reset and artifact cleanup leave workspace `out/` under the caller's control.

Each direct job gets a fresh VM. GizMoth holds one writer per workspace across command execution, VM shutdown and artifact publication. A second direct job waits while an unrelated workspace can take a free slot. The host pins both directories and checks their identities before execution and publication. Keep them in place through completion and keep host writers idle. Use a dedicated `out/` on the project volume and keep other worktrees outside it. The direct export checks the output descriptor and its current directory ancestry before writes and metadata changes. Open anonymous temporary files remain usable until close. Hard-link creation and writes to files with multiple links are rejected.

`artifacts` globs in direct mode are relative to `/out`, regardless of `cwd`. They select regular files and skip symlinks. Literal path components are case-sensitive, including on case-insensitive host volumes. Overlapping patterns archive each matched path once using its on-disk spelling. A glob can include files left by a previous invocation, so select the deliverables for the current configuration. GizMoth archives just the matches after the VM flushes and exits. On a compatible volume it takes independent APFS copy-on-write clones. Across volumes it copies each selected file once. The archive records logical sizes, SHA-256 hashes and the collecting job's ID. It survives subsequent builds and worktree removal, with the usual pin, install and deletion semantics. Unselected intermediates remain solely in `out/` and fall outside artifact usage totals. A [minimal request](execution.md#request-document) builds and archives a C executable.

## Shared caches

Shared caches live in the working directory's host cache store and survive VM boots and slot work-directory resets. Each VM reaches them through its own vsock socket at guest `127.0.0.1:7421`; no separate host network port is opened.

| Cache | Guest configuration | Key format |
| --- | --- | --- |
| Bazel | `/etc/bazel.bazelrc`: `build --remote_cache=http://127.0.0.1:7421/bazel` | Bazel HTTP AC/CAS; CAS uploads are verified |
| ccache | `/etc/ccache.conf`: HTTP remote storage with `remote_only = true` | ccache HTTP storage, when supported by the installed formula |

ccache reads and writes compilation results directly through the shared host namespace; there is no per-slot result-cache tier or local size setting. Its temporary files use `/tmp/ccache` on the guest's tmpfs, and local statistics are disabled. Existing `localGib` settings are ignored, while the shared size limit is preserved. Previous `/work/ccache` directories are left untouched but are no longer used. Bazel still needs per-slot output bases, execroots and repository downloads; sharing its AC/CAS does not replace those build directories.

Only the built-in Bazel and ccache namespaces are supported. Their size limits are configurable; custom namespaces and cache environment-variable mappings are not. Legacy `caches.custom` settings are ignored and removed when settings are saved. Existing custom-cache files remain on disk and are not included in the active-cache overview.

Caches are always on and use size limits with least-recently-used eviction to 90%. A zero size limit disables committed-cache eviction. Upload staging remains bounded separately: at most 64 uploads and 8 GiB in total, with each namespace also limited to its configured size or 8 GiB when unlimited. Writes reserve space before reaching disk and retain 256 MiB of free volume headroom. Partial uploads leave committed values intact and abandoned staging is removed on service startup. All VM cache listeners share a 128-connection admission limit, checked before request threads and buffers are allocated. If startup cleanup fails, uploads retry cleanup after the filesystem is repaired, while protecting active staging files. The page shows size, entries, hits, misses and uploads and can clear individual caches. Counts start at service startup. GizMoth generates Bazel runtime/cache configuration, tool activation variables and synchronization policy. These are not global user-editable settings; legacy `bazelrc`, `env` and `excludes` settings are ignored on load and rejected by settings updates. Project flags belong in the build command or project configuration. Build-specific variables and extra exclusions use the request's `env` and `exclude` fields and do not affect other builds. Compile workers always have dependency access through GizMoth-managed NAT; connectivity is not a user-configurable VM setting. Dependency downloads use this network; request variables such as `BAZELISK_BASE_URL` can select project-specific mirrors.

## Build WebSocket: `gizmoth.compile.v1`

Discover the URL, headers and request schema without starting the service:

```bash
gizmoth --tool compile_pipe '{"client":"coding-agent","title":"Build firmware"}'
```

Connect to `ws://<service>/v1/compile/ws?client=<name>&title=<task>` with `Authorization: Bearer <API key>` from Settings → Service. Authorization is checked again on submission. API-key rotation or disabling Service revokes sockets and cancels unfinished builds. After the `hello` frame, send one build request:

```json
{"id":"build-1","method":"build","params":{
  "apiVersion":"gizmoth.dev/v1",
  "kind":"CompileTask",
  "metadata":{"name":"Build firmware"},
  "spec":{
    "workspace":"/Users/me/firmware",
    "command":["bazel","build","//app:firmware"],
    "artifacts":["bazel-bin/app/*.update.tar.zlib"],
    "env":{"GIZOS_BOARD":"demo"},
    "timeoutSeconds":3600
  }
}}
```

The JSON document inside `params` can be kept in a project's `gizmoth-task.json` and sent as a single build request. `apiVersion` and `kind` select the supported configuration schema; `metadata.name` is the task title (1–120 characters). The service rejects unsupported versions, kinds, unknown fields and mixed manifest/legacy parameters before queuing work. The connection still determines the submitting client. The fields below belong to `spec`:

| Parameter | Meaning |
| --- | --- |
| `workspace` | Absolute workspace on the service computer; local connections only |
| `workspaceMode` | `sync` (default) uses a writable workspace copy. `direct` runs in `/src` and writes to `/out`. |
| `command` | argv run as `builder` in the selected workspace. Use `["bash","-lc","…"]` for shell syntax |
| `cwd` | Relative directory in the selected workspace |
| `env` | Extra environment variables |
| `artifacts` | Globs (`*`, `?`, `**`) rooted at the workspace copy in sync mode or `/out` in direct mode |
| `exclude` | Extra workspace globs omitted from synchronization. Direct mode accepts only an empty list. |
| `timeoutSeconds` | Stops the command with SIGINT, SIGTERM, then SIGKILL |
| `collectOnFailure` | Collect outputs after a failed command too |

Every accepted task has an immutable configuration derived from its accepted request: the canonical host workspace, argv, relative working directory, environment, sync exclusions, artifact rules, timeout and failure-collection policy. Compiling → Builds → task → **Configuration** displays this document with a copy action. `GET /v1/compile/jobs/{id}/config` returns the same document on demand, without adding environment values to every live status snapshot. Omitted fields are expanded to their defaults (`cwd: ""`, empty maps/lists, `timeoutSeconds: null`, `collectOnFailure: false`). A null timeout leaves the command without a requested deadline. The configuration is available while its task remains in the service's in-memory history; restarting the service clears that history.

Existing clients may still send the original flat build parameters. They receive the same configuration view, with the WebSocket title as `metadata.name`. The WebSocket protocol remains `gizmoth.compile.v1`; `compile_pipe` and `hello.commands` advertise both accepted forms.

A `result` returns the job summary. JSON events report `queued`, `status` (`booting`, `syncing`, `running`, `collecting`), `started`, `synced`, `exit`, `artifact`, and finally `finished`; binary frames carry the combined log. The server then closes the socket. `lagged` reports skipped events for a slow reader; use `compile_job` for the full log.

**Closing the socket cancels the build**, including queued work. `{"method":"cancel"}` cancels and `{"method":"status"}` reads the summary. Open one socket per build; free slots take eligible queued jobs in FIFO order. Builds receive `GIZMOTH_SLOT`, `GIZMOTH_JOB_ID` and `GIZMOTH_WORKSPACE`.

## Artifacts and tools

Collected files are read-only under `<working directory>/artifacts/<id>/files/`, with `artifact.json` containing the source job and SHA-256 per file. All agents can query them:

```bash
gizmoth --tool compile_artifacts '{"limit":10}'
```

To install firmware, send `{"method":"install","params":{"artifact":"<id>"}}` on a granted device WebSocket. Add `file` when several firmware packages exist. Artifact IDs also work over LAN, whose callers cannot submit local paths. Compiling → Artifacts lists one row per artifact with its title, file summary and time in shared columns, so times line up whether or not a row is pinned or installable; narrow windows drop the file summary first. Clicking a row, or Enter/Space on it, expands its files, sizes, full-path and ID copy actions, workspace, command and **Show in Finder** inline below it, like download management. The `…` menu at the right edge of each row is a separate control with **Install**, **Pin**/**Unpin** and **Delete** for that one artifact. Install opens the existing device dialog and stays visible but disabled when the artifact has no `.update.tar.zlib` firmware package; Delete asks for confirmation first. **View artifact** on a build log opens this list with that artifact expanded and scrolled into view, extending the 50-row paging as far as its page; returning later through the sidebar starts at the normal first page. Retention removes the oldest unpinned artifacts beyond the count or size limit, counting pinned bytes toward the total size budget. If a newly collected artifact cannot survive that budget, the build reports a retention error instead of returning a missing artifact. Failed deletions remain indexed for retry; directories already missing are reconciled out of the index. An artifact-specific store revision refreshes open lists after another client's pin changes, independently of build and log notifications.

The artifacts row in **Settings → Storage** offers confirmed bulk cleanup of unpinned outputs. The backend checks current pins under the same lock as pin changes and collection, leaves in-progress staging alone, and refuses bulk cleanup while device firmware preparation is copying its input. Artifact-ID resolution and preparation reservation are serialized with this cleanup. After preparation, installation uses its private copy. Work directories, shared caches and downloaded runtime data are preserved. Explicit single-artifact deletion retains its existing behavior.

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
| `PATCH /v1/compile/slots` | Set the total slot `count` (integer 0–16) without interrupting active builds or deleting detached work directories |
| `POST /v1/compile/slots`, `PATCH\|DELETE /v1/compile/slots/{id}` | Compatibility APIs to create/rename slots using only `name`, or explicitly delete a slot |
| `POST /v1/compile/slots/{id}/stop\|reset` | Stop a slot or clear its host work directory and retained legacy data |
| `POST /v1/compile/slots/stop-idle` | Stop warm idle VMs to release shared capacity |
| `DELETE /v1/compile/caches/{name}` | Clear a shared cache |
| `GET /v1/compile/artifacts[/{id}]`, `PATCH\|DELETE /v1/compile/artifacts/{id}` | Inspect, pin or explicitly delete individual artifacts |
| `DELETE /v1/compile/artifacts` | Clear committed unpinned artifacts, preserving current pins and refusing active firmware preparation |
| `GET /v1/compile/jobs/{id}[/log]`, `POST /v1/compile/jobs/{id}/cancel` | Inspect or cancel a build |
| `GET /v1/compile/jobs/{id}/config` | Read the accepted versioned task configuration, including explicit defaults |
| `POST /v1/compile/prepare` | Start or resume automatic formula downloads |
| `POST /v1/compile/packages/pause` | Pause automatic upkeep |
| `POST /v1/compile/packages/refresh` | Refresh registry and retry failures |
| `DELETE /v1/compile/packages/downloads` | Clear raw downloads/index while idle, preserve packed runtime and exclusions, pause upkeep |
| `DELETE /v1/compile/packages` | Remove the exact `{kind, id, version, platform}` managed recipe and persist its exclusion |
| `POST /v1/compile/packages/reinstall` | Reinstall the exact `{kind, id, version, platform}` recipe through fresh runtime qualification |
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
  slots/<id>/work/                    writable host build data; console logs alongside
  slots/<id>/work.img                 retained legacy image, only if one existed
  slots/<id>/work.migrated.json        completed legacy migration record
  cache/<namespace>/…                 shared caches
  artifacts/<id>/                     artifact.json and files/
/tmp/gizmoth-<uid>-<pid>/              per-process private runtime directory
  <slot>.vsock[_<port>]               hv_sock endpoints and share/cache sockets
```

A new slot creates its `work/` directory without allocating a virtual work disk. If a slot has a legacy `work.img`, GizMoth copies or APFS-clones it to a temporary `work.migration.img`. The guest mounts that clone so journal recovery cannot modify the original, then copies the old files, home, symlinks and hard links into a writable host `work.migrating/` export. A completion marker binds the copy to the source fingerprint. After the guest reports readiness, the host verifies the marker, atomically publishes the staging directory as `work/`, and records `work.migrated.json`. Descriptor-bound exports keep working across this root rename. Migration copies file data without implicitly applying source permissions, then retains rwx and sticky bits while clearing setuid/setgid bits unsupported by writable 9P. Ownership and hard links are preserved independently of that normalization; the source image is unchanged. Access and modification times are retained to the destination filesystem's supported precision. The current Linux 9P client uses one-second timestamp granularity and truncates fractional seconds before sending updates, even though the host filesystem and 9P wire format support nanoseconds.

The original image is retained as a backup and contributes to displayed slot storage usage. An interrupted migration does not publish partial work; a nonempty existing destination without the matching completion marker is not overwritten. Clearing the slot's work directory or deleting the slot is an explicit destructive action that also removes its retained legacy work data. Cleanup detaches the retired tree and deletes it outside scheduler locks; storage migration waits for cleanup to finish. Before switching roots, migration retries any remaining detached trees so failed cleanup is not stranded in the old directory. Cancelling that recovery leaves remaining data available for a later retry.

To stage compile resources for a development or ARM app build:

```bash
rustup target add aarch64-unknown-linux-musl
npm run compile-agent:build
brew install protobuf
npm run openvmm:build
npm run linux-kernel:stage
```

The kernel stage runs on macOS or Linux with Node, `curl` and `tar`, without Docker or sudo. It caches the pinned archive in `~/.cache/gizmoth/linux-kernel` (`XDG_CACHE_HOME` respected; `GIZMOTH_KERNEL_CACHE` overrides the whole cache folder) and checks the SHA on every use. It writes only `src-tauri/resources/linux-kernel/{Image,kernel.json}`. OpenVMM builds use the pinned source revision plus `scripts/patches/openvmm-hvsock-recheck.patch`. The build script creates an isolated worktree under `src-tauri/target/openvmm/<revision>-<patch SHA256>` and verifies its expected tree before building; `OPENVMM_SOURCE` supplies the pinned repository without modifying that checkout. `CARGO_TARGET_DIR` can reuse a build cache. Development lookup checks environment overrides, application resources, then staged development files: `GIZMOTH_OPENVMM`, `GIZMOTH_COMPILE_AGENT` and `GIZMOTH_LINUX_KERNEL` (a folder).

CI and release ARM app builds stage these resources directly and add `--config src-tauri/tauri.compile.conf.json`. `scripts/verify-macos-release.mjs --compile` checks OpenVMM, agent and kernel resources and rejects legacy Linux system-image resources before checking macOS signing/notarization. The bundle contains no downloaded formula archives or packed userland.

## Code and verification

- `src-tauri/src/compile/mod.rs`: settings, scheduler, snapshots and HTTP API
- `image.rs`: kernel lookup and boot metadata; `assets.rs`: verified downloads and initramfs
- `vm.rs`: OpenVMM and per-process runtime sockets; `p9.rs`, `p9/writable.rs`: read-only and writable host exports
- `work.rs`: host work directories and publication of legacy migrations; `packages.rs`: formula upkeep and packing
- `jobs.rs`, `sync.rs`, `cache.rs`, `artifacts.rs`, `storage.rs`: builds, copies, caches, output retention and storage moves
- `native/compile-agent/src/`: static bootstrap, packer, worker and shared protocol
- `src-tauri/src/devenv/registry.rs`: formula parsing and registry checkout
- `scripts/stage-linux-kernel.mjs`, `scripts/linux-kernel.json`: kernel staging and pins

Completed local checks on an Apple silicon Mac include:

- Booting from the kernel and static agent, assembling downloaded userland, running Git 2.39.5 and native GCC 12.2, and rejecting writes to `/src` and `/usr/bin`.
- Compiling and executing a real GCC program with temporary files and outputs on the writable host `/work` export, without creating `work.img`.
- Writable Python `mmap`, flush/fsync, cross-process `lockf` conflict and unlock, atomic replacement while an old file descriptor retains its inode, and verification of the resulting bytes directly on the host.
- A Bazel 8.2.1 genrule and reuse of its persistent build server across builds on the host work directory.
- Migration of legacy build files, home, symlinks and hard links through the staging export, with the original image's full SHA-256 unchanged. Two further copy regressions passed as UID/GID 1000 with fixture files on native Linux tmpfs and destinations on the actual host-backed 9P export. They verify data-only copying, special-mode normalization, supported timestamp precision, ownership, hard links and source preservation; they do not exercise root-only legacy mounting or the full migration entrypoint.
- Opening 8,193 file descriptors as the build user. PID 1 sets the guest's soft/hard open-file limits to 65,536 before forking, so vendor LTO linkers can raise their working limit beyond the kernel's initial 4,096 hard limit.
- Execution of the downloaded x86-64 GNU assembler through QEMU user-mode/binfmt_misc on the ARM64 kernel, producing a valid x86-64 ELF object. This verifies that tool, not universal x86 compatibility or Rosetta support.

A live source-acquisition check also fetched and independently rehashed the complete pinned ESP-IDF, BK7258 SMP, BK IDK, AC695N, AC707N and AC791N source containers. It followed recursive submodules and materialized AC791N's upstream LFS objects. This verifies source acquisition; Linux installation and firmware compilation require separate checks. Reproduce acquisition against a frozen candidate `SavedSnapshot` without using the installed application's data:

```bash
GIZMOTH_SDK_TEST_DATA=<isolated test directory> GIZMOTH_TEST_REGISTRY_SNAPSHOT=<snapshot.json> cargo test --locked --manifest-path src-tauri/Cargo.toml --lib live_complete_sdk_git_acquisition -- --ignored --nocapture
```

The test refuses a nonempty directory without its test marker and retains verified inputs for installation retries. `live_registry_recipes_download_install_and_qualify` uses the same isolated data, staged VM resources and `GIZMOTH_LIVE_PACK_SEED` to run actual Linux installation and read-only command checks. Its optional `GIZMOTH_TEST_SDKS` comma-separated list limits SDKs while retaining all candidate toolchains; an unset value tests every candidate SDK. It streams installation output to `compile/sdk-install.log`, including when installation is interrupted.

A production-condition live run using the patched OpenVMM sidecar completed installation of all six SDKs and 34 toolchain/runtime packages, offline Git restoration, pinned Python setup, read-only builder command qualification and runtime publication in 921.50 seconds. It used no REPL wake commands or temporary guest/9P diagnostics, and resolved ESP-IDF's managed Ninja and ROM-data links correctly. The resulting 40-package runtime is paused in isolated test data; the formal app continues to follow registry main. This verifies installation of the inputs declared by that candidate, not firmware-build usability or the cause of the original transport failure. The first real ESP32-S3 firmware build subsequently failed ESP-IDF's complete tool check because the candidate omitted both GDB families, OpenOCD, esp-clangd and esp-idf-configdep. A BK7258 analysis also reached the shared ESP-IDF repository check and failed for the same reason. Candidate recipes now declare those five tools and their native Linux runtime libraries, and require the complete upstream tool check during installation. The next live run executed clangd and configdep successfully but rejected configdep's qualification because its version was printed to stderr. The corrected recipe captures that output while preserving the tool's exit status; fixture tests cover both stderr capture and failure propagation. A subsequent all-toolchain installation passed in 115.55 seconds, including both GDB Python 3.12 launchers, OpenOCD, clangd and configdep. The corrected six-SDK installation then passed in 875.83 seconds, including the full upstream ESP-IDF tool check, offline setup, read-only qualification and publication of 46 SDK/tool/runtime packages. The isolated runtime remains paused. This establishes installation and tool readiness; real firmware/package targets require their own successful builds.

### Embedded firmware qualification routes

Source inspection of the GizOS companion based on `a4cf8ce6dda53aa73552106eb5ba7d012346c3b5` identifies these representative product entries. Each package below is relative to `//projects/h2loader/targets/h2_loader_tar_zlib/loader/`; append the target after the colon. These are build routes, not completed qualification results or an exhaustive board acceptance matrix.

| Bazel config | Package | Native firmware target | H2Loader package target |
| --- | --- | --- | --- |
| `esp32s3` | `espressif/esp32s3_devkitc_1` | `firmware_usb`, `firmware_uart` | `package_usb`, `package_uart` |
| `esp32p4` | `waveshare/esp32p4_wifi6_touch_lcd_4_3` | `firmware` | `package` |
| `esp32s31` | `espressif/mosaico` | `firmware` | `package` |
| `bk7258` | `beken/bk7258_v3_202405` | `firmware` | `package` |
| `ac791n` | `jieli/jieli_ac791n_devkit` | `firmware` (`wl82`) | `package` |

ESP32-C5, AC695N and AC707N have platform/rule declarations but no concrete GizOS product firmware entry in that checkout. Their upstream examples can qualify vendor compilation independently. AC695N uses `br23`; AC707N uses `br35`. The H2Loader JieLi layout registry contains only WL82, and its package rule accepts WL82 managed firmware and BR35 Apps only. An AC695N or AC707N vendor ELF therefore does not qualify a GizOS Loader or H2Loader update package. BK IDK also has no separate native product consumer there: a successful BK7258 SMP build cannot qualify IDK. BK7259 has an additional unqualified registry recipe and no corresponding GizOS config/product entry.

An isolated ESP32-C5 upstream `hello_world` build also passed with the managed ESP-IDF Python environment and compiler. GizMoth collected and independently rehashed its 3,361,984-byte RISC-V ELF, 140,144-byte application image, bootloader, partition table and ELF header. This qualifies the vendor example; the inspected GizOS checkout has no C5 product package entry.

An isolated AC695N upstream soundbox build completed compilation and linking using the managed JieLi compiler under QEMU in the Linux worker. Independent host inspection verified its 5,682,312-byte ELF executable (SHA-256 `63d39581f3857d5071d165e621d0e95fcf9de386f815f3e785b91f6129b9857a`), 404,050-byte map and saved ELF header. The first harness run then failed artifact collection because its pattern matched no files. A subsequent worker job with the corrected flat-file pattern published the ELF, map and header through GizMoth; independent host hashing confirmed all three files match the original build outputs. This establishes vendor linking, with no H2Loader package or hardware qualification.

The ESP32-S3 DevKitC-1 `package_usb` target subsequently built a real GizOS H2Loader package using only the managed SDK/tool runtime. GizMoth collected the update archive, firmware metadata, recovery image and combined factory image. Independent host checks matched every collected size and SHA-256, boundedly decompressed the update archive, matched its plaintext manifest against the firmware metadata and verified the 1,533,568-byte ESP app image (SHA-256 `a040ba5d983c1acc2c8f96469b1df2d547126dcccecd46124787079c55067d1f`). The 999,808-byte update archive has SHA-256 `085e18d6255e88ecfa2dd690da7fe85d9b67d08c723fd5ea18797a92e2f5698f`. Its `checksum` covers optional data entries; Loader packages contain none, so the empty-content SHA-256 is correct. This build used compile-only Wi-Fi credentials and establishes package generation, with no physical device validation.

The ESP32-P4 Waveshare WiFi6 Touch LCD 4.3 `package` target also passed using the same managed runtime. All collected outputs, package manifest and embedded app image were independently verified. The 1,010,195-byte update archive has SHA-256 `ea7b99257416124eed719b04516f21a28bb1e553e1386225155cbb2c317455e9`; its 1,893,184-byte app image has SHA-256 `103d568ed95fcf6a15439c947ef7812f21a07ef904d1ef22db5dae7b3db33240`. Independent inspection also matched the retained 22,166,444-byte RISC-V ELF and 9,989,768-byte map against the firmware metadata. This establishes package generation with compile-only Wi-Fi credentials, without hardware validation.

The BK7258 V3 202405 `package` target passed with both AP and CP outputs. Independent checks verified the collected archive, firmware metadata, recovery image, plaintext manifest and embedded application, then matched both ARM ELF/map/bin sets against the firmware metadata. The 1,269,305-byte update archive has SHA-256 `bc00d0a308708bbdf0bcd14eded5b61835e4b23138f07f79d2cb35f6d9405bdc`; its embedded 2,437,120-byte image has SHA-256 `0b1abf221cb56b005a0736d413fb4a57e031df061c99589ca2c0b810227c3c14`. The retained AP ELF is 9,629,296 bytes and CP ELF is 13,118,844 bytes. This establishes the SMP product route; BK IDK and Beken native AES/XTS remain separate qualification requirements.

The ESP32-S31 Mosaico `package` target also passed. Independent checks verified all collected outputs, the plaintext manifest, the 2,217,264-byte embedded image (SHA-256 `c216ce1ae9ed5d879b3854540dcdbaede0fab834c5dcf97bc21abfd8d1c83b3c`), and the retained 22,220,928-byte RISC-V ELF and 14,004,795-byte map. The 1,340,106-byte update archive has SHA-256 `7de7a0e39445ae52fc8a4777f826483c61cf0261ef99b962bfcaa2275a9804d6`. Both this build and BK7258 used compile-only Wi-Fi credentials, without physical device validation.

The AC791N DevKit `package` target passed after adding the observed post-build runtime dependencies through original-source formulae. A fresh six-SDK installation, including offline setup, read-only tool qualification and publication of 75 SDK/tool/runtime packages, passed in 882.67 seconds. `jieli-postbuild` 2026.7.28+2 keeps the original vendor archive and adds the headless library closure; published +1 remains unchanged. Independent checks verified the 1,045,691-byte H2Loader update archive (SHA-256 `fdf66201c6a34b90366d061d7dfab4ce9d36d2769b90a1ef7120af67fd9b9793`), firmware metadata and plaintext manifest, the embedded 1,056,917-byte `update.ufw` (SHA-256 `1efd69a120023ebde4ee702643c66e42c7e6fef7941ded8a87f180cac6112562`), and the retained 9,505,620-byte native ELF. This establishes firmware packaging without physical device validation.

The AC707N upstream vendor example also compiled and linked using managed `pi32v2` tools through QEMU. GizMoth collected its 10,944,112-byte ELF (SHA-256 `11344cdf3a8f4c91306010a90153882889de7606577b491b68e2315371da2a01`), 717,205-byte map and ELF header; independent checks verified the collected sizes, hashes and executable header. The vendor post-build script was disabled for this example. This qualifies vendor linking, without a GizOS product package or hardware qualification.

Record acquisition, installation, Bazel analysis, linked ELF and packaged firmware separately. BK7258 qualification requires both AP and CP outputs. A JieLi native Bazel action performs postbuild packaging after linking, so a later packaging failure does not erase evidence that an ELF linked. A successful package build establishes offline artifact generation; physical flashing, boot and device behavior require their own authorized checks. Apple/Xcode and Windows/MSVC targets cannot be qualified by this Linux guest.

### Other live execution checks

A real VM check also verified synchronization of file, directory and dangling source symlinks into the host-backed build copy; writes through `/src` were denied and the original host source remained unchanged.

The real packer ran a setup command, then compiled and executed a probe as UID 1000 with installed programs and probe sources mounted read-only. A deliberately missing required stdout string rejected the next pack while preserving the previously published runtime; both VM reservations were released. Reproduce this execution-layer check with an isolated verified archive seed:

```bash
GIZMOTH_LIVE_PACK_SEED=<isolated data>/compile cargo test --locked --manifest-path src-tauri/Cargo.toml --lib live_pack_runs_setup -- --ignored --nocapture
```

The opt-in isolated native app test, extended with host-directory and source-link assertions, is:

```bash
GIZMOTH_LIVE_COMPILE_TEST=1 node --test --test-concurrency=1 test/integration/compile.test.mjs
```

It uses staged boot resources. `GIZMOTH_E2E_COMPILE_SEED=<app data>/compile` can reuse verified downloaded runtime data through APFS clones. The fixture stays off the LAN and shuts down its VMs afterward. Windows/Linux hosts, Intel Mac VM hosts and builds from LAN workspace paths are not supported. Installing a build artifact on physical hardware and signing the packaged OpenVMM sidecar require their own verification.
