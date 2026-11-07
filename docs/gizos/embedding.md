# Embedding and runtime capabilities

Host lifetime, worker threads, source package, build and bindings; capabilities are supplied by firmware.

## 嵌入分层与源码包

`//libs/lua:lua_runtime` 是 portable 下层：包含 Lua Core、Host、modules、`//libs/trie`、`//libs/runtime` 和 PAL headers/inline wrappers，以及 Bazel 固定版本的 Lua 5.5、yyjson。下层的平台访问只依赖 PAL interfaces，不能依赖具体 provider、board、SDK 或 `bleikcp`。`//libs/lua:lua_core` 继续只依赖 upstream Lua。`//libs/lua:lua` 保留现有入口；平台组装由现有 firmware、Desktop、Web launcher 或外部 embedder 完成。可选 `//libs/lua:lua_link` 在上层接入 `bleikcp`，不进入 portable 源码包；未启用时仍遵守既有 `link: unavailable` 合同。

**PAL vtables 就是 embedding hooks。** Embedder 构造既有 `h2_pal_*_api_t` 的 `user + vtable`，填入 Runtime config；不需要另一套 callback ABI。Runtime 初始化要求完整 API surface，不支持的能力使用 canonical unsupported API object。源码包因此同时带上 `//libs/pal:unsupported` 的 portable 实现，供 embedder 填充默认值；这不会为 `lua_runtime` 增加 provider 依赖。Display、Button、Touch、Audio 可以来自宿主 UI，Memory、Task、Queue、Sync、Time、Timer、Filesystem 可以来自已有 provider 或宿主自己的 C 实现。

### 线程与生命周期

Vtable 函数可能从 Runtime input task、Lua worker 或 PAL 自己的线程调用，不保证在 UI/main thread。API 的 `user`、vtable 及其底层资源必须覆盖 Runtime 和 Host 的整个使用期；先停止输入与外部 completion producer，再对 Host 执行 stop/join/destroy，最后 deinit Runtime 并释放 provider。不要从 PAL 回调直接重入同一个 Lua VM。

对于只允许异步通知的 UI bridge，C vtable 应复制像素、rect、音频或其他仅在当前调用期间有效的 buffer 到宿主拥有的有界队列，然后按 PAL 合同及时返回。容量不足时返回对应的错误或 backpressure，不能保留借用指针、等待 UI 回调，或虚报实际未接收的数据。需要同步结果的 Memory、Sync、Queue 等服务必须在 native 层实现；单纯的异步 UI callback 不能满足这些 vtable。原有 PAL 的返回值、timeout、partial-write 和生命周期语义保持不变。

Capability 在 `h2_lua_host_start()` 前注册，start 后 registry 冻结。异步 call callback 复制 input/options 和 request ID 后返回 `H2_PAL_ERR_WOULD_BLOCK`；宿主完成工作时调用 `h2_lua_capability_complete()`，由 owning worker 恢复 Lua。Lua 收到既有的 `ok, output, error` 三元组。宿主必须处理 cancel callback，及时释放其排队工作，并在销毁 Host 前停止所有 completion producer；晚到或重复 completion 会被拒绝。

Registry 的 `capability_capacity` 由 exact 与 prefix 共用，0 默认 16，上限
`H2_LUA_CAPABILITY_CAPACITY_MAX` 为 256，超过上限 create 返回
`H2_PAL_ERR_INVALID_ARG`，注册满时返回 `H2_PAL_ERR_FULL`。Entry 和 route storage
在 create 时通过 Runtime Memory 分配。名称和 prefix 都复制保存，长度为 1..47
字节（小于内部 `H2_LUA_NAME_MAX=48`）。256 条最长名称最多需要 12,033 个
trie node（含 root，约 118 KiB），既支持数百个宿主能力，又限制内存并保持在
`h2_trie` 的 16 位索引范围内；默认容量不会预留此最大节点空间。

`h2_lua_host_start()` 按 `1 + sum(strlen(name))` 分配 node storage 并用
`//libs/trie` 的 `h2_trie_build()` 建立 immutable lookup，再启动 worker。
节点分配失败返回 `H2_PAL_ERR_NO_MEMORY`，Host 仍可 destroy 或重试 start。
`capability.call()` 通过 `h2_trie_handle()` 选择 entry，不扫描全部注册名称；
trie 按字节遍历，每层 sibling 搜索由字节字符集限制，不随注册数线性增长。

