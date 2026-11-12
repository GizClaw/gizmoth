// Development-only browser fixture for the serial service. It imitates the
// daemon's snapshot/log contract; nothing here touches real hardware.
import type { LogEntry, RemoteNode, RemoteProbe, SerialJob, SerialPort, SerialSnapshot, SystemPort } from "./serial";

const start = Date.now();
const instanceId = `demo-${start}`;
// OS discovery is independent of GizMoth's mutable configured-port list.
const systemPorts: SystemPort[] = [
  {
    path: "/dev/cu.usbmodem2012101",
    name: "USB JTAG/serial debug unit",
    usb: {
      vid: 0x303a,
      pid: 0x1001,
      serial: "30:ED:A0:AE:0F:84",
      product: "USB JTAG/serial debug unit",
      manufacturer: "Espressif",
    },
  },
  {
    path: "/dev/cu.usbmodem201401",
    name: "USB Quad_Serial",
    usb: { vid: 0x1a86, pid: 0x55e8, serial: "BC4E6F8EABCDD7A9", product: "USB Quad_Serial", manufacturer: "wch.cn" },
  },
  {
    path: "/dev/cu.usbmodem201403",
    name: "USB Quad_Serial",
    usb: { vid: 0x1a86, pid: 0x55e8, serial: "BC4E6F8EABCDD7A9", product: "USB Quad_Serial", manufacturer: "wch.cn" },
  },
  { path: "/dev/cu.debug-console", name: "Serial", usb: null },
  // A CH348 octal adapter exposes eight ports at once, the case batch adding is for.
  ...Array.from({ length: 8 }, (_, index) => ({ path: `/dev/cu.CH34802110000-${index}`, name: "Serial", usb: null })),
];
let version = 1;
let added = 0;
let running = true;
let ownerEpoch = 1;
let observationEpoch = 0;
const logs = new Map<string, LogEntry[]>();
const seqs = new Map<string, number>();
const status = (board: string, chip: string, role: "app" | "loader", ver: string) => ({
  board,
  target: chip,
  chip,
  deviceUid: "30eda0ae0f86",
  capabilities: 7,
  // Real ESP32-S3 Loader value (no reboot-to-App); App role adds that bit.
  commandAvailability: role === "app" ? 4144447 : 4144446,
  active: { role, version: ver, imageChecksum: "d640554971010f73", imageSize: 1609680 },
  runningPartition: 0,
  nextPartition: 1,
  bootIntent: "auto" as const,
  stage: {
    valid: false,
    packageSize: 0,
    imageSize: 0,
    packageChecksum: "",
    imageChecksum: "",
    version: "",
    board: "",
    target: "",
    role: "unknown" as const,
  },
  partition1: {
    valid: true,
    packageSize: 0,
    imageSize: 0,
    packageChecksum: "",
    imageChecksum: "",
    version: "0.1.10-1",
    board,
    target: chip,
    role: "loader" as const,
  },
  partition2: {
    valid: true,
    packageSize: 0,
    imageSize: 0,
    packageChecksum: "",
    imageChecksum: "",
    version: "1.4.2",
    board,
    target: chip,
    role: "app" as const,
  },
  lastResult: 0,
});
const base = {
  mode: "takeover" as const,
  autoGrant: true,
  createdAt: start,
  rxBytes: 0,
  txBytes: 0,
  history: [] as SerialJob[],
  logSeq: 0,
  logGeneration: 0,
};
const emptyLists = typeof location !== "undefined" && new URLSearchParams(location.search).has("empty");
const ports: SerialPort[] = emptyLists
  ? []
  : [
      {
        ...base,
        id: "demo-amoled",
        name: "AMOLED 开发板",
        path: "/dev/cu.usbmodem2012101",
        protocol: "auto",
        baud: 115200,
        enabled: true,
        usb: {
          vid: 0x303a,
          pid: 0x1001,
          serial: "30:ED:A0:AE:0F:84",
          product: "USB JTAG/serial debug unit",
          manufacturer: "Espressif",
        },
        present: true,
        connection: "connected",
        detected: "h2loader",
        activity: "idle",
        connectedAt: start - 8 * 60_000,
        h2loader: status("amoled", "esp32s3", "loader", "0.1.10-1"),
        h2loaderAt: start,
      },
      {
        ...base,
        id: "demo-bk",
        name: "BK7258 V3",
        path: "/dev/cu.usbmodem201401",
        protocol: "h2loader",
        baud: 460800,
        enabled: true,
        usb: {
          vid: 0x1a86,
          pid: 0x55e8,
          serial: "BC4E6F8EABCDD7A9",
          product: "USB Quad_Serial",
          manufacturer: "wch.cn",
        },
        present: true,
        connection: "connected",
        detected: "h2loader",
        activity: "idle",
        connectedAt: start - 3 * 60_000,
        h2loader: status("bk7258_v3_202405", "bk7258", "app", "1.4.2"),
        h2loaderAt: start,
      },
      {
        ...base,
        id: "demo-uart",
        mode: "piping",
        autoGrant: false,
        holder: {
          kind: "client",
          id: 1,
          client: "claude-code",
          title: "排查 Wi-Fi 重连",
          reason: "抓取启动日志，对比重启前后的网络状态",
          since: start - 4 * 60_000,
        },
        queue: [
          { id: 2, client: "codex", title: "串口回归测试", reason: "核对重启前后的输出", since: start - 90_000 },
          { id: 3, client: "cursor-agent", title: "查看固件启动输出", reason: "", since: start - 20_000 },
        ],
        name: "CH348 调试串口",
        path: "/dev/cu.CH34802110000-0",
        protocol: "serial",
        baud: 115200,
        enabled: true,
        present: true,
        connection: "connected",
        detected: "serial",
        activity: "idle",
        connectedAt: start - 60_000,
      },
      {
        ...base,
        id: "demo-off",
        name: "备用板",
        path: "/dev/cu.usbmodem201405",
        protocol: "auto",
        baud: 115200,
        enabled: false,
        present: true,
        connection: "off",
        activity: "idle",
      },
      {
        ...base,
        id: "demo-away",
        mode: "piping",
        autoGrant: false,
        holder: {
          kind: "client",
          id: 4,
          client: "factory-agent",
          title: "验证夹具掉线后重新连接并继续读取设备状态",
          reason: "等待 USB 设备重新接入",
          since: start - 2 * 60_000,
        },
        queue: [{ id: 5, client: "test-agent", title: "复查设备状态", reason: "", since: start - 30_000 }],
        name: "产线夹具 2",
        path: "/dev/cu.usbmodem3101",
        protocol: "h2loader",
        baud: 460800,
        enabled: true,
        present: false,
        connection: "waiting",
        activity: "idle",
      },
    ];

