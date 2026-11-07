# Embedding and runtime capabilities

Host lifetime, worker threads, source package, build and bindings; capabilities are supplied by firmware.

## Embedding layers and source package

`//libs/lua:lua_runtime` is the portable lower layer. It contains Lua Core, Host, modules, `//libs/trie`, `//libs/runtime`, PAL headers/inline wrappers, and Bazel-pinned Lua 5.5 and yyjson. Its platform access depends only on PAL interfaces, never concrete providers, boards, SDKs or `bleikcp`. `//libs/lua:lua_core` continues to depend only on upstream Lua. `//libs/lua:lua` retains the existing entry point; existing firmware, Desktop/Web launchers or external embedders assemble the platform. Optional `//libs/lua:lua_link` integrates `bleikcp` in the upper layer and is excluded from the portable source package. When disabled, it retains the existing `link: unavailable` contract.

**PAL vtables are the embedding hooks.** Embedders construct existing `h2_pal_*_api_t` objects with `user + vtable` and put them in Runtime config; no second callback ABI is needed. Runtime initialization requires the full API surface, with canonical unsupported API objects for unsupported capabilities. The source package therefore includes portable implementations from `//libs/pal:unsupported` for embedder defaults, without adding a provider dependency to `lua_runtime`. Display, Button, Touch and Audio can come from the host UI. Memory, Task, Queue, Sync, Time, Timer and Filesystem can come from existing providers or the host's own C implementations.

### Threads and lifecycle

Vtable functions may be called from Runtime input tasks, Lua workers or PAL's own threads; UI/main-thread execution is not guaranteed. API `user`, vtables and underlying resources must outlive all Runtime and Host use. First stop input and external completion producers, then stop/join/destroy Host, and finally deinit Runtime and release providers. Do not reenter the same Lua VM directly from PAL callbacks.

For UI bridges allowing only asynchronous notification, C vtables should copy pixels, rects, audio or other buffers valid only during the current call into host-owned bounded queues, then return promptly according to PAL contracts. Insufficient capacity must return the appropriate error or backpressure; do not retain borrowed pointers, wait for UI callbacks or falsely report unaccepted data as accepted. Services requiring synchronous results, such as Memory, Sync and Queue, must be implemented natively; asynchronous UI callbacks alone cannot satisfy those vtables. Existing PAL return, timeout, partial-write and lifetime semantics remain unchanged.

Capabilities register before `h2_lua_host_start()`; start freezes the registry. An asynchronous call callback copies input/options and request ID, then returns `H2_PAL_ERR_WOULD_BLOCK`. When work finishes, the host calls `h2_lua_capability_complete()`, and the owning worker resumes Lua. Lua receives the existing `ok, output, error` tuple. Hosts must handle cancel callbacks, promptly release queued work and stop all completion producers before destroying Host. Late or duplicate completions are rejected.

Registry `capability_capacity` is shared by exact and prefix entries. Zero defaults to 16; maximum `H2_LUA_CAPABILITY_CAPACITY_MAX` is 256. Exceeding it makes create return `H2_PAL_ERR_INVALID_ARG`; registering into a full registry returns `H2_PAL_ERR_FULL`. Entry and route storage are allocated through Runtime Memory at create. Names and prefixes are copied, with lengths of 1..47 bytes, below internal `H2_LUA_NAME_MAX=48`. 256 maximum-length names require at most 12,033 trie nodes including root, approximately 118 KiB. This supports hundreds of host capabilities while bounding memory and staying within `h2_trie`'s 16-bit index range. Default capacity does not reserve this maximum node space.

`h2_lua_host_start()` allocates node storage using `1 + sum(strlen(name))`, builds immutable lookup through `//libs/trie`'s `h2_trie_build()`, then starts workers. Node allocation failure returns `H2_PAL_ERR_NO_MEMORY`; Host can still be destroyed or start retried. `capability.call()` selects an entry through `h2_trie_handle()` without scanning all registered names. The trie traverses bytes; sibling search at each level is bounded by the byte character set rather than growing linearly with registration count.

