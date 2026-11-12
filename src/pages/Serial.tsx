import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from "react";
import {
  ArrowDown,
  ArrowsClockwise,
  Bluetooth,
  CaretRight,
  Clock,
  Cpu,
  Desktop,
  DownloadSimple,
  Eraser,
  Globe,
  Laptop,
  MagnifyingGlass,
  PaperPlaneRight,
  Plus,
  PlugsConnected,
  SpinnerGap,
  UploadSimple,
  Usb,
} from "@phosphor-icons/react";
import { t, useI18n } from "../i18n";
import { errorMessage, native } from "../bridge";
import { Button, Dialog, Dot, Empty, IconButton, Input, PageHeader, Select } from "../ui";
import {
  formatBytes,
  serial,
  useSerialLogs,
  useSerialSnapshot,
  type LogEntry,
  type PackageInfo,
  type SerialJob,
  type SerialJobKind,
  type SerialPort,
  type SerialProtocol,
  type SerialSnapshot,
  type H2LoaderStatus,
  deviceCommands,
  findSerialPort,
  jobCommands,
  type RemoteNode,
  type RemoteProbe,
  type SystemPort,
} from "../serial";
import "./Serial.css";

const BAUDS = [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1500000, 2000000];

const protocolLabel = (protocol: SerialProtocol | "h2loader" | "serial") =>
  t(protocol === "auto" ? "自动识别" : protocol === "h2loader" ? "H2Loader" : "普通串口");
const jobLabel = (kind: SerialJobKind) =>
  t(
    {
      refresh: "读取状态",
      stage: "发送固件",
      install: "安装固件",
      rebootApp: "重启到 App",
      rebootLoader: "重启到 Loader",
      rebootUpgrade: "重启升级",
    }[kind],
  );
const phaseLabel = (phase: string) =>
  t(
    {
      queued: "排队中",
      stage: "传输固件",
      rebootUpgrade: "重启升级",
      rebootApp: "重启到 App",
      rebootLoader: "重启到 Loader",
      verify: "核验结果",
      status: "读取状态",
      done: "完成",
    }[phase] ?? phase,
  );
const activityLabel: Record<SerialPort["activity"], string> = {
  idle: "空闲",
  handshaking: "握手中",
  sending: "发送数据",
  transferring: "传输固件",
  rebooting: "等待重启",
  verifying: "核验中",
  reconnecting: "重新连接",
};

function useNow(interval = 500) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), interval);
    return () => clearInterval(timer);
  }, [interval]);
  return now;
}

