# 固件开发

固件产物将 App、平台实现、板级接线、SDK 配置与打包组合在一起。开始前，需要 GizOS 源码、Bazel 9.2.0，以及选定已有目标要求的精确 SDK/工具链环境。产品或最终产物入口负责镜像策略与 Runtime 生命周期；可移植库不应承担板级接线和固件注册。

## 组合统一的固件依赖图

最终入口位于 `projects/<project>/targets/<artifact-rule>/<app>[/<variant>]/`。每个固件入口拥有一个 `firmware_lib_component`，收集其选择的 Bazel 库。由 SDK 编译的源码、板级代码与启动器使用 `firmware_native_component`，显式声明源码、头文件、元数据和依赖。

Runner 将此依赖图转换为当前调用专用的原生清单。ESP-IDF CMake 或 BK SDK Make/CMake 仍负责生成的 SDK 配置、厂商源码、编译、链接、引导程序与分区机制。原生构建文件消费已声明的依赖图，不通过扫描仓库搜集第一方源码。

## 构建交付目标

文档提供了以下 BK7258 示例：

```sh
bun tools/gizos/src/cli.ts setup bk 3.1.1-r6bbf627
eval "$(bun tools/gizos/src/cli.ts env bk 3.1.1-r6bbf627 --shell sh)"
bazel build --config=bk7258 //projects/example/targets/h2loader_tar_zlib/display/bk7258_v3_202405:package
```

H2Loader 交付入口将内部 `:firmware` 与公共 `:package` 分开。打包会经过原生构建，生成 `.update.tar.zlib` 和 `.firmware.json`。ESP 原生输出包含 ELF/map、应用、引导程序、分区表及合并出厂镜像；BK 输出包含 AP/CP ELF/map/镜像、受管理的更新镜像、恢复镜像和分区元数据。不要用任意 SDK 中间文件代替声明的交付产物。

## 设备操作独立执行

构建动作不执行烧录、复位、串口连接或硬件监视。Gizloader 是用于固件操作与日志的独立桌面/命令行工具：

```sh
bunx gizos install gizloader
gizloader --help
gizloader scan
```

Gizloader 支持 macOS Intel/Apple Silicon 和 Windows x64 串口设备；Windows BLE 尚未实现，桌面版的蓝牙操作通过 CLI 执行。GUI 扫描端口列表不会连接设备或写入固件。操作设备前，应选择兼容包并查看 Loader 的实际帮助。硬件传输与重启验收需要单独的实机证据。

GizMoth 当前支持 Lua 项目。GizMoth 中的 Giz/原生固件项目创建与固件烧录属于后续功能。

## 来源

- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
- [tools/bazel/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/bazel/README.md)
- [tools/gizloader/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizloader/README.md)
