# Application files and key/value storage

App-scoped storage and kv APIs, quotas, file formats and atomic persistence.

## App storage

`storage` lets Lua Apps retain small amounts of data across restarts, such as high scores and settings. Through `h2_lua_host_config_t.storage`, the board or host supplies a borrowed PAL Filesystem, a root directory in its namespace (for example `/data/lua` on ESP LittleFS's `data` partition), and per-App content-byte and file-count limits. Fields, defaults and maximums are defined by the Doxygen for `h2_lua_storage_config_t` in `h2_lua.h`. fs must provide `mkdir`, `open`, `read`, `write`, `close`, `stat`, `remove` and destination-overwriting `rename`; otherwise `h2_lua_host_create()` returns `UNSUPPORTED`. Invalid roots or limits return `INVALID_ARG`. Root need not exist beforehand; the first write creates it one level at a time. Ancestors may be invisible mounts or reject mkdir. KV/storage continue trying visible paths and rely on stat/is_dir of the final App directory. Only one Host may use a storage root at a time. A Host serializes all jobs' storage operations with one mutex.

Every `h2_lua_job_submit_*()` accepts an app id. App ids and filenames are `1..32` bytes using `a-z`, `0-9`, `_`, `-`, `.`, and cannot begin with `.`. Absolute paths, `/`, `..`, backslashes, uppercase letters and hidden files are therefore rejected; case-insensitive filesystems cannot make two names refer to the same file. An invalid app id makes submission return `INVALID_ARG`. Jobs with the same app id share files under `<root>/<app_id>/`; different app ids cannot see each other's files.

`storage` can always be required. If Host has no configured fs or the job has no app id, all calls except `join_path` return `nil, "storage: unavailable"`, while `exists` returns `false`, allowing the App to keep running. When storage is available, `exists(name)` returns `true` for existing files and `false` for nonexistent valid names; invalid names and I/O errors return `nil, message`.

The module retains ESP-Claw `storage` function names and success returns, with these differences:

- Directories are flat. `get_root_dir()` returns `""`, so `join_path(get_root_dir(), name)` produces a bare filename. As in ESP-Claw, `join_path` only concatenates strings. There is no `mkdir`; `listdir()` accepts only omission or `""`, returning `not found` for other valid names.
- `stat(name)` returns only `type` (always `"file"`) and `size`; `listdir()` entries additionally have `name`. Neither provides `mtime` or `mode`.
- `get_free_space()` returns `{ total, free, used }` for this App's quota, not partition capacity.
- Failures return `nil, message` instead of raising a Lua error. message is `storage: ` followed by `invalid name`, `not found`, `quota exceeded`, `too many files`, `no space` (filesystem full), `busy` or `io error`. Argument type errors still raise errors following Lua convention.

`write_file(name, data)` first checks file count and quota, counting only the new size when replacing an existing file. It writes to a temporary file in the App directory, performs `sync` and `close`, then uses `rename` to replace the target. If any step fails, the target retains its old content or remains absent, and the temporary file is deleted.

PAL Filesystem cannot list directories, so each App directory has an atomically replaced `.index` name list. New names enter the index before file creation; deletion removes the file before updating the index. Interrupted operations can leave at most an index entry without a file, cleaned up on the next index read. Ordinary files absent from the index neither appear in `listdir()` nor count against the quota. The reserved `.kv` file is counted separately, sharing the App quota without being listed. Storage operations execute synchronously on the owning worker; quota bounds each operation's data size, and `read_file` buffers count against the VM memory limit.

## App KV storage

`require("kv")` is a C module in `libs/lua`, reusing `h2_lua_host_config_t.storage`, the job app id and the same Host storage mutex. Jobs with the same app id share data; different app ids are isolated. There are no cross-App or custom namespace parameters. Native PAL calls are not protected by this Lua-layer isolation. The module name is reserved, so custom modules cannot register the same name. The ESP-Claw compatibility inventory is unchanged.

| Lua call | Success or absence | Failure |
| --- | --- | --- |
| `kv.get(key)` | Returns value if present; a single nil if absent | `nil, err` |
| `kv.set(key, value)` | true | `nil, err` |
| `kv.remove(key)` | true if removed; false if absent | `nil, err` |
| `kv.exists(key)` | true / false | `nil, err` |
| `kv.keys()` | Array sorted in ascending key byte order | `nil, err` |

Keys must be strings of 1..32 bytes containing only `a-z`, `0-9`, `_`, `-`, `.`, without leading `.` or NUL. Numbers are not implicitly converted to strings. At most `H2_LUA_KV_MAX_KEYS` (128) keys are allowed; updating an existing key uses no new slot. Values support strings, booleans and finite numbers, preserving integer/float type, integer precision and floating-point negative zero. Strings may be empty, UTF-8 or arbitrary binary bytes. Nil, table, function, userdata and thread are unsupported. `set(key, nil)` does not mean deletion; NaN and Infinity are rejected.

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

Argument type errors raise Lua argument exceptions; OOM uses Lua memory errors. Other errors return `nil, "kv: <reason>"`, where reason is `unavailable`, `invalid key`, `invalid value` (nonfinite numbers), `quota exceeded`, `too many files`, `too many keys`, `no space`, `busy`, `io error`, `corrupt data` or `unsupported version`. Without configured storage fs or a job app id, all valid calls, including exists/keys, report unavailable rather than treating unavailability as a missing key. Existing `storage.exists` behavior is unchanged.

### Files and quotas

Each App stores a `<root>/<app_id>/.kv` snapshot, with `.kv.tmp` for atomic replacement. The public storage API rejects dot-prefixed filenames, preventing direct access to KV files, and `storage.listdir()` does not show them. The `.index` format is unchanged. KV files enter shared accounting through a separate stat: all encoded bytes count against the App quota, an existing snapshot uses one file slot, and `storage.get_free_space()` includes KV usage. Removing the final key deletes the snapshot and frees its bytes and file slot. A missing snapshot means empty storage; reading or deleting missing keys does not create a file.

KV and ordinary storage writes check quotas and commit under the same mutex. Temporary files do not count toward logical quota but require extra physical disk space. The existing storage temp-write, sync, close and rename guarantees apply; power-loss durability depends on the PAL provider. Older-firmware compatibility, downgrade and historical data migration are outside KV's scope. Unknown versions are still rejected deterministically without automatically clearing or repairing data.

### Execution, locking and memory

Operations execute synchronously on the owning worker. All Apps in one Host share `storage_mutex`. Internal `_locked` helpers require callers to hold the lock and do not lock recursively. One critical section reads the latest snapshot, validates, modifies, encodes, checks shared quota and commits a KV mutation. Concurrent updates to different keys are not lost; for the same key, the later commit wins. Consecutive get/set calls do not form a transaction or atomic increment. There is no cache across calls, TTL, batch transaction or clear API.

The operation briefly locks to query size, unlocks to allocate a Lua userdata buffer, then locks again to recheck size and read the latest content. Reads use one snapshot; mutations use old and new buffers, scanning records sequentially without building a C object tree. If concurrent growth exceeds buffer capacity, it unlocks and reallocates, with at most four attempts before returning busy. Same-size changes are still reread. No Lua calls or dynamic allocations occur under the lock. Return values are constructed after unlocking, so OOM cannot leave the lock held.

All snapshot userdata counts against the current VM budget and is collected after dereferencing rather than freed immediately. Retry garbage, returned strings/arrays and script input also consume memory; two live snapshots are not a strict peak bound. The implementation neither forces global GC nor bypasses the VM allocator; sufficient disk quota does not prevent OOM.

### Snapshot v1 format

All multibyte fields are little-endian and encoded individually; C structs are not written directly. Fixed file overhead is 20 bytes: a 16-byte header, the record area and a 4-byte checksum.

| Header field | Encoding |
| --- | --- |
| magic | ASCII `GZKV`, 4 bytes |
| version | uint16, 1 |
| flags | uint16, 0 |
| entry_count | uint32, at most 128 |
| payload_length | uint32, total record-area bytes |

Records are sorted strictly in ascending key byte order. Each contains uint8 key_length, uint8 value_type, uint32 value_length, raw key bytes and raw value bytes. Type 1 is raw string bytes; 2 is a one-byte boolean (0/1); 3 is an 8-byte signed two's-complement integer; 4 is an 8-byte IEEE 754 binary64, preserving negative zero and rejecting nonfinite values. Integer and floating-point representations are checked at compile time; silent precision truncation is forbidden.

The final uint32 is CRC-32/ISO-HDLC over the header and record area, stored little-endian. The polynomial is 0x04C11DB7 (reflected form 0xEDB88320), init/xorout are both 0xFFFFFFFF, and input/output are reflected. The checksum for `123456789` is 0xCBF43926. Total encoded length is 20 plus `6 + key_length + value_length` per record.

Truncation, overflow, inconsistent lengths, invalid keys/types/values, duplicate or unordered keys, nonzero flags, trailing bytes and CRC mismatch return corrupt data. An unknown version with a recognizable header returns unsupported version. Ordinary mutations must not overwrite corrupt or unknown-version data. CRC detects corruption; it does not replace atomic commit or access control.

## Sources

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L441), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
