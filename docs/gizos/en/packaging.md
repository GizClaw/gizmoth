# Lua application packaging

app_id, entry Lua file, manifest, asset paths, integrity and firmware package limits.

## Lua App distribution packages

The public `lua_app` rule in `libs/lua/lua_app.bzl` declares a distributable application: a required `<app_id>.lua` text entry point and an optional explicit file list from a matching `<app_id>/` data directory. No directory is needed without data files. The entry filename determines the stable app id, following Host's lowercase-letter, digit, `_`, `-`, `.` and 1..32-byte limits. Changing the entry basename changes the application and saved-data identity. Source directory layouts and existing single-file labels can remain unchanged.

Each app must consume its own declared `app_version` target, using the same 31-byte SemVer contract as other declarative artifacts. There is no default version, and neither firmware image versions nor release batches are read. Different apps upgrade independently by default.

```bzl
load("@gizos//libs/lua:lua_app.bzl", "lua_app")
load("@gizos//tools/bazel:app_version.bzl", "app_version")

app_version(name = "abc_version", value = "0.1.0")

lua_app(
    name = "abc_app",
    entry = "abc.lua",
    data = ["abc/tone.pcm"],  # Optional; omit for a single-file app.
    version = ":abc_version",
)
```

The rule outputs `<app_id>-<version>.lua-app.tar.zlib`, a `.manifest.json` with the same stem, and `.json` release metadata. It exposes identity and all three Files through `LuaAppInfo`. The archive is zlib-compressed USTAR (DEFLATE wrapped in RFC 1950), containing `manifest.json`, the entry point and data files. Existing zlib inflate mode can decompress it. File ordering, mtime, mode, uid/gid and compression parameters are fixed; machine paths and release batches are excluded. The manifest has `format=1` and `type=lua-app`, recording app id, version, entry, nullable data_dir, and each payload file's path, size and SHA-256. Release metadata retains the manifest and records archive basename, size and SHA-256. Download checksums and internal payload checksums serve separate purposes.

Optional `compact=True` reuses the same public `embed_resource.py` source compactor, preserving literal bytes and source line count. The `compact` fields in manifest and release metadata record the choice; the default preserves original text. Data files must come from the matching directory next to the entry point. Absolute paths, traversal, hidden files and reserved `.kv`/`.index` saved-data files are forbidden. All archive entries are regular files; entry points reject bytecode and NUL. Packages may declare nested data paths, but this does not alter the existing flat-directory `storage` contract below. Consuming hosts must validate compatibility with their own resource access and quotas. Packaging neither creates Runtime, runs scripts, installs devices nor changes existing saved data.

Release consumers use public `lua_app_catalog.cquery` to read app id, version, entry and data_dir from the Provider, and `lua_app_release_files.cquery` for structured output paths. They must not infer identity from Bazel package paths or maintain a second version table. Product repositories declare release admission tags. GizOS runtime C source-package releases and Lua app packages are separate artifacts.

Validate the public rule and real Bazel outputs:

```sh
bazel test --config=macos_arm64 //libs/lua:app_package_test
make bazel-test-downstream-consumer
```

The Host package test validates all three output groups: archive, manifest and metadata. The downstream consumer declares apps both with and without data through the real `@gizos` rule, runs both public cquery formatters, then uses `bazel test` to compare Provider identity, formatter paths and the `DefaultInfo` file set against every generated file. The same test also checks the internal manifest, payload and release download checksums.

## Sources

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L406), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
