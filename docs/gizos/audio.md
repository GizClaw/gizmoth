# Audio playback and recording

Audio track and input frame contracts; runtime-dependent sound and microphone support.

### Audio Track 的帧契约

`audio.new_output()` 只接收 script 指定的 `sample_rate`、`channels`、
`bits_per_sample`（固定 16）和 `volume`；Track 的 frame 大小不由 script 决定，
而是由模块向 Runtime Audio System 查询 playback format 后填入。ES8311 等由
PAL mixer 支撑的 Audio System 只接受 frame 大小与设备一致的 Track，也只接受
整数个设备 frame 的写入，因此 script 不需要、也不应该猜测这个值。

- `output:info()` 返回 `role`、`opened`、`sample_rate`、`channels`、
  `bits_per_sample`、`bytes_per_frame` 和 `frame_samples`。`frame_samples` 是
  设备每个 frame 的每通道 sample 数；设备没有固定 frame 时为 `0`。
  `frame_samples * bytes_per_frame` 是一个设备 frame 的字节数。
- `output:write(pcm)` 接受长度为 `bytes_per_frame` 正整数倍的非空 PCM。同一条
  Track 上的连续 `write` 属于同一条流：`frame_samples` 非 `0` 时模块按设备 frame
  切分写入，末尾不足一个 frame 的部分**留在 Track 内，与下一次 `write` 的开头
  拼接**，因此 script 不必按 frame 对齐也不会被插入静音。`frame_samples` 为 `0`
  时整块一次写入，此时 frame 数不得超过 `65535`。
- `output:close()` 把仍不足一个 frame 的残留补零后尽力写出，使流的结尾不被丢弃；
  Track 忙时该残留被丢弃，close 不因此失败或阻塞。job 回收时同样处理。
- `write` 成功返回 `true`。被留作残留的字节也算已接收。
- 输入本身不合法时返回两个值 `nil, "audio output: invalid frame"`，不返回
  `written`。空字符串属于这一类：虽然长度 `0` 也是 `bytes_per_frame` 的整数倍，
  但没有可写入的 frame。Track 已关闭或长度不是 `bytes_per_frame` 整数倍同样落入
  这一类。
- 输入合法但设备侧未能全部接收时返回三个值 `nil, err, written`，其中 `err` 为
  `"audio output: busy"` 或 `"audio output: write failed"`，`written` 是本次调用
  中已被 Track 接收的字节数。script 应从 `written` 偏移处续写剩余数据，不要重放
  已被接收的前缀；失败时已有的残留保持不变，重传同一 buffer 是安全的。

### Audio Input 的帧契约

`audio.new_input()` 不接收参数，一个 job 同一时间只能持有一个 Input；已打开时
再次调用返回 `nil, "audio input: already open"`。打开成功时按设备 mic format
（`sample_rate`、`channels`、固定 16 `bits_per_sample`、每通道 `frame_samples`）
分配一次性的 job 私有缓冲区；Runtime 不支持 mic、mic format 不是 16-bit PCM
时返回 `nil, "audio input: unavailable"`，缓冲区分配失败返回 `nil,
"audio input: no memory"`。返回的闭包表带有一个内部 generation 标记：`close()`
或 job 结束后旧闭包的方法一律返回 `nil, "audio input: closed"`，不能再操作已
释放、或已被下一次 `new_input()` 复用的缓冲区。

- `input:info()` 返回 `role`（固定 `"input"`）、`opened`、`sample_rate`、
  `channels`、`bits_per_sample`、`frame_samples` 和 `bytes_per_frame`；未打开
  时除 `role` 外全部为 `0`/`false`。
- `input:read(timeout_ms)` 读取一个完整设备 frame，成功返回该 frame 的 PCM
  string。`timeout_ms` 省略时为 `0`（非阻塞，立即返回是否已有 frame）。
  设备侧超时（含 `timeout_ms=0` 且当前无数据）返回 `nil,
  "audio input: busy"`；读到的数据格式或大小不合法返回 `nil,
  "audio input: read failed"`。
- `input:level(timeout_ms)` 读取一个 frame 后返回三个值：RMS 响度、峰值
  （归一化到 `[0, 1]`）和相邻样本差分估算的高频占比；错误路径与
  `input:read` 相同。
- `input:close()` 释放缓冲区、递减 Host 级 mic user 并使当前闭包的
  generation 失效；已关闭时再次调用仍返回 `true`（幂等）。
- Runtime microphone 一旦没有 frame 会持续阻塞底层 PAL 调用，因此 `read`/
  `level` 按有界时间片轮询 PAL、在每个时间片之间检查 job 取消和 Host
  stopping 状态，而不是把整个 `timeout_ms` 一次性透传给阻塞的 PAL 调用；
  job 被取消或 Host 正在停止时立即返回 `nil, "audio input: cancelled"`，
  不会让调用方的大超时（包括逼近 `UINT32_MAX` 的取值）拖住 Lua worker 或
  阻塞 Host shutdown。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L230), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