// Other GizMoth nodes (one level): one on the LAN through Tailscale, one offline.
const studioId = "6f0c1d2e-9a41-4f7b-8c3d-1e2f3a4b5c6d";
const labId = "b2e4c6a8-1d3f-4a5b-9c7d-0e1f2a3b4c5d";
const remotePort = (node: string, port: SerialPort): SerialPort => ({
  ...port,
  id: `${node}--${port.id}`,
  node,
  remote: { portId: port.id, stale: false, control: { state: "none" } },
});
const nodes: RemoteNode[] = emptyLists
  ? []
  : [
      {
        id: studioId,
        name: "工作室 Mac mini",
        url: "http://studio-mini.tail3f2a1.ts.net:7420",
        status: "online",
        bootId: `demo-studio-${start}`,
        version: "0.1.12",
        lastSeenAt: start,
        ports: [
          remotePort(studioId, {
            ...base,
            id: "7a1d3c5e-0b2f-4d6a-8e9c-1f3b5d7a9c0e",
            name: "ESP32-S3 DevKit",
            path: "/dev/cu.usbmodem1101",
            protocol: "auto",
            baud: 115200,
            enabled: true,
            present: true,
            connection: "connected",
            detected: "h2loader",
            activity: "idle",
            connectedAt: start - 26 * 60_000,
            h2loader: status("esp32s3_devkit", "esp32s3", "app", "1.3.0"),
            h2loaderAt: start,
          }),
          remotePort(studioId, {
            ...base,
            id: "3c5e7a9b-2d4f-4a6c-8e0b-2d4f6a8c0e1b",
            name: "GizClaw 主板",
            path: "/dev/cu.usbserial-0001",
            protocol: "serial",
            baud: 115200,
            enabled: true,
            present: true,
            connection: "connected",
            detected: "serial",
            activity: "idle",
            mode: "piping",
            holder: {
              kind: "client",
              id: 0,
              client: "codex",
              title: "运行串口回归测试",
              reason: "",
              since: start - 2 * 60_000,
            },
            connectedAt: start - 40 * 60_000,
          }),
          ...Array.from({ length: 6 }, (_, index) =>
            remotePort(studioId, {
              ...base,
              id: `ch348-${index + 1}`,
              name: `CH348 · 端口 ${index + 1}`,
              path: `/dev/cu.CH348-${index + 1}`,
              protocol: "serial",
              baud: 115200,
              enabled: true,
              present: true,
              connection: "connected",
              detected: "serial",
              activity: "idle",
              connectedAt: start - 40 * 60_000,
            }),
          ),
        ],
      },
      {
        id: labId,
        name: "实验室 PC",
        url: "http://192.168.1.42:7420",
        status: "offline",
        error: "无法连接：Connection refused (os error 61)",
        bootId: `demo-lab-${start}`,
        version: "0.1.12",
        lastSeenAt: start - 18 * 60_000,
        ports: [
          {
            ...remotePort(labId, {
              ...base,
              id: "9e1b3d5f-4a6c-4e8a-a0c2-4e6a8c0e2a4c",
              name: "CH348 · 端口 1",
              path: "COM7",
              protocol: "h2loader",
              baud: 460800,
              enabled: true,
              present: true,
              connection: "connected",
              detected: "h2loader",
              activity: "idle",
              h2loader: status("bk7258_v3_202405", "bk7258", "app", "1.4.1"),
            }),
            remote: { portId: "9e1b3d5f-4a6c-4e8a-a0c2-4e6a8c0e2a4c", stale: true, control: { state: "none" } },
          },
        ],
      },
    ];
