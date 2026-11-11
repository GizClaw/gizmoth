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
- 管理芯片与通用开发环境：配方来自 [GizClaw/gizmoth-devenv](https://github.com/GizClaw/gizmoth-devenv)，包含声明 Bazel、Python 依赖的 GizOS 基础环境。工具链和 Python 支持多版本并存、精确依赖复用、查看并打开安装目录、删除与重新安装；本地工具链只登记路径，不修改原文件。参见[开发环境说明](docs/devenv.md)（英文）。
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
- `src-tauri/`：桌面原生功能与设备集成，串口服务位于 `src-tauri/src/serial/`，开发环境位于 `src-tauri/src/devenv/`
- `native/serial-core/`：链接 GizOS Host Core、用于 H2Loader 串口的 Bazel 模块（`npm run serial-core:build`）
- `docs/gizos/`：内置的 [GizOS 文档](docs/gizos/README.md)

## 当前限制

直接安装到设备的压缩包上限为 512 KiB，Lua 项目暂不支持二进制资源。固件项目构建尚未实现；开发环境目录已发布 Bazelisk、多个 Python 版本和 GizOS 基础工具环境，芯片 SDK 配方仍待补齐；串口安装适用于已运行 H2Loader 的设备，不包括空白板的 ROM 恢复烧录。蓝牙串口与 Remote GizMoth 串口暂不支持。设备互通与模拟器中的应用完整运行仍需进一步验证。

项目和对话保存在本地，设备密钥保存在应用私有数据中，模型服务密钥保存在系统凭据存储中。AI 请求会发送给配置的模型服务。

## 许可证

[MIT](LICENSE)
