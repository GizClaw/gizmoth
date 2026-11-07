# GizOS development documentation

[中文概览](zh-CN/gizos-overview.md) · [English overview](en/gizos-overview.md)

This documentation center contains finished development guides, explained APIs and practical examples for GizOS. Lua App development is one section of the platform. Raw headers, source mirrors and historical-archive collections are not included in the reader, MCP or installed documentation.

## Development guides

| Area | 中文 | English |
| --- | --- | --- |
| Platform overview | [认识 GizOS](zh-CN/gizos-overview.md) | [Getting started](en/gizos-overview.md) |
| Lua device APIs | [Lua 设备接口](zh-CN/lua-device-api.md) | [Lua device APIs](en/lua-device-api.md) |
| Lua Runtime | [Lua Runtime](overview.md) | [Lua Runtime](en/overview.md) |
| Native applications | [原生应用开发](zh-CN/native-apps.md) | [Native applications](en/native-apps.md) |
| Runtime and PAL | [Runtime 与 PAL](zh-CN/runtime-pal.md) | [Runtime and PAL](en/runtime-pal.md) |
| Libraries and protocols | [库与协议](zh-CN/libraries.md) | [Libraries and protocols](en/libraries.md) |
| Boards and platforms | [板卡与平台](zh-CN/boards-platforms.md) | [Boards and platforms](en/boards-platforms.md) |
| Build and toolchains | [构建与工具链](zh-CN/build-toolchains.md) | [Build and toolchains](en/build-toolchains.md) |
| Firmware and installation | [固件开发与安装](zh-CN/firmware-development.md) | [Firmware and installation](en/firmware-development.md) |
| Testing and validation | [测试与验证](zh-CN/validation-testing.md) | [Testing and validation](en/validation-testing.md) |

There are 20 paired Chinese/English guides and two official Lua application examples: the [screen/button example](input-example.md) and [Flappy Bird](game-example.md). The guides explain behavior, responsibilities, prerequisites and workflows; examples preserve their executable code. Literal API names and commands remain the same in both languages.

The reader and default local `gizos-docs` MCP use exactly the same Markdown bytes. The app installs the files in `$RESOURCE/gizos-docs/`. Reading and MCP access work offline without a model provider, web permission or outside-project filesystem permission. Source citations are document-footer links, not a separate source library.

## Content organization

- `en/` and `zh-CN/`: paired platform and API guides. The existing Chinese Lua prose is in the root alongside its English translations.
- `catalog.json`: Lua guide and example metadata.
- `localized-guides.json`: platform-guide entries and development areas.
- `src/gizos-docs/sections.json`: the shared development-area taxonomy used by the reader and MCP.

Current explanations are grounded in GizOS revision `75deb77229d3fb07dd10d39683c69009b1eb45e9`. Existing Lua prose also cites its original `81e8cbfc64760b5bcc89e8be771393662e876ea5` source; the updater verifies that `libs/lua` is unchanged between those revisions. Citations and content hashes preserve provenance without publishing raw implementation files as user documentation.

GizMoth currently implements Lua project development. Native-application and firmware guides describe GizOS capabilities and tools, not yet-implemented GizMoth project creation or flashing actions. Hardware behavior still depends on the selected firmware's capabilities and launcher configuration.

## Maintenance

Normal builds require only this repository:

```bash
npm run docs:gizos:bundle
```

After reviewing pinned source entries in `src/gizos-docs/sources.json`, maintainers can refresh the selected Lua prose and application examples from a local checkout:

```bash
npm run docs:gizos:update -- /path/to/GizClaw/gizos
```

Recheck translations whenever their source changes. Add future development areas in `src/gizos-docs/sections.json`, register paired guides in `localized-guides.json`, and rebuild the bundle. The bundler accepts guides and examples only and clears obsolete generated assets.

Upstream license and sample-attribution notices are retained in the application's license assets; they are not presented as development-documentation topics.
