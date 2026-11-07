# BLE peer links

Host/join, reliable and unreliable messages, streams and launcher-dependent link support.

### BLE peer link

`link` 让两台相邻设备通过 BLE 配对，在同一条连接上提供三种传输：可靠消息、不可靠
消息和字节流。它只使用 BLE Host、Task、Sync、Time、Mem 和 System Event PAL，不
使用 Wi-Fi、Netif 或任何网络 API，产品在对局中关闭 Wi-Fi 不影响链路。

**启用。** `//libs/lua:lua_link` 是独立 target，不使用 link 的 image 不链接 BLE
iKCP。Launcher 在 `h2_lua_host_start()` 前调用
`h2_lua_link_enable(host, &(h2_lua_link_config_t){adv_type, scan_type})`，按板级
BLE stack 选择 legacy 或 extended advertising/scan。Extended 广播对 legacy scanner
不可见，所以需要互通的设备必须选同一种 advertising 类型；link 广播只有 21 字节，
legacy 可以放下。ESP 板级 BLE 需要开启 `CONFIG_BT_NIMBLE_EXT_ADV`：host 使用 advertising set
API，NimBLE 在该选项关闭时不提供它，`host()` 会以 `LINK_ERROR "ble"` 结束。Runtime 没有 `ble_host`、
没有 `system_event`、或接入的是 canonical unsupported object 时返回
`H2_PAL_ERR_UNSUPPORTED`，link 保持不可用；start 之后或重复调用返回
`H2_PAL_ERR_INVALID_STATE`。Launcher 负责启动 BLE Host，并保证它存活到
`h2_lua_host_destroy()` 返回。

**GATT profile。** 所有 link 共用一个固定 service，UUID 是库内常量，不可配置：

| 用途 | UUID | 属性 |
| --- | --- | --- |
| Service | `0685b801-18da-449c-88a2-66c491b17772` | primary |
| KCP TX | `0685b802-18da-449c-88a2-66c491b17772` | notify（bleikcp） |
| KCP RX | `0685b803-18da-449c-88a2-66c491b17772` | write / write-no-rsp（bleikcp） |
| Datagram | `0685b804-18da-449c-88a2-66c491b17772` | write-no-rsp / notify |

每次 `host()` 都用同一组 UUID 和同样三个 characteristic 调用
`h2_bleikcp_server_open()`，session 结束时 `h2_bleikcp_server_close()` 调用
`h2_pal_ble_unregister_gatt_service()`，只解绑 link service。对只增不减的 GATT table（ESP NimBLE 最多
4 个 service、每个 3 个 characteristic，unregister 只解绑回调），再次注册已存在的
service UUID 且 characteristic 布局相同时，backend 复用保留的 service slot，重新绑定
回调并写回 handle，因此连续多次 host 始终只占一个 slot；这正是 service UUID 必须固定
的原因。bleikcp server 通过 `extra_characteristics` 把 Datagram 放进同一个 service。`tag` 不进入 GATT：host 广播 session UUID
`0221d1f2-9dce-4921-bac4-eeb9XXXXXXXX`，末 4 字节为 tag 的 FNV-1a hash，join 按它
过滤扫描结果；连接后双方在 KCP 上交换 `HELLO`（版本 + 完整 tag），不符报
`"mismatch"`。ESP/BK7258 上 bleikcp server close 只解绑 link service，
launcher 常驻管理 service 的回调保持有效。只有 provider 返回 `H2_PAL_ERR_UNSUPPORTED`
时才回退到全局 `h2_pal_ble_unregister_gatt_services()`；这类 provider 的共存仍由 launcher 负责。

**Lua API。**

- `link.host({tag=, timeout_ms=})`：开始广播并接受一个 peer；`timeout_ms` 省略或
  为 `0` 时一直广播到 close。
- `link.join({tag=, timeout_ms=})`：扫描并连接同一 tag 的 host；默认 10000 ms，
  上限 60000 ms，覆盖 scan、connect 和握手。`tag` 为 1..32 字节，选项非法时抛出
  Lua argument error。
- 可靠消息 `link.send(msg)`：1..256 字节，走 KCP，有序、不丢，端到端流控；本地
  KCP 发送缓冲放不下整条消息返回 `nil, "link: busy"`。
- 不可靠消息 `link.send_unreliable(msg)`：1..244 字节，直接写 Datagram
  characteristic（host 用 notify，join 用 write-no-rsp），不排队、不重试；超过本连接
  `max_datagram`（ATT MTU − 3）返回 `nil, "link: too large"`，BLE Host 暂时发不出
  返回 `nil, "link: busy"`。接收端事件环满时丢弃。
- 字节流 `link.write(bytes)` 返回本次接收的字节数，发送缓冲满时可能小于长度甚至为
  `0`，App 从该偏移续写；`link.read(max, timeout_ms)` 在 Lua 协程内等待，返回
  1..`max`（上限 4096）字节，超时返回 `""`，断开后先读完剩余字节再返回
  `nil, "link: closed"`。流字节在 KCP 上以 `STREAM` 帧承载，不保留写入边界，与可靠
  消息共享 KCP 顺序。接收缓冲 4096 字节，未读满时 reader 停止，KCP 窗口随之关闭，
  对端 `write` 返回较小值。