/** What the wire is doing right now, for status text and the flow animation. */
type Flow = "off" | "idle" | "in" | "out" | "both" | "wait" | "bad";
function flowOf(port: SerialPort, now: number): Flow {
  // An imported port whose node is disconnected shows no live traffic.
  if (port.remote?.stale) return "off";
  if (port.connection === "error" || port.connection === "occupied") return "bad";
  if (port.connection === "connecting" || port.connection === "waiting") return "wait";
  if (port.connection !== "connected") return "off";
  if (port.activity === "transferring" || port.activity === "sending") return "out";
  if (port.activity === "rebooting" || port.activity === "verifying" || port.activity === "handshaking") return "wait";
  const rx = port.lastRxAt && now - port.lastRxAt < 1500;
  const tx = port.lastTxAt && now - port.lastTxAt < 1500;
  return rx && tx ? "both" : rx ? "in" : tx ? "out" : "idle";
}
function tone(port: SerialPort): "ok" | "off" | "warn" | "idle" {
  if (port.remote?.stale) return "off";
  if (port.connection === "connected") return "ok";
  if (port.connection === "error" || port.connection === "occupied") return "warn";
  if (port.connection === "off") return "off";
  return "idle";
}
function connectionText(port: SerialPort) {
  return t(
    {
      off: "未连接",
      waiting: "等待设备接入",
      connecting: port.activity === "handshaking" ? "识别协议中" : "连接中",
      connected: "已连接",
      occupied: "被其他程序占用",
      error: "连接异常",
    }[port.connection],
  );
}
function percent(job: SerialJob) {
  return job.total ? Math.min(100, Math.round((job.acknowledged / job.total) * 100)) : null;
}
function statusText(port: SerialPort, now: number) {
  if (port.remote?.stale) return t("状态待确认");
  const job = port.job;
  if (job) {
    const pct = job.phase === "stage" ? percent(job) : null;
    return pct === null ? `${jobLabel(job.kind)} · ${phaseLabel(job.phase)}` : `${jobLabel(job.kind)} · ${pct}%`;
  }
  if (port.connection !== "connected") return connectionText(port);
  if (port.remote?.control.state === "granted") return t("本机控制中");
  if (port.mode === "piping") return port.holder?.kind === "client" ? t("已连接 · WS 独占") : t("WS 独占 · 等待连接");
  const flow = flowOf(port, now);
  if (port.activity !== "idle") return t(activityLabel[port.activity]);
  if (flow === "in" || flow === "both") return t("接收数据");
  if (flow === "out") return t("发送数据");
  return t("已连接 · 空闲");
}
/** Older daemons supplied only a reason; keep their active sessions readable during migration. */
function taskTitle(session: { title?: string; reason: string }) {
  return session.title?.trim() || session.reason.trim() || t("未提供任务标题");
}
function deviceLine(port: SerialPort) {
  const s = port.h2loader;
  if (!s) return port.detected === "serial" ? `${port.baud} baud` : "";
  const role = s.active.role === "loader" ? "Loader" : s.active.role === "app" ? "App" : "";
  return [s.board, s.chip || s.target, [role, s.active.version].filter(Boolean).join(" ")].filter(Boolean).join(" · ");
}
/** `http://host:7420` -> `host:7420`, as the user typed it. */
function hostLabel(url: string) {
  return url.replace(/^\w+:\/\//, "");
}
function nodeTone(node: RemoteNode): "ok" | "off" | "warn" | "idle" {
  return node.status === "online" ? "ok" : node.status === "connecting" ? "idle" : "warn";
}
function nodeFlow(node: RemoteNode): Flow {
  return node.status === "online"
    ? "idle"
    : node.status === "connecting"
      ? "wait"
      : node.status === "error"
        ? "bad"
        : "off";
}
function nodeStatusText(node: RemoteNode, now: number) {
  if (node.status === "online") return t("在线 · {count} 个串口", { count: node.ports.length });
  if (node.status === "connecting") return t("连接中");
  if (node.status === "error") return t("需要处理");
  return node.lastSeenAt ? t("离线 · {time}前在线", { time: since(node.lastSeenAt, now) }) : t("离线");
}
function PortIcon({ port, size = 18 }: { port: SerialPort; size?: number }) {
  return port.detected === "h2loader" ? <Cpu size={size} /> : <Usb size={size} />;
}

function ServiceLine({ snapshot, onService }: { snapshot: SerialSnapshot; onService: () => void }) {
  useI18n();
  const gateway = snapshot.gateway;
  return (
    <div className="serial-service">
      <Dot tone={snapshot.running ? "ok" : "off"} />
      <span>{snapshot.running ? t("串口服务运行中") : t("串口服务未运行，打开任一串口时启动")}</span>
      {!snapshot.h2loaderCore && <span className="serial-service-warn">{t("此版本未包含 H2Loader 内核")}</span>}
      {snapshot.running && gateway && (
        <button type="button" className="text-action" onClick={onService}>
          {gateway.error
            ? t("服务异常")
            : gateway.enabled
              ? t("服务 {scope} · {port}", {
                  scope: gateway.scope === "lan" ? t("局域网") : t("本机"),
                  port: gateway.port,
                })
              : t("服务已关闭")}
        </button>
      )}
    </div>
  );
}

function EnableSwitch({ port, onError }: { port: SerialPort; onError: (message: string) => void }) {
  const [busy, setBusy] = useState(false);
  return (
    <input
      type="checkbox"
      role="switch"
      className="switch"
      aria-label={t("连接 {name}", { name: port.name })}
      title={port.enabled ? t("断开串口连接，保留 WS 通信权和排队连接") : t("连接串口")}
      checked={port.enabled}
      disabled={busy}
      onClick={(event) => event.stopPropagation()}
      onChange={(event) => {
        const enabled = event.target.checked;
        setBusy(true);
        void serial
          .update(port.id, { enabled })
          .catch((e) => onError(errorMessage(e)))
          .finally(() => setBusy(false));
      }}
    />
  );
}

function PortRow({
  port,
  now,
  onOpen,
  onError,
}: {
  port: SerialPort;
  now: number;
  onOpen: (id: string) => void;
  onError: (message: string) => void;
}) {
  const flow = flowOf(port, now);
  const pct = port.job ? percent(port.job) : null;
  return (
    <li className={port.remote?.stale ? "stale" : undefined}>
      <button type="button" className="serial-row" onClick={() => onOpen(port.id)}>
        <span className={`serial-row-icon flow-${flow}`}>
          <PortIcon port={port} />
        </span>
        <span className="serial-row-copy">
          <strong title={port.name}>{port.name}</strong>
          <span title={port.path}>
            {[port.path, port.detected ? protocolLabel(port.detected) : protocolLabel(port.protocol), deviceLine(port)]
              .filter(Boolean)
              .join(" · ")}
          </span>
          {port.holder?.kind === "client" && (
            <span className="serial-row-owner">
              <span className="serial-row-owner-client" title={port.holder.client}>
                {t("WS · {client}", { client: port.holder.client })}
              </span>
              <span className="serial-task-title">{taskTitle(port.holder)}</span>
            </span>
          )}
        </span>
        <span className="serial-row-status">
          <Dot tone={tone(port)} />
          <span>{statusText(port, now)}</span>
          {port.job && pct !== null && port.job.phase === "stage" && (
            <span className="serial-row-progress" aria-hidden="true">
              <i style={{ width: `${pct}%` }} />
            </span>
          )}
        </span>
      </button>
      {/* An imported port is switched on and off by the GizMoth it belongs to. */}
      {port.remote ? (
        <span className="serial-row-slot" aria-hidden="true" />
      ) : (
        <EnableSwitch port={port} onError={onError} />
      )}
      <button
        type="button"
        className="serial-row-open"
        aria-label={t("打开 {name}", { name: port.name })}
        onClick={() => onOpen(port.id)}
      >
        <CaretRight size={14} />
      </button>
    </li>
  );
}

/** One imported GizMoth: its identity and state above its ports. */
function NodeGroup({
  node,
  now,
  onOpen,
  onError,
  onRemove,
}: {
  node: RemoteNode;
  now: number;
  onOpen: (id: string) => void;
  onError: (message: string) => void;
  onRemove: () => void;
}) {
  useI18n();
  return (
    <section className="serial-group" id={`serial-node-${node.id}`} aria-label={node.name}>
      <header className="serial-group-head">
        <Desktop size={16} aria-hidden="true" />
        <strong title={node.name}>{node.name}</strong>
        <span className="serial-group-state">
          <Dot tone={nodeTone(node)} />
          <span>{nodeStatusText(node, now)}</span>
        </span>
        <span className="serial-group-url" title={node.url}>
          {hostLabel(node.url)}
        </span>
        <button type="button" className="text-action danger-text" onClick={onRemove}>
          {t("移除")}
        </button>
      </header>
      {node.status !== "online" && node.error && (
        <p className={node.status === "error" ? "error serial-group-note" : "serial-group-note"}>
          {node.status === "offline" ? t("正在重连：{error}", { error: t(node.error) }) : t(node.error)}
        </p>
      )}
      {node.ports.length ? (
        <ul className="card serial-list" aria-label={t("{name} 的串口", { name: node.name })}>
          {node.ports.map((port) => (
            <PortRow key={port.id} port={port} now={now} onOpen={onOpen} onError={onError} />
          ))}
        </ul>
      ) : (
        <p className="card serial-group-empty">
          {node.status === "online" ? t("这台 GizMoth 还没有添加串口。") : t("连接后会显示它的串口。")}
        </p>
      )}
    </section>
  );
}

export function SerialPage({ onOpen, onService }: { onOpen: (id: string) => void; onService: () => void }) {
  useI18n();
  const { snapshot, error: loadError } = useSerialSnapshot();
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<RemoteNode | null>(null);
  const [error, setError] = useState("");
  const now = useNow();
  const ports = snapshot?.ports ?? [];
  const nodes = snapshot?.nodes ?? [];
  const all = [...ports, ...nodes.flatMap((node) => node.ports)];
  const any = ports.length > 0 || nodes.length > 0;
  return (
    <>
      <PageHeader crumbs={[{ label: t("设备") }, { label: t("串口") }]} />
      <div className="page list-page serial-page">
        <div className="page-description">
          <span>
            {t(
              "这台电脑上的串口由一个常驻服务统一连接，界面和通过服务接入的 agent 共用它的状态与日志。也可以接入局域网或 Tailscale 上另一台 GizMoth 的串口。",
            )}
          </span>
        </div>
        {any && <Topology ports={ports} nodes={nodes} selected="" onSelect={onOpen} now={now} />}
        {any && snapshot && (
          <div className="page-list-toolbar">
            <button type="button" className="text-action" onClick={() => setAdding(true)}>
              <Plus size={14} aria-hidden="true" />
              {t("添加串口")}
            </button>
            <ServiceLine snapshot={snapshot} onService={onService} />
            <span className="page-list-summary">
              {t("{connected}/{count} 个已连接", {
                connected: all.filter((p) => p.connection === "connected" && !p.remote?.stale).length,
                count: all.length,
              })}
            </span>
          </div>
        )}
        {(error || loadError) && (
          <p role="alert" className="error">
            {t(error || loadError)}
          </p>
        )}
        {any ? (
          <>
            {ports.length > 0 && (
              <section className="serial-group" aria-label={t("这台电脑")}>
                {nodes.length > 0 && (
                  <header className="serial-group-head">
                    <Laptop size={16} aria-hidden="true" />
                    <strong>{t("这台电脑")}</strong>
                    {snapshot?.node && <span className="serial-group-url">{snapshot.node.name}</span>}
                  </header>
                )}
                <ul className="card serial-list" aria-label={t("串口列表")}>
                  {ports.map((port) => (
                    <PortRow key={port.id} port={port} now={now} onOpen={onOpen} onError={setError} />
                  ))}
                </ul>
              </section>
            )}
            {nodes.map((node) => (
              <NodeGroup
                key={node.id}
                node={node}
                now={now}
                onOpen={onOpen}
                onError={setError}
                onRemove={() => setRemoving(node)}
              />
            ))}
          </>
        ) : snapshot ? (
          <Empty
            icon={<PlugsConnected size={26} />}
            title={t("添加你的第一个串口")}
            body={t(
              "选择本机串口，或接入另一台 GizMoth 上的串口。本机串口打开开关后由串口服务连接，并自动识别 H2Loader 设备。",
            )}
            action={
              <Button variant="primary" controlSize="lg" onClick={() => setAdding(true)}>
                <Plus size={16} aria-hidden="true" />
                {t("添加串口")}
              </Button>
            }
          />
        ) : native ? (
          <p className="notice" role="status">
            {t("正在读取串口…")}
          </p>
        ) : null}
      </div>
      {adding && (
        <AddSerialDialog
          existing={ports}
          onClose={() => setAdding(false)}
          onAdded={(added) => {
            setAdding(false);
            // One port goes straight to its page; a batch stays on the list where all of them show.
            if (added.length === 1) onOpen(added[0].id);
          }}
        />
      )}
      {removing && (
        <RemoveNodeDialog
          node={removing}
          onClose={() => setRemoving(null)}
          onError={(message) => {
            setRemoving(null);
            setError(message);
          }}
        />
      )}
    </>
  );
}

function RemoveNodeDialog({
  node,
  onClose,
  onError,
}: {
  node: RemoteNode;
  onClose: () => void;
  onError: (message: string) => void;
}) {
  useI18n();
  const [busy, setBusy] = useState(false);
  return (
    <Dialog title={t("移除远端 GizMoth")} busy={busy} onClose={onClose}>
      <p className="dialog-text">
        {t("不再显示「{name}」上的串口，并结束本机在那里的通信会话。对方的串口、日志和设置不受影响。", {
          name: node.name,
        })}
      </p>
      <div className="dialog-actions">
        <Button disabled={busy} onClick={onClose}>
          {t("取消")}
        </Button>
        <Button
          variant="danger"
          disabled={busy}
          onClick={() => {
            setBusy(true);
            void serial
              .remoteRemove(node.id)
              .then(onClose)
              .catch((e) => onError(errorMessage(e)));
          }}
        >
          {t("移除")}
        </Button>
      </div>
    </Dialog>
  );
}

/** The name a port gets when the user leaves its name field empty. */
function defaultPortName(port: SystemPort) {
  const tail = port.path.split(/[\\/]/).pop() ?? "";
  return port.usb?.product ? `${port.usb.product} · ${tail.replace(/^cu\./, "")}` : tail;
}
function systemPortMeta(port: SystemPort) {
  return [
    port.path,
    port.usb?.manufacturer,
    port.usb && `${port.usb.vid.toString(16).padStart(4, "0")}:${port.usb.pid.toString(16).padStart(4, "0")}`,
  ]
    .filter(Boolean)
    .join(" · ");
}

interface AddFailure {
  path: string;
  name: string;
  message: string;
}

/** Publishes a scroller's classic scrollbar width (0 for overlay scrollbars) as `--serial-add-scrollbar`. */
function useScrollbarWidth(scroller: RefObject<HTMLElement | null>, active: boolean) {
  useLayoutEffect(() => {
    const element = scroller.current;
    if (!active || !element) return;
    const update = () =>
      element.style.setProperty("--serial-add-scrollbar", `${element.offsetWidth - element.clientWidth}px`);
    update();
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => observer.disconnect();
  }, [scroller, active]);
}

function AddSerialDialog({
  existing,
  onClose,
  onAdded,
}: {
  existing: SerialPort[];
  onClose: () => void;
  onAdded: (ports: SerialPort[]) => void;
}) {
  useI18n();
  const [source, setSource] = useState<"local" | "remote" | null>(null);
  const [ports, setPorts] = useState<SystemPort[] | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => new Set());
  const [names, setNames] = useState<Record<string, string>>({});
  const [protocol, setProtocol] = useState<SerialProtocol>("auto");
  const [baud, setBaud] = useState(115200);
  const [connect, setConnect] = useState(true);
  const [progress, setProgress] = useState<{ current: number; total: number } | null>(null);
  // Paths added by an earlier, partly failed batch, before the snapshot catches up.
  const [added, setAdded] = useState<ReadonlySet<string>>(() => new Set());
  const [error, setError] = useState("");
  const [failures, setFailures] = useState<{ total: number; items: AddFailure[] } | null>(null);
  const anchor = useRef<string | null>(null);
  const extendRange = useRef(false);
  const list = useRef<HTMLUListElement>(null);
  useScrollbarWidth(list, source === "local");
  const busy = progress !== null;
  const scan = () => {
    setError("");
    setPorts(null);
    void serial
      .systemPorts()
      .then(setPorts)
      .catch((e) => {
        setError(errorMessage(e));
        setPorts([]);
      });
  };
  useEffect(() => {
    if (source === "local") scan();
  }, [source]);
  const taken = new Set([...existing.map((p) => p.path), ...added]);
  const available = ports?.filter((p) => !taken.has(p.path)) ?? [];
  const chosen = available.filter((p) => selected.has(p.path));
  const allChosen = available.length > 0 && chosen.length === available.length;
  const nameOf = (port: SystemPort) => names[port.path]?.trim() || defaultPortName(port);
  /** Shift extends from the last toggled port, like a Finder list. */
  const toggle = (path: string, checked: boolean) => {
    const to = available.findIndex((p) => p.path === path);
    const from = extendRange.current && anchor.current ? available.findIndex((p) => p.path === anchor.current) : -1;
    extendRange.current = false;
    if (to < 0) return;
    const range = from >= 0 ? available.slice(Math.min(from, to), Math.max(from, to) + 1) : [available[to]];
    setSelected((current) => {
      const next = new Set(current);
      for (const port of range) {
        if (checked) next.add(port.path);
        else next.delete(port.path);
      }
      return next;
    });
    anchor.current = path;
  };
  async function submit() {
    const batch = chosen;
    if (!batch.length || busy) return;
    setError("");
    setFailures(null);
    const done: SerialPort[] = [];
    const failed: AddFailure[] = [];
    for (const [index, port] of batch.entries()) {
      setProgress({ current: index + 1, total: batch.length });
      try {
        done.push(
          await serial.add({
            path: port.path,
            name: nameOf(port),
            protocol,
            baud,
            usb: port.usb,
            enabled: connect,
          }),
        );
      } catch (e) {
        failed.push({ path: port.path, name: nameOf(port), message: errorMessage(e) });
      }
    }
    setProgress(null);
    if (!failed.length) {
      onAdded(done);
      return;
    }
    // Keep the dialog open: what worked is marked added, what failed stays checked for a retry.
    setAdded((current) => new Set([...current, ...done.map((p) => p.path)]));
    setSelected(new Set(failed.map((f) => f.path)));
    setFailures({ total: batch.length, items: failed });
  }
  if (!source)
    return (
      <Dialog title={t("添加串口")} onClose={onClose}>
        <div className="serial-sources" role="list">
          <button type="button" role="listitem" className="serial-source" onClick={() => setSource("local")}>
            <Usb size={20} />
            <span>
              <strong>{t("本机串口")}</strong>
              <small>{t("USB 串口、调试器和开发板")}</small>
            </span>
            <CaretRight size={14} />
          </button>
          <button type="button" role="listitem" className="serial-source" disabled>
            <Bluetooth size={20} />
            <span>
              <strong>{t("蓝牙串口")}</strong>
              <small>{t("即将支持")}</small>
            </span>
          </button>
          <button type="button" role="listitem" className="serial-source" onClick={() => setSource("remote")}>
            <Globe size={20} />
            <span>
              <strong>Remote GizMoth</strong>
              <small>{t("使用局域网或 Tailscale 上另一台 GizMoth 的串口")}</small>
            </span>
            <CaretRight size={14} />
          </button>
        </div>
        <div className="dialog-actions">
          <Button onClick={onClose}>{t("取消")}</Button>
        </div>
      </Dialog>
    );
  if (source === "remote")
    return <RemoteSourceDialog onClose={onClose} onBack={() => setSource(null)} onAdded={() => onAdded([])} />;
  return (
    <Dialog title={t("添加本机串口")} busy={busy} onClose={onClose} wide className="serial-add-dialog">
      <form
        className="serial-add"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <div className="serial-add-settings">
          <div className="serial-add-fields">
            <label>
              {t("协议")}
              <Select
                aria-label={t("协议")}
                value={protocol}
                disabled={busy}
                onValueChange={(v) => setProtocol(v as SerialProtocol)}
                options={[
                  { value: "auto", label: t("自动识别") },
                  { value: "h2loader", label: "H2Loader" },
                  { value: "serial", label: t("普通串口") },
                ]}
              />
            </label>
            <label>
              {t("波特率")}
              <Select
                aria-label={t("波特率")}
                value={String(baud)}
                disabled={busy || protocol === "h2loader"}
                onValueChange={(v) => setBaud(Number(v))}
                options={BAUDS.map((b) => ({ value: String(b), label: String(b) }))}
              />
            </label>
          </div>
          <p className="hint">
            {protocol === "auto"
              ? t("先尝试 H2Loader 握手；没有响应时按普通串口以所选波特率监听。")
              : protocol === "h2loader"
                ? t("H2Loader 使用固定的 460800 baud 可靠会话。")
                : t("只接收和发送原始数据，不发送任何握手。")}
          </p>
          <label className="serial-add-connect">
            <input type="checkbox" checked={connect} disabled={busy} onChange={(e) => setConnect(e.target.checked)} />
            {t("添加后立即连接")}
          </label>
        </div>
        <div className="serial-add-head">
          <span>
            {t("选择端口")}
            {chosen.length > 0 && (
              <span className="serial-add-count">{t("已选 {count} 个", { count: chosen.length })}</span>
            )}
          </span>
          <span className="serial-add-tools">
            {available.length > 1 && (
              <button
                type="button"
                className="text-action"
                disabled={busy}
                onClick={() => setSelected(allChosen ? new Set() : new Set(available.map((p) => p.path)))}
              >
                {allChosen ? t("全不选") : t("全选")}
              </button>
            )}
            <button type="button" className="text-action" onClick={scan} disabled={busy || ports === null}>
              <ArrowsClockwise size={13} aria-hidden="true" />
              {t("重新扫描")}
            </button>
          </span>
        </div>
        {/* Only the list scrolls; its frame stays put and the scrollbar runs at the dialog's edge. */}
        <div className="serial-add-list">
          <ul
            ref={list}
            className="serial-add-ports"
            aria-label={t("本机串口")}
            onPointerDown={(event) => {
              extendRange.current = event.shiftKey;
            }}
            onKeyDown={(event) => {
              extendRange.current = event.shiftKey;
            }}
          >
            {ports === null && <li className="serial-add-note">{t("正在扫描…")}</li>}
            {ports?.length === 0 && <li className="serial-add-note">{t("没有找到串口，请检查连接线或驱动。")}</li>}
            {ports?.map((port) => {
              const isTaken = taken.has(port.path);
              const checked = !isTaken && selected.has(port.path);
              return (
                <li key={port.path} className={isTaken ? "disabled" : checked ? "selected" : undefined}>
                  <label className="serial-add-pick">
                    <input
                      type="checkbox"
                      value={port.path}
                      disabled={isTaken || busy}
                      checked={checked}
                      onChange={(event) => toggle(port.path, event.target.checked)}
                    />
                    <Usb size={16} aria-hidden="true" />
                    <span className="serial-add-copy">
                      <strong title={port.usb?.product || port.name}>{port.usb?.product || port.name}</strong>
                      <small title={systemPortMeta(port)}>{systemPortMeta(port)}</small>
                    </span>
                    {isTaken && <small className="serial-add-taken">{t("已添加")}</small>}
                  </label>
                  {checked && (
                    <Input
                      controlSize="sm"
                      className="serial-add-name"
                      value={names[port.path] ?? ""}
                      maxLength={80}
                      disabled={busy}
                      placeholder={defaultPortName(port)}
                      title={t("名称")}
                      aria-label={t("{port} 的名称", { port: port.path })}
                      onChange={(event) => {
                        const value = event.target.value;
                        setNames((current) => ({ ...current, [port.path]: value }));
                      }}
                    />
                  )}
                </li>
              );
            })}
          </ul>
        </div>
        {error && (
          <p role="alert" className="error">
            {t(error)}
          </p>
        )}
        {failures &&
          (failures.total === 1 ? (
            <p role="alert" className="error">
              {t(failures.items[0].message)}
            </p>
          ) : (
            <div role="alert" className="error serial-add-failures">
              <p>
                {t("{failed}/{total} 个串口没有添加成功，仍保持勾选，可以直接重试。", {
                  failed: failures.items.length,
                  total: failures.total,
                })}
              </p>
              <ul>
                {failures.items.map((failure) => (
                  <li key={failure.path}>
                    <strong>{failure.name}</strong>
                    <span>{t(failure.message)}</span>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        <div className="dialog-actions">
          <Button disabled={busy} onClick={() => setSource(null)}>
            {t("返回")}
          </Button>
          <Button type="submit" variant="primary" disabled={busy || !chosen.length}>
            {progress
              ? progress.total > 1
                ? t("添加中 {current}/{total}…", progress)
                : t("添加中…")
              : chosen.length > 1
                ? t("添加 {count} 个串口", { count: chosen.length })
                : t("添加串口")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

/** Connects to another GizMoth's LAN service with its address and API key. */
function RemoteSourceDialog({
  onClose,
  onBack,
  onAdded,
}: {
  onClose: () => void;
  onBack: () => void;
  onAdded: () => void;
}) {
  useI18n();
  const [url, setUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [probe, setProbe] = useState<RemoteProbe | null>(null);
  const [busy, setBusy] = useState<"probe" | "add" | null>(null);
  const [error, setError] = useState("");
  const reset = () => {
    setProbe(null);
    setError("");
  };
  const check = async () => {
    setBusy("probe");
    setError("");
    try {
      const result = await serial.remoteProbe(url, apiKey);
      setProbe(result);
    } catch (e) {
      setProbe(null);
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };
  const add = async () => {
    setBusy("add");
    setError("");
    try {
      await serial.remoteAdd(url, apiKey);
      onAdded();
    } catch (e) {
      setError(errorMessage(e));
      setBusy(null);
    }
  };
  const found = probe?.node && !probe.self ? probe.node : null;
  const ready = url.trim() !== "" && apiKey.trim() !== "";
  return (
    <Dialog title={t("添加远端 GizMoth")} busy={busy !== null} onClose={onClose} className="serial-remote-dialog">
      <form
        className="serial-remote"
        onSubmit={(event) => {
          event.preventDefault();
          if (busy || !ready) return;
          if (found) void add();
          else void check();
        }}
      >
        <label>
          {t("地址")}
          <Input
            value={url}
            autoFocus
            autoComplete="off"
            spellCheck={false}
            placeholder="192.168.1.20 · studio-mac.tailnet.ts.net"
            disabled={busy !== null}
            onChange={(event) => {
              setUrl(event.target.value);
              reset();
            }}
          />
        </label>
        <label>
          API Key
          <Input
            type="password"
            value={apiKey}
            autoComplete="off"
            spellCheck={false}
            placeholder="gmk_…"
            disabled={busy !== null}
            onChange={(event) => {
              setApiKey(event.target.value);
              reset();
            }}
          />
        </label>
        <p className="hint">
          {t(
            "在对方的 设置 › 服务 中开启局域网访问，复制它的地址和 API Key。不在同一网络时，使用对方的 Tailscale 地址。默认端口 7420。",
          )}
        </p>
        {probe?.node && (
          <div className={`serial-remote-found${probe.self ? " self" : ""}`}>
            <Desktop size={20} aria-hidden="true" />
            <span>
              <strong>{probe.node.name}</strong>
              <small>
                {probe.self
                  ? t("这是本机的 GizMoth，无需添加")
                  : [
                      `GizMoth ${probe.node.version}`,
                      probe.ports == null ? null : t("{count} 个串口", { count: probe.ports }),
                    ]
                      .filter(Boolean)
                      .join(" · ")}
              </small>
            </span>
          </div>
        )}
        {error && (
          <p role="alert" className="error">
            {t(error)}
          </p>
        )}
        <div className="dialog-actions">
          <Button disabled={busy !== null} onClick={onBack}>
            {t("返回")}
          </Button>
          <Button type="submit" variant="primary" disabled={busy !== null || !ready || !!probe?.self}>
            {busy === "probe" ? t("连接中…") : busy === "add" ? t("添加中…") : found ? t("添加") : t("连接")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

const MAP_ROW = 36;
const MAP_GAP = 4;
const MAP_GROUP_GAP = 24;
const MAP_HOST_HEIGHT = 112;
const mapPortHeight = (count: number) => Math.max(MAP_ROW, count * (MAP_ROW + MAP_GAP) - MAP_GAP);

/** Every computer uses the same fan-out shape. Its children can be serial
 * ports or another computer with a fan-out of its own. */
function MapWires({
  height,
  targets,
}: {
  height: number;
  targets: { id: string; y: number; flow: Flow; selected?: boolean }[];
}) {
  return (
    <svg
      className="serial-wires"
      viewBox={`0 0 100 ${height}`}
      preserveAspectRatio="none"
      style={{ height }}
      aria-hidden="true"
    >
      {targets.map((target) => {
        const d = `M0 ${height / 2} C 55 ${height / 2}, 45 ${target.y}, 100 ${target.y}`;
        return (
          <g key={target.id} className={`wire flow-${target.flow}${target.selected ? " selected" : ""}`}>
            <path className="wire-base" d={d} vectorEffect="non-scaling-stroke" />
            <path className="wire-flow" d={d} vectorEffect="non-scaling-stroke" />
          </g>
        );
      })}
    </svg>
  );
}

function MapPorts({
  ports,
  selected,
  onSelect,
  now,
}: {
  ports: SerialPort[];
  selected: string;
  onSelect: (id: string) => void;
  now: number;
}) {
  return (
    <ul className="serial-map-ports" style={{ gap: MAP_GAP }}>
      {ports.map((port) => {
        const board = port.h2loader?.board.trim();
        const portNumber = port.path.split(/[\\/]/).pop() || port.path;
        return (
          <li key={port.id} style={{ height: MAP_ROW }}>
            <button
              type="button"
              className={`serial-map-port tone-${tone(port)}${port.id === selected ? " selected" : ""}`}
              aria-current={port.id === selected ? "true" : undefined}
              onClick={() => onSelect(port.id)}
              title={[board, port.path, statusText(port, now)].filter(Boolean).join(" · ")}
            >
              <strong className={board ? "serial-map-board" : undefined}>{board || portNumber}</strong>
              <small>
                {board
                  ? portNumber
                  : port.job || port.connection !== "connected" || port.remote?.stale
                    ? statusText(port, now)
                    : deviceLine(port) || statusText(port, now)}
              </small>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

function Topology({
  ports: allPorts,
  nodes: allNodes = [],
  selected,
  onSelect,
  now,
}: {
  ports: SerialPort[];
  nodes?: RemoteNode[];
  selected: string;
  onSelect: (id: string) => void;
  now: number;
}) {
  useI18n();
  const ports = allPorts.filter((port) => port.connection !== "off");
  const nodes = allNodes
    .map((node) => ({ ...node, ports: node.ports.filter((port) => port.connection !== "off") }))
    .filter((node) => node.ports.length > 0);
  if (!ports.length && !nodes.length) return null;
  const localHeight = ports.length ? mapPortHeight(ports.length) : 0;
  let offset = localHeight;
  const branches = nodes.map((node, index) => {
    if (ports.length || index > 0) offset += MAP_GROUP_GAP;
    const height = Math.max(MAP_HOST_HEIGHT, mapPortHeight(node.ports.length));
    const branch = { node, height, y: offset + height / 2 };
    offset += height;
    return branch;
  });
  const height = Math.max(MAP_HOST_HEIGHT, offset);
  const localTargets = ports.map((port, index) => ({
    id: port.id,
    y: index * (MAP_ROW + MAP_GAP) + MAP_ROW / 2,
    flow: flowOf(port, now),
    selected: port.id === selected,
  }));
  const targets = [...localTargets, ...branches.map(({ node, y }) => ({ id: node.id, y, flow: nodeFlow(node) }))];
  return (
    <section className="serial-map" aria-label={t("连接拓扑")} tabIndex={0}>
      <div className="serial-map-tree serial-map-root" style={{ minHeight: height }}>
        <div className="serial-map-host">
          <Laptop size={30} weight="light" aria-hidden="true" />
          <span className="serial-map-host-label">{t("这台电脑")}</span>
        </div>
        <MapWires height={height} targets={targets} />
        <div className="serial-map-children" style={{ gap: MAP_GROUP_GAP }}>
          {ports.length > 0 && <MapPorts ports={ports} selected={selected} onSelect={onSelect} now={now} />}
          {branches.map(({ node, height: branchHeight }) => {
            const portsHeight = mapPortHeight(node.ports.length);
            const inset = (branchHeight - portsHeight) / 2;
            return (
              <section
                key={node.id}
                className="serial-map-tree serial-map-remote"
                aria-label={node.name}
                style={{ height: branchHeight }}
              >
                <button
                  type="button"
                  className={`serial-map-host serial-map-remote-host flow-${nodeFlow(node)}`}
                  title={`${node.name} · ${node.url} · ${nodeStatusText(node, now)}`}
                  onClick={() =>
                    document
                      .getElementById(`serial-node-${node.id}`)
                      ?.scrollIntoView({ block: "nearest", behavior: "smooth" })
                  }
                >
                  <span className="serial-map-computer">
                    <Desktop size={30} weight="light" aria-hidden="true" />
                    <Dot tone={nodeTone(node)} />
                  </span>
                  <span className="serial-map-host-label">
                    <strong>{node.name}</strong>
                    <small>{nodeStatusText(node, now)}</small>
                  </span>
                </button>
                <MapWires
                  height={branchHeight}
                  targets={node.ports.map((port, index) => ({
                    id: port.id,
                    y: inset + index * (MAP_ROW + MAP_GAP) + MAP_ROW / 2,
                    flow: flowOf(port, now),
                    selected: port.id === selected,
                  }))}
                />
                {node.ports.length ? (
                  <MapPorts ports={node.ports} selected={selected} onSelect={onSelect} now={now} />
                ) : (
                  <span className="serial-map-empty">
                    {node.status === "online" ? t("这台 GizMoth 还没有添加串口。") : t("连接后会显示它的串口。")}
                  </span>
                )}
              </section>
            );
          })}
        </div>
      </div>
    </section>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="serial-field">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

function since(ms: number | null | undefined, now: number) {
  if (!ms) return "—";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return t("{count} 秒", { count: s });
  if (s < 3600) return t("{count} 分钟", { count: Math.floor(s / 60) });
  return t("{count} 小时", { count: Math.floor(s / 3600) });
}

function H2LoaderPanel({ port, onError }: { port: SerialPort; onError: (message: string) => void }) {
  useI18n();
  const status = port.h2loader;
  const [pending, setPending] = useState<{ path: string; info: PackageInfo } | null>(null);
  const [busy, setBusy] = useState(false);
  const ready = port.connection === "connected" && port.detected === "h2loader" && !port.job && !port.remote?.stale;
  const start = (kind: SerialJobKind, path?: string) => {
    setBusy(true);
    return serial
      .job(port.id, kind, path)
      .catch((e) => onError(errorMessage(e)))
      .finally(() => setBusy(false));
  };
  const choose = async () => {
    try {
      const path = await serial.choosePackage();
      if (!path) return;
      setBusy(true);
      setPending({ path, info: await serial.inspect(path) });
    } catch (e) {
      onError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  if (!status) {
    // A plain serial port has no device protocol, so there is nothing to show.
    if (port.detected === "serial" || port.protocol === "serial") return null;
    return (
      <section className="card serial-panel">
        <h2>{t("设备状态")}</h2>
        <p className="hint">{t("连接后，识别为 H2Loader 的设备会在这里显示板型、版本和固件分区。")}</p>
      </section>
    );
  }
  const available = deviceCommands(status);
  /** Enable an action only when the device accepts every command it needs. */
  const gate = (kind: SerialJobKind) => {
    const allowed = jobCommands[kind].every((command) => available.has(command));
    return {
      disabled: !ready || busy || !allowed,
      title: allowed ? undefined : t("设备当前不接受此操作（{state}）", { state: roleState(status) }),
    };
  };
  const mismatch = pending && (pending.info.board !== status.board || pending.info.target !== status.target);
  const meta = (m: H2LoaderMeta) => (m.valid ? `${m.role === "loader" ? "Loader" : "App"} ${m.version}` : "—");
  return (
    <section className="card serial-panel">
      <h2>
        H2Loader
        <small>{port.h2loaderAt ? t("{time}前更新", { time: since(port.h2loaderAt, Date.now()) }) : ""}</small>
      </h2>
      <dl className="serial-fields">
        <Field label={t("板型")}>{status.board || "—"}</Field>
        <Field label={t("芯片")}>{[...new Set([status.chip, status.target].filter(Boolean))].join(" · ") || "—"}</Field>
        <Field label={t("设备 UID")}>
          <code>{status.deviceUid || "—"}</code>
        </Field>
        <Field label={t("正在运行")}>
          {status.active.role === "loader" ? "Loader" : status.active.role === "app" ? "App" : t("未知")}{" "}
          {status.active.version}
        </Field>
        <Field label={t("启动方式")}>{status.bootIntent === "loader" ? t("停留在 Loader") : t("自动")}</Field>
        <Field label="Stage">{meta(status.stage)}</Field>
        <Field label={t("分区 1 / 2")}>
          {meta(status.partition1)} / {meta(status.partition2)}
        </Field>
        <Field label={t("可用命令")}>
          <span className="serial-commands">
            {COMMAND_LABELS.filter(([command]) => available.has(command)).map(([command, label]) => (
              <span key={command} className="serial-chip">
                {t(label)}
              </span>
            ))}
          </span>
        </Field>
      </dl>
      <div className="serial-actions">
        <Button variant="primary" {...gate("install")} onClick={() => void choose()}>
          <UploadSimple size={14} />
          {t("安装固件…")}
        </Button>
        <Button {...gate("refresh")} onClick={() => void start("refresh")}>
          {t("读取状态")}
        </Button>
        <Button {...gate("rebootApp")} onClick={() => void start("rebootApp")}>
          {t("重启到 App")}
        </Button>
        <Button {...gate("rebootLoader")} onClick={() => void start("rebootLoader")}>
          {t("重启到 Loader")}
        </Button>
      </div>
      {pending && (
        <Dialog title={t("安装固件")} busy={busy} onClose={() => setPending(null)}>
          <dl className="serial-fields">
            <Field label={t("固件包")}>{pending.info.name ?? pending.path.split(/[\\/]/).pop()}</Field>
            <Field label={t("类型与版本")}>
              {pending.info.role === "loader" ? "Loader" : "App"} {pending.info.version}
            </Field>
            <Field label={t("适用设备")}>
              {pending.info.board} · {pending.info.target}
            </Field>
            <Field label={t("大小")}>{formatBytes(pending.info.bytes)}</Field>
          </dl>
          <p className={mismatch ? "error" : "dialog-text"}>
            {mismatch
              ? t("固件包适用于 {board} {target}，与设备 {device} 不匹配。", {
                  board: pending.info.board,
                  target: pending.info.target,
                  device: `${status.board} ${status.target}`,
                })
              : t("先发送固件包，再重启升级并重连核验结果。过程中请勿断开设备。")}
          </p>
          <div className="dialog-actions">
            <Button onClick={() => setPending(null)}>{t("取消")}</Button>
            <Button
              variant="primary"
              disabled={!!mismatch || busy || !ready}
              onClick={() => {
                const path = pending.path;
                setPending(null);
                void start("install", path);
              }}
            >
              {t("开始安装")}
            </Button>
          </div>
        </Dialog>
      )}
    </section>
  );
}
const COMMAND_LABELS: [string, string][] = [
  ["stagePayload", "接收固件"],
  ["stageUrl", "URL 下载固件"],
  ["rebootUpgrade", "重启升级"],
  ["rebootApp", "重启到 App"],
  ["rebootLoader", "重启到 Loader"],
  ["status", "读取状态"],
  ["coredumpStatus", "Coredump"],
  ["wifiConnect", "Wi-Fi"],
  ["mfg", "产测"],
];
function roleState(status: H2LoaderStatus) {
  return status.active.role === "loader" ? "Loader" : status.active.role === "app" ? "App" : t("未知");
}
type H2LoaderMeta = NonNullable<SerialPort["h2loader"]>["stage"];

function JobPanel({ port, onError }: { port: SerialPort; onError: (message: string) => void }) {
  useI18n();
  const job = port.job;
  const recent = (port.history ?? []).slice(0, 4);
  if (!job && !recent.length) return null;
  return (
    <section className="card serial-panel serial-job">
      <h2>{t("任务")}</h2>
      {job && (
        <div className="serial-job-active">
          <div className="serial-job-line">
            <SpinnerGap size={14} className="spin" />
            <strong>{jobLabel(job.kind)}</strong>
            <span>{phaseLabel(job.phase)}</span>
            {job.source === "mcp" && <span className="serial-chip">MCP</span>}
            {job.source === "ws" && <span className="serial-chip">WS</span>}
            <button
              type="button"
              className="text-action danger-text"
              onClick={() => void serial.cancelJob(job.id).catch((e) => onError(errorMessage(e)))}
            >
              {t("取消")}
            </button>
          </div>
          {job.phase === "stage" && job.total > 0 && (
            <>
              <div
                className="serial-progress"
                role="progressbar"
                aria-valuenow={percent(job) ?? 0}
                aria-valuemin={0}
                aria-valuemax={100}
              >
                <i style={{ width: `${percent(job)}%` }} />
              </div>
              <small>
                {formatBytes(job.acknowledged)} / {formatBytes(job.total)} · {percent(job)}%
              </small>
            </>
          )}
        </div>
      )}
      {recent.length > 0 && (
        <ul className="serial-history">
          {recent.map((item) => (
            <li key={item.id}>
              <Dot tone={item.state === "succeeded" ? "ok" : item.state === "cancelled" ? "idle" : "warn"} />
              <span>{jobLabel(item.kind)}</span>
              <small>
                {item.state === "succeeded"
                  ? t("完成")
                  : item.state === "cancelled"
                    ? t("已取消")
                    : (item.error ?? t("失败"))}
              </small>
              <time>{new Date(item.finishedAt ?? item.createdAt).toLocaleTimeString()}</time>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

const logClock = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});
function clock(ms: number) {
  const d = new Date(ms);
  return `${logClock.format(d)}.${String(d.getMilliseconds()).padStart(3, "0")}`;
}

function Highlight({ text, needle }: { text: string; needle: string }) {
  if (!needle) return <>{text}</>;
  const parts: React.ReactNode[] = [];
  const lower = text.toLowerCase();
  let from = 0;
  for (let at = lower.indexOf(needle); at >= 0; at = lower.indexOf(needle, from)) {
    parts.push(text.slice(from, at), <mark key={at}>{text.slice(at, at + needle.length)}</mark>);
    from = at + needle.length;
  }
  parts.push(text.slice(from));
  return <>{parts}</>;
}

const LOG_SOURCES = [
  { value: "all", label: "全部" },
  { value: "rx", label: "设备日志" },
  { value: "tx", label: "已发送" },
  { value: "system", label: "事件" },
] as const;

const LogRow = memo(function LogRow({ entry, times, needle }: { entry: LogEntry; times: boolean; needle: string }) {
  return (
    <div className={`serial-log-line ${entry.source}`}>
      {times && <time>{clock(entry.at)}</time>}
      <span className="serial-log-mark" aria-hidden="true">
        {entry.source === "tx" ? "›" : entry.source === "system" ? "•" : ""}
      </span>
      <span className="serial-log-text">
        <Highlight text={entry.text || " "} needle={needle} />
      </span>
    </div>
  );
});

const LogPanel = memo(function LogPanel({
  port,
  running,
  instanceId,
}: {
  port: SerialPort;
  running: boolean;
  instanceId?: string | null;
}) {
  useI18n();
  const {
    entries,
    reset,
    error: readError,
  } = useSerialLogs(port.id, port.logSeq, running, instanceId, port.logGeneration);
  const [query, setQuery] = useState("");
  const [source, setSource] = useState<"all" | LogEntry["source"]>("all");
  const [times, setTimes] = useState(true);
  const [pausedAfter, setPausedAfter] = useState<number | null>(null);
  const [text, setText] = useState("");
  const [ending, setEnding] = useState<"lf" | "crlf" | "cr" | "none">("lf");
  const [error, setError] = useState("");
  const box = useRef<HTMLDivElement>(null);
  const needle = query.trim().toLowerCase();
  const visible = useMemo(
    () =>
      entries.filter(
        (e) => (source === "all" || e.source === source) && (!needle || e.text.toLowerCase().includes(needle)),
      ),
    [entries, needle, source],
  );
  const last = visible.at(-1)?.seq ?? 0;
  const follow = pausedAfter === null;
  const unseen = useMemo(
    () =>
      pausedAfter === null ? 0 : visible.reduce((count, entry) => (entry.seq > pausedAfter ? count + 1 : count), 0),
    [visible, pausedAfter],
  );
  useEffect(() => {
    if (follow && box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [visible, follow]);
  const toLatest = () => {
    setPausedAfter(null);
    if (box.current) box.current.scrollTop = box.current.scrollHeight;
  };
  // An imported port asks its node for control when sending; that node arbitrates.
  const canSend =
    port.connection === "connected" &&
    port.detected === "serial" &&
    (port.remote ? !port.remote.stale : port.mode !== "piping");
  return (
    <section className="card serial-log" aria-label={t("日志")}>
      <header className="serial-log-tools">
        <div className="serial-log-sources" role="radiogroup" aria-label={t("日志来源")}>
          {LOG_SOURCES.map((item) => (
            <button
              key={item.value}
              type="button"
              role="radio"
              aria-checked={source === item.value}
              className={source === item.value ? "active" : undefined}
              onClick={() => setSource(item.value)}
            >
              {t(item.label)}
            </button>
          ))}
        </div>
        <label className="serial-log-search">
          <MagnifyingGlass size={13} aria-hidden="true" />
          <input
            placeholder={t("筛选日志")}
            aria-label={t("筛选日志")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          {needle && <small>{visible.length}</small>}
        </label>
        <IconButton
          aria-label={t("显示时间")}
          title={t("显示时间")}
          aria-pressed={times}
          className={times ? "active" : undefined}
          onClick={() => setTimes((v) => !v)}
        >
          <Clock size={15} />
        </IconButton>
        <IconButton
          aria-label={t("清空日志")}
          title={port.remote ? t("日志由远端 GizMoth 管理") : t("清空日志")}
          disabled={!running || !!port.remote}
          onClick={() =>
            void serial
              .clearLogs(port.id)
              .then(reset)
              .catch(() => {})
          }
        >
          <Eraser size={15} />
        </IconButton>
        <IconButton
          aria-label={t("导出日志")}
          title={t("导出日志")}
          disabled={!running || !entries.length}
          onClick={() => void serial.exportLogs(port.id, port.name).catch((e) => setError(errorMessage(e)))}
        >
          <DownloadSimple size={15} />
        </IconButton>
      </header>
      <div className="serial-log-body">
        <div
          className={`serial-log-lines${times ? "" : " no-time"}`}
          ref={box}
          role="log"
          aria-live="off"
          onScroll={(e) => {
            const el = e.currentTarget;
            const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
            setPausedAfter((current) => (bottom ? null : (current ?? last)));
          }}
        >
          {visible.length ? (
            visible.slice(-2000).map((entry) => <LogRow key={entry.seq} entry={entry} times={times} needle={needle} />)
          ) : (
            <p className="serial-log-empty">
              {!running ? t("串口服务未运行") : needle ? t("没有匹配的日志") : t("暂无日志")}
            </p>
          )}
        </div>
        {!follow && (
          <button type="button" className="serial-log-latest" onClick={toLatest}>
            <ArrowDown size={13} />
            {unseen ? t("{count} 条新日志", { count: unseen }) : t("回到最新")}
          </button>
        )}
      </div>
      {port.detected === "serial" && (
        <form
          className="serial-send"
          onSubmit={(event) => {
            event.preventDefault();
            if (!canSend || (!text && ending === "none")) return;
            setError("");
            void serial
              .send(port.id, text, ending)
              .then(() => {
                setText((current) => (current === text ? "" : current));
                toLatest();
              })
              .catch((e) => setError(errorMessage(e)));
          }}
        >
          <span className="serial-send-prompt" aria-hidden="true">
            ›
          </span>
          <input
            value={text}
            aria-label={t("发送到设备")}
            placeholder={
              canSend
                ? port.remote && port.remote.control.state !== "granted"
                  ? t("发送到设备，回车发送；会先向远端申请通信权")
                  : t("发送到设备，回车发送")
                : port.remote?.stale
                  ? t("远端 GizMoth 未连接")
                  : port.mode === "piping"
                    ? t("WS 独占期间仅持有连接可发送")
                    : t("串口未连接")
            }
            disabled={!canSend}
            onChange={(e) => setText(e.target.value)}
          />
          <Select
            aria-label={t("换行")}
            controlSize="sm"
            variant="quiet"
            value={ending}
            onValueChange={(v) => setEnding(v as typeof ending)}
            options={[
              { value: "lf", label: "LF" },
              { value: "crlf", label: "CRLF" },
              { value: "cr", label: "CR" },
              { value: "none", label: t("无换行") },
            ]}
          />
          <IconButton
            type="submit"
            aria-label={t("发送")}
            title={t("发送")}
            disabled={!canSend || (!text && ending === "none")}
          >
            <PaperPlaneRight size={15} />
          </IconButton>
        </form>
      )}
      {(error || readError) && (
        <p role="alert" className="error serial-log-error">
          {t(error || readError)}
        </p>
      )}
    </section>
  );
});

/** Ownership follows the socket; the app can pause grants and reclaim control. */
function OwnershipControl({ port, onError }: { port: SerialPort; onError: (message: string) => void }) {
  useI18n();
  const [busy, setBusy] = useState(false);
  const holder = port.holder?.kind === "client" ? port.holder : null;
  const [confirming, setConfirming] = useState<typeof holder>(null);
  const reclaim = (connection?: number) => {
    setBusy(true);
    void (
      connection === undefined
        ? serial.update(port.id, { mode: "takeover" })
        : serial.release(port.id, connection, true)
    )
      .catch((e) => onError(errorMessage(e)))
      .finally(() => setBusy(false));
  };
  if (!holder && port.mode !== "piping" && !port.queue?.length) return null;
  return (
    <>
      <Button
        controlSize="sm"
        disabled={busy || (!holder && !!port.job)}
        onClick={() => (holder ? setConfirming(holder) : reclaim())}
      >
        {holder ? t("收回通信权") : t("暂停独占请求")}
      </Button>
      {confirming && (
        <Dialog title={t("收回通信权")} onClose={() => setConfirming(null)}>
          <p className="dialog-text">
            {t(
              "{client} 正为「{title}」保留 WS 独占通信权。收回只断开此 WS 并暂停自动批准，其他排队连接继续等待。串口连接可用时，界面恢复操作。",
              { client: confirming.client, title: taskTitle(confirming) },
            )}
          </p>
          <div className="dialog-actions">
            <Button onClick={() => setConfirming(null)}>{t("取消")}</Button>
            <Button
              variant="primary"
              onClick={() => {
                setConfirming(null);
                reclaim(confirming.id);
              }}
            >
              {t("收回并暂停自动批准")}
            </Button>
          </div>
        </Dialog>
      )}
    </>
  );
}

/** Ownership and its task stay visible even while the physical port is unavailable. */
function OwnerChip({ port, owner = "GizMoth" }: { port: SerialPort; owner?: string }) {
  useI18n();
  const holder = port.holder;
  const waiting = port.queue?.length ?? 0;
  const label =
    port.remote?.control.state === "granted"
      ? t("本机")
      : holder?.kind === "client"
        ? holder.client
        : holder?.kind === "gizmoth"
          ? owner
          : t("空闲");
  const title =
    holder?.kind === "client"
      ? [t("独占通信方：{client}", { client: holder.client }), taskTitle(holder), holder.reason]
          .filter(Boolean)
          .join("\n")
      : holder?.kind === "gizmoth"
        ? t("GizMoth 可发送指令")
        : t("没有 WS 连接持有通信权");
  return (
    <span className={`serial-owner${holder?.kind === "client" ? " client" : ""}`} title={title}>
      <span className="serial-owner-copy">
        <span className="serial-owner-line">
          <span className="serial-owner-label">{t("通信方")}</span>
          <span className="serial-ellipsis">{label}</span>
        </span>
        {holder?.kind === "client" && <span className="serial-task-title">{taskTitle(holder)}</span>}
      </span>
      {waiting > 0 && <span className="serial-owner-queue">{t("{count} 排队", { count: waiting })}</span>}
    </span>
  );
}

function OccupancyPanel({
  port,
  pipeBase,
  onError,
}: {
  port: SerialPort;
  pipeBase?: string;
  onError: (message: string) => void;
}) {
  useI18n();
  const now = useNow(1000);
  const [busy, setBusy] = useState(false);
  const holder = port.holder?.kind === "client" ? port.holder : null;
  const queue = port.queue ?? [];
  const piping = port.mode === "piping";
  const url = `${pipeBase ?? "ws://127.0.0.1:7420/v1/ports"}/${encodeURIComponent(port.id)}/pipe?client=<name>&title=<task>`;
  const run = (action: () => Promise<unknown>) => {
    setBusy(true);
    void action()
      .catch((e) => onError(errorMessage(e)))
      .finally(() => setBusy(false));
  };
  return (
    <section className="card serial-panel serial-occupancy">
      <h2>
        {t("独占通信")}
        <label className="serial-auto-grant">
          <span>{t("自动批准独占请求")}</span>
          <input
            type="checkbox"
            role="switch"
            className="switch"
            aria-label={t("自动批准独占请求")}
            checked={port.autoGrant ?? true}
            disabled={busy}
            onChange={(event) => run(() => serial.update(port.id, { autoGrant: event.target.checked }))}
          />
        </label>
      </h2>
      <dl className="serial-fields">
        <Field label={t("通信方")}>
          {holder ? (
            <>
              <span className="serial-chip serial-ellipsis" title={holder.client}>
                {holder.client}
              </span>
              <span>{t("已独占 {time}", { time: since(holder.since, now) })}</span>
              <button
                type="button"
                className="text-action danger-text serial-occupancy-action"
                disabled={busy}
                title={t("仅断开 {client} 的此 WS 连接，其他排队连接保留", { client: holder.client })}
                aria-label={t("断开此 WS：{client} · {title}", { client: holder.client, title: taskTitle(holder) })}
                onClick={() => run(() => serial.release(port.id, holder.id))}
              >
                {t("断开此 WS")}
              </button>
            </>
          ) : piping ? (
            t("等待 WS 独占连接")
          ) : (
            "GizMoth"
          )}
        </Field>
        {holder && (
          <Field label={t("任务标题")}>
            <span className="serial-task-title">{taskTitle(holder)}</span>
          </Field>
        )}
        {holder?.reason.trim() && holder.reason.trim() !== taskTitle(holder) && (
          <Field label={t("补充说明")}>
            <span className="serial-task-title">{holder.reason}</span>
          </Field>
        )}
        <Field label={t("WS 通信地址")}>
          <code className="serial-ellipsis" title={url}>
            {url}
          </code>
        </Field>
        <Field label={t("WS 认证")}>
          <code className="serial-task-title">Authorization: Bearer &lt;API Key&gt;</code>
        </Field>
      </dl>
      {queue.length > 0 && (
        <div className="serial-queue">
          <h3>{t("排队中 · {count}", { count: queue.length })}</h3>
          <ol>
            {queue.map((request, index) => (
              <li key={request.id}>
                <span className="serial-queue-index">{index + 1}</span>
                <div className="serial-queue-copy">
                  <strong className="serial-ellipsis" title={request.client}>
                    {request.client}
                  </strong>
                  <span className="serial-task-title">{taskTitle(request)}</span>
                  {request.reason.trim() && request.reason.trim() !== taskTitle(request) && (
                    <span className="serial-task-title">{request.reason}</span>
                  )}
                </div>
                <small>{t("已等待 {time}", { time: since(request.since, now) })}</small>
                <div className="serial-queue-actions">
                  <Button
                    controlSize="sm"
                    disabled={busy}
                    title={t("拒绝并断开此等待 WS，其他连接保留")}
                    aria-label={t("断开此 WS：{client} · {title}", {
                      client: request.client,
                      title: taskTitle(request),
                    })}
                    onClick={() => run(() => serial.answerRequest(port.id, request.id, false))}
                  >
                    {t("断开此 WS")}
                  </Button>
                  <Button
                    controlSize="sm"
                    variant="primary"
                    disabled={busy || !!port.job || port.connection !== "connected" || port.activity === "handshaking"}
                    title={
                      port.connection !== "connected" || port.activity === "handshaking"
                        ? t("串口就绪后可接受请求；仍可断开此等待 WS")
                        : piping
                          ? holder
                            ? t("断开 {client} 的 WS，授予此连接独占通信权", { client: holder.client })
                            : t("授予此 WS 连接独占通信权")
                          : t("暂停界面发送，授予此 WS 连接独占通信权")
                    }
                    onClick={() => run(() => serial.answerRequest(port.id, request.id, true))}
                  >
                    {t("接受")}
                  </Button>
                </div>
              </li>
            ))}
          </ol>
        </div>
      )}
      <p className="hint">
        {(port.autoGrant ?? true)
          ? t(
              "默认自动批准：一次仅授予一个 WS 连接独占通信权，其余按顺序排队。独占期间界面及其他发送入口暂停，仍可查看日志。串口始终由 GizMoth 管理。",
            )
          : t(
              "请求等待你接受或拒绝。获批后仅该 WS 连接可发送；界面和其他发送入口暂停，仍可查看日志。串口始终由 GizMoth 管理。",
            )}
      </p>
      <p className="hint">{t("设备掉线、重启或关闭串口连接时，WS 通信权和排队连接仍保留。")}</p>
      <p className="hint">
        {t("client 和 title 为必填，title 最多 120 个字符；reason 可选。API Key 通过请求头传递。")}
      </p>
    </section>
  );
}

/** Control of an imported port: the owning node arbitrates; this GizMoth
 * holds a session there only while it sends, installs or reboots. */
function RemoteControlPanel({
  port,
  node,
  onError,
}: {
  port: SerialPort;
  node: RemoteNode;
  onError: (message: string) => void;
}) {
  useI18n();
  const now = useNow(1000);
  const [busy, setBusy] = useState(false);
  const control = port.remote?.control ?? { state: "none" as const };
  const holder = port.holder?.kind === "client" && control.state !== "granted" ? port.holder : null;
  const queued = port.queue?.length ?? 0;
  const session =
    control.state === "granted"
      ? t("已获得通信权")
      : control.state === "queued"
        ? t("排队中 · 第 {position} 位", { position: control.position ?? 1 })
        : control.state === "opening"
          ? t("正在申请…")
          : t("未占用");
  return (
    <section className="card serial-panel serial-occupancy">
      <h2>{t("远端通信权")}</h2>
      <dl className="serial-fields">
        <Field label={t("通信方")}>
          {control.state === "granted" ? (
            t("本机")
          ) : holder ? (
            <>
              <span className="serial-chip">{holder.client}</span>
              <span className="serial-ellipsis" title={holder.reason}>
                {[holder.reason, t("已独占 {time}", { time: since(holder.since, now) })].filter(Boolean).join(" · ")}
              </span>
            </>
          ) : (
            node.name
          )}
          {queued > 0 && <span className="serial-owner-queue">{t("{count} 排队", { count: queued })}</span>}
        </Field>
        <Field label={t("本机会话")}>
          <span>{session}</span>
          {control.state !== "none" && (
            <button
              type="button"
              className="text-action danger-text serial-occupancy-action"
              disabled={busy || control.connectionId === undefined}
              onClick={() => {
                setBusy(true);
                void serial
                  .release(port.id, control.connectionId!)
                  .catch((e) => onError(errorMessage(e)))
                  .finally(() => setBusy(false));
              }}
            >
              {t("释放通信权")}
            </button>
          )}
        </Field>
      </dl>
      <p className="hint">
        {t(
          "发送数据、安装固件和重启时，本机会向「{node}」申请独占通信权，空闲 1 分钟后自动释放。查看状态和日志不需要通信权；审批与自动批准由「{node}」管理。",
          { node: node.name },
        )}
      </p>
    </section>
  );
}

export function SerialPortPage({ id, onBack }: { id: string; onBack: () => void }) {
  useI18n();
  const { snapshot } = useSerialSnapshot();
  const now = useNow(400);
  const [error, setError] = useState("");
  const [removing, setRemoving] = useState(false);
  const [tab, setTab] = useState<"status" | "log">(() =>
    localStorage.getItem("gizmoth.serial.tab") === "log" ? "log" : "status",
  );
  const chooseTab = (next: "status" | "log") => {
    setTab(next);
    localStorage.setItem("gizmoth.serial.tab", next);
  };
  const found = findSerialPort(snapshot, id);
  const port = found?.port;
  const node = found?.node;
  useEffect(() => setError(""), [id]);
  if (!snapshot)
    return (
      <div className="page">
        <p className="notice">{t("正在读取串口…")}</p>
      </div>
    );
  if (!port)
    return (
      <>
        <PageHeader crumbs={[{ label: t("串口"), onClick: onBack }]} />
        <div className="page">
          <Empty icon={<PlugsConnected size={26} />} title={t("串口不存在")} body={t("它可能已从列表中移除。")} />
        </div>
      </>
    );
  const update = (patch: Parameters<typeof serial.update>[1]) =>
    void serial.update(port.id, patch).catch((e) => setError(errorMessage(e)));
  const stale = !!port.remote?.stale;
  return (
    <>
      <PageHeader
        crumbs={
          node
            ? [{ label: t("串口"), onClick: onBack }, { label: node.name, onClick: onBack }, { label: port.name }]
            : [{ label: t("串口"), onClick: onBack }, { label: port.name }]
        }
        actions={
          node ? undefined : (
            <button type="button" className="text-action danger-text serial-remove" onClick={() => setRemoving(true)}>
              {t("从列表移除")}
            </button>
          )
        }
      />
      <div className="page serial-detail">
        <header className="serial-head">
          <span className={`serial-row-icon flow-${flowOf(port, now)}`}>
            <PortIcon port={port} />
          </span>
          <div className="serial-head-copy">
            <h1 title={port.name}>{port.name}</h1>
            <p>
              <Dot tone={tone(port)} />
              <span>{statusText(port, now)}</span>
              <span className="serial-head-meta">
                {[
                  port.path,
                  port.detected ? protocolLabel(port.detected) : protocolLabel(port.protocol),
                  deviceLine(port),
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </span>
            </p>
            {port.job?.phase === "stage" && port.job.total > 0 && (
              <div className="serial-progress serial-head-progress" aria-hidden="true">
                <i style={{ width: `${percent(port.job)}%` }} />
              </div>
            )}
          </div>
          {port.job && (
            <button
              type="button"
              className="text-action danger-text"
              onClick={() => void serial.cancelJob(port.job!.id).catch((e) => setError(errorMessage(e)))}
            >
              {t("取消任务")}
            </button>
          )}
          <OwnerChip port={port} owner={node?.name} />
          {!node && <OwnershipControl port={port} onError={setError} />}
          {!node && <EnableSwitch port={port} onError={setError} />}
        </header>
        {node && stale && (
          <p className="notice serial-head-stale" role="status">
            {node.lastSeenAt
              ? t("「{node}」未连接，以下是 {time}前的状态。", {
                  node: node.name,
                  time: since(node.lastSeenAt, now),
                })
              : t("「{node}」未连接。", { node: node.name })}
          </p>
        )}
        {(error || (port.error && port.connection !== "connected")) && (
          <p role="alert" className="error serial-head-error">
            {t(error || port.error || "")}
          </p>
        )}
        <div className="serial-tabs" role="tablist" aria-label={t("串口详情")}>
          {(["status", "log"] as const).map((item) => (
            <button
              key={item}
              type="button"
              role="tab"
              aria-selected={tab === item}
              className={tab === item ? "active" : undefined}
              onClick={() => chooseTab(item)}
            >
              {item === "log" ? t("日志") : t("实时状态")}
            </button>
          ))}
        </div>
        {tab === "log" ? (
          <LogPanel
            port={port}
            running={snapshot.running && !stale}
            instanceId={node ? node.bootId : snapshot.instanceId}
          />
        ) : (
          <div className="serial-overview" role="tabpanel">
            <div className="serial-overview-inner">
              <div className="serial-overview-grid">
                <section className="card serial-panel">
                  <h2>{t("连接信息")}</h2>
                  <dl className="serial-fields">
                    {node && (
                      <Field label={t("所在 GizMoth")}>
                        <span className="serial-ellipsis" title={node.url}>
                          {node.name} · {hostLabel(node.url)}
                        </span>
                      </Field>
                    )}
                    <Field label={t("路径")}>
                      <code>{port.path}</code>
                      {port.present === false && <span className="serial-chip">{t("未接入")}</span>}
                    </Field>
                    <Field label={t("协议")}>
                      {node ? (
                        port.detected && port.protocol === "auto" ? (
                          t("自动识别 · {protocol}", { protocol: protocolLabel(port.detected) })
                        ) : (
                          protocolLabel(port.protocol)
                        )
                      ) : (
                        <Select
                          aria-label={t("协议")}
                          controlSize="sm"
                          value={port.protocol}
                          disabled={!!port.job}
                          onValueChange={(v) => update({ protocol: v as SerialProtocol })}
                          options={[
                            { value: "auto", label: t("自动识别") },
                            { value: "h2loader", label: "H2Loader" },
                            { value: "serial", label: t("普通串口") },
                          ]}
                        />
                      )}
                      {!node && port.protocol === "auto" && port.detected && (
                        <span className="serial-chip">
                          {t("识别为 {protocol}", { protocol: protocolLabel(port.detected) })}
                        </span>
                      )}
                    </Field>
                    <Field label={t("波特率")}>
                      {port.protocol === "h2loader" || port.detected === "h2loader" ? (
                        "460800"
                      ) : node ? (
                        String(port.baud)
                      ) : (
                        <Select
                          aria-label={t("波特率")}
                          controlSize="sm"
                          value={String(port.baud)}
                          onValueChange={(v) => update({ baud: Number(v) })}
                          options={(BAUDS.includes(port.baud) ? BAUDS : [...BAUDS, port.baud]).map((b) => ({
                            value: String(b),
                            label: String(b),
                          }))}
                        />
                      )}
                    </Field>
                    {port.usb && (
                      <Field label="USB">
                        {[port.usb.product, port.usb.manufacturer, port.usb.serial].filter(Boolean).join(" · ")}
                      </Field>
                    )}
                    <Field label={t("收 / 发")}>
                      {formatBytes(port.rxBytes ?? 0)} / {formatBytes(port.txBytes ?? 0)}
                    </Field>
                    <Field label={t("已连接")}>
                      {port.connection === "connected" ? since(port.connectedAt, now) : "—"}
                    </Field>
                  </dl>
                </section>
                {!node && port.mode === "piping" ? (
                  <OccupancyPanel port={port} pipeBase={snapshot.pipeBase} onError={setError} />
                ) : (
                  <H2LoaderPanel port={port} onError={setError} />
                )}
              </div>
              {node ? (
                <RemoteControlPanel port={port} node={node} onError={setError} />
              ) : (
                port.mode === "takeover" && (
                  <OccupancyPanel port={port} pipeBase={snapshot.pipeBase} onError={setError} />
                )
              )}
              <JobPanel port={port} onError={setError} />
            </div>
          </div>
        )}
      </div>
      {removing && (
        <Dialog title={t("移除串口")} onClose={() => setRemoving(false)}>
          <p className="dialog-text">
            {t("断开并从列表移除「{name}」？日志会一并清除，设备本身不受影响。", { name: port.name })}
          </p>
          <div className="dialog-actions">
            <Button onClick={() => setRemoving(false)}>{t("取消")}</Button>
            <Button
              variant="danger"
              disabled={!!port.job}
              onClick={() =>
                void serial
                  .remove(port.id)
                  .then(onBack)
                  .catch((e) => setError(errorMessage(e)))
                  .finally(() => setRemoving(false))
              }
            >
              {t("移除")}
            </Button>
          </div>
        </Dialog>
      )}
    </>
  );
}
