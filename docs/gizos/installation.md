# Installed applications and streaming installation

Host-side app directory and install behavior; packaging is distinct from successful runtime execution.

## Lua App 目录与流式安装

`//libs/lua/apps:apps` 拥有独立于 Lua Host 和 GizClaw 的应用目录与安装器；[API Reference](/references/lua-apps) 直接来自生产 Public Header。调用方注入 Memory、Filesystem、Sync，并配置 `preinstalled_index` 与专用 `install_root`。用户 KV 必须使用安装目录以外的独立根路径，不能把用户数据放进安装器拥有的目录。 FS 可以只暴露嵌套的 mount/view 根；安装器从目标父目录向上寻找最近可见的现有目录，再向下创建缺失部分，不要求 `/data` 等不可见祖先的 stat/mkdir 成功，不扩大 view。父目录的 stat 证明该位置是文件时返回格式错误，其他 FS 错误继续原样失败。应用目录可跨 Service 重连保留，销毁前先停止并释放借用它的 Service 和安装会话。

预装和远程安装使用相同 format-1 Lua App manifest、`app_id` 和版本。`lua_app_index` 从 `LuaAppInfo.manifest` 生成 `format=1, apps=[{root,manifest}]`，不维护手写版本表。规则的 `installed_entry` output group 提供与包内完全相同的原始或 compact Lua 文本，`installed_files` 同时包含该 entry 与原有 data 文件；consumer 将 entry 映射为安装根下的 `<app_id>.lua`，data 保持同名子目录结构。原有 archive/manifest/metadata 默认发行输出不变。加载时缺少所需文件、文件大小不符或 entry 超限的预装项不会进入 listing；已安装的同 ID 项覆盖预装项，每个 ID 只列出一次。

URL 安装入口仅接受 HTTP、HTTPS，长度最多 1024 字节，不接受 data URL；内存中的压缩包通过增量 Binary 输入安装。HTTP 请求不自动重试；redirect 继续限制为 HTTP(S)，允许由 HTTP PAL 解析的相对地址。HTTP 流和增量 Binary 输入均只使用固定窗口，完整压缩包不落盘；`libs/crypto/sha256` 增量计算压缩包与各文件摘要，版本校验复用 `libs/semver`。

增量安装使用 begin/write/finish/abort，同一个会话的调用由一个 owner 串行执行。会话持有目录 reservation，其他 task 上的目录操作返回 BUSY；同一 owner 不重入目录操作。finish 只有在声明长度、可选压缩 SHA-256、manifest、全部文件长度与 SHA-256、完整 USTAR 结束块、zlib EOS 均通过后才发布。默认限制为 32 个应用身份、64 个 payload 文件、256 KiB entry、每包 2 MiB payload 和总计 8 MiB 已安装 payload；预装字节不计入安装字节配额。

安装器先写 ordinal staging 文件，再将验证后的文件移到该 ID 的非活动 slot，最后通过 replacing atomic rename 发布总 index。清理前检查两个 slot 的 manifest identity；文件系统将不同 app_id 映射到同一目录时，拒绝该安装并保留已发布的应用。安装器观察到的取消、输入错误和发布失败保留旧版本与独立 KV。传输超时不能证明设备未安装，重新连接后应查询 listing，不能自动重试安装。成功发布后旧 slot 的清理不会把已完成的安装报告为失败。Filesystem 必须提供同步写入及替换式原子 rename；close 错误无法证明句柄是否已被消费时，目录进入终态存储错误并保留自身与借用依赖，禁止自动重试 close 或释放可能仍存活的 backend，需要结束该进程或设备生命周期后恢复。

`h2_lua_apps_uninstall` 按稳定 `app_id` 幂等卸载并保留独立用户存储。它先原子发布目录移除，再按 manifest 清理两个可写版本 slot；目录发布失败保留旧应用，发布后的清理失败使应用保持不可用，重复卸载重试清理。预装文件不可变，目录用 `{"app_id":"demo","removed":true}` 持久隐藏预装身份及其覆盖版本，重启不会恢复；重新安装同 ID 后恢复正常 listing。远程独有身份移出索引并释放应用数与字节预算。只读目录不公布卸载能力；运行 owner 需要协调仍在使用 entry/data_dir 快照的任务。

`h2_lua_apps_get` 返回 caller-owned entry/data_dir 快照，由产品 owner 在自己的运行流程中提交给 Lua Host。安装器不启动 VM，也不切换产品界面。运行中的 owner 应协调应用更新；快照中的安装路径不会在后续更新后永久保留。

Host 可设置 `on_display_present`，在 Display worker 成功完成 PAL present 后观察完整 RGB565 framebuffer。像素与 callback context 均有明确借用生命周期，回调不能保留像素或重入 Host；它可用于摘要/golden 验证，只证明 renderer 与 PAL 接受了该帧，不证明面板光学输出。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L593), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
