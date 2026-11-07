# Application files and key/value storage

App-scoped storage and kv APIs, quotas, file formats and atomic persistence.

## App 存储

`storage` 让 Lua App 在重启后保留少量数据，例如最高分和设置。Board 或宿主通过 `h2_lua_host_config_t.storage` 提供一个借用的 PAL Filesystem、其命名空间中的 root 目录（例如 ESP LittleFS `data` 分区上的 `/data/lua`）、每个 App 的内容字节配额和文件数上限；字段、默认值和上限以 `h2_lua.h` 中 `h2_lua_storage_config_t` 的 Doxygen 为准。fs 必须提供 `mkdir`、`open`、`read`、`write`、`close`、`stat`、`remove` 和覆盖目标的 `rename`，否则 `h2_lua_host_create()` 返回 `UNSUPPORTED`；root 或上限非法时返回 `INVALID_ARG`。Root 不需要预先存在，首次写入时逐级创建。 祖先可能是不可见或拒绝 mkdir 的 mount；KV/storage 继续尝试可见路径，并以最终 App 目录的 stat/is_dir 为准。一个 storage root 同一时间只能由一个 Host 使用，Host 用一个 mutex 串行化所有 job 的存储操作。

每个 `h2_lua_job_submit_*()` 都接收 app id。App id 和文件名都是 `1..32` 字节的 `a-z`、`0-9`、`_`、`-`、`.`，且不能以 `.` 开头，因此绝对路径、`/`、`..`、反斜线、大写字母和隐藏文件都会被拒绝，大小写不敏感的文件系统也不会让两个名字指向同一个文件。非法 app id 使提交返回 `INVALID_ARG`。相同 app id 的 job 共享 `<root>/<app_id>/` 下的文件，不同 app id 互相不可见。

`storage` 始终可以 `require`。Host 未配置 fs 或 job 没有 app id 时，除 `join_path` 外的调用都返回 `nil, "storage: unavailable"`，`exists` 返回 `false`，App 可以继续运行。存储可用时 `exists(name)` 对存在的文件返回 `true`、对不存在的合法名字返回 `false`，非法名字和 I/O 错误返回 `nil, message`。

模块保持 ESP-Claw `storage` 的函数名和成功返回值，差异如下：

- 目录是扁平的。`get_root_dir()` 返回 `""`，因此 `join_path(get_root_dir(), name)` 得到裸文件名；`join_path` 与 ESP-Claw 一样只拼接字符串。没有 `mkdir`；`listdir()` 只接受省略或 `""`，其他合法名字返回 `not found`。
- `stat(name)` 只返回 `type`（固定 `"file"`）和 `size`，`listdir()` 的 entry 另有 `name`，都没有 `mtime` 和 `mode`。
- `get_free_space()` 返回本 App 配额的 `{ total, free, used }`，不是分区容量。
- 失败不抛 Lua error，而是返回 `nil, message`，message 为 `storage: ` 加上 `invalid name`、`not found`、`quota exceeded`、`too many files`、`no space`（文件系统已满）、`busy` 或 `io error`。参数类型错误仍按 Lua 惯例抛错。

`write_file(name, data)` 先检查文件数和配额（替换已有文件只计算新大小），再把内容写入 App 目录中的临时文件，经 `sync`、`close` 后用 `rename` 覆盖目标。任一步失败时目标保持旧内容或不存在，临时文件被删除。

PAL Filesystem 无法列目录，因此每个 App 目录有一个同样原子替换的 `.index` 名字列表：新名字先进入 index 再创建文件，删除时先删文件再更新 index。中断的操作最多留下没有文件的 index 项，下次读取 index 时被清理；未被 index 记录的普通文件不会出现在 `listdir()` 中，也不计入配额；保留的 `.kv` 文件单独统计，共享 App 配额但不展示。存储操作在 owning worker 上同步执行，单次数据量受配额约束，`read_file` 的缓冲区计入 VM 内存上限。

## App KV 存储

`require("kv")` 是 `libs/lua` 的 C 模块，复用 `h2_lua_host_config_t.storage`、job app id 和同一把 Host storage mutex。相同 app id 的 job 共享数据，不同 app id 隔离；没有跨 App 或自定义 namespace 参数。原生 PAL 调用不受此 Lua 层隔离保护。模块名称保留，不能注册同名自定义模块；不改变 ESP-Claw 兼容库存。

| Lua 调用 | 成功或缺失 | 失败 |
| --- | --- | --- |
| `kv.get(key)` | 存在返回 value；缺失返回单个 nil | `nil, err` |
| `kv.set(key, value)` | true | `nil, err` |
| `kv.remove(key)` | 已删除 true；缺失 false | `nil, err` |
| `kv.exists(key)` | true / false | `nil, err` |
| `kv.keys()` | 按 key 字节序升序排列的数组 | `nil, err` |

