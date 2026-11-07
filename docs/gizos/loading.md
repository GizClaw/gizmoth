# Module loading, errors and return values

require roots, compiled resources versus files, job outcomes and Display ownership.

## Source loading and failure

`h2_lua_resource` 默认将 `.lua` 源文件逐字节嵌入 C resource；需要减少固件只读存储占用的 consumer 可以显式设置 `compact = True`。精简发生在构建期，仅删除注释、行首/行尾空白和多余横向空白，不重命名标识符、不改写表达式、不生成 bytecode，也不增加运行时解压或 buffer。短字符串（包括转义）、任意等号层级的长字符串保持原始字节；token 间仍保留必要分隔，字符串之外的换行按 Lua 的 CR/LF 配对规则归一化以保留原源码行号。含 NUL 的输入（包括注释内）、短字符串内未经转义的换行，以及未终止的字符串或长注释在生成阶段报错；完整语法仍由现有 Lua 文本加载器验证。默认生成行为、C symbol 与 Host lifecycle 不变，`source_size` 和 source limit 以实际嵌入文本计。

生成器的 Python 回归覆盖 token 分隔、字符串/注释边界、换行和默认兼容性；`//libs/lua:compact_resource_test` 在实际 VM 中对比原始与精简后的同一 fixture，验证结果和错误行号。业务 consumer 仍需对自己的精简资源执行功能回归和 exact firmware build，不能把构建期节省直接写成 VM 内存或运行时性能收益。

嵌入资源走 `luaL_loadbufferx` 文本加载，不提供 `luaL_loadfilex` 的 shebang 首行跳过行为；精简不会把 `#!` 首行当注释删除，也不会把 `1..2` 等非法数字改写为合法表达式。空源文件或精简后为空的资源使用一个零字节作为 C backing array，逻辑 size 仍为零；非空资源的默认生成内容不变。

所有入口只加载 Lua 文本。绝对路径、空段、`.`/`..`、反斜线、非受限 root、
bytecode、超限或 malformed chunk 都失败关闭。`package.cpath` 为空，
`package.loadlib` 不存在；local `require()` 只能读取当前 Skill root 下的 `.lua`
文本或 immutable compiled resource。

job 只有一个终态：success、failed、cancelled、timed out 或 Host stop 产生的
stopped。事件、callback、job、coroutine 和 capability request 全部有界；full、
unsupported、invalid payload、missing component/capability 和 late completion 都
返回明确错误，不静默成功。取消后的 capability 在 job release 前保留 CLOSED 语义，
release 时归还 bounded request 槽位。

## Job 返回值

`h2_lua_job_get_result(host, id, buffer, capacity, &size, &has_result)` 只在 `SUCCEEDED` 时读取主 chunk 的第一个返回值，忽略后续返回值。转换遵循 `h2_lua_vm_execute_text()`：string 原样保留（包括内嵌 NUL），number 使用 Lua 数字字符串，`nil` 为 `"nil"`，boolean 为 `"true"`/`"false"`；table 使用 `__tostring`，未定义时为 Lua 默认 `table: ...` 表示，其地址部分不稳定，不能用作 序列化协议。无 return 时 `has_result=0, size=0`；空字符串则 `has_result=1, size=0`。 转换异常使 job 失败；转换不支持 yield。

结果保留在主 task 的 Lua 栈上，计入 `vm_memory_limit_bytes` 和 status 的 `memory_used`，有效期到 `h2_lua_job_release()`。超过 Host 的 `output_limit_bytes`（不含终止 NUL）使 job 进入 `FAILED`，message 为 `H2_LUA_VM_OUTPUT_TOO_LARGE`；等于上限允许成功，不静默截断。

调用方拥有 buffer，`size` 返回不含终止 NUL 的完整字节数。`size` 与 `has_result` 输出指针必填。`buffer=NULL, capacity=0` 查询长度并返回 `OK`； 读取有结果的 job 需要 `capacity > size`，否则返回 `H2_PAL_ERR_NO_SPACE`， 仍报告长度和存在标志，buffer 不变。成功复制后补终止 NUL。 非 `SUCCEEDED` 状态（含 failed/cancelled/timed-out/stopped）返回 `H2_PAL_ERR_INVALID_STATE`，未知或已 release 的 id 返回 `H2_PAL_ERR_NOT_FOUND`； 无效参数或 `H2_LUA_JOB_ID_NONE` 返回 `H2_PAL_ERR_INVALID_ARG`。 这些错误将有效的输出长度和存在标志清零。

## Validation

```sh
bazel test //libs/lua:all
bazel test //libs/lua:lua_link_test
bazel query 'somepath(//libs/lua:lua_core, //libs/runtime:runtime)'
rg -n 'h2_runtime_(poll|wait)_event' libs/lua/src
bazel run //projects/e2e/targets/cc_binary/lua-runtime:e2e-lua-runtime
```

query 和 `rg` 都应为空。E2E 的九个固定 case 见 [E2E 测试 App](/apps/e2e)。

单个 Lua 脚本可以用 `//libs/lua/web:lua_web_app.bzl` 的 `h2_lua_web_app()` 直接生成浏览器页面、`:serve` 与
`:browser_test`，不写 C 入口；见 [Web](/apps/web)。

## 借用 Display 与 UI 交接

已有 UI 持有 Display 时，Host 配置的 `borrow_display` 可借用已打开设备。
调用方先暂停其他写屏者，并保证同一时间只有一个使用 Display 的 Lua job。
库仍分配和释放自己的 framebuffer；模块 `deinit`、初始化失败、job release、
取消后 Host stop/join/destroy 都不调用 Display PAL open/close。
调用方必须等待任务释放或 Host 停止、join 和销毁完成后才恢复 UI 写屏。
默认配置保留独立运行时由 Lua 打开和关闭 Display 的行为。

需要保证启动时的连续像素空间时，调用方可按实际 Display geometry 配置 `display_framebuffer_bytes`，让 Host 在 VM 堆之前预留绘图缓冲，并在各 Job 交接 Display 时复用。容量、失败行为和生命周期见 `h2_lua_host_config_t`；预留不会提前打开 Display 或启动显示任务，worker 的发送缓冲仍计入 VM。

MP4 播放器配置也支持同名选项。启动动画可以借用同一个 Display，阻塞播放
返回后交还 UI；失败和协作取消同样只清理播放器自己的资源。
借用选项不会自动暂停 LVGL，也不提供多个写屏者之间的调度。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L520), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