const allPorts = () => [...ports, ...nodes.flatMap((node) => node.ports)];
const online = (port: SerialPort) => !port.node || nodes.find((node) => node.id === port.node)?.status === "online";

function log(id: string, source: LogEntry["source"], text: string) {
  const seq = (seqs.get(id) ?? 0) + 1;
  seqs.set(id, seq);
  const all = logs.get(id) ?? [];
  all.push({ seq, at: Date.now(), source, text });
  if (all.length > 3000) all.shift();
  logs.set(id, all);
  const port = allPorts().find((p) => p.id === id);
  if (port) {
    port.logSeq = seq;
    if (source === "rx") {
      port.rxBytes = (port.rxBytes ?? 0) + text.length + 1;
      port.lastRxAt = Date.now();
    }
    if (source === "tx") {
      port.txBytes = (port.txBytes ?? 0) + text.length + 1;
      port.lastTxAt = Date.now();
    }
  }
  version++;
}
for (const port of allPorts())
  if (port.connection === "connected")
    log(port.id, "system", `已连接（${port.detected === "h2loader" ? "H2Loader" : `普通串口，${port.baud} baud`}）`);

let tick = 0;
let ticking: ReturnType<typeof setInterval> | undefined;
const simulate = () => {
  tick++;
  for (const port of allPorts()) {
    if (port.connection !== "connected" || !online(port)) continue;
    if (port.name === "ESP32-S3 DevKit" && tick % 4 === 0)
      log(port.id, "rx", `I (${8812004 + tick * 731}) app: heartbeat uptime=${tick}s heap=182340`);
    if (port.id === "demo-amoled" && tick % 3 === 0)
      log(
        port.id,
        "rx",
        `I (${23788411 + tick * 1062}) mfg: H2_MFG_STEP step=done ticket=17f881ef ready=0 error=0 complete=1 code=0`,
      );
    if (port.id === "demo-uart" && tick % 2 === 0)
      log(port.id, "rx", `[${(tick / 2).toFixed(0)}] sensor temp=${(24 + Math.sin(tick / 5)).toFixed(2)}C`);
    const job = port.job;
    if (job && job.phase === "stage") {
      job.acknowledged = Math.min(job.total, job.acknowledged + 96 * 1024);
      port.lastTxAt = Date.now();
      if (job.acknowledged >= job.total) {
        job.phase = "rebootUpgrade";
        port.activity = "rebooting";
        log(port.id, "system", "固件已写入 Stage，重启升级");
      }
      version++;
    } else if (job && job.phase === "rebootUpgrade" && tick % 6 === 0) {
      job.phase = "verify";
      port.activity = "verifying";
      version++;
    } else if (job && job.phase === "verify" && tick % 3 === 0) {
      job.state = "succeeded";
      job.phase = "done";
      job.finishedAt = Date.now();
      port.history = [job, ...(port.history ?? [])].slice(0, 20);
      port.job = null;
      port.activity = "idle";
      if (port.h2loader && job.package)
        port.h2loader.active = { ...port.h2loader.active, role: "app", version: job.package.version };
      log(port.id, "system", "任务完成：安装固件");
    }
  }
};