Key 必须是 string，1..32 字节，仅允许 `a-z`、`0-9`、`_`、`-`、`.`，不能以 `.` 开头，不能包含 NUL，也不把数字隐式转成字符串。最多 `H2_LUA_KV_MAX_KEYS`（128）个 key，更新已有 key 不占新名额。Value 支持 string、boolean、有限 number；保留 integer/float 类型、整数精度与浮点负零。字符串允许空串、UTF-8 和任意二进制字节。Nil、table、function、userdata、thread 不支持；`set(key, nil)` 不表示删除，NaN 和 Infinity 被拒绝。

```lua
local kv = require("kv")
local volume, err = kv.get("volume")
if err then
    print(err)
elseif volume == nil then
    volume = 80
end
local ok, write_err = kv.set("volume", 60)
if not ok then print(write_err) end
```

参数类型错误抛 Lua 参数异常，OOM 沿用 Lua 内存错误。其他错误返回 `nil, "kv: <reason>"`，reason 为 `unavailable`、`invalid key`、`invalid value`（非有限数）、`quota exceeded`、`too many files`、`too many keys`、`no space`、`busy`、`io error`、`corrupt data` 或 `unsupported version`。未配置 storage fs 或 job 无 app id 时，所有有效调用（包括 exists/keys）都报告 unavailable，不把不可用当成 key 缺失；原有 `storage.exists` 行为不变。

### 文件与配额

每 App 保存 `<root>/<app_id>/.kv` 快照，`.kv.tmp` 用于原子替换。公开 storage API 拒绝点开头的文件名，不能直接操作 KV 文件，`storage.listdir()` 也不展示它。`.index` 格式不变，KV 文件通过单独 stat 纳入共享统计：完整编码字节计入 App 配额，存在的快照占一个文件名额，`storage.get_free_space()` 包含 KV 占用。删除最后一个 key 移除快照，释放字节和文件名额。缺失快照是空存储；读取和删除缺失 key 不创建文件。

KV 和普通 storage 写入在同一 mutex 下检查配额和提交。临时文件不计入逻辑配额，但写入需要额外的实际磁盘空间。沿用 storage 的 temp-write、sync、close、rename 保证，掉电持久性取决于 PAL provider。旧固件兼容、降级和历史数据迁移不在 KV 范围内；未知版本仍确定性拒绝，不自动清空或修复数据。

### 执行、锁与内存

操作在 owning worker 同步执行。一个 Host 的所有 App 共用 `storage_mutex`；内部 `_locked` helper 要求调用者持锁，不递归加锁。KV mutation 在一个临界区内读取最新快照、校验、修改、编码、检查共享配额并提交；不同 key 的并发更新不会丢失，同 key 后提交者覆盖。连续 get/set 不组成事务或原子自增。没有跨调用缓存、TTL、批量事务或 clear API。

先短暂持锁查询大小，解锁后分配 Lua userdata 缓冲区，再持锁复查大小并读取最新内容。读操作使用一份快照；mutation 使用旧、新两份缓冲区，顺序扫描记录，不建立 C 对象树。并发增长超过缓冲容量时解锁并重新分配，最多四次尝试，耗尽返回 busy。同尺寸变更仍会重新读取。锁内不调用 Lua、不动态分配；返回值在解锁后构造，OOM 不遗留锁。

快照 userdata 全部计入当前 VM 内存预算，解除引用后由 GC 回收，并非立即释放。重试垃圾、返回字符串/数组和脚本输入也占内存，两份有效快照不是严格峰值上限。不强制全局 GC，也不绕过 VM allocator；磁盘配额足够仍可能 OOM。

### 快照 v1 格式

所有多字节字段为小端序，逐字段编码，不直接落盘 C struct。文件固定开销 20 字节：16 字节 header、记录区、4 字节 checksum。

| Header 字段 | 编码 |
| --- | --- |
| magic | ASCII `GZKV`，4 字节 |
| version | uint16，1 |
| flags | uint16，0 |
| entry_count | uint32，最多 128 |
| payload_length | uint32，记录区总字节数 |

记录按 key 字节序严格升序排列，每条包含 uint8 key_length、uint8 value_type、uint32 value_length、key 原始字节、value 原始字节。类型 1 为字符串原始字节；2 为单字节 boolean（0/1）；3 为 8 字节有符号二进制补码整数；4 为 8 字节 IEEE 754 binary64，保留负零，拒绝非有限数。整数和浮点表示在编译时检查；不允许静默精度截断。

末尾 uint32 为 CRC-32/ISO-HDLC，覆盖 header 和记录区，以小端序保存。多项式 0x04C11DB7（反射形式 0xEDB88320），init/xorout 均为 0xFFFFFFFF，输入/输出反射，`123456789` 的校验值为 0xCBF43926。编码总长为 20 加上每条的 `6 + key_length + value_length`。

截断、溢出、长度不符、非法 key/type/value、重复或乱序 key、非零 flags、尾随字节和 CRC 不符返回 corrupt data；可识别 header 的未知版本返回 unsupported version。损坏和未知版本不得被普通 mutation 覆盖。CRC 检测数据损坏，不代替原子提交或访问控制。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L441), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
