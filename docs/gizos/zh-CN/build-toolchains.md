# 构建与工具链

Bazel 是 GizOS 在依赖解析、平台兼容性、构建、测试与产物方面的统一依据。使用固定源码 `.bazelversion` 指定的 Bazel 9.2.0、适当的 C/C++ 工具链及所选平台依赖。Bazel 固件入口之下，原生 SDK 仍负责自身的编译/链接策略和生成配置。

## 从主机构建开始

在 GizOS 源码目录运行：

```sh
make bazel-build BAZEL_CONFIG=linux_x86_64
make bazel-test BAZEL_CONFIG=linux_x86_64
```

Apple Silicon 使用 `BAZEL_CONFIG=macos_arm64`。这些入口请求兼容的构建/测试依赖图，不能据此认为仓库中每个目标都适用于当前主机。

## 准备共享 SDK

`gizos` 工具箱要求 Bun 1.3.9 或更新版本。公开包无需配置自定义 registry 映射。SDK 准备需要 Git、Bash、SDK 支持的主机 Python 和归档工具；安装支持 macOS 与 Linux，但各资源另有主机限制。

```sh
bunx gizos --help
bunx gizos list esp-idf
bunx gizos setup esp-idf 6.2
bunx gizos env esp-idf
```

工具箱在各工作树之间共享安装和记录。不同 SDK 版本可保留独立路径，同时共享兼容工具。`env` 只报告路径，不执行安装；源码别名及精确版本/提交固定值来自维护中的目录，不接受任意源码 URL。

文档提供了以下基于源码目录的 ESP32-S3 构建流程：

```sh
bun tools/gizos/src/cli.ts setup idf 6.2
eval "$(bun tools/gizos/src/cli.ts env idf 6.2 --shell sh)"
bazel build --config=esp32s3 //projects/example/targets/h2loader_tar_zlib/ble-broadcaster/devkit:package
```

## 理解执行边界

ESP-IDF 与 BK7258 原生固件动作在本机执行，不使用沙箱或远程执行；其声明的输出可以参与缓存。它们不执行烧录、复位或监视设备，硬件操作属于独立步骤。

主机支持因工具而异：BK7258 的 GNU Arm 10.3 编译器在 Apple Silicon 上需要 Rosetta 2，JieLi 编译器和后处理工具则在 Linux x86_64 上运行。SDK 准备完成不能证明板卡受支持，也不能证明实机运行成功。

GizMoth 当前支持 Lua 项目。原生/Giz 项目创建与固件烧录属于后续功能；本页命令用于独立的 GizOS 开发流程。

## 来源

- [tools/bazel/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/bazel/README.md)
- [tools/gizos/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizos/README.md)
- [README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/README.md)
