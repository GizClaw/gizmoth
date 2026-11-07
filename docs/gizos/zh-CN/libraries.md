# 可复用库

GizOS 提供网络、媒体、协议、UI、游戏运行时、命令路由、序列化、安全与工具等可复用能力。库通过公共接口供 App 组合使用，避免应用依赖私有实现或某一块板卡。接入前，应阅读公共头文件，确认支持的操作、所有权、线程与结果语义，并准备目标平台的编译环境。

## 选择正确的归属

仓库级公共能力放在 `libs/<library>/`，包含公共 `include/`、私有 `src/`、本地 `tests/` 和 `BUILD.bazel`。有明确项目所有者的能力保留在 `projects/<project>/libs/<library>/`，即使其他项目也使用它。调用方依赖提供方的公共 API，依赖之间不得形成环。

可移植库核心通过注入的 PAL API、配置或回调获取平台能力。依赖 Runtime 的适配层应与独立核心分开。板级接线、SDK 注册和最终镜像策略分别由对应的平台与产物所有者负责。

## 接入与构建

在所属包的 `BUILD.bazel` 中声明源码、头文件、数据和依赖。主要目标与库目录同名。只有源码、定义、依赖或兼容性确实不同时，才增加 `_embed`、`_desktop`、`_mobile` 或 `_web` 变体。

下游产品可通过 Bzlmod 固定 GizOS 版本：

```starlark
bazel_dep(name = "gizos", version = "0.0.0")

git_override(
    module_name = "gizos",
    commit = "75deb77229d3fb07dd10d39683c69009b1eb45e9",
    remote = "https://github.com/GizClaw/gizos.git",
)
```

然后依赖适当的 `@gizos//...` 公共目标。使用方仓库仍负责平台选择与产品组合。

## 生命周期与验证

遵守所属模块的分配器契约，通过同一个分配器配对分配和释放。借用依赖的生命周期必须覆盖正在进行的操作。异步结果应明确由谁释放，以及取消如何达到终态。保留不支持、超时、取消、饱和及操作失败等结果之间的区别。

通过库自身的主机测试和调用方验证行为。`make bazel-test-downstream-consumer` 可检查独立依赖上下文中的公共接口消费。主机测试不能证明设备支持；某个库被列出也不代表它兼容所有目标。

GizMoth 目前支持 Lua 项目。原生/Giz 项目创建与固件烧录仍属于 GizMoth 的后续功能。

## 来源

- [README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/README.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
- [guides/development.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/development.md)
