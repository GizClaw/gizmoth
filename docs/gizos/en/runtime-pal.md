# Runtime and PAL

Runtime is the App-facing composition of platform capabilities, component mapping, events, and current state. PAL defines the public capability contracts beneath it. `h2_runtime.h` is the umbrella Runtime header; `h2_pal.h` brings together PAL interfaces for system services, networking, peripherals, and media. These are native C interfaces; using them requires a configured native build and a platform composition that supplies the necessary providers.

## Contracts and implementations

`libs/pal/` owns public contracts and canonical unsupported objects. Platform implementations live under `libs/pal/providers/<platform-or-library>/` when Bazel compiles them. Components compiled and registered by a vendor SDK belong under `native_component_src/<sdk-family>/`.

The `//libs/pal:pal` target is header-only. A consumer that uses canonical unsupported accessors also selects `//libs/pal:unsupported`. Provider code implements a capability; the final artifact decides which App runs and which physical peripherals satisfy its logical components.

Runtime configuration accepts injected API objects for capabilities such as memory, time, tasks, synchronization, filesystems, networking, display, audio, and input. The available operations depend on the selected implementation. A non-null API object does not prove a particular operation is supported. Required capability failures must reach the caller; optional fallback needs an explicit design.

## Work with events and lifetimes

Use Runtime events for ordered facts and Runtime state for current snapshots, including initial state. Runtime owns input acquisition and system/component producers, so Apps should not create duplicate polling tasks for the same inputs.

For App or library completions, use public custom events with copied values, or an owned bounded dispatch queue and `h2_runtime_notify()`. Handle queue saturation explicitly and keep main-loop dispatch bounded. Do not represent application completions as Runtime system facts.

API objects, buffers, and callback contexts may be borrowed. Their owners must remain alive until all operations finish. Stop producers, cancel and join workers, and finish pending cleanup before destroying Runtime. Only APIs explicitly documented as ISR-safe may be called from an ISR.

## GizMoth scope

GizMoth supports Lua projects today. Native Runtime/PAL integration and firmware composition use the GizOS source workflow; Giz/native project creation and flashing in GizMoth are future features.

## Sources

- [libs/runtime/include/h2_runtime.h](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/runtime/include/h2_runtime.h)
- [libs/pal/include/h2_pal.h](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/pal/include/h2_pal.h)
- [guides/development.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/development.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