`h2_lua_register_capability_prefix()` 可以一次注册 `litelink.` 命名空间。
沿用 trie 原生语义：完整 exact 优先，否则最长 prefix 优先，与注册顺序无关；
prefix 后必须有非空 remainder，因此 `litelink.` 本身不会命中同名 prefix。
同一 exact 或同一 prefix 重复注册返回 `H2_PAL_ERR_INVALID_STATE`，相同文本可以
各注册一次 exact 和 prefix，两者分别占容量。Start 后拒绝注册；调用方须串行化
registration/start/destruction。Prefix callback 额外收到完整请求名称，借用期仅为
本次 call；异步工作须复制名称。它复用 exact 的 request、completion、cancel 和
Lua tuple 路径，unknown name 仍返回 `false, nil, "unknown capability: <name>"`。
Registry 不提供名称枚举，因为 namespace 注册不是全部可调用名称的清单。

### 构建与 manifest

在 macOS 或 Linux host 构建源码包：

```sh
bazel build //libs/lua:runtime_sources
python3 libs/lua/tests/test_source_package.py bazel-bin/libs/lua/gizos-lua-runtime-src.tar.gz
bazel test //libs/lua:all
bazel query 'deps(//libs/lua:lua_runtime) union deps(//libs/lua:lua_core)'
bazel query 'filter("//libs/pal/providers/|//libs/bleikcp|//boards/|//native_component_src/", deps(//libs/lua:lua_runtime))'
```

`gizos-lua-runtime-src.tar.gz` 根目录包含 `manifest.json` 和 GizOS `LICENSE`，其余 C/H 文件保持 package-relative 路径；Lua、TLSF 与 yyjson 的 external repository 文件分别放在稳定的 `third_party/lua/`、`third_party/tlsf/`、`third_party/yyjson/` 下，所有 manifest 路径同步重写。文件清单、include dirs、defines 和各 translation unit 的编译参数由 Bazel aspect 从已配置的依赖图生成，不手工复制维护。包使用与 GizOS native build 相同的 Lua source selection 和受限标准库；固件专用 stdio/newlib shim 仍由原 embedded build 配置选择，不把 ESP libc 兼容代码加入 native host。它包含可移植的 C11 atomic provider，不包含预编译库、Bazel toolchain、PAL provider 或 board code。 新增的 trie C 源码和头文件也随依赖图自动收录。

Manifest schema version 1：

| 字段 | 合同 |
| --- | --- |
| `schema_version` | 整数 `1`；consumer 拒绝未知版本 |
| `runtime_profile_id` | 固定为 `runtime.lua.gizos` |
| `sources` | 所有需编译一次的 `.c`，相对解包根目录 |
| `include_dirs` | 相对解包根目录的 include search paths |
| `defines` | 公共 compiler definitions，不含 `-D` 前缀 |
| `cflags` | 公共 C compiler arguments，JSON array 中每项为独立参数 |
| `compilation_units` | 分组的 `sources`、附加 `cflags`、附加 `defines`；各 source 恰好属于一个分组 |
| `per_os` | OS 名到附加 `link_flags` 的映射；`linux`、`darwin`、`android`、`ios` 的数学库为 `-lm` |

Consumer 对每个 source 应用公共参数及所属分组参数，按自身目标工具链追加 architecture、sysroot、PIC、visibility 和 deployment target，再链接所有 object 与该 OS 的 extras。参数必须逐项传给 compiler，不通过 shell 拼接解析。当前包表达 GCC/Clang C11 编译合同；Windows/MSVC 和 WebAssembly 工具链需单独适配，不由该 manifest 声明支持。Profile ID 标识 Lua surface，不替代 content id pin；不同内容的源码、header 和 binding 不应混用。Manifest 不包含 commit、发布版本或 timestamp；`schema_version` 只标识 manifest 格式。

独立测试只依赖 Python 3.11.8+（支持 tar extraction filter）、`cc`/Clang 和 pthread。它在临时目录解包，根据 manifest 编译全部 C sources，链接测试自己填写的 OS、240×240 Display、Touch、`ok`/`back` Button vtables。测试验证 async echo、显示 dirty rect 与全部像素、Runtime push edge 到 Lua callback、pending job cancellation、默认/配置容量、exact/prefix 优先级、完整名称传递、重复和 start 后注册拒绝、节点分配失败及重试。Python runner 不调用 Bazel，也不从 checkout 查找 runtime source；`CC` 可指定兼容 compiler。Bazel test 只是把源码包和 harness 作为测试输入交给同一个 runner。

Embedder 执行 App method 时，让主 chunk `return app[method](...)`，等待 job 成功后查询长度、分配宿主 buffer、调用 `h2_lua_job_get_result()` 复制结果，最后 release。Flutter/cgo 都通过同一公开 accessor 读取，不需要私有 native module 转存返回值；复杂值应由 App 显式编码为 JSON 等稳定格式。

