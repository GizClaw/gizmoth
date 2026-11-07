# Asynchronous Display submission

Single in-flight submit, flush, status and busy handling for frame loops.

## 单在途 Display 提交原型

所有 target（含 Web）的 Display 都使用公共显示 worker，没有同步后端模式或启用开关。首次 `require("display")` 获取 Display 时创建 `$lua/display` 任务，未使用 Display 的 Job 不创建显示任务。`present/end_frame` 仍等待当前帧完成；`submit/flush/status` 提供单在途异步提交，完整合同以 `h2_lua_display.h` 为准。

默认 `//libs/lua` / `:lua_runtime` 的任务清单包含 `$lua/worker` 和 `$lua/display`，下层 Runtime 的既有任务清单不变。消费目标必须为 `$lua/display` 配置核、优先级、栈和内存区域，或审查预算后显式填写 `default`；缺失策略仍由原有 task-policy audit 拒绝。源码包包含同一实现，非 Bazel embedder 提供 PAL Task、Sync semaphore、Time 和 Display，并负责自己的任务预算。Host 零初始化配置即可使用 worker，`display_worker_stack_size` 只调整栈预算。

Host 支持多个 Job，但同时只允许一个 Job 获取 Display，竞争获取返回 busy；成功 `deinit` 或 checked release 后其他 Job 可重新获取。这个占用保护不能协调其他 Host、Runtime 和直接 PAL/UI writer，调用方必须先排空并暂停这些外部访问。后端必须允许串行移交到一个任务，且成功的 draw/present 必须完成传输。open/info/draw/present/close 在同一提交任务执行；`borrow_display` 保留不调用 PAL open/close 的合同。

Display 只持有两份全屏 RGB565 像素：Lua 绘制缓冲 A 与 worker 发送缓冲 B。规划在发布前比较 A 与上次成功的 B；worker 先按最终计划把 A 复制到 B，保持原始行 stride，然后发布复制交接完成并通过 PAL 发送 B。`submit` 等待复制交接，随后 Lua 可以继续绘制 A，与 B 的传输并行；它不等待传输完成。首次发送缓冲分配期间的 finalizer 重入提交返回 busy，避免再分配另一份全屏 B；OOM 会先清除这个准备保护再抛出错误，释放其他内存后可正常重试。B 完整传输成功后直接兼任 retained baseline，回执不再复制整帧或清除下一帧 dirty 状态。首帧或基线失效仍全帧复制，无变化帧不复制像素；没有第三份全屏快照。完整 tile 位图在复制、传输阶段分别遍历，超过矩形容量也不截断。

提交最多有一个未完成帧，包括复制与传输阶段；busy 由 Lua 调用方通过 `runtime.sleep()` 后重试，没有额外排队帧。发送缓冲 B、mailbox 和有界计划使用 VM userdata，受 `vm_memory_limit_bytes` 与共享 VM 堆约束。240×240 RGB565 两份像素合计 230400 字节：获取时由 Host allocator 分配 A，首次提交（包括 `present`）只增加一份 115200 字节的 VM 计费 B 加元数据。这里不包含应用主动创建的背景/区域缓存或驱动自己的 DMA 缓冲。PAL Task/Atomic/Sync 的内部资源属于平台预算，不冒充 VM charge。`display_worker_stack_size` 默认请求 8192 字节，实际栈还受 target policy 下限影响。新增 `$lua/display` 任务必须在消费目标的 task policy 声明核、优先级、栈与内存区域，并实测总峰值及连续空闲块；库不修改驱动、SDK 或编译参数。

`deinit`、job release 和 Host join 在帧传输未完成时返回 busy，保留 VM 引用和所有权，调用方稍后重试。关闭请求通过同一 worker 执行；随后 PAL join 可等待退出或返回 busy，成功后才释放占用和缓冲区。`h2_lua_host_destroy_checked()` 只有成功才释放 Host；失败后不能销毁 Runtime 或恢复借用 Display 的 UI。兼容的 void destroy 也保留失败对象，但调用方必须使用 checked 入口判断释放是否完成。PAL 错误会锁存并停止新提交；唤醒或复制交接等待失败不能证明 worker 已停止读取 A，因此同时冻结绘制并保留两份缓冲。提交任务退出后可 join，但不会在不确定设备状态下自动 close/open 或重试。该原型没有解除故障隔离的 API：fault 后保留 Host/VM/device lease，消费端须维持依赖并走其外部恢复或进程重启流程。PAL 的无限等待可能令同步调用或任务无法退出；软件 busy 不意味着已取消硬件，不能强删任务后 free。

帧率只用成功且像素变化的完成记录计算；submitted、无变化 submit 和失败都不能计为变化帧 FPS。时间是 PAL monotonic transport 完成时间，不是面板 scanout；Web 的传输完成也不证明浏览器已合成到屏幕。观察者不能并发读取 worker 正在更新的普通 64 位字段，应消费 `status/flush` 发布的完成记录。已提交的同源消费例位于 `libs/lua/tests/display_submit.lua`，由 native worker 和 `projects/example/targets/pkg_tar/lua-script-submit` 的浏览器测试共同运行。

Host 回归可直接编译生产目录、GizClaw handler 和 Desktop PAL，运行包校验、卸载、持久化、故障恢复及 stream 测试：

```sh
python3 libs/lua/apps/tests/run_host_tests.py --yyjson-dir /path/to/yyjson/src --gizclaw-dir /path/to/gizclaw --sanitize
```

`gizclaw` checkout 应包含生成的 C contract 与已初始化的 pinned nanopb submodule。该验证使用 Host PAL；真机 Golden E2E 另行验证下载、显示与产品任务生命周期。

公共回归 `//libs/lua/apps/web/tests/scoped_http:browser_test` 通过真实 Web FS 和 HTTP 下载安装 format-1 包，跨两次刷新运行 Lua 并验证独立 KV 与 storage 文件在卸载后保留；同一 target 由独立 Bzlmod consumer 执行。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L724), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
