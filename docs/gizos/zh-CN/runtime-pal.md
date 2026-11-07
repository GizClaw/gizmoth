# Runtime 与 PAL

Runtime 面向 App 组合平台能力、组件映射、事件与当前状态，PAL 则定义底层能力的公共契约。`h2_runtime.h` 是 Runtime 的统一入口头文件；`h2_pal.h` 汇集系统服务、网络、外设与媒体等 PAL 接口。这些属于原生 C 接口，使用前需要配置原生构建，并由平台组合提供必需的 Provider。

## 契约与实现

`libs/pal/` 负责公共契约和标准的不支持实现对象。由 Bazel 编译的平台实现放在 `libs/pal/providers/<platform-or-library>/`；由厂商 SDK 编译和注册的组件放在 `native_component_src/<sdk-family>/`。

`//libs/pal:pal` 是仅提供头文件的目标。使用标准不支持访问器的调用方还需选择 `//libs/pal:unsupported`。Provider 实现具体能力；最终产物负责决定运行哪个 App，以及哪些物理外设满足它的逻辑组件。

Runtime 配置通过注入的 API 对象接收内存、时间、任务、同步、文件系统、网络、显示、音频和输入等能力。可用操作取决于选中的实现。API 对象非空不代表某项操作受支持；必需能力的失败应传递给调用方，可选降级则必须有明确设计。

## 处理事件与生命周期

用 Runtime 事件接收有序事实，用 Runtime 状态读取当前快照，包括初始状态。Runtime 负责输入采集和系统/组件事件生产，因此 App 不应针对同一输入创建重复的轮询任务。

App 或库的完成结果可通过携带复制值的公共自定义事件传递，也可使用自行持有的有界分发队列配合 `h2_runtime_notify()`。必须明确处理队列饱和，并限制每次主循环的分发工作量。不要把应用完成结果伪装为 Runtime 系统事实。

API 对象、缓冲区与回调上下文可能是借用的；在所有操作结束前，其所有者必须保持有效。销毁 Runtime 前，应停止生产者、取消并等待工作线程结束，再完成待处理清理。只有明确标注 ISR-safe 的接口才能在 ISR 中调用。

## GizMoth 当前范围

GizMoth 当前支持 Lua 项目。原生 Runtime/PAL 集成与固件组合使用 GizOS 源码工作流；GizMoth 中的 Giz/原生项目创建与烧录属于后续功能。

## 来源

- [libs/runtime/include/h2_runtime.h](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/runtime/include/h2_runtime.h)
- [libs/pal/include/h2_pal.h](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/pal/include/h2_pal.h)
- [guides/development.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/development.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