### 内容标识与发布下载

Bazel action 为源码包生成 `bazel-bin/libs/lua/runtime_sources.content_id`，内容为完整小写十六进制 SHA-256 加一个 LF。可单独构建 `//libs/lua:runtime_sources_content_id`；构建 `//libs/lua:runtime_sources` 也会生成它。

算法精确定义：枚举包内全部普通文件（包含 `LICENSE` 和 `manifest.json`，不包含目录 entry、tar header、content id sidecar）；路径为相对 archive root 的 POSIX 路径，无 `./` 前缀。按路径的 UTF-8 字节字典序排序，每个文件拼接 `UTF8(path) + 0x00 + ASCII(lowercase_hex(SHA256(file_bytes))) + 0x0a`，再对全部拼接字节计算 SHA-256。路径来自受控 C source graph，不能包含换行、NUL 或绝对路径。哈希输入只包含路径和文件 bytes，故不依赖 mtime、机器、绝对路径、Bazel canonical repository name、commit、timestamp 或 gzip/zlib/rules_pkg 版本；任一文件内容、路径或清单变化都会改变 id。Manifest 自身也是内容，编译参数变化同样改变 id。

本地 archive 名保持 `gizos-lua-runtime-src.tar.gz`，发布 slice 将它复制为 `gizos-lua-runtime-src-<content_id>.tar.gz`，不改写包内任何 bytes，并生成 `.sha256`。这里的 `.sha256` 是压缩 tar 的下载校验值，与未压缩内容的 `content_id` 含义不同；换压缩器可以保持 content id 不变但改变下载 SHA-256/size。

