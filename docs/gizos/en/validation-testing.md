# Validation and testing

GizOS uses Bazel to select compatible builds, tests, and artifacts. Validation should match the claim being made: library behavior needs executable tests, platform wiring needs the final artifact, and device behavior needs real device evidence. Prepare the selected host compiler or native SDK environment before running the relevant checks.

## Start with the owning boundary

Validate a public library through its own host tests and consumers. Test App transitions through production event paths. When changing platform wiring, build the final target; a successful intermediate library build cannot establish correct launcher composition.

For the documented Linux host workflow:

```sh
make bazel-build BAZEL_CONFIG=linux_x86_64
make bazel-test BAZEL_CONFIG=linux_x86_64
make bazel-test-downstream-consumer
```

The downstream check exercises GizOS as a public dependency. Apple Silicon host builds use `BAZEL_CONFIG=macos_arm64`.

Ordinary tests should cover meaningful behavior, malformed input, partial initialization, cancellation, cleanup, and concurrency boundaries. Compiled fixtures and checks of real generated outputs are useful evidence. Assertions based only on source strings, private function names, or directory inventories do not establish runtime behavior.

## Separate automatic and manual evidence

CI requests each execution class's full compatible build and automatic test graph. Tests requiring devices, external services, or manual preparation use `manual` and must be invoked by exact Bazel label. A passing automatic graph therefore does not imply those tests ran.

Native firmware actions build declared outputs but never flash, reset, or monitor devices. Claims about serial operation, transfer, interoperability, reboot, or coredumps require the relevant physical checks. Gizloader's package, process, GUI fixture, and installer tests are also separate from hardware transfer/reboot acceptance.

## Coverage and handoff

On Linux x86_64, `make bazel-coverage-report` produces reconciled test results and C/C++ coverage under `build/coverage/test/`. Uncovered targets have unavailable metrics rather than invented zero percentages. A partial artifact is diagnostic, and Linux coverage does not establish firmware, mobile, Windows, audio, network, or rendered UI coverage.

Before handing off source changes:

```sh
make format-check
git diff --check
```

Report the checks that actually ran and any unavailable validation. GizMoth currently supports Lua projects; native/Giz project creation and firmware flashing are future features, so these native validation workflows run outside GizMoth.

## Sources

- [guides/validation.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/validation.md)
- [tools/bazel/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/bazel/README.md)
- [tools/gizloader/README.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/tools/gizloader/README.md)
