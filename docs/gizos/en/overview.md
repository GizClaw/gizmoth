# Lua Runtime overview and module inventory

Lua 5.5 sandbox, runtime modules, args, buttons, display and timing capabilities.

# Lua Runtime

`libs/lua` provides the Lua 5.5 VM Core and a Runtime Host that borrows `h2_runtime_t`. Core handles only text chunks, the Lua stack, GC, coroutines and restricted standard libraries. Host handles the allocator, workers, Filesystem, event delivery, native modules and each Skill's isolated lifecycle.

The Host's normalized `allocator` is also used for Lua Link objects, mutexes/condition variables, BLE-KCP and Lua audio tracks. Worker and Link task stacks are configured by the platform task provider. A NULL Host allocator still falls back to Runtime mem; shared platform resources and driver internals retain their own allocators.

## Ownership

```text
App owns Runtime event consumption
        │ explicit copied event + job_id
        ▼
Runtime Host ── one job / one lua_State ── fixed Runtime worker
        │
        ├── Runtime Memory / Time / Task / Queue / Sync / Filesystem
        ├── private yyjson conversion
        ├── Runtime Display / Touch / Audio System singleton modules
        └── Runtime physical component proxy

VM Core ── upstream Lua 5.5 only; no Runtime or PAL dependency
```

Host borrows Runtime. The caller must keep Runtime alive until `stop`, `join` and `destroy` have all completed. From publication through destruction, a VM is entered serially by one fixed worker; multiple VMs can be assigned to multiple Runtime Tasks. Lua coroutines are cooperative tasks within one VM: they neither allocate PAL Task stacks nor run in parallel across CPUs. The Web Task provider advances them cooperatively on one browser thread; Desktop and device providers can run different VMs in parallel on multiple workers.

The current build surface includes Desktop, Web, ESP32-S3/P4 and BK7258. Embedded builds do not provide upstream Lua with libc files or standard streams. The vendor overlay forcibly includes `h2_lua_embedded_stdio.h`, which, after `<stdio.h>`, redirects `stdin`, `stdout`, `stderr` and the stdio functions Lua uses, such as `fopen` and `getc`, to fail-closed shims in `//third_party/lua_patch`. This prevents C library macros from expanding them through reentrancy state such as `_impure_ptr`. Only ESP32-S3/P4 additionally link `lua_esp_libc_compat.c` to supply newlib compatibility symbols missing from ESP-IDF picolibc; BK uses the SDK toolchain's newlib. `//libs/lua:lua_firmware_abi` runs a firmware archive ABI check on the complete Lua archive closure in every embedded configuration. GizOS itself has no BK image that links Lua; the firmware consuming Lua is responsible for runtime validation on BK devices.

## Host and jobs

All capacities in `h2_lua_host_config_t` are bounded: `worker_count`, `worker_stack_size`, `max_jobs`, `max_coroutines_per_vm`, `ready_queue_capacity`, `waiter_capacity`, `event_delivery_capacity`, `callback_capacity_per_job`, `audio_track_capacity_per_job`, `pending_capability_capacity`, `capability_capacity`, `instruction_quantum`, `resume_time_budget_ms`, `source_limit_bytes`, `output_limit_bytes`, `vm_memory_limit_bytes` and `vm_heap_bytes`. Zero selects the declared default. Ready/waiter capacities must not be smaller than the VM's coroutine limit. `storage` configures persistent storage per App; see [App storage](storage.md). An all-zero configuration means storage is unconfigured.

The optional `allocator` specifies the `h2_pal_mem_api_t` used for all Host-owned allocations: Host/job state, queues, buffers, audio tracks, Lua Link, reserved VM heaps and, without a reservation, individual VM blocks. NULL uses Runtime mem. Host borrows this pointer without copying it, so both the pointer and its `user` context must remain valid until `h2_lua_host_destroy()` returns. Callers can use this to place Host-owned storage in their own arena. This is called the “Host allocator” below.

The optional `vm_heap_bytes` reserves a dedicated VM heap from the Host allocator during `h2_lua_host_create()`. All jobs/workers in that Host share it through TLSF protected by a PAL mutex. The default `0` keeps allocating VM blocks individually from the Host allocator; the Web entry point also keeps this default. Reservation suits devices with fragmented system heaps, where large allocations such as strings or full-screen `display.capture_region` userdata compete with other modules for contiguous space. The VM itself, Lua states, userdata, strings and tables use the reserved heap; callbacks, events, tasks and framebuffers use the Host allocator directly.