const gateway = {
  enabled: true,
  scope: "local" as "local" | "lan",
  port: 7420,
  apiKey: "gmk_demo0d9c41f7a2b84e6c9f15e3a7b20c8d4419e2a6f0",
};
const gatewayUrls = () =>
  gateway.enabled
    ? [
        `http://127.0.0.1:${gateway.port}`,
        ...(gateway.scope === "lan"
          ? [`http://192.168.1.59:${gateway.port}`, `http://100.76.124.89:${gateway.port}`]
          : []),
      ]
    : [];
const snapshot = (): SerialSnapshot => ({
  running,
  ownerEpoch,
  observationEpoch: ++observationEpoch,
  instanceId,
  instanceGeneration: running ? 1 : 0,
  version,
  h2loaderCore: true,
  remoteBusy: nodes.some((node) => node.ports.some((port) => port.remote?.control.state !== "none")),
  gateway: { enabled: gateway.enabled, scope: gateway.scope, port: gateway.port },
  pipeBase: `ws://127.0.0.1:${gateway.port}/v1/ports`,
  node: { id: "c41d8e2a-5b3f-4c6d-9e0a-1b2c3d4e5f60", name: "Yan 的 MacBook Pro" },
  // Imported views come from their node's snapshot, which names its own holder.
  nodes: structuredClone(nodes).map((node) => ({
    ...node,
    ports: node.ports.map((port) => ({
      ...port,
      holder: port.holder ?? (port.connection === "connected" ? { kind: "gizmoth" as const } : null),
    })),
  })),
  ports: ports.map((port) => ({
    ...structuredClone(port),
    holder:
      port.mode === "takeover" && port.connection === "connected"
        ? { kind: "gizmoth" as const }
        : port.mode === "piping"
          ? (port.holder ?? null)
          : null,
  })),
});
const find = (id: unknown) => {
  const port = allPorts().find((p) => p.id === id);
  if (!port) throw new Error("串口不存在");
  return port;
};

