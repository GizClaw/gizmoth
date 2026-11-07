# Native App development

A native GizOS App implements application behavior against Runtime and portable public libraries. Its entry receives an initialized `h2_runtime_t *` and optional App-owned public configuration. The final artifact entry constructs and destroys Runtime, selects the platform, and supplies the hardware mappings. This is a source-level development workflow requiring the GizOS checkout and the selected target's compiler environment.

## Organize the application

A standalone App uses `projects/<app>/app/{include,src}/`. A grouped App uses `projects/<group>/apps/<app>/app/{include,src}/`. Keep assets and package data with their owner. Project-specific reusable libraries live in named `projects/<project>/libs/<library>/` packages.

Define the required logical `component_id` values in the App's public contract. Before starting the App, the launcher validates every mapping. Missing required mappings are wiring errors. Resolve physical peripheral IDs through Runtime only when performing the PAL operation; do not retain them as the App's portable identity.

## Implement a predictable execution model

Keep business state under one main-loop writer. Runtime events provide ordered facts, while Runtime state provides current snapshots. Workers return results to that loop instead of changing application state themselves. Blocking networking, storage, or decoding belongs outside event handlers and UI observers.

Use `h2_runtime_wait_notify()` to wait for a Runtime wake or App deadline, drain Runtime events before waiting again, and service library dispatch queues. Wakes can coalesce, so they are not a count of completed operations. If the App uses LVGL, keep its calls, subject changes, and widget lifecycle on the App/LVGL thread.

## Finish and validate

Make buffer ownership and callback lifetimes explicit. On exit, stop new work, request cancellation, wake blocked users, join workers, clean up pending completions, and release integrations before their borrowed PAL dependencies disappear. A cancellation request alone does not establish completion.

Test application transitions through production paths, then build the final target to verify launcher wiring. The selected capabilities must be supported; an API object's presence is insufficient evidence.

GizMoth currently supports Lua projects. Native/Giz project creation and firmware flashing are future GizMoth features; follow this workflow in the GizOS source/toolchain today.

## Sources

- [guides/development.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/development.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
- [libs/runtime/include/h2_runtime.h](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/runtime/include/h2_runtime.h)
