# 验证与测试

GizOS 使用 Bazel 选择兼容的构建、测试与产物。验证应对应所作结论：库行为需要可执行测试，平台接线需要最终产物，设备行为需要实机证据。运行相关检查前，应准备所选主机编译器或原生 SDK 环境。

## 从所属边界开始

通过公共库自身的主机测试及调用方验证其行为。App 状态转换应通过生产事件路径测试。修改平台接线时应构建最终目标；中间库编译成功不能证明启动器组合正确。

文档中的 Linux 主机工作流为：

```sh
make bazel-build BAZEL_CONFIG=linux_x86_64
make bazel-test BAZEL_CONFIG=linux_x86_64
make bazel-test-downstream-consumer
```

下游检查会将 GizOS 作为公共依赖使用。Apple Silicon 主机构建使用 `BAZEL_CONFIG=macos_arm64`。

普通测试应覆盖有意义的行为、畸形输入、部分初始化、取消、清理及并发边界。编译执行的测试样例和真实生成产物检查可以提供有效证据；只检查源码字符串、私有函数名或目录清单，不能证明运行时行为。

## 区分自动与手动证据

CI 为每类执行环境请求完整的兼容构建图及自动测试图。需要设备、外部服务或人工准备的测试使用 `manual`，必须通过精确 Bazel 标签调用。因此，自动测试图通过并不代表这些测试已经执行。

原生固件动作构建声明的输出，但从不执行烧录、复位或监视设备。串口操作、传输、互通、重启或 coredump 方面的结论，需要对应的实机检查。Gizloader 的包、进程、GUI fixture 与安装器测试，也独立于硬件传输/重启验收。

## 覆盖率与交付

在 Linux x86_64 上，`make bazel-coverage-report` 会在 `build/coverage/test/` 中生成已核对的测试结果与 C/C++ 覆盖率。未覆盖目标的指标标为不可用，不会虚构为零百分比。部分生成的产物仅用于诊断；Linux 覆盖率不能证明固件、移动端、Windows、音频、网络或渲染 UI 的覆盖情况。

交付源码修改前运行：

```sh
make format-check
git diff --check
```

说明实际运行了哪些检查，以及哪些验证无法进行。GizMoth 当前支持 Lua 项目；原生/Giz 项目创建与固件烧录属于后续功能，因此这些原生验证流程在 GizMoth 之外执行。

## 来源

- [guides/validation.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/validation.md)
- [tools/bazel/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/bazel/README.md)
- [tools/gizloader/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizloader/README.md)