When the Host allocator has a sufficiently large contiguous block, reservation uses one block. Otherwise, after each rejected allocation, Host reduces the requested size by 1/8 to approach the largest actual free block. It takes at most 16 blocks, each at least 64 KiB, except that the final remainder may be smaller, and adds them to the same TLSF instance. Taking the largest blocks first places pools capable of serving large individual allocations first; the minimum only determines how much of the tail remains usable. A heap with 512+384+256+256+192 KiB and five 64 KiB blocks offers 1.9 MiB, whereas a 256 KiB minimum could collect only 1.4 MiB. Each individual VM allocation must still fit in one block. If these limits prevent reserving enough memory, creation returns `H2_PAL_ERR_NO_MEMORY`, creates no Host and leaks no acquired blocks. Destroy returns all blocks after all jobs/VMs have been released.

`vm_memory_limit_bytes` remains a separate per-VM quota; reservation size does not alter quota checks. The reserved heap must cover every live VM, including completed jobs not yet released, plus TLSF metadata and per-block allocation headers. Measure actual workloads rather than equating the reservation with the quota. A reservation smaller than the quota is allowed; in that case the reserved heap runs out first. Lua still attempts emergency GC before reporting an unsatisfied allocation as a memory error. The minimum nonzero value is `tlsf_size() + tlsf_pool_overhead() + 8 * (tlsf_block_size_min() + tlsf_alloc_overhead())`; the usable pool must also not exceed `tlsf_block_size_max()`. Out-of-range values return `H2_PAL_ERR_INVALID_ARG`.

The normal Host lifecycle is:

1. `h2_lua_host_create()` borrows Runtime and allocates fixed capacities.
2. Register native modules and capabilities before starting.
3. `h2_lua_host_start()` freezes the registry and creates workers.
4. Submit a job from text, a compiled resource or Runtime Filesystem, supplying the app id that determines the `storage` scope; it may be `NULL`. `h2_lua_job_submit_path()` accepts a caller-supplied path and chunk name. `h2_lua_job_submit_file()` accepts a restricted relative path and automatically generates the `@<path>` chunk name. For both, Host feeds source to the compiler in chunks through its own 4 KiB window. Neither caller nor Host holds the complete source: on a fragmented heap, a 200 KiB app no longer needs a contiguous block of that size. Exceeding `source_limit_bytes` returns `NO_SPACE`, embedded NUL returns `INVALID_ARG`, and a precompiled chunk returns `FORMAT`, as with text submission, but checked as bytes arrive. The reader closes the file when it reads the last byte, or on the first read for an empty file. If a compilation error ends reading early, it also closes the file before publishing a `FAILED` job. Only successful closure allows job publication and worker wakeup. Filesystem failures, including close failures, or truncation during reading abort submission, release the job slot and return the original result. `*out_job_id` remains `H2_LUA_JOB_ID_NONE`; no job is created or run. Early rejection also closes an already-open file. An existing submission or read error takes precedence over a close error during cleanup.
5. App consumes the Runtime Event queue and uses `h2_lua_dispatch_runtime_event()` to deliver explicitly to a live `job_id`. Enqueueing returns `H2_PAL_OK`; malformed events or a component kind that does not match the Runtime component return `H2_PAL_ERR_INVALID_ARG`; unknown or released jobs return `H2_PAL_ERR_NOT_FOUND`; terminal jobs return `H2_PAL_ERR_CLOSED`; and reaching the job's `event_delivery_capacity` for undelivered events returns `H2_PAL_ERR_FULL`. Failure does not enqueue the event. A job can become terminal between events in the same Runtime batch, so `CLOSED` can immediately follow `OK` for the same job.
6. `stop()` rejects new jobs and cancels waits; `join()` waits for workers to exit; finally call `destroy()`.

`h2_lua_host_step()` only signals workers that new work exists; it never enters a VM on the calling thread. Asynchronous capability completion only records completion and wakes a worker. Actual Lua resumes, callbacks and resource release happen on the owning worker or after stop/join. Each resume records its own start time. The instruction hook checks cancellation and the overall job timeout every `instruction_quantum`, and yields after reaching `resume_time_budget_ms`, allowing CPU-bound Lua to give its owning worker back. Each Runtime event callback also runs as a short-lived scheduler task within the same VM, so it can yield and is subject to the same quantum, cancellation and timeout limits.

The shared Runtime payload for Button `ACTION` contains only `pressed_at_ms` and `released_at_ms`. When projecting it into a VM, Lua Runtime computes `duration_ms` and supplies Button `gesture_kind`: 1 for a normal press, 2 for a short press and 3 for a long press, with a compatibility threshold of 500 ms. This classification belongs only to the Lua adapter and is not part of the shared `libs/runtime` Button payload.