`h2_lua_register_capability_prefix()` can register the `litelink.` namespace in one call. It follows native trie semantics: complete exact matches win, otherwise the longest prefix wins, independent of registration order. A prefix must have a nonempty remainder, so `litelink.` alone does not match that prefix. Duplicate registration of the same exact name or prefix returns `H2_PAL_ERR_INVALID_STATE`. Identical text can be registered once as exact and once as prefix, using separate capacity slots. Registration is rejected after start; callers must serialize registration/start/destruction. Prefix callbacks additionally receive the complete requested name, borrowed only for the current call; asynchronous work must copy it. They reuse exact-entry request, completion, cancel and Lua-tuple paths. Unknown names still return `false, nil, "unknown capability: <name>"`. The registry does not enumerate names, because registering a namespace is not a list of every callable name.

### Build and manifest

Build the source package on a macOS or Linux host:

```sh
bazel build //libs/lua:runtime_sources
python3 libs/lua/tests/test_source_package.py bazel-bin/libs/lua/gizos-lua-runtime-src.tar.gz
bazel test //libs/lua:all
bazel query 'deps(//libs/lua:lua_runtime) union deps(//libs/lua:lua_core)'
bazel query 'filter("//libs/pal/providers/|//libs/bleikcp|//boards/|//native_component_src/", deps(//libs/lua:lua_runtime))'
```

The root of `gizos-lua-runtime-src.tar.gz` contains `manifest.json` and the GizOS `LICENSE`. Other C/H files preserve package-relative paths. External repository files for Lua, TLSF and yyjson reside under stable `third_party/lua/`, `third_party/tlsf/` and `third_party/yyjson/` paths respectively, with manifest paths rewritten accordingly. A Bazel aspect generates the file list, include directories, defines and per-translation-unit compiler arguments from the configured dependency graph; no manually copied list is maintained. The package uses the same Lua source selection and restricted standard libraries as GizOS native builds. Firmware-only stdio/newlib shims remain selected by existing embedded build configuration; ESP libc compatibility code is not added to native hosts. It contains a portable C11 atomic provider, but no precompiled libraries, Bazel toolchains, PAL providers or board code. New trie C sources and headers are also included automatically through the dependency graph.

Manifest schema version 1:

| Field | Contract |
| --- | --- |
| `schema_version` | Integer `1`; consumers reject unknown versions |
| `runtime_profile_id` | Fixed as `runtime.lua.gizos` |
| `sources` | All `.c` files to compile once, relative to extraction root |
| `include_dirs` | Include search paths relative to extraction root |
| `defines` | Common compiler definitions without `-D` prefixes |
| `cflags` | Common C compiler arguments, each JSON-array item a separate argument |
| `compilation_units` | Groups of `sources`, extra `cflags` and extra `defines`; each source belongs to exactly one group |
| `per_os` | OS-name mapping to additional `link_flags`; math library is `-lm` for `linux`, `darwin`, `android` and `ios` |

Consumers apply common and group-specific arguments to each source, append architecture, sysroot, PIC, visibility and deployment target for their target toolchain, then link all objects with OS extras. Arguments must be passed individually to the compiler, not concatenated and parsed through a shell. The current package expresses a GCC/Clang C11 compilation contract. Windows/MSVC and WebAssembly toolchains require separate adaptation and are not declared supported by this manifest. Profile ID identifies the Lua surface and does not replace a content-id pin. Sources, headers and bindings from different content versions must not be mixed. The manifest contains no commit, release version or timestamp; `schema_version` identifies only the manifest format.

The standalone test depends only on Python 3.11.8+ (tar extraction filter support), `cc`/Clang and pthread. It extracts into a temporary directory, compiles all C sources according to the manifest, and links test-provided OS, 240×240 Display, Touch and `ok`/`back` Button vtables. Tests verify async echo, display dirty rects and all pixels, Runtime push edges reaching Lua callbacks, pending-job cancellation, default/configured capacities, exact/prefix precedence, complete-name delivery, duplicate and post-start registration rejection, and node allocation failure/retry. The Python runner neither invokes Bazel nor searches the checkout for runtime sources; `CC` can specify a compatible compiler. Bazel test simply supplies the source package and harness as inputs to the same runner.

When invoking an App method, embedders make the main chunk `return app[method](...)`, wait for job success, query length, allocate a host buffer, copy through `h2_lua_job_get_result()`, then release the job. Flutter and cgo use the same public accessor, without a private native module to transfer return values. Apps should explicitly encode complex values in a stable format such as JSON.

