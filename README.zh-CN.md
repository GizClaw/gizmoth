<p align="center"><img src="branding/preview-mono-512.png" width="128" alt="GizMoth 图标"></p>

# GizMoth

[English](README.md) · 简体中文

GizMoth 是一款桌面应用，用于管理 GizOS 设备，并借助 AI 编程助手开发 Lua 应用。项目基于 Tauri 2、React 和 TypeScript。

## 功能

- 通过蓝牙或设备凭据链接配对，并管理设备支持的控制项。
- 创建、编辑、检查和导出 Lua 项目，或将应用安装到已配对设备。
- 使用可配置模型服务的 AI 助手和离线 GizOS 文档。
- 创建独立模拟器、选择固件并发送应用运行请求。
- 通过常驻串口服务管理本机串口：每个串口的实时状态与日志、H2Loader 识别与固件安装，并通过 GizMoth 全局 MCP（`gizmoth --mcp`）供其他工具使用。
- 在搭载 Apple 芯片的 Mac 上用 Linux 虚拟机编译项目（侧边栏“编译”）：OpenVMM 每个 slot 运行一台虚拟机以并行编译，使用随应用提供、由 CI 构建的 Ubuntu 系统镜像，自动下载、打包并更新 [GizClaw/gizmoth-devenv](https://github.com/GizClaw/gizmoth-devenv) 中的全部 Linux 工具链，增量同步工作空间，Bazel 远程缓存与 ccache 统一保存在可在“设置 → 存储”中选择的工作目录；编译产物可由代理通过 CLI/MCP 查询，并按 ID 安装到设备。代理通过 WebSocket 提交编译，排队、实时日志，断开即取消。参见[编译说明](docs/compiling.md)（英文）。
- 在 Agents 页面预览、安装、更新或卸载 GizMoth Skill，供外部编程代理通过 CLI/MCP 查询设备，再通过单个 WebSocket 独占连接执行设备命令。连接会申请访问权、等待授权，承载发送、安装、重启及任务事件，并在关闭时释放访问权；不会修改全局指令，也无需配置 MCP。参见[代理集成说明](docs/agents.md)（英文）。
- 支持简体中文和英文，以及浅色和深色主题。

## 安装

可在 [Releases](https://github.com/GizClaw/gizmoth/releases) 页面查找 macOS（`.dmg`）和 Windows（`.exe`）安装包。如尚无发布版本，请按下方说明从源码构建。

## 开发

安装 [Node.js 22.19+](https://nodejs.org/)、[Rust 1.90+](https://www.rust-lang.org/tools/install) 和 [Tauri 平台依赖](https://v2.tauri.app/start/prerequisites/)，然后运行：

```bash
npm ci
npm run app
```

如需在没有更新签名密钥的情况下构建安装包：

```bash
npm run app:build -- --config src-tauri/tauri.unsigned.conf.json
```

如只需在浏览器中预览，运行 `npm run dev` 并访问 `http://127.0.0.1:1420/?demo=1`。测试、发布和协议参考见[维护指南](docs/maintenance.md)。

- `src/`：React 界面与编程助手
- `src-tauri/`：桌面原生功能与设备集成，串口服务位于 `src-tauri/src/serial/`，工具链目录位于 `src-tauri/src/devenv/`，编译虚拟机位于 `src-tauri/src/compile/`
- `native/compile-agent/`：编译虚拟机内的 Linux agent（`npm run compile-agent:build`）；`npm run openvmm:build` 构建 OpenVMM sidecar
- `native/serial-core/`：链接 GizOS Host Core、用于 H2Loader 串口的 Bazel 模块（`npm run serial-core:build`）
- `docs/gizos/`：内置的 [GizOS 文档](docs/gizos/README.md)

## 当前限制

直接安装到设备的压缩包上限为 512 KiB，Lua 项目暂不支持二进制资源。编译虚拟机仅支持搭载 Apple 芯片的 macOS，客体为 arm64 Linux，不模拟 x86 工具链；工具链目录已发布 Bazelisk 和多个 Python 版本，芯片 SDK 配方仍待补齐；串口安装适用于已运行 H2Loader 的设备，不包括空白板的 ROM 恢复烧录。蓝牙串口暂不支持；串口级联目前只支持一级，接入的远端串口不会再转出给其他 GizMoth。设备互通与模拟器中的应用完整运行仍需进一步验证。

项目和对话保存在本地，设备密钥保存在应用私有数据中，模型服务密钥保存在系统凭据存储中。AI 请求会发送给配置的模型服务。

## 许可证

[MIT](LICENSE)