- 未连接时 `send`、`send_unreliable`、`write`、`read` 返回
  `nil, "link: not connected"`。
- `link.close()` 幂等、不阻塞；丢弃该 session 未投递的事件和未读的流字节，本地关闭
  不再产生事件。`link.state()` 返回 `"idle"`、`"hosting"`、`"joining"`、
  `"connected"`，未启用时为 `"unavailable"`。
- 进程内同一时间只有一个 session；已有 session 时 `host`/`join` 返回
  `nil, "link: busy"`，已结束的 session 由下一次 `host`/`join` 回收；收到 `LINK_DISCONNECTED` 或
`LINK_ERROR` 之后立即调用不会返回 `busy`。`close()` 或 job 结束之后
  session task 仍在发送 `BYE` 和释放 BLE（通常不超过约 1 s，连接建立中最长为一次
  connect 超时 5 s），期间 `state()` 已为 `"idle"`，但 `host`/`join` 仍返回 `busy`，
  App 应稍后重试。

**事件。** `link.on(kind, fn)` 返回 handle，`link.off(handle)` 或
`runtime.components.off(handle)` 注销；handle 与 `runtime.components.on` 共用
`callback_capacity_per_job`。`kind` 为：

- `runtime.event.LINK_CONNECTED`：`role`（`"host"`/`"join"`）和 `max_datagram`。
- `LINK_MESSAGE`：`data` 和 `reliable`（可靠消息为 `true`，Datagram 为 `false`）。
- `LINK_DISCONNECTED`：`reason` 为 `"peer_closed"` 或 `"lost"`，以及 `result`。
- `LINK_ERROR`：`reason` 为 `"timeout"`、`"not_found"`、`"mismatch"` 或 `"ble"`，
  以及 PAL `result`。

事件表同时带 `event_type`、`sequence`、`timestamp_ms`，`component_id` 与
`component_kind` 为 `0`。回调作为同一 VM 的 scheduler task 运行，受相同 quantum、
取消和超时约束；上一个事件的回调全部结束后才投递下一个，可靠消息因此保持到达顺序。
Datagram 可能先于对端 `HELLO` 到达，握手完成前最多暂存 4 条，排在 `LINK_CONNECTED`
之后投递。没有注册回调的事件被丢弃，App 应在 `host`/`join` 前注册。
`LINK_ERROR` 或 `LINK_DISCONNECTED` 之后 session 结束。

终态事件发布后，`link.read()` 仍先返回断开前已接收的流字节；缓冲耗尽后返回 `nil, "link: closed"`，不依赖后台 session task 是否已经返回。Lua 回调可能先于该 task 的最终退出运行，因此不能用 task 退出状态代替已发布的 session 终态。

**连接与协议。** Join 扫描时每 1.5 s 重启一次 scan：controller 的 duplicate filter
在一次 scan 内对同一地址只上报一次，host 在开始 `host()` 之前已经用同一地址广播其他
内容（例如 H2Loader 管理服务）时，不重启就永远看不到 link 广播。Join 以 30 ms
interval、2000 ms supervision timeout 连接；掉电或离开范围在一个 supervision timeout
内报告 `"lost"`。Host 可以重新协商连接参数：运行 H2Loader BLE 命令服务的 App image
会把每个 peripheral 连接改为 15 ms interval、4000 ms supervision timeout，此时
`"lost"` 约 4 s 后到达。bleikcp 使用 244-byte datagram、16-segment
window、32 帧输入队列和 4096-byte TX/RX buffer，关闭 congestion window。输入队列在 bleikcp worker 一个 slice 内被一个 window 加其重传塞满时只丢帧、由对端 KCP 重传，不会结束 session。KCP 上的帧
为 `[type u8][len u16 big-endian][payload]`：`HELLO`（双方先发，5000 ms 内校验）、
`BYE`（close、job 结束或 Host stop 时发送并最多 flush 400 ms，对端立即报告
`"peer_closed"`；BYE 是有界的尽力而为，预算内未送达时对端报告 `"lost"`）、`MESSAGE`（一条可靠消息）和 `STREAM`（最多 512 字节流数据）。
超长或未知帧按协议错误结束为 `"lost"`。

**线程与回收。** 每个 session 一个 `$lua/link` task 负责建立连接；join 的读循环在该
task 上，host 的读循环在 bleikcp server handler 上。锁顺序为 job mutex → link
mutex，session task 只取 link mutex，并在该 mutex 内、确认未关闭后用
`h2_lua_host_wake_job()` 唤醒 job；Datagram 的 GATT 回调和 system event 只短暂持有
link mutex，从不阻塞 BLE Host。`link.close()`、job 进入终态、`h2_lua_host_stop()`
和 `h2_lua_job_release()` 在持有 job mutex、槽位释放或复用之前请求关闭；session
task 随后发送 `BYE`，停止广播/扫描，关闭 server 或 stream 并断开连接。
`h2_lua_host_destroy()` join session task 并释放 provider 后才返回。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L290), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
