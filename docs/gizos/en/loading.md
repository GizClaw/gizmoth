# Module loading, errors and return values

require roots, compiled resources versus files, job outcomes and Display ownership.

## Source loading and failure

By default, `h2_lua_resource` embeds `.lua` source byte for byte in a C resource. Consumers that need to reduce firmware read-only storage can explicitly set `compact = True`. Compaction happens at build time and removes only comments, leading/trailing whitespace and redundant horizontal whitespace. It does not rename identifiers, rewrite expressions, generate bytecode or add runtime decompression or buffers. Short strings, including escapes, and long strings with any level of equals signs preserve their original bytes. Required token separators remain. Newlines outside strings are normalized according to Lua's CR/LF pairing rules to preserve original source line numbers. Generation rejects NUL input, including in comments, unescaped newlines in short strings, and unterminated strings or long comments. The existing Lua text loader still validates complete syntax. Default generation, C symbols and Host lifecycle are unchanged; `source_size` and source limits use the actual embedded text.

The generator's Python regressions cover token separation, string/comment boundaries, newlines and default compatibility. `//libs/lua:compact_resource_test` compares original and compacted versions of the same fixture in the actual VM, checking results and error line numbers. Application consumers still need functional regressions and an exact firmware build for their own compacted resources. Build-time savings must not be presented directly as VM memory or runtime performance gains.

Embedded resources use `luaL_loadbufferx` text loading, without `luaL_loadfilex`'s first-line shebang skipping. Compaction neither removes a `#!` first line as a comment nor rewrites invalid numerals such as `1..2` into valid expressions. Empty source files, or resources empty after compaction, use one zero byte as the C backing array while retaining logical size zero. Default generated content for nonempty resources is unchanged.

All entry points load Lua text only. Absolute paths, empty path segments, `.`/`..`, backslashes, roots outside the restricted scope, bytecode, oversized or malformed chunks fail closed. `package.cpath` is empty and `package.loadlib` absent. Local `require()` can read only `.lua` text or immutable compiled resources under the current Skill root.

A job has exactly one terminal state: success, failed, cancelled, timed out, or stopped due to Host stop. Events, callbacks, jobs, coroutines and capability requests are all bounded. Full capacity, unsupported operations, invalid payloads, missing components/capabilities and late completions return explicit errors rather than silently succeeding. Cancelled capabilities retain CLOSED semantics until job release, which returns their bounded request slots.

## Job return values

`h2_lua_job_get_result(host, id, buffer, capacity, &size, &has_result)` reads only the main chunk's first return value, and only in `SUCCEEDED`; later return values are ignored. Conversion follows `h2_lua_vm_execute_text()`: strings are preserved verbatim, including embedded NUL; numbers use Lua's number strings; `nil` becomes `"nil"`; booleans become `"true"`/`"false"`. Tables use `__tostring`, or Lua's default `table: ...` representation if absent. Its address portion is unstable and cannot serve as a serialization protocol. No return produces `has_result=0, size=0`; an empty string produces `has_result=1, size=0`. Conversion exceptions fail the job; conversion cannot yield.

The result stays on the main task's Lua stack, counts against `vm_memory_limit_bytes` and status `memory_used`, and remains valid until `h2_lua_job_release()`. Exceeding Host `output_limit_bytes`, excluding the terminating NUL, makes the job `FAILED` with message `H2_LUA_VM_OUTPUT_TOO_LARGE`. Exactly reaching the limit succeeds; no silent truncation occurs.

The caller owns buffer; `size` reports the complete byte length excluding the terminating NUL. Both `size` and `has_result` output pointers are required. `buffer=NULL, capacity=0` queries length and returns `OK`. Reading a job with a result requires `capacity > size`; otherwise it returns `H2_PAL_ERR_NO_SPACE`, still reporting length and presence and leaving buffer unchanged. Successful copying appends a terminating NUL. Any non-`SUCCEEDED` state, including failed/cancelled/timed-out/stopped, returns `H2_PAL_ERR_INVALID_STATE`. Unknown or released ids return `H2_PAL_ERR_NOT_FOUND`. Invalid arguments or `H2_LUA_JOB_ID_NONE` return `H2_PAL_ERR_INVALID_ARG`. These errors clear valid output length and presence fields.

## Validation

```sh
bazel test //libs/lua:all
bazel test //libs/lua:lua_link_test
bazel query 'somepath(//libs/lua:lua_core, //libs/runtime:runtime)'
rg -n 'h2_runtime_(poll|wait)_event' libs/lua/src
bazel run //projects/e2e/targets/cc_binary/lua-runtime:e2e-lua-runtime
```

Both query and `rg` should produce no output. See [E2E test App](/apps/e2e) for the nine fixed E2E cases.

A single Lua script can use `h2_lua_web_app()` from `//libs/lua/web:lua_web_app.bzl` to generate a browser page, `:serve` and `:browser_test` directly without a C entry point. See [Web](/apps/web).

## Borrowing Display and handing off the UI

When an existing UI owns Display, Host's `borrow_display` configuration can borrow the already-open device. The caller first pauses other screen writers and ensures only one Lua job uses Display at a time. The library still allocates and releases its own framebuffer. Module `deinit`, initialization failure, job release, and Host stop/join/destroy after cancellation never call Display PAL open/close. The caller must wait for job release or completed Host stop, join and destruction before resuming UI drawing. Default configuration preserves Lua opening and closing Display for standalone operation.

To ensure contiguous pixel space at startup, callers can set `display_framebuffer_bytes` according to actual Display geometry. Host reserves the drawing buffer before the VM heap and reuses it as jobs hand off Display. See `h2_lua_host_config_t` for capacity, failure behavior and lifecycle. Reservation neither opens Display early nor starts display tasks; the worker's send buffer still counts against the VM.

MP4 player configuration supports the same option. A startup animation can borrow the same Display and return it to the UI after blocking playback returns. Failure and cooperative cancellation likewise clean up only player-owned resources. Borrowing does not automatically pause LVGL or schedule multiple screen writers.

## Sources

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L520), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
