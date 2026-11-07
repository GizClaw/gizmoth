# GizOS overview

GizOS is a portable firmware and application platform. It brings application runtimes, hardware abstraction, reusable libraries, board support, packaging, and validation into one Bazel workspace. Lua App development is one part of this platform; native applications, firmware composition, libraries, and platform integration have their own responsibilities.

## How the layers fit together

Applications use Runtime and public libraries. Runtime and libraries access system capabilities through the Platform Abstraction Layer (PAL). Providers implement those contracts using operating systems, SDKs, and hardware. Board packages describe physical wiring and hardware defaults; final artifact entries combine an App, platform, component mappings, and image configuration.

Reusable platform code belongs in GizOS. A product repository owns its hardware composition, configuration, assets, and release policy and can consume public targets through `@gizos//...` using Bzlmod. This boundary lets the same application logic be reused without embedding a particular board's pin assignments or SDK objects in it.

## Choose an entry point

- For Lua projects in GizMoth, start with the Lua App guides.
- For compiled application behavior, read the native App and Runtime/PAL guides.
- For reusable capabilities, read the libraries guide.
- For hardware integration and deliverables, read boards, toolchains, and firmware development.
- For confidence in a change, use the validation guide and the owning package's tests.

For an initial Linux host build, prepare Bazel 9.2.0 or a compatible launcher, a C/C++ toolchain, and the selected platform's dependencies. From the GizOS checkout:

```sh
make help
make bazel-build BAZEL_CONFIG=linux_x86_64
make bazel-test BAZEL_CONFIG=linux_x86_64
```

Apple Silicon host builds use `BAZEL_CONFIG=macos_arm64`. Compatibility is selected by the Bazel graph; a platform's presence does not promise every library or hardware operation.

## GizMoth scope

GizMoth currently supports Lua projects. Giz/native firmware project creation and firmware flashing are future GizMoth features. The native build and loader workflows documented here describe the GizOS toolchain and separate tools, not actions already available in GizMoth.

## Sources

These guides use GizOS revision `75deb77229d3fb07dd10d39683c69009b1eb45e9`.

- [README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/README.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
