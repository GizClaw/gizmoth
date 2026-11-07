# Lua application packaging

app_id, entry Lua file, manifest, asset paths, integrity and firmware package limits.

## Lua App 发布包

`libs/lua/lua_app.bzl` 的公共 `lua_app` rule 声明一个可分发应用：必选的 `<app_id>.lua` 文本入口，以及可选的同名 `<app_id>/` data 目录中的显式文件列表。没有 data 文件时不需要创建目录。入口文件名决定稳定 app id，沿用 Host 的小写字母、数字、`_`、`-`、`.` 和 1..32 字节限制；更改入口 basename 会改变应用与存档身份。源码目录布局和现有单文件 label 可以保持不变。

每个 app 必须消费自己声明的 `app_version` target，使用与其他声明式 artifact 相同的 31-byte SemVer 合同。没有默认版本，也不读取 firmware image 版本或 release batch；不同 app 默认独立升级。

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

Rule 输出 `<app_id>-<version>.lua-app.tar.zlib`、同 stem 的 `.manifest.json` 和 `.json` release metadata，并通过 `LuaAppInfo` 暴露 identity 与三个 File。Archive 是 zlib 格式压缩的 USTAR（RFC 1950 包装的 DEFLATE），含 `manifest.json`、入口和 data 文件，可使用现有 zlib inflate 模式解压；文件排序、mtime、mode、uid/gid 和压缩参数固定，不包含机器路径或发布批次。Manifest `format=1`、`type=lua-app`，记录 app id、version、entry、可空的 data_dir，以及每个 payload 文件的 path、size 和 SHA-256。Release metadata 保留 manifest，并记录压缩包 basename、size 和 SHA-256；下载校验值与包内文件校验值分别使用。

可选的 `compact=True` 复用同一公共 `embed_resource.py` 源码精简器，保留 literal bytes 和源码行数，manifest 与 release metadata 的 `compact` 字段记录该选择；默认保持原始文本。Data 文件必须来自入口旁的同名目录，不能包含绝对路径、路径穿越、隐藏文件或保留的 `.kv`/`.index` 存档。所有 archive entry 都是普通文件；入口拒绝 bytecode 和 NUL。Package 可以声明多层 data 路径，但这不改变下文现有 `storage` 的扁平目录合同；消费宿主必须按自身资源访问能力和配额验证兼容性。打包不创建 Runtime、不运行脚本、不安装设备，也不改变已有存档。

Release consumer 使用公共 `lua_app_catalog.cquery` 从 Provider 读取 app id、version、entry 和 data_dir，使用 `lua_app_release_files.cquery` 读取结构化输出路径，不能从 Bazel package 路径猜测 identity 或维护第二份版本表。发布准入 tag 由产品仓库声明；GizOS 的 runtime C 源码包 release 与 Lua app 包是不同的 artifact。

验证公共 rule 与真实 Bazel 输出：

```sh
bazel test --config=macos_arm64 //libs/lua:app_package_test
make bazel-test-downstream-consumer
```

Host package test 同时校验 archive、manifest 和 metadata 三个 output group。Downstream consumer 用真实 `@gizos` rule 声明无 data 与有 data 的应用，执行两个公共 cquery formatter，再通过 `bazel test` 把 Provider identity、formatter 路径和 `DefaultInfo` 文件集合与全部生成文件逐一核对；同一测试也校验包内 manifest、payload 和发布下载校验值。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L406), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
