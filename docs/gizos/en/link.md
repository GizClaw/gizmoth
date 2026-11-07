# BLE peer links

Host/join, reliable and unreliable messages, streams and launcher-dependent link support.

### BLE peer link

`link` pairs two nearby devices over BLE and provides three transports on the same connection: reliable messages, unreliable messages and byte streams. It uses only BLE Host, Task, Sync, Time, Mem and System Event PAL, without Wi-Fi, Netif or any network API. Turning off Wi-Fi during a game does not affect the link.

**Enabling.** `//libs/lua:lua_link` is a separate target; images that do not use link do not link BLE iKCP. Before `h2_lua_host_start()`, Launcher calls `h2_lua_link_enable(host, &(h2_lua_link_config_t){adv_type, scan_type})`, selecting legacy or extended advertising/scanning for the board's BLE stack. Extended advertisements are invisible to legacy scanners, so interoperating devices must choose the same advertising type. Link advertisements are only 21 bytes and fit in legacy advertising. ESP board-level BLE requires `CONFIG_BT_NIMBLE_EXT_ADV`: host uses the advertising set API, which NimBLE does not provide with this option disabled, causing `host()` to end with `LINK_ERROR "ble"`. If Runtime lacks `ble_host` or `system_event`, or uses a canonical unsupported object, enabling returns `H2_PAL_ERR_UNSUPPORTED` and link remains unavailable. Calling after start or enabling twice returns `H2_PAL_ERR_INVALID_STATE`. Launcher starts BLE Host and keeps it alive until `h2_lua_host_destroy()` returns.

**GATT profile.** All links share one fixed service. UUIDs are library constants and cannot be configured:

| Purpose | UUID | Properties |
| --- | --- | --- |
| Service | `0685b801-18da-449c-88a2-66c491b17772` | primary |
| KCP TX | `0685b802-18da-449c-88a2-66c491b17772` | notify (bleikcp) |
| KCP RX | `0685b803-18da-449c-88a2-66c491b17772` | write / write-no-rsp (bleikcp) |
| Datagram | `0685b804-18da-449c-88a2-66c491b17772` | write-no-rsp / notify |

Every `host()` calls `h2_bleikcp_server_open()` with the same UUIDs and three characteristics. At session end, `h2_bleikcp_server_close()` calls `h2_pal_ble_unregister_gatt_service()` to unbind only the link service. For append-only GATT tables, such as ESP NimBLE's limit of 4 services with 3 characteristics each, unregister only unbinds callbacks. Registering an existing service UUID with the same characteristic layout reuses the retained service slot, rebinds callbacks and writes handles back. Repeated hosting therefore always occupies one slot; this is why the service UUID must remain fixed. The bleikcp server adds Datagram to that service through `extra_characteristics`. `tag` is not part of GATT: host advertises session UUID `0221d1f2-9dce-4921-bac4-eeb9XXXXXXXX`, whose last 4 bytes are the tag's FNV-1a hash. Join filters scan results using it. After connection, both sides exchange `HELLO` (version + complete tag) over KCP; a mismatch reports `"mismatch"`. On ESP/BK7258, bleikcp server close only unbinds the link service, leaving Launcher's resident management-service callbacks valid. Fallback to global `h2_pal_ble_unregister_gatt_services()` happens only if the provider returns `H2_PAL_ERR_UNSUPPORTED`; Launcher remains responsible for coexistence with such providers.

**Lua API.**

- `link.host({tag=, timeout_ms=})`: starts advertising and accepts one peer. Omitted or `0` `timeout_ms` advertises until close.
- `link.join({tag=, timeout_ms=})`: scans for and connects to a host with the same tag. Default timeout is 10000 ms, maximum 60000 ms, covering scan, connection and handshake. `tag` is 1..32 bytes; invalid options raise a Lua argument error.
- Reliable messages, `link.send(msg)`: 1..256 bytes, using KCP for ordered, lossless delivery with end-to-end flow control. If the local KCP send buffer cannot fit the complete message, returns `nil, "link: busy"`.
- Unreliable messages, `link.send_unreliable(msg)`: 1..244 bytes, written directly to the Datagram characteristic (notify for host, write-no-rsp for join), with no queue or retry. Exceeding this connection's `max_datagram` (ATT MTU − 3) returns `nil, "link: too large"`; temporary BLE Host inability to send returns `nil, "link: busy"`. Messages are dropped if the receiving event ring is full.
- Byte streams: `link.write(bytes)` returns the byte count accepted in this call. A full send buffer may accept fewer bytes or even `0`; App resumes at that offset. `link.read(max, timeout_ms)` waits within a Lua coroutine and returns 1..`max` bytes, with a maximum of 4096. Timeout returns `""`. After disconnection, it drains remaining bytes before returning `nil, "link: closed"`. Stream bytes use KCP `STREAM` frames, do not preserve write boundaries and share KCP ordering with reliable messages. The receive buffer is 4096 bytes. When unread data fills it, the reader stops, closing the KCP window and causing the peer's `write` to return smaller values.
- When not connected, `send`, `send_unreliable`, `write` and `read` return `nil, "link: not connected"`.
- `link.close()` is idempotent and nonblocking. It discards the session's undelivered events and unread stream bytes; local close generates no further events. `link.state()` returns `"idle"`, `"hosting"`, `"joining"` or `"connected"`, or `"unavailable"` when not enabled.
- Only one session may exist in the process at a time. With an existing session, `host`/`join` return `nil, "link: busy"`. The next `host`/`join` reclaims a finished session; calling immediately after `LINK_DISCONNECTED` or `LINK_ERROR` does not return `busy`. After `close()` or job termination, the session task is still sending `BYE` and releasing BLE, usually within approximately 1 s, or up to one 5 s connect timeout during connection establishment. During that interval `state()` is already `"idle"`, but `host`/`join` still return `busy`; App should retry later.