LiteLink 从 [手动 Release 的 metadata](./bazel.md#lua-源码包进入手动-release) 读取 `packages.lua_runtime`，固定 `{url, sha256, size}` 和 `content_id`。URL 使用 `https://github.com/GizClaw/gizos/releases/download/v<release_id>/<file>`，不使用 latest。先检查下载长度和压缩文件 SHA-256，再安全解包并重算 content id；binding 必须匹配包内 headers。Release metadata 的 commit 只用于追溯，不能注入 Lua manifest。LiteLink 在 Flutter App 的原生层提供自己的 PAL 实现。

独立测试可显式传入 sidecar，重算内容标识后继续编译：

```sh
python3 libs/lua/tests/test_source_package.py bazel-bin/libs/lua/gizos-lua-runtime-src.tar.gz libs/lua/tests/test_embedder.c bazel-bin/libs/lua/runtime_sources.content_id
```

Bazel test 不递归启动 Bazel。本地发布 slice 的验证命令见 [Lua 源码包进入手动 Release](./bazel.md#lua-源码包进入手动-release)。

### Flutter 与 cgo

LiteLink 需要新增原生宿主库和 Dart 桥接。构建环节读取解包后的 manifest，分别用 iOS device/simulator 的 Apple Clang 和 Android 各 ABI 的 NDK Clang 编译每个 translation unit，再与 LiteLink 的原生代码链接；manifest 的公共及分组参数必须逐项传给编译器，目标 SDK、架构、PIC、可见性、deployment target 和系统库由 LiteLink 的目标构建配置提供。不能依赖 GizOS 的 Bazel archive 或预编译 library。

LiteLink 的原生宿主库创建并持有 PAL 对象及其生命周期，负责实现 Runtime 所需的 allocator、任务/同步、时间、文件系统、显示与输入等接口，并把平台能力接到 iOS/Android；发布包自带的 C11 atomic provider 不代替 PAL。Dart 侧通过 `dart:ffi` 调用稳定的 Host/Runtime C API，原生桥接负责线程边界、callback 生命周期和消息复制，UI 操作再交给 Dart 渲染。FFI binding 必须匹配随包 header 的 struct layout 与 callback signatures。LiteLink 当前还没有这套原生构建和桥接，macOS/Linux 的源码包编译测试只验证可移植源码与 manifest，**不证明 iOS/Android 目标已编译或 App 已集成**；移动端验收需由 LiteLink 在真实目标工具链、各 ABI 和 App 启动/调用链上完成。

Go/cgo consumer 同样在自己的构建步骤中读取 manifest，用目标 C compiler 编译包内 sources 与自有 PAL bridge，再把 object/archive 接入 cgo linker。cgo 不会递归编译这些子目录中的 C 文件，也不能忽略不同 source group 的 flags。Go 层通过 C bridge 发起 job、推送输入与完成 capability；PAL `user` 可由 C 分配的 context 或受管理的 opaque handle 表示，不能把生命周期不受控的 Go 指针留给 worker。宿主的 pthread、UI framework 等依赖由上层 bridge 自己声明，不属于 portable runtime manifest。

### Prepared 数值阶段与共享几何

workspace 默认通过 `load` 复制状态；可分发 Lua app 也可以创建标准 f64 numeric buffer，通过 `bind` 明确保留当前与历史位置。绑定后，公共 buffer 写入与 `node` 更新相互可见，绘制直接消费当前位置，无需每子步完整导出。每个 buffer 只能被一个活 workspace 绑定；workspace 强引用保活 buffer，弱 owner 记录不阻止 workspace 回收。重新绑定重置活动拓扑、lambda、span 与 sweep；成功 `load` 安全复制后退出绑定，失败保留旧状态。各阶段继续使用私有暂存区和原子提交；绑定状态不能同时作为阶段系数、bounds、mobility 或 `copy` 输出。`multipliers` 只返回选定边与 span 的数学结果，张力、出线与游戏规则仍由 Lua 解释。

`vmath.constraints` 提供固定容量、VM 计费的阶段 workspace。`load/begin` 明确重置 lambda 与交替 sweep 次序；`integrate` 执行调用方提供的两级 gain 和增量，`node/edge/span` 只读取或更新明确的状态，`solve` 每轮依次执行边、span、位置 bounds，`damp` 执行邻居位移和有序轴向阻尼。Lua 在 integration 之后执行依赖端点的应用规则。每个阶段独立原子提交，失败不撤销上一成功阶段，也不清空已有 lambda。完整参数、范围、两浮点补偿与 float 位移规则以 [numeric production header 生成的 API](../../references/lua.md) 为准。

`geometry.rotations` 只保存调用方给出的段、权重和旋转轴；端点 reduction 在明确误差域内使用 18 moments/17 次多项式，域外执行完整循环。它不生成形状、权重函数或受力模型。`geometry.batch` 保存不可变二维顶点、拓扑和两个可选位移权重通道；`geometry.pose` 保存按原顺序计算的变形、旋转、缩放、平移和可选参数化平面透视结果。缓存键含全部 evaluate 输入与不可变 geometry，位于 layer/color 之前，应用自行决定共享时机。

`display.draw_pose` 直接消费 pose，通过既有 polygon/line raster 绘制。`display.polyline` / `compile_line_style` / `draw_polyline` 保留源段身份、方向、共享端点投影、严格异号切分及近裁剪。颜色在原始端点求值，先 RGB888 插值和 floor，再转 RGB565；触平面及共面段用调用方显式提供的 boundary style。点、camera、plane、order 或 style 改动会重新准备 transient fragments，不保留可过期的跨 draw 投影缓存。各 draw 的 clip、layer、颜色只影响本次重绘，不使已发布的 pre-layer pose 失效。完整绘制参数与相机布局见 [Display API](../../references/lua.md)。

这些对象及 scratch 均由 VM userdata 持有，成功暖调用不分配或逐元素回调 Lua；构造失败与 GC/VM teardown 回收所有引用。绘制在参数解析后重新检查 Display acquisition，完整验证最终坐标和样式后才写像素，并沿用 dirty/background bookkeeping；不会隐式 present。游戏状态、标量受力方程、材质转换、形状通道生成、相机 recipe、层语义和采样时钟仍属于可分发 Lua app。公共功能只做原始算法拆分；实机逐阶段帧率对齐属于下游成对验证，Host/Web 测试不能代替。

Prepared workspace 的 `displacements` 将指定范围的 double 位置差在相减后转成 f32，写入可复用的 packed xyz 输出前缀。`displacement-f32` 积分的 before/gain0/gain1 可分别使用 f32 或 f64，mobility/after/bounds 保持 f64；环境分支及系数公式仍由 Lua 决定。显式 `vmath.length3_refined` 使用原版 float 开方种子与一次 double 修正，适用范围、误差与 fallback 见 numeric Public Header；不改变原有 `length3` 或 `normalize3`。

库内绘制回归使用通用生成输入和独立参考算法：`material_strip.lua` 覆盖 strip 与空面，`quad_material_projective.lua` 覆盖透视映射和 Q24 边界，`raster_workload.lua` 覆盖多边形与直线。完整应用姿态、玩法和整帧序列由消费仓库验证；公共测试不保存第二份游戏回放数据。默认测试保留正确性所需的完整回调次数，计时通过 `lua_test --strip-benchmark`、`--projective-benchmark`、`--primitive-benchmark` 等显式入口执行；脚本和 VM 的既有限额不因测试合并而放宽。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L611), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
