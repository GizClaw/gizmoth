# Reusable libraries

GizOS includes reusable networking, media, protocol, UI, game runtime, command routing, serialization, security, and utility capabilities. Libraries expose public interfaces so Apps can compose them without depending on private implementation or one board. Before integrating a library, read its public headers for supported operations, ownership, threading, and result semantics, and prepare the compiler environment for the intended target.

## Choose the correct owner

Repository-wide capabilities live in `libs/<library>/`, with public `include/`, private `src/`, local `tests/`, and `BUILD.bazel`. A capability with an explicit project owner stays in `projects/<project>/libs/<library>/`, even when another project consumes it. Consumers depend on the provider's public API; dependencies must not form cycles.

Portable library cores receive platform capabilities through injected PAL APIs, configuration, or callbacks. Keep Runtime-aware adapters separate from independent cores. Board wiring, SDK registration, and final image policy belong to their respective platform and artifact owners.

## Integrate and build

Declare sources, headers, data, and dependencies in the owning package's `BUILD.bazel`. The primary target matches the library directory name. Add `_embed`, `_desktop`, `_mobile`, or `_web` variants only when sources, definitions, dependencies, or compatibility actually differ.

A downstream product can pin GizOS through Bzlmod:

```starlark
bazel_dep(name = "gizos", version = "0.0.0")

git_override(
    module_name = "gizos",
    commit = "75deb77229d3fb07dd10d39683c69009b1eb45e9",
    remote = "https://github.com/GizClaw/gizos.git",
)
```

Then depend on the appropriate public `@gizos//...` targets. The consuming repository still owns platform selection and product composition.

## Lifetime and validation

Use the owner's allocator contract and pair allocation/free through the same allocator. Borrowed dependencies must outlive active operations. Define who releases asynchronous results and how cancellation reaches a terminal outcome. Preserve distinctions such as unsupported, timeout, cancellation, saturation, and operational failure.

Validate the library through its host tests and consumers. `make bazel-test-downstream-consumer` checks public consumption from a separate dependency context. A host test does not establish device support, and a listed library is not necessarily compatible with every target.

GizMoth currently supports Lua projects. Native/Giz project creation and firmware flashing remain future GizMoth features.

## Sources

- [README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/README.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
- [guides/development.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/development.md)
