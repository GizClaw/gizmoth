<p align="center"><img src="branding/preview-mono-512.png" width="128" alt="GizMoth 图标"></p>

# GizMoth

[English](README.md) · 简体中文

GizMoth 是一款桌面应用，用于管理 GizOS 设备，并借助 AI 编程助手开发 Lua 应用。项目基于 Tauri 2、React 和 TypeScript。

## 功能

- 通过蓝牙或设备凭据链接配对，并管理设备支持的控制项。
- 创建、编辑、检查和导出 Lua 项目，或将应用安装到已配对设备。
- 使用可配置模型服务的 AI 助手和离线 GizOS 文档。
- 创建独立模拟器、选择固件并发送应用运行请求。
- 支持简体中文和英文，以及浅色和深色主题。

## 安装

可在 [Releases](https://github.com/GizClaw/gizmoth/releases) 页面查找 macOS（`.dmg`）和 Windows（`.exe`）安装包。如尚无发布版本，请按下方开发步骤配置环境，再运行 `npm run app:build` 从源码构建。

## 开发

安装 [Node.js 22.13+](https://nodejs.org/)、[Rust 1.90+](https://www.rust-lang.org/tools/install) 和 [Tauri 平台依赖](https://v2.tauri.app/start/prerequisites/)，然后运行：

```bash
npm ci
npm run app
```

如只需在浏览器中预览，运行 `npm run dev` 并访问 `http://127.0.0.1:1420/?demo=1`。测试、发布和协议参考见[维护指南](docs/maintenance.md)。

- `src/`：React 界面与编程助手
- `src-tauri/`：桌面原生功能与设备集成
- `docs/gizos/`：内置的 [GizOS 文档](docs/gizos/README.md)

## 当前限制

直接安装到设备的压缩包上限为 512 KiB，Lua 项目暂不支持二进制资源。固件开发和烧录尚未实现，设备互通与模拟器中的应用完整运行仍需进一步验证。

项目和对话保存在本地，设备密钥保存在应用私有数据中，模型服务密钥保存在系统凭据存储中。AI 请求会发送给配置的模型服务。

## 许可证

[MIT](LICENSE)
