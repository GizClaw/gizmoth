# Boards and platforms

A GizOS platform combines public capability contracts with implementations and a build configuration. A board adds physical wiring, peripheral IDs, hardware defaults, BSP interfaces, and hardware/partition layout inputs. An SDK installation, a compatible build graph, and a validated physical device are different levels of support.

## Published target scope

At the pinned revision, the top-level README lists:

| Family | Targets |
| --- | --- |
| Embedded | ESP32-S3, ESP32-P4, ESP32-C5, BK7258 AP/CP, AC695N, AC791N |
| Desktop | Linux x86_64, macOS arm64, Windows x86_64 |
| Mobile | iOS Simulator arm64, Android arm64 |
| Web | WebAssembly via Emscripten |

Bazel compatibility selects which packages apply to each configuration. The Windows guide explicitly limits current support to the compatible native graph; that graph does not itself provide Windows PAL, Runtime, Desktop application, or packaging. Gizloader separately has Windows x64 serial support, while its native Windows BLE provider is not implemented.

## Where integration belongs

Keep every chip/core of one physical board under `boards/<board>/<chip-or-target>/`. Board packages own wiring and layout rather than App policy or task scheduling. Reusable providers compiled by Bazel belong under `libs/pal/providers/`; SDK-compiled registration and lifecycle glue belong under `native_component_src/<sdk-family>/`.

The final artifact maps logical App components onto physical peripherals, supplies image configuration, and owns Runtime lifetime. This lets an App request a capability without selecting behavior by board name.

## Prepare and verify

Start with the intended existing board and final artifact, then prepare its required SDK and host tools. Inspect the toolbox catalog with `gizos list` and the selected environment with `gizos env`. Use the final target's declared compatibility and dependencies to choose the build, then validate the actual board separately.

The toolbox also manages BK7259, BK IDK, and JieLi AC707N SDK entries. Those catalog entries do not establish additional GizOS firmware boards; the documented BK7258 build remains tied to its SMP SDK. SDK tools have host restrictions, including Linux x86_64 execution for JieLi tools.

GizMoth currently supports Lua projects. Native/Giz board-project creation and firmware flashing in GizMoth are future features.

## Sources

- [README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/README.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
- [tools/bazel/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/bazel/README.md)
- [tools/gizos/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizos/README.md)
- [tools/gizloader/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizloader/README.md)