### Content identity and release downloads

A Bazel action generates `bazel-bin/libs/lua/runtime_sources.content_id`, containing the complete lowercase hexadecimal SHA-256 followed by one LF. It can be built separately through `//libs/lua:runtime_sources_content_id`; building `//libs/lua:runtime_sources` also generates it.

The algorithm is precisely defined: enumerate all regular files in the package, including `LICENSE` and `manifest.json` but excluding directory entries, tar headers and the content-id sidecar. Paths are POSIX paths relative to archive root, without `./` prefixes. Sort lexicographically by UTF-8 path bytes. For each file, concatenate `UTF8(path) + 0x00 + ASCII(lowercase_hex(SHA256(file_bytes))) + 0x0a`, then SHA-256 the entire concatenation. Paths come from the controlled C source graph and cannot contain newlines, NUL or absolute paths. Hash input contains only paths and file bytes, so it is independent of mtime, machine, absolute paths, Bazel canonical repository names, commits, timestamps and gzip/zlib/rules_pkg versions. Any change to file content, paths or the file list changes the id. The manifest is itself content, so compiler-argument changes also change the id.

The local archive remains `gizos-lua-runtime-src.tar.gz`. The release slice copies it to `gizos-lua-runtime-src-<content_id>.tar.gz` without changing internal bytes and generates `.sha256`. This `.sha256` checks the compressed tar download and differs from the uncompressed-content `content_id`. Changing compressors may preserve content id while changing download SHA-256/size.

