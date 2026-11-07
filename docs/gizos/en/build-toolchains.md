# Builds and toolchains

Bazel is GizOS's source of truth for dependency resolution, platform compatibility, builds, tests, and artifacts. Use Bazel 9.2.0 from the pinned source's `.bazelversion`, a suitable C/C++ toolchain, and the selected platform's dependencies. Native SDKs remain responsible for their compiler/linker policy and generated configuration beneath the Bazel firmware entry.

## Begin with a host build

Run from the GizOS checkout:

```sh
make bazel-build BAZEL_CONFIG=linux_x86_64
make bazel-test BAZEL_CONFIG=linux_x86_64
```

Use `BAZEL_CONFIG=macos_arm64` on Apple Silicon. These entries request the compatible build/test graph. Do not infer that every repository target is available on the chosen host.

## Prepare a shared SDK

The `gizos` toolbox requires Bun 1.3.9 or newer. The public package needs no custom registry mapping. SDK setup requires Git, Bash, a host Python supported by the SDK, and archive tools; installation supports macOS and Linux with resource-specific restrictions.

```sh
bunx gizos --help
bunx gizos list esp-idf
bunx gizos setup esp-idf 6.2
bunx gizos env esp-idf
```

The toolbox shares installations and records across worktrees. SDK versions can keep independent paths while sharing compatible tools. `env` reports paths without installing anything; source aliases and exact version/commit pins come from the maintained catalog. Arbitrary source URLs are rejected.

For the documented source-checkout ESP32-S3 build:

```sh
bun tools/gizos/src/cli.ts setup idf 6.2
eval "$(bun tools/gizos/src/cli.ts env idf 6.2 --shell sh)"
bazel build --config=esp32s3 //projects/example/targets/h2loader_tar_zlib/ble-broadcaster/devkit:package
```

## Understand the boundaries

ESP-IDF and BK7258 native firmware actions execute locally, without sandboxing or remote execution, and publish declared outputs that can participate in caching. They do not flash, reset, or monitor devices. Hardware operations are a separate step.

Host support varies: BK7258's GNU Arm 10.3 compiler needs Rosetta 2 on Apple Silicon, while JieLi compiler/post-build tools run on Linux x86_64. A prepared SDK is not proof of a supported board or successful device run.

GizMoth supports Lua projects today. Native/Giz project creation and firmware flashing are future GizMoth features; these commands belong to the external GizOS workflow.

## Sources

- [tools/bazel/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/bazel/README.md)
- [tools/gizos/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizos/README.md)
- [README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/README.md)