`runtime.sleep(ms)` and `delay.delay_ms(ms)` record a Runtime monotonic wake deadline for the current coroutine, then yield. The fixed worker scans waiters and shortens its wake-queue wait to the nearest deadline. It does not create, stop or destroy a PAL Timer for each sleep, or create native threads for coroutines. Cancel/Stop use the existing worker wakeup and join mechanism without waiting for a long sleep to expire. Other platforms' PAL Timer capabilities remain independent. `//libs/lua/web/tests/sleep_progress:browser_test` uses an injected API that rejects Timer creation on real pthread WASM to verify progress through 240 sleeps, return values, and Stop/teardown during a long sleep.

## Lua surface

| Module | Initial interfaces | Backing / boundaries |
| --- | --- | --- |
| Standard libraries | base, coroutine, table, string, math, utf8, restricted package, `debug.traceback` | No `io`, `os.execute`, `load`, `string.dump`, native loader or bytecode |
| `args` | Independent argument table per job | Copied to Lua strings at submission |
| `print` | Bounded single-line diagnostics | Runtime Log |
| `runtime` | `components.get/on/off`, `spawn/yield/sleep/status/join/cancel`, `event.*` | Numeric component IDs; VM coroutines; no exposed periph IDs |
| `json` | `encode`, `decode` | Private yyjson provider; bounded depth and output |
| `capability` | `call(name, payload, options)` | Frozen C registry; immediate/pending/cancel/late completion support |
| `delay` | `delay_ms`, `delay_us` | ESP-Claw profile; millisecond waits yield, microsecond waits use Runtime monotonic time |
| `system` | `time`, `date`, `millis`, `uptime` | Runtime Time; fixed UTC offset |
| `display` | `clear`, rectangles, lines, circles, AA circles, rounded rectangles, triangles, polygons, ellipses, retained commands, framebuffer fade, frame, text, `present` and `deinit` | Direct Runtime singleton Display API; dirty regions always clipped to the framebuffer |
| `lcd_touch` | `read`, `poll`, `sync` and upstream touch result fields | Direct Runtime singleton Touch API; no SDK handle accepted |
| Button proxy | `get_key_level` | Runtime normalized Button snapshot; does not create a GPIO button |
| `storage` | `get_root_dir`, `join_path`, `exists`, `stat`, `read_file`, `write_file`, `listdir`, `remove`, `rename`, `get_free_space` | Host-configured PAL Filesystem; one flat directory per app id, quota and file-count limits, atomic write replacement |
| `kv` | `get/set/remove/exists/keys` | Persistent scalar KV sharing the App namespace, quota and Host mutex with storage |
| `audio` | `new_output` (`write/info/close` per Track), `new_input` (`read/level/info/close`) | Direct Runtime singleton Audio System; Track frame size comes from the device playback format, Input frame size from the device mic format; PAL mixes multiple Tracks; no codec handle accepted |
| `link` | `available`, `host`, `join`, `send`, `send_unreliable`, `write`, `read`, `close`, `state`, `on`, `off` | After Launcher calls `h2_lua_link_enable()`, `//libs/lua:lua_link` provides it through BLE Host PAL (unreliable messages) and `libs/bleikcp` (reliable messages and byte streams); when disabled or BLE is absent, `available()` is `false` and operations return `nil, "link: unavailable"` |

`kv`, `link` and `runtime` are new GizOS modules, outside the ESP-Claw compatibility inventory.

`runtime.components.getByName()`, `board_manager`, SDK handles and dynamic C modules are outside the initial contract. Display, Touch and Audio retain ESP-Claw's module acquisition, calling Runtime singleton PAL APIs directly inside. Only physical peripherals such as Button replace their constructors with `runtime.components.get(id)`. Each `audio.new_output()` creates an independent PAL Track. A single job can write to multiple Tracks concurrently up to `audio_track_capacity_per_job`, with mixing handled by Audio System. Each job holding Tracks acquires one Host-level speaker user. Closing that job's last Track or reclaiming the job releases it; Speaker stops only after the last speaker user in the Host is released. Ending one job must not interrupt another job still writing a Track. `audio.new_input()` mirrors this reference-counted lifecycle: a job acquires one Host-level mic user; `close()`, job cancel/timeout/stop or job release releases it. Runtime microphone stops only after the last mic user is released. Ending one job must not interrupt another job still reading.

## Sources

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L1), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