**Events.** `link.on(kind, fn)` returns a handle; `link.off(handle)` or `runtime.components.off(handle)` unregisters it. Handles share `callback_capacity_per_job` with `runtime.components.on`. `kind` is:

- `runtime.event.LINK_CONNECTED`: `role` (`"host"`/`"join"`) and `max_datagram`.
- `LINK_MESSAGE`: `data` and `reliable` (`true` for reliable messages, `false` for Datagram).
- `LINK_DISCONNECTED`: `reason` (`"peer_closed"` or `"lost"`) and `result`.
- `LINK_ERROR`: `reason` (`"timeout"`, `"not_found"`, `"mismatch"` or `"ble"`) and PAL `result`.

Event tables also contain `event_type`, `sequence` and `timestamp_ms`; `component_id` and `component_kind` are `0`. Callbacks run as scheduler tasks in the same VM, with the same quantum, cancellation and timeout constraints. The next event is delivered only after all callbacks for the previous event finish, preserving reliable-message arrival order. Datagrams may arrive before the peer's `HELLO`; at most 4 are buffered before handshake completion and delivered after `LINK_CONNECTED`. Events with no registered callback are discarded; App should register before `host`/`join`. `LINK_ERROR` or `LINK_DISCONNECTED` ends the session.

After a terminal event is published, `link.read()` still returns stream bytes received before disconnection first, then `nil, "link: closed"` after the buffer is drained, regardless of whether the background session task has returned. Lua callbacks can run before that task finally exits; task exit state cannot substitute for the published terminal session state.

**Connection and protocol.** Join restarts scanning every 1.5 s. The controller's duplicate filter reports each address only once per scan. If host had advertised other content at the same address before `host()`, such as the H2Loader management service, link advertisements would never become visible without restarting. Join connects with a 30 ms interval and 2000 ms supervision timeout. Power loss or moving out of range reports `"lost"` within one supervision timeout. Host may renegotiate connection parameters: App images running the H2Loader BLE command service change every peripheral connection to a 15 ms interval and 4000 ms supervision timeout, making `"lost"` arrive after approximately 4 s. bleikcp uses 244-byte datagrams, a 16-segment window, a 32-frame input queue and 4096-byte TX/RX buffers, with the congestion window disabled. If one window plus retransmissions fills the input queue within one bleikcp worker slice, frames are dropped and retransmitted by peer KCP; the session does not end. KCP frames are `[type u8][len u16 big-endian][payload]`: `HELLO` (both sides send first, validated within 5000 ms), `BYE` (sent on close, job termination or Host stop, with up to 400 ms flushing; the peer immediately reports `"peer_closed"`; delivery is bounded best effort, and failure within the budget makes the peer report `"lost"`), `MESSAGE` (one reliable message) and `STREAM` (up to 512 stream bytes). Oversized or unknown frames terminate with `"lost"` as protocol errors.

**Threads and reclamation.** Each session has a `$lua/link` task to establish the connection. Join's read loop runs on that task; host's read loop runs on the bleikcp server handler. Lock order is job mutex → link mutex. The session task takes only the link mutex, checks that the session is not closed and calls `h2_lua_host_wake_job()` while holding it. Datagram GATT callbacks and system events hold the link mutex only briefly and never block BLE Host. `link.close()`, job transition to a terminal state, `h2_lua_host_stop()` and `h2_lua_job_release()` request closure while holding the job mutex, before slot release or reuse. The session task then sends `BYE`, stops advertising/scanning, closes the server or stream and disconnects. `h2_lua_host_destroy()` joins the session task and releases providers before returning.

## Sources

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L290), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