LiteLink reads `packages.lua_runtime` from [manual Release metadata](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/bazel.md#lua-源码包进入手动-release), pinning `{url, sha256, size}` and `content_id`. URLs use `https://github.com/GizClaw/gizos/releases/download/v<release_id>/<file>`, never latest. Check download length and compressed-file SHA-256 first, then safely extract and recompute content id. Bindings must match packaged headers. Release metadata commit is for traceability only and must not be injected into the Lua manifest. LiteLink supplies its own PAL implementation in the Flutter App's native layer.

The standalone test can explicitly accept a sidecar and recompute content identity before compiling:

```sh
python3 libs/lua/tests/test_source_package.py bazel-bin/libs/lua/gizos-lua-runtime-src.tar.gz libs/lua/tests/test_embedder.c bazel-bin/libs/lua/runtime_sources.content_id
```

Bazel test does not recursively launch Bazel. See [Lua source packages in manual Release](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/bazel.md#lua-源码包进入手动-release) for local release-slice validation commands.

### Flutter and cgo

LiteLink needs a new native host library and Dart bridge. Its build reads the extracted manifest and compiles every translation unit with Apple Clang for iOS device/simulator and NDK Clang for each Android ABI, then links with LiteLink native code. Common and group-specific manifest arguments must be passed individually to the compiler. LiteLink target build configuration supplies target SDK, architecture, PIC, visibility, deployment target and system libraries. It must not depend on GizOS Bazel archives or precompiled libraries.

LiteLink's native host library creates and owns PAL objects and their lifetimes, implements Runtime's allocator, task/synchronization, time, filesystem, display and input interfaces, and connects platform capabilities to iOS/Android. The packaged C11 atomic provider does not replace PAL. Dart calls stable Host/Runtime C APIs through `dart:ffi`. The native bridge handles thread boundaries, callback lifetimes and message copying, then hands UI operations to Dart rendering. FFI bindings must match packaged headers' struct layouts and callback signatures. LiteLink does not yet have this native build and bridge. macOS/Linux source-package compilation tests validate only portable sources and the manifest; they **do not prove iOS/Android targets have compiled or the App is integrated**. LiteLink must complete mobile acceptance using actual target toolchains, each ABI and App startup/call paths.

Go/cgo consumers likewise read the manifest in their own build steps, compile packaged sources and their own PAL bridge with the target C compiler, and pass objects/archives to the cgo linker. cgo does not recursively compile C files in these subdirectories and cannot ignore source-group-specific flags. Go initiates jobs, pushes input and completes capabilities through a C bridge. PAL `user` may be a C-allocated context or managed opaque handle; unmanaged-lifetime Go pointers must not be left with workers. Host pthread, UI framework and other dependencies are declared by the upper bridge and are outside the portable runtime manifest.

### Prepared numeric stages and shared geometry

A workspace copies state through `load` by default. Distributable Lua apps may also create standard f64 numeric buffers and explicitly retain current and historical positions through `bind`. Once bound, public buffer writes and `node` updates are mutually visible, and drawing consumes current positions directly without complete export each substep. A buffer can be bound to only one live workspace. The workspace strongly references buffers to keep them alive; weak owner records do not prevent workspace collection. Rebinding resets active topology, lambda, span and sweep. Successful `load` safely copies data then leaves bound mode; failure preserves old state. Each stage retains private scratch and atomic commit. Bound state cannot simultaneously serve as stage coefficients, bounds, mobility or `copy` output. `multipliers` returns only mathematical results for selected edges and spans; Lua still interprets tension, line payout and game rules.

`vmath.constraints` provides fixed-capacity, VM-charged stage workspaces. `load/begin` explicitly reset lambda and alternating sweep order. `integrate` applies caller-provided two-stage gains and increments; `node/edge/span` only read or update explicit state. Each `solve` iteration performs edges, spans and position bounds in order. `damp` performs neighbor displacement and ordered axial damping. Lua applies endpoint-dependent application rules after integration. Each stage commits atomically on its own; failure neither undoes the previous successful stage nor clears existing lambda. The [API generated from the numeric production header](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/references/lua.md) defines complete arguments, ranges, two-float compensation and float displacement rules.

`geometry.rotations` stores only caller-provided segments, weights and rotation axes. Endpoint reduction uses 18 moments/a degree-17 polynomial inside an explicit error domain, and the complete loop outside it. It does not generate shapes, weight functions or force models. `geometry.batch` stores immutable 2D vertices, topology and two optional displacement-weight channels. `geometry.pose` stores deformation, rotation, scale, translation and optional parameterized planar perspective results computed in original order. Cache keys include every evaluate input and immutable geometry, before layer/color; applications choose when to share.

`display.draw_pose` consumes poses directly through existing polygon/line rasterization. `display.polyline` / `compile_line_style` / `draw_polyline` preserve source-segment identity, direction, shared-endpoint projection, strict opposite-sign splitting and near clipping. Colors are evaluated at original endpoints, interpolated and floored in RGB888, then converted to RGB565. Segments touching or lying in the plane use caller-supplied boundary styles. Changes to points, camera, plane, order or style prepare transient fragments anew, without a stale projection cache across draws. Each draw's clip, layer and color affect only that repaint and do not invalidate published pre-layer poses. See [Display API](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/references/lua.md) for complete drawing arguments and camera layout.

These objects and scratch are all held by VM userdata. Successful warm calls neither allocate nor invoke Lua per element. Construction failure and GC/VM teardown reclaim all references. Drawing rechecks Display acquisition after argument parsing and fully validates final coordinates/styles before writing pixels, using existing dirty/background bookkeeping without implicit present. Game state, scalar force equations, material conversion, shape-channel generation, camera recipes, layer semantics and sampling clocks remain the distributable Lua app's responsibility. Public functionality only decomposes the original algorithms. Stage-by-stage frame-rate matching on actual devices requires paired downstream validation; Host/Web tests cannot replace it.

Prepared workspace `displacements` subtracts double positions over a specified range, converts the differences to f32, then writes a reusable packed xyz output prefix. In `displacement-f32` integration, before/gain0/gain1 may individually be f32 or f64, while mobility/after/bounds remain f64. Lua still decides environmental branches and coefficient formulas. Explicit `vmath.length3_refined` uses the original float square-root seed and one double correction. See the numeric Public Header for domain, error and fallback; existing `length3` and `normalize3` are unchanged.

Library drawing regressions use generic generated inputs and independent reference algorithms: `material_strip.lua` covers strips and empty faces, `quad_material_projective.lua` covers projective mapping and Q24 boundaries, and `raster_workload.lua` covers polygons and lines. Consuming repositories validate complete application poses, gameplay and full-frame sequences; public tests do not store a second copy of game replay data. Default tests retain the complete callback counts needed for correctness. Timing uses explicit entry points such as `lua_test --strip-benchmark`, `--projective-benchmark` and `--primitive-benchmark`. Combining tests does not relax existing script or VM limits.

## Sources

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L611), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
