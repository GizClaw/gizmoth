# 板卡与平台

GizOS 平台将公共能力契约、具体实现和构建配置组合在一起。板卡在此基础上增加物理接线、外设 ID、硬件默认值、BSP 接口及硬件/分区布局输入。能够安装 SDK、构建图兼容和实机验证通过，代表不同层次的支持。

## 已公布的目标范围

固定修订版本的顶层 README 列出以下目标：

| 类别 | 目标 |
| --- | --- |
| 嵌入式 | ESP32-S3、ESP32-P4、ESP32-C5、BK7258 AP/CP、AC695N、AC791N |
| 桌面 | Linux x86_64、macOS arm64、Windows x86_64 |
| 移动端 | iOS Simulator arm64、Android arm64 |
| Web | 通过 Emscripten 构建的 WebAssembly |

Bazel 兼容性声明决定各配置适用哪些包。Windows 指南明确将当前能力限定为兼容的原生构建图；该构建图本身不提供 Windows PAL、Runtime、Desktop 应用或打包。独立的 Gizloader 支持 Windows x64 串口，但原生 Windows BLE Provider 尚未实现。

## 集成代码放在哪里

同一块物理板卡的所有芯片/核心保留在 `boards/<board>/<chip-or-target>/` 下。板级包负责接线与布局，不负责 App 策略或任务调度。由 Bazel 编译的可复用 Provider 放在 `libs/pal/providers/`；SDK 编译的注册与生命周期衔接代码放在 `native_component_src/<sdk-family>/`。

最终产物负责将 App 逻辑组件映射到物理外设、提供镜像配置并管理 Runtime 生命周期。App 因而可以请求能力，而不必根据板卡名称选择行为。

## 准备与验证

先确定已有板卡及最终产物，再准备其要求的 SDK 和主机工具。使用 `gizos list` 查看工具箱目录，使用 `gizos env` 查看选中的环境。根据最终目标声明的兼容性与依赖选择构建，再独立验证实际板卡。

工具箱还管理 BK7259、BK IDK 和 JieLi AC707N 等 SDK 条目。这些条目不能证明新增了对应的 GizOS 固件板卡；文档中的 BK7258 构建仍绑定 SMP SDK。SDK 工具有主机限制，例如 JieLi 工具需要在 Linux x86_64 上执行。

GizMoth 目前支持 Lua 项目。GizMoth 中的原生/Giz 板卡项目创建与固件烧录属于后续功能。

## 来源

- [README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/README.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
- [tools/bazel/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/bazel/README.md)
- [tools/gizos/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizos/README.md)
- [tools/gizloader/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizloader/README.md)