/** A command on an imported port takes this node's exclusive session there. */
function acquire(port: SerialPort) {
  if (!port.remote) return;
  if (!online(port)) throw new Error("远端 GizMoth 当前未连接");
  if (port.holder?.kind === "client" && port.remote.control.state !== "granted") {
    port.remote.control = { state: "queued", connectionId: 95, position: 1, holder: port.holder.client };
    version++;
    throw new Error(`已在远端排队（第 1 位，当前由 ${port.holder.client} 使用），获得通信权后可继续操作`);
  }
  if (port.remote.control.state !== "granted") {
    port.remote.control = { state: "granted", connectionId: 94 };
    port.mode = "piping";
    port.holder = {
      kind: "client",
      id: 94,
      client: "GizMoth · Yan 的 MacBook Pro",
      title: "通过级联操作串口",
      reason: "通过级联在另一台 GizMoth 上操作此串口",
      since: Date.now(),
    };
    log(port.id, "system", "授予 GizMoth · Yan 的 MacBook Pro WS 独占通信权");
  }
}

/** A lease survives device loss; new grants still wait for a ready physical connection. */
function grantNext(port: SerialPort) {
  if (
    port.autoGrant === false ||
    port.holder?.kind === "client" ||
    port.connection !== "connected" ||
    port.job ||
    port.activity === "handshaking"
  )
    return;
  const [next, ...rest] = port.queue ?? [];
  if (!next) return;
  port.mode = "piping";
  port.holder = { kind: "client", ...next, since: Date.now() };
  port.queue = rest;
  log(port.id, "system", `按顺序授予 ${next.client}`);
}

