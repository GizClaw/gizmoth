# Lua Runtime overview and module inventory

Lua 5.5 sandbox, runtime modules, args, buttons, display and timing capabilities.

# Lua Runtime

`libs/lua` 提供 Lua 5.5 VM Core 和借用 `h2_runtime_t` 的 Runtime Host。Core 只负责文本 chunk、Lua stack、GC、coroutine 与受限标准库；Host 负责 allocator、 worker、Filesystem、事件投递、原生 module 和每个 Skill 的隔离生命周期。

Host 归一化后的 `allocator` 同时用于 Lua Link 对象、mutex/cond、BLE-KCP 和 Lua 音轨；worker 与 Link 任务的栈由平台 task provider 配置。Host allocator 为 NULL 时仍回退 Runtime mem，共享平台和驱动内部资源保留各自的分配器。

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

Host 借用 Runtime；调用方必须保证 Runtime 存活到 `stop`、`join`、`destroy` 全部
完成。一个 VM 从发布到销毁只由固定 worker 串行进入；多个 VM 可以分配给多个
Runtime Task。Lua coroutine 是同一 VM 内的协作任务，不分配 PAL Task stack，也
不跨 CPU 并行。Web Task provider 在一个浏览器线程中协作推进，Desktop 和设备
provider 可以让不同 VM 在多个 worker 上并行。

当前 build surface 包含 Desktop、Web、ESP32-S3/P4、BK7258。Embedded
构建不给 upstream Lua 提供 libc 文件或标准流：vendor overlay 强制包含
`h2_lua_embedded_stdio.h`，在 `<stdio.h>` 之后把 `stdin`、`stdout`、`stderr`
以及 Lua 使用的 `fopen`、`getc` 等 stdio 函数重定向到
`//third_party/lua_patch` 中 fail-closed 的 shim，避免 C library 用宏通过
`_impure_ptr` 等 reentrancy 状态展开它们。只有 ESP32-S3/P4 额外链接 `lua_esp_libc_compat.c`，补齐 ESP-IDF picolibc
缺少的 newlib 兼容符号；BK 使用 SDK toolchain 自带的 newlib。
`//libs/lua:lua_firmware_abi` 在每个 embedded 配置中对完整 Lua archive closure
运行 firmware archive ABI 检查。GizOS 自身没有链接 Lua 的 BK image，BK 设备上的
运行验证由消费 Lua 的 firmware 负责。

## Host 和 job

