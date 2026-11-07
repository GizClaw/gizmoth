# Audio playback and recording

Audio track and input frame contracts; runtime-dependent sound and microphone support.

### Audio Track frame contract

`audio.new_output()` accepts only script-specified `sample_rate`, `channels`, `bits_per_sample` (fixed at 16) and `volume`. The script does not determine Track frame size: the module queries Runtime Audio System's playback format and fills it in. Audio Systems backed by the PAL mixer, such as ES8311, accept only Tracks matching the device frame size and writes containing whole device frames. Scripts therefore neither need nor should attempt to guess this value.

- `output:info()` returns `role`, `opened`, `sample_rate`, `channels`, `bits_per_sample`, `bytes_per_frame` and `frame_samples`. `frame_samples` is the device's samples per channel per frame, or `0` when the device has no fixed frame size. `frame_samples * bytes_per_frame` is the byte size of one device frame.
- `output:write(pcm)` accepts nonempty PCM whose length is a positive integer multiple of `bytes_per_frame`. Consecutive writes on one Track belong to the same stream. When `frame_samples` is nonzero, the module splits writes into device frames. Any trailing partial frame is **retained inside the Track and joined to the beginning of the next `write`**, so scripts need not align writes to device frames and no silence is inserted. When `frame_samples` is `0`, the entire block is written at once; its frame count must not exceed `65535`.
- `output:close()` zero-pads any remaining partial frame and makes a best-effort write so the stream's end is not lost. If the Track is busy, the remainder is discarded without making close fail or block. Job reclamation handles it the same way.
- Successful `write` returns `true`. Bytes retained as a remainder also count as accepted.
- Invalid input returns two values, `nil, "audio output: invalid frame"`, without `written`. An empty string is invalid: although `0` is an integer multiple of `bytes_per_frame`, it contains no writable frames. A closed Track or a length not divisible by `bytes_per_frame` produces the same result.
- Valid input that the device cannot accept completely returns three values: `nil, err, written`. `err` is `"audio output: busy"` or `"audio output: write failed"`; `written` is the number of bytes accepted by the Track in this call. Scripts should resume at offset `written` without replaying the accepted prefix. Existing retained data remains unchanged on failure, so retransmitting the same buffer is safe.

### Audio Input frame contract

`audio.new_input()` takes no arguments. A job may hold only one Input at a time; calling it again while open returns `nil, "audio input: already open"`. Successful opening allocates a one-time job-private buffer according to the device mic format: `sample_rate`, `channels`, fixed 16 `bits_per_sample`, and per-channel `frame_samples`. If Runtime does not support a mic or its format is not 16-bit PCM, it returns `nil, "audio input: unavailable"`. Buffer allocation failure returns `nil, "audio input: no memory"`. The returned closure table carries an internal generation marker. After `close()` or job termination, methods on old closures return `nil, "audio input: closed"`; they cannot access a released buffer or one reused by a later `new_input()`.

- `input:info()` returns `role` (always `"input"`), `opened`, `sample_rate`, `channels`, `bits_per_sample`, `frame_samples` and `bytes_per_frame`. When not open, all except `role` are `0`/`false`.
- `input:read(timeout_ms)` reads one complete device frame and returns its PCM string on success. Omitted `timeout_ms` defaults to `0`: a nonblocking check that returns immediately depending on frame availability. A device-side timeout, including `timeout_ms=0` with no data, returns `nil, "audio input: busy"`. Invalid data format or size returns `nil, "audio input: read failed"`.
- `input:level(timeout_ms)` reads one frame and returns three values: RMS loudness, peak (normalized to `[0, 1]`), and a high-frequency proportion estimated from differences between adjacent samples. Error paths match `input:read`.
- `input:close()` releases the buffer, decrements the Host-level mic user count and invalidates the current closure's generation. Repeated calls after closure still return `true` (idempotent).
- Without frames, Runtime microphone would keep the underlying PAL call blocked. Therefore `read`/`level` poll PAL in bounded time slices, checking job cancellation and Host stopping between slices, rather than forwarding the whole `timeout_ms` to one blocking PAL call. Cancellation or Host stopping immediately returns `nil, "audio input: cancelled"`. Large caller timeouts, including values approaching `UINT32_MAX`, cannot hold up a Lua worker or block Host shutdown.

## Sources

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L230), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
