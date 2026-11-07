# GizOS 概览

GizOS 是面向固件与应用的可移植平台，在同一个 Bazel 工作区内提供应用运行时、硬件抽象、可复用库、板级支持、打包与验证能力。Lua App 开发只是其中一个部分；原生应用、固件组合、库开发和平台接入各有独立职责。

## 各层如何协作

应用使用 Runtime 和公共库。Runtime 与库通过平台抽象层（PAL）访问系统能力，Provider 再利用操作系统、SDK 和硬件实现这些契约。板级包描述物理接线与硬件默认值；最终产物入口组合 App、平台、组件映射和镜像配置。

可复用的平台代码属于 GizOS。产品仓库负责自身的硬件组合、配置、资源与发布策略，并可通过 Bzlmod 使用 `@gizos//...` 公共目标。这样的边界允许复用同一套应用逻辑，而不必把特定板子的引脚分配或 SDK 对象写入其中。

## 选择开发入口

- 在 GizMoth 中开发 Lua 项目，请从 Lua App 指南开始。
- 编写需要编译的应用逻辑，请阅读原生 App 与 Runtime/PAL 指南。
- 提供可复用能力，请阅读库开发指南。
- 接入硬件和制作交付产物，请阅读板卡、工具链与固件开发指南。
- 验证修改，请结合验证指南与所属包的测试。

首次在 Linux 主机构建时，需要准备 Bazel 9.2.0 或兼容的启动器、C/C++ 工具链，以及所选平台的依赖。在 GizOS 源码目录运行：

```sh
make help
make bazel-build BAZEL_CONFIG=linux_x86_64
make bazel-test BAZEL_CONFIG=linux_x86_64
```

Apple Silicon 主机构建使用 `BAZEL_CONFIG=macos_arm64`。兼容性由 Bazel 依赖图选择；某个平台出现在支持列表中，并不代表每个库或硬件操作都可用。

## GizMoth 当前范围

GizMoth 目前支持 Lua 项目。Giz/原生固件项目创建与固件烧录属于 GizMoth 的后续功能。这里介绍的原生构建与 Loader 工作流属于 GizOS 工具链及独立工具，不代表 GizMoth 已经提供这些操作。

## 来源

本组指南依据 GizOS 修订版本 `75deb77229d3fb07dd10d39683c69009b1eb45e9` 编写。

- [README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/README.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