`h2_lua_host_config_t` 的容量均有界：`worker_count`、`worker_stack_size`、`max_jobs`、`max_coroutines_per_vm`、`ready_queue_capacity`、`waiter_capacity`、`event_delivery_capacity`、`callback_capacity_per_job`、`audio_track_capacity_per_job`、`pending_capability_capacity`、`capability_capacity`、`instruction_quantum`、`resume_time_budget_ms`、`source_limit_bytes`、`output_limit_bytes`、`vm_memory_limit_bytes` 和 `vm_heap_bytes`。零使用声明的默认值；ready/waiter 容量不得小于 VM 的 coroutine 上限。`storage` 配置每个 App 的持久化存储，见 [App 存储](#app-存储)；全零表示未配置。

`allocator` 可选地指定 Host 自身的所有分配（Host/job 状态、队列、缓冲、音频 track、Lua Link、VM 堆预留，以及未预留时的每个 VM 块）使用的 `h2_pal_mem_api_t`，为 NULL 时使用 Runtime mem。Host 只借用这个指针、不复制它，因此它和它的 `user` 上下文必须保持有效，直到 `h2_lua_host_destroy()` 返回；调用方可以借此把 Host 自身的存储放进自己的 arena。下文统称为“Host allocator”。

`vm_heap_bytes` 可选地在 `h2_lua_host_create()` 时从 Host allocator 预留一段 VM 专用堆，供同一 Host 的所有 job/worker 通过带 PAL mutex 保护的 TLSF 共享。默认 `0` 保持 VM 逐块向 Host allocator 申请，Web 入口也保持此默认值。适合设备系统堆碎片化、大字符串或全屏 `display.capture_region` userdata 等大块分配会与其他模块争抢连续空间的场景。VM 本体、Lua 状态、userdata、字符串和表都使用预留堆；callbacks、events、tasks 和 framebuffer 等直接使用 Host allocator。

Host allocator 有足够大的连续块时预留为一整块；否则 Host 每次被拒后把申请大小缩小 1/8、贴近实际最大空闲块，最多取 16 块、每块至少 64 KiB（只有最后的余量可以更小），全部加入同一个 TLSF。因为总是先取最大的块，能服务大块单次分配的池排在前面，下限只决定尾部还能用掉多少：一个还剩 512+384+256+256+192 KiB 和五个 64 KiB 块的堆共有 1.9 MiB 可给，而 256 KiB 的下限只能凑出 1.4 MiB。单次 VM 分配仍必须能放进其中一块。在这些限制内凑不够时返回 `H2_PAL_ERR_NO_MEMORY`，不创建 Host，也不泄漏已取得的块；destroy 在所有 job/VM 释放后归还全部块。

`vm_memory_limit_bytes` 仍是独立的每 VM 配额，预留大小不会改变配额检查。预留堆需
覆盖所有同时存活的 VM（包括尚未 release 的已完成 job）以及 TLSF 元数据和每块分配
头；按实际负载测量设定，不要直接等同配额。预留小于配额也允许，此时预留堆先耗尽；
Lua 仍会先尝试 emergency GC，无法满足分配时再报内存错误。非零值的最小值为
`tlsf_size() + tlsf_pool_overhead() + 8 * (tlsf_block_size_min() + tlsf_alloc_overhead())`，
可用池也不得超过 `tlsf_block_size_max()`；越界返回 `H2_PAL_ERR_INVALID_ARG`。

Host 的正常生命周期是：

1. `h2_lua_host_create()` 借用 Runtime 并分配固定容量；
2. 在 start 前注册 native module 和 capability；
3. `h2_lua_host_start()` 冻结 registry 并创建 worker；
4. 通过 text、compiled resource 或 Runtime Filesystem 提交 job，同时给出决定 `storage` 作用域的 app id（可为 `NULL`）；`h2_lua_job_submit_path()` 接收调用方给的路径与 chunk 名，`h2_lua_job_submit_file()` 接收受限相对路径并自动生成 `@<path>` chunk 名，两者都由 Host 用自己的 4 KiB 窗口把源码分片喂给编译器，调用方和 Host 都不持有整份源码：在碎片化的堆上，一个 200 KiB 的 app 不再需要一块同样大的连续内存。超出 `source_limit_bytes` 返回 `NO_SPACE`、内嵌 NUL 返回 `INVALID_ARG`、预编译 chunk 返回 `FORMAT`，与 text 提交一致，只是改为随字节到达时判定；reader 在读完最后一个字节时关闭文件，空文件在首次读取时关闭；若编译错误使读取提前结束，也在发布 `FAILED` job 前关闭。只有关闭成功后才发布 job 并唤醒 worker；文件系统失败（包括关闭失败）或读到一半截断中止提交、释放 job 槽位并原样返回该结果，`*out_job_id` 保持 `H2_LUA_JOB_ID_NONE`，不会产生或运行 job。提前拒绝提交也会关闭已打开的文件，已有提交或读取错误优先于清理时的关闭错误；
5. App 消费 Runtime Event queue，并通过 `h2_lua_dispatch_runtime_event()` 定向
   投递给一个 live `job_id`；事件入队返回 `H2_PAL_OK`，事件格式错误或 component
   kind 与 Runtime component 不符返回 `H2_PAL_ERR_INVALID_ARG`，未知或已 release 的
   job 返回 `H2_PAL_ERR_NOT_FOUND`，job 已进入终态返回 `H2_PAL_ERR_CLOSED`，job 内
   未投递事件已达 `event_delivery_capacity` 返回 `H2_PAL_ERR_FULL`，失败时事件不
   入队。job 可能在同一批 Runtime event 之间进入终态，所以同一 job 上 `CLOSED`
   可以紧跟在 `OK` 之后出现；
6. `stop()` 拒绝新 job、取消等待，`join()` 等待 worker 退出，最后 `destroy()`。

`h2_lua_host_step()` 只用于提示 worker 有新工作，不会让调用线程进入 VM。
异步 capability completion 只记录完成并唤醒 worker；真正的 Lua resume、callback 和资源释放都在 owning worker 或 stop/join 后执行。每次 resume 记录独立开始时间；instruction hook 按 `instruction_quantum` 检查取消和 job 总超时， 并在达到 `resume_time_budget_ms` 后 yield，使 CPU-bound Lua 归还 owning worker。 每个 Runtime event callback 也作为同一 VM 内的短生命周期 scheduler task 执行， 因此可以 yield，并受相同的 quantum、取消和超时约束。

Button `ACTION` 的共享 Runtime payload 只有 `pressed_at_ms` 和 `released_at_ms`。Lua Runtime 在投影到 VM 时自行计算 `duration_ms`，并提供 Button `gesture_kind`：普通按压为 1、短按为 2、长按为 3，兼容阈值为 500 ms。这个分类只属于 Lua adapter，不进入共享的 `libs/runtime` Button payload。

`runtime.sleep(ms)` 与 `delay.delay_ms(ms)` 在当前 coroutine 记录 Runtime monotonic wake deadline 后 yield。固定 worker 扫描等待者并把 wake queue 等待上限缩短到最近 deadline，不为每次 sleep 创建、stop 或 destroy PAL Timer，也不为 coroutine 创建原生线程。Cancel/Stop 通过已有 worker 唤醒和 join 完成，不必等长 sleep 到期；其他平台 PAL Timer 能力保持独立。`//libs/lua/web/tests/sleep_progress:browser_test` 在真实 pthread WASM 上使用拒绝 Timer create 的注入 API，验证 240 次 sleep 持续推进、返回值和长 sleep 中 Stop/teardown。

## Lua surface

| 模块 | 首期接口 | backing / 边界 |
| --- | --- | --- |
| 标准库 | base、coroutine、table、string、math、utf8、受限 package、`debug.traceback` | 无 `io`、`os.execute`、`load`、`string.dump`、native loader 或 bytecode |
| `args` | 每个 job 的独立参数表 | 提交时复制为 Lua string |
| `print` | 有界单行诊断 | Runtime Log |
| `runtime` | `components.get/on/off`、`spawn/yield/sleep/status/join/cancel`、`event.*` | 数字 component ID；VM coroutine；不暴露 periph ID |
| `json` | `encode`、`decode` | 私有 yyjson provider；深度和输出有界 |
| `capability` | `call(name, payload, options)` | 冻结的 C registry；支持 immediate/pending/cancel/late completion |
| `delay` | `delay_ms`、`delay_us` | ESP-Claw profile；毫秒等待 yield，微秒等待使用 Runtime monotonic time |
| `system` | `time`、`date`、`millis`、`uptime` | Runtime Time；固定 UTC offset |
| `display` | `clear`、矩形、线、圆、AA circle、圆角矩形、三角形、多边形、椭圆、保留命令、framebuffer fade、frame、text、`present` 和 `deinit` | 直接使用 Runtime singleton Display API；dirty region 始终裁剪到 framebuffer |
| `lcd_touch` | `read`、`poll`、`sync` 及 upstream touch result fields | 直接使用 Runtime singleton Touch API，不接收 SDK handle |
| Button proxy | `get_key_level` | Runtime normalized Button snapshot，不创建 GPIO button |
| `storage` | `get_root_dir`、`join_path`、`exists`、`stat`、`read_file`、`write_file`、`listdir`、`remove`、`rename`、`get_free_space` | Host 配置的 PAL Filesystem；每个 app id 一个扁平目录，受配额和文件数限制，写入原子替换 |
| `kv` | `get/set/remove/exists/keys` | 与 storage 共享 App namespace、配额和 Host mutex 的持久化标量 KV |
| `audio` | `new_output`（每条 Track 的 `write/info/close`）、`new_input`（`read/level/info/close`） | 直接使用 Runtime singleton Audio System；Track frame 大小取自设备 playback format，Input frame 大小取自设备 mic format；PAL 混合多条 Track，不接收 codec handle |
| `link` | `available`、`host`、`join`、`send`、`send_unreliable`、`write`、`read`、`close`、`state`、`on`、`off` | Launcher 调用 `h2_lua_link_enable()` 后由 `//libs/lua:lua_link` 经 BLE Host PAL（不可靠消息）与 `libs/bleikcp`（可靠消息、字节流）提供；未启用或没有 BLE 时 `available()` 为 `false`，操作返回 `nil, "link: unavailable"` |

`kv`、`link` 与 `runtime` 同属 GizOS 新增 module，不在 ESP-Claw 兼容库存内。

`runtime.components.getByName()`、`board_manager`、SDK handle 和动态 C module
不属于首期合同。Display、Touch 和 Audio 保持 ESP-Claw 的 module acquisition，内部
直接调用 Runtime singleton PAL API；只有 Button 等物理外设把 constructor 改成
`runtime.components.get(id)`。`audio.new_output()` 每次创建独立 PAL Track；同一 job
可以在 `audio_track_capacity_per_job` 上限内同时写入多条 Track，由 Audio System
负责混音。每个持有 Track 的 job 获取一个 Host 级 speaker user；关闭该 job 的最后
一条 Track 或回收 job 时释放它，只有同一 Host 的最后一个 speaker user 释放后才停止
Speaker。一个 job 结束不能中断另一个仍在写 Track 的 job。`audio.new_input()`
镜像同一套 ref-counted 生命周期：job 获取一个 Host 级 mic user，
`close()`、job cancel/timeout/stop 或 job release 释放它，只有最后一个 mic
user 释放后才停止 Runtime microphone；一个 job 结束不能中断另一个仍在读取的
job。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L1), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
