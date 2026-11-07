# Firmware development

A firmware artifact combines an App, platform implementations, board wiring, SDK configuration, and packaging. Prepare the GizOS checkout, Bazel 9.2.0, and the exact SDK/toolchain environment required by the selected existing target. The product or final artifact entry owns image policy and Runtime lifetime; portable libraries remain independent of board wiring and firmware registration.

## Compose one firmware graph

Final entries live under `projects/<project>/targets/<artifact-rule>/<app>[/<variant>]/`. Each firmware entry owns one `firmware_lib_component` collecting its selected Bazel libraries. SDK-compiled sources, boards, and launchers use `firmware_native_component` with explicit sources, headers, metadata, and dependencies.

The runner turns this graph into invocation-local native manifests. ESP-IDF CMake or BK SDK Make/CMake keeps responsibility for generated SDK configuration, vendor sources, compilation, linking, bootloader, and partition mechanics. Native build files consume the declared graph rather than scanning the repository for first-party sources.

## Build the delivery target

A documented BK7258 example is:

```sh
bun tools/gizos/src/cli.ts setup bk 3.1.1-r6bbf627
eval "$(bun tools/gizos/src/cli.ts env bk 3.1.1-r6bbf627 --shell sh)"
bazel build --config=bk7258 //projects/example/targets/h2loader_tar_zlib/display/bk7258_v3_202405:package
```

H2Loader delivery entries separate internal `:firmware` from public `:package`. Packaging traverses the native build and emits `.update.tar.zlib` and `.firmware.json`. ESP native outputs include ELF/map, application, bootloader, partition table, and combined factory image; BK outputs include AP/CP ELF/map/images, managed update image, recovery image, and partition metadata. Do not substitute arbitrary SDK intermediates for declared deliverables.

## Device operations are separate

Build actions do not flash, reset, connect to serial devices, or monitor hardware. Gizloader is a separate desktop/CLI tool for firmware operations and logs:

```sh
bunx gizos install gizloader
gizloader --help
gizloader scan
```

Gizloader supports macOS Intel/Apple Silicon and Windows x64 serial devices; Windows BLE is not implemented, and desktop Bluetooth operations use the CLI. GUI port-list scanning does not connect or write firmware. Select a compatible package and follow the loader's actual help before device operations. Hardware transfer/reboot acceptance requires separate physical evidence.

GizMoth currently supports Lua projects. Giz/native firmware project creation and firmware flashing in GizMoth are future features.

## Sources

- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
- [tools/bazel/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/bazel/README.md)
- [tools/gizloader/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizloader/README.md)