export function demoSerial(command: string, args: Record<string, unknown>): unknown {
  // Simulated traffic starts with the first serial request, not at import.
  if (typeof window !== "undefined") ticking ??= setInterval(simulate, 500);
  switch (command) {
    case "serial_overview":
      return snapshot();
    case "serial_watch": {
      const since = Number(args.since ?? 0);
      return new Promise((resolve) => {
        const started = Date.now();
        const wait = () =>
          version > since || Date.now() - started > 5000 ? resolve(snapshot()) : setTimeout(wait, 150);
        wait();
      });
    }
    case "serial_system_ports":
      return structuredClone(systemPorts);
    case "serial_add": {
      const input = args.port as {
        path: string;
        name?: string;
        protocol: SerialPort["protocol"];
        baud: number;
        enabled: boolean;
      };
      const port: SerialPort = {
        ...base,
        // Batch adds can land within one millisecond.
        id: `demo-${Date.now()}-${++added}`,
        name: input.name || input.path.split("/").pop()!,
        path: input.path,
        protocol: input.protocol,
        baud: input.baud,
        enabled: input.enabled,
        present: true,
        connection: input.enabled ? "connected" : "off",
        detected: input.enabled ? (input.protocol === "h2loader" ? "h2loader" : "serial") : null,
        activity: "idle",
        connectedAt: Date.now(),
      };
      ports.push(port);
      log(port.id, "system", "已添加到串口列表");
      return structuredClone(port);
    }
    case "serial_update": {
      const port = find(args.id);
      const patch = args.patch as Partial<SerialPort>;
      Object.assign(port, patch);
      if (patch.mode === "takeover") {
        if (patch.autoGrant === undefined) port.autoGrant = false;
        if (port.holder?.kind === "client")
          log(port.id, "system", `${port.holder.client} 的 WS 独占通信已结束：GizMoth 已收回通信权`);
        port.holder = port.connection === "connected" ? { kind: "gizmoth" } : null;
      } else if (patch.mode === "piping" && port.holder?.kind !== "client") port.holder = null;
      // The physical switch does not close a WS holder or remove queued connections.
      if (patch.enabled === false) {
        port.connection = "off";
        port.connectedAt = null;
        port.activity = "idle";
        log(port.id, "system", "串口连接已关闭，WS 通信权和排队连接保留");
      } else if (patch.enabled) {
        port.connection = port.present ? "connected" : "waiting";
        port.connectedAt = port.present ? Date.now() : null;
        if (port.present) port.detected ??= "serial";
        log(port.id, "system", port.present ? "已连接" : "等待设备接入，WS 通信权和排队连接保留");
      }
      grantNext(port);
      version++;
      return structuredClone(port);
    }
    case "serial_remove": {
      const index = ports.findIndex((p) => p.id === args.id);
      if (index >= 0) ports.splice(index, 1);
      version++;
      return { removed: args.id };
    }
    case "serial_logs": {
      const all = logs.get(String(args.id)) ?? [];
      const limit = Number(args.limit ?? 500);
      const after = args.after as number | undefined;
      const entries = after === undefined ? all.slice(-limit) : all.filter((e) => e.seq > after).slice(0, limit);
      return {
        instanceId,
        generation: find(args.id).logGeneration ?? 0,
        entries,
        next: entries.at(-1)?.seq ?? after ?? seqs.get(String(args.id)) ?? 0,
        truncated: false,
      };
    }
    case "serial_clear_logs": {
      const port = find(args.id);
      logs.set(port.id, []);
      port.logGeneration = (port.logGeneration ?? 0) + 1;
      version++;
      return { cleared: true };
    }
    case "serial_send":
      acquire(find(args.id));
      log(String(args.id), "tx", String(args.text));
      setTimeout(() => log(String(args.id), "rx", `> ${String(args.text)}`), 300);
      return { queued: true };
    case "serial_choose_package":
      return "/Users/demo/Downloads/amoled-app-1.5.0.update.tar.zlib";
    case "serial_inspect":
      return {
        board: "amoled",
        target: "esp32s3",
        version: "1.5.0",
        role: "app",
        sha256: "ab12",
        bytes: 2_310_144,
        format: 2,
        name: "amoled-app-1.5.0.update.tar.zlib",
      };
    case "serial_job": {
      const port = find(args.id);
      acquire(port);
      const kind = args.kind as SerialJob["kind"];
      const job: SerialJob = {
        id: `job-${Date.now()}`,
        kind,
        state: "running",
        phase: kind === "install" ? "stage" : "verify",
        acknowledged: 0,
        total: kind === "install" ? 2_310_144 : 0,
        source: "app",
        createdAt: Date.now(),
        package: kind === "install" ? (demoSerial("serial_inspect", {}) as SerialJob["package"]) : null,
      };
      port.job = job;
      port.activity = kind === "install" ? "transferring" : "verifying";
      log(port.id, "system", `开始任务：${kind}`);
      return structuredClone(job);
    }
    case "serial_answer_request": {
      const port = find(args.id);
      const queue = port.queue ?? [];
      const request = queue.find((r) => r.id === args.request);
      if (!request) throw new Error("请求不存在或已结束");
      if (args.accept) {
        if (port.connection !== "connected" || port.activity === "handshaking") throw new Error("串口未连接");
        if (port.job) throw new Error("此串口正在执行任务");
      }
      port.queue = queue.filter((r) => r !== request);
      if (args.accept) {
        if (port.holder?.kind === "client")
          log(port.id, "system", `${port.holder.client} 的占据已结束：GizMoth 将串口交给了 ${request.client}`);
        port.mode = "piping";
        port.holder = { kind: "client", ...request, since: Date.now() };
        log(port.id, "system", `已接受 ${request.client} 的请求`);
      } else log(port.id, "system", `已拒绝 ${request.client} 的请求`);
      version++;
      return structuredClone(port);
    }
    case "serial_release": {
      const port = find(args.id);
      if (port.remote) {
        if (port.remote.control.connectionId !== args.connection) throw new Error("连接不存在或已结束");
        const held = port.remote.control.state === "granted";
        port.remote.control = { state: "none" };
        if (held) {
          port.mode = "takeover";
          port.holder = { kind: "gizmoth" };
        }
        log(port.id, "system", "Yan 的 MacBook Pro 已释放串口");
        return structuredClone(port);
      }
      const holder = port.holder?.kind === "client" && port.holder.id === args.connection ? port.holder : null;
      const request = port.queue?.find((item) => item.id === args.connection);
      // Validate before changing policy: a stale target must not pause or evict its replacement.
      if (!holder && !request) throw new Error("连接不存在或已结束");
      if (args.pauseAutomatic === true) port.autoGrant = false;
      if (holder) {
        log(port.id, "system", `${holder.client} 的占据已结束：GizMoth 已断开此 WS`);
        port.holder = null;
        port.mode = "takeover";
      } else {
        port.queue = port.queue?.filter((item) => item.id !== args.connection);
        log(port.id, "system", `已断开 ${request!.client} 的等待 WS`);
      }
      grantNext(port);
      version++;
      return structuredClone(port);
    }
    case "serial_cancel_job":
      for (const port of allPorts()) {
        const job = port.job;
        if (job && job.id === args.jobId) {
          port.history = [{ ...job, state: "cancelled" as const, finishedAt: Date.now() }, ...(port.history ?? [])];
          port.job = null;
          port.activity = "idle";
          log(port.id, "system", "任务已取消");
        }
      }
      return { cancelling: args.jobId };
    case "serial_stop_service":
      if (running) ownerEpoch++;
      running = false;
      version++;
      setTimeout(() => {
        if (!running) {
          running = true;
          ownerEpoch++;
        }
      }, 4000);
      return null;
    case "serial_export_logs":
      return null;
    case "serial_remote_probe": {
      const url = String(args.url ?? "").trim();
      if (!url) throw new Error("请输入远端 GizMoth 的地址");
      if (String(args.apiKey ?? "").length < 8) throw new Error("API Key 无效，或远端未向局域网开放服务");
      const host = url.replace(/^\w+:\/\//, "").replace(/[:/].*$/, "");
      const normalized = `http://${host}:${url.match(/:(\d+)\s*$/)?.[1] ?? 7420}`;
      return {
        url: normalized,
        node: { id: "d7e9f1a3-5c6e-4b8d-a0f2-c4e6a8b0d2f4", name: "客厅 Mac mini", version: "0.1.12" },
        ports: 1,
      } satisfies RemoteProbe;
    }
    case "serial_remote_add": {
      const probe = demoSerial("serial_remote_probe", args) as RemoteProbe;
      if (!probe.node) throw new Error("远端未响应");
      if (nodes.some((node) => node.id === probe.node!.id)) throw new Error("这台 GizMoth 已在列表中");
      const id = probe.node.id;
      nodes.push({
        id,
        name: probe.node.name,
        url: probe.url,
        status: "online",
        bootId: `demo-${id}`,
        version: probe.node.version,
        lastSeenAt: Date.now(),
        ports: [
          remotePort(id, {
            ...base,
            id: "0d2f4a6c-8e0b-4d2f-a4c6-8e0a2c4e6a8c",
            name: "电视盒子调试口",
            path: "/dev/cu.usbserial-1420",
            protocol: "serial",
            baud: 115200,
            enabled: true,
            present: true,
            connection: "connected",
            detected: "serial",
            activity: "idle",
            connectedAt: Date.now(),
          }),
        ],
      });
      version++;
      return { id, name: probe.node.name, url: probe.url };
    }
    case "serial_remote_remove": {
      const index = nodes.findIndex((node) => node.id === args.id);
      if (index < 0) throw new Error("远端 GizMoth 不存在");
      nodes.splice(index, 1);
      version++;
      return { removed: args.id };
    }
    case "gateway_status":
      return {
        ...gateway,
        listening: gateway.enabled,
        boundPort: gateway.port,
        urls: gatewayUrls(),
        node: { id: "c41d8e2a-5b3f-4c6d-9e0a-1b2c3d4e5f60", name: "Yan 的 MacBook Pro" },
      };
    case "gateway_update": {
      const patch = args.patch as {
        enabled?: boolean;
        scope?: "local" | "lan";
        port?: number;
        regenerateKey?: boolean;
      };
      if (patch.enabled !== undefined) gateway.enabled = patch.enabled;
      if (patch.scope) gateway.scope = patch.scope;
      if (patch.port) gateway.port = patch.port;
      if (patch.regenerateKey) gateway.apiKey = `gmk_demo${Math.random().toString(16).slice(2)}`;
      version++;
      return demoSerial("gateway_status", {});
    }
    default:
      throw new Error(`demo: ${command} is not simulated`);
  }
}
