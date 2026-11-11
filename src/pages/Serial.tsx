import { memo, useEffect, useMemo, useRef, useState, type RefObject } from "react";
import {
  ArrowDown,
  ArrowsClockwise,
  Bluetooth,
  CaretRight,
  Clock,
  Cpu,
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
import { errorMessage } from "../bridge";
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
  type SerialMode,
  type SerialSnapshot,
  type H2LoaderStatus,
  deviceCommands,
  jobCommands,
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
  const job = port.job;
  if (job) {
    const pct = job.phase === "stage" ? percent(job) : null;
    return pct === null ? `${jobLabel(job.kind)} · ${phaseLabel(job.phase)}` : `${jobLabel(job.kind)} · ${pct}%`;
  }
  if (port.connection !== "connected") return connectionText(port);
  if (port.mode === "piping")
    return port.holder?.kind === "client"
      ? t("透传 · {client} 占据中", { client: port.holder.client })
      : t("透传 · 等待外部程序");
  const flow = flowOf(port, now);
  if (port.activity !== "idle") return t(activityLabel[port.activity]);
  if (flow === "in" || flow === "both") return t("接收数据");
  if (flow === "out") return t("发送数据");
  return t("已连接 · 空闲");
}
function deviceLine(port: SerialPort) {
  const s = port.h2loader;
  if (!s) return port.detected === "serial" ? `${port.baud} baud` : "";
  const role = s.active.role === "loader" ? "Loader" : s.active.role === "app" ? "App" : "";
  return [s.board, s.chip || s.target, [role, s.active.version].filter(Boolean).join(" ")].filter(Boolean).join(" · ");
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
      title={port.enabled ? t("断开") : t("连接")}
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

export function SerialPage({ onOpen, onService }: { onOpen: (id: string) => void; onService: () => void }) {
  useI18n();
  const { snapshot, error: loadError } = useSerialSnapshot();
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState("");
  const now = useNow();
  const ports = snapshot?.ports ?? [];
  return (
    <>
      <PageHeader crumbs={[{ label: t("设备") }, { label: t("串口") }]} />
      <div className="page list-page serial-page">
        <div className="page-description">
          <span>{t("这台电脑上的串口由一个常驻服务统一连接，界面和通过服务接入的 agent 共用它的状态与日志。")}</span>
        </div>
        {ports.length > 0 && <Topology ports={ports} selected="" onSelect={onOpen} now={now} />}
        {ports.length > 0 && snapshot && (
          <div className="page-list-toolbar">
            <button type="button" className="text-action" onClick={() => setAdding(true)}>
              <Plus size={14} aria-hidden="true" />
              {t("添加串口")}
            </button>
            <ServiceLine snapshot={snapshot} onService={onService} />
            <span className="page-list-summary">
              {t("{connected}/{count} 个已连接", {
                connected: ports.filter((p) => p.connection === "connected").length,
                count: ports.length,
              })}
            </span>
          </div>
        )}
        {(error || loadError) && (
          <p role="alert" className="error">
            {t(error || loadError)}
          </p>
        )}
        {ports.length ? (
          <ul className="card serial-list" aria-label={t("串口列表")}>
            {ports.map((port) => {
              const flow = flowOf(port, now);
              const pct = port.job ? percent(port.job) : null;
              return (
                <li key={port.id}>
                  <button type="button" className="serial-row" onClick={() => onOpen(port.id)}>
                    <span className={`serial-row-icon flow-${flow}`}>
                      <PortIcon port={port} />
                    </span>
                    <span className="serial-row-copy">
                      <strong title={port.name}>{port.name}</strong>
                      <span title={port.path}>
                        {[
                          port.path,
                          port.detected ? protocolLabel(port.detected) : protocolLabel(port.protocol),
                          deviceLine(port),
                        ]
                          .filter(Boolean)
                          .join(" · ")}
                      </span>
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
                  <EnableSwitch port={port} onError={setError} />
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
            })}
          </ul>
        ) : snapshot ? (
          <Empty
            icon={<PlugsConnected size={26} />}
            title={t("添加你的第一个串口")}
            body={t("选择本机串口，打开开关后由串口服务连接，并自动识别 H2Loader 设备。")}
            action={
              <Button variant="primary" controlSize="lg" onClick={() => setAdding(true)}>
                <Plus size={16} aria-hidden="true" />
                {t("添加串口")}
              </Button>
            }
          />
        ) : (
          <p className="notice" role="status">
            {t("正在读取串口…")}
          </p>
        )}
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
    </>
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

/** Tracks whether the dialog's own scroll hides content above or below its pinned header and footer. */
function useScrollEdges(content: RefObject<HTMLElement | null>, active: boolean) {
  const [edges, setEdges] = useState({ above: false, below: false });
  useEffect(() => {
    const element = content.current;
    const scroller = element?.closest("dialog");
    if (!active || !element || !scroller) return;
    const update = () => {
      const above = scroller.scrollTop > 0;
      const below = scroller.scrollTop + scroller.clientHeight < scroller.scrollHeight - 1;
      setEdges((current) => (current.above === above && current.below === below ? current : { above, below }));
    };
    update();
    scroller.addEventListener("scroll", update, { passive: true });
    const observer = new ResizeObserver(update);
    observer.observe(scroller);
    observer.observe(element);
    return () => {
      scroller.removeEventListener("scroll", update);
      observer.disconnect();
    };
  }, [content, active]);
  return edges;
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
  const [source, setSource] = useState<"local" | null>(null);
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
  const form = useRef<HTMLFormElement>(null);
  const anchor = useRef<string | null>(null);
  const extendRange = useRef(false);
  const edges = useScrollEdges(form, source === "local");
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
          <button type="button" role="listitem" className="serial-source" disabled>
            <Globe size={20} />
            <span>
              <strong>Remote GizMoth</strong>
              <small>{t("使用另一台 GizMoth 上的串口 · 即将支持")}</small>
            </span>
          </button>
        </div>
        <div className="dialog-actions">
          <Button onClick={onClose}>{t("取消")}</Button>
        </div>
      </Dialog>
    );
  return (
    <Dialog
      title={t("添加本机串口")}
      busy={busy}
      onClose={onClose}
      wide
      className={`serial-add-dialog${edges.above ? " is-scrolled" : ""}${edges.below ? " has-more" : ""}`}
    >
      <form
        ref={form}
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
        <ul
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

/** One computer fanning out to every port it holds, drawn on the page itself. */
function Topology({
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
  const row = 36;
  const gap = 4;
  const height = Math.max(row, ports.length * (row + gap) - gap);
  return (
    <section className="serial-map" aria-label={t("连接拓扑")}>
      <div className="serial-map-host">
        <Laptop size={30} weight="light" />
        <span>{t("这台电脑")}</span>
      </div>
      <svg
        className="serial-wires"
        viewBox={`0 0 100 ${height}`}
        preserveAspectRatio="none"
        style={{ height }}
        aria-hidden="true"
      >
        {ports.map((port, index) => {
          const y = index * (row + gap) + row / 2;
          const d = `M0 ${height / 2} C 55 ${height / 2}, 45 ${y}, 100 ${y}`;
          return (
            <g key={port.id} className={`wire flow-${flowOf(port, now)}${port.id === selected ? " selected" : ""}`}>
              <path className="wire-base" d={d} vectorEffect="non-scaling-stroke" />
              <path className="wire-flow" d={d} vectorEffect="non-scaling-stroke" />
            </g>
          );
        })}
      </svg>
      <ul className="serial-map-ports" style={{ gap }}>
        {ports.map((port) => (
          <li key={port.id} style={{ height: row }}>
            <button
              type="button"
              className={`serial-map-port tone-${tone(port)}${port.id === selected ? " selected" : ""}`}
              aria-current={port.id === selected ? "true" : undefined}
              onClick={() => onSelect(port.id)}
            >
              <strong>{port.name}</strong>
              <small>
                {port.job || port.connection !== "connected"
                  ? statusText(port, now)
                  : deviceLine(port) || statusText(port, now)}
              </small>
            </button>
          </li>
        ))}
      </ul>
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
  const ready = port.connection === "connected" && port.detected === "h2loader" && !port.job;
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
  const canSend = port.connection === "connected" && port.detected === "serial" && port.mode !== "piping";
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
          title={t("清空日志")}
          disabled={!running}
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
                ? t("发送到设备，回车发送")
                : port.mode === "piping"
                  ? t("透传模式下由占据者发送")
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

/** Take-over: GizMoth controls the port. Piping: one external holder, GizMoth records. */
function ModeSwitch({ port, onError }: { port: SerialPort; onError: (message: string) => void }) {
  useI18n();
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const apply = (mode: SerialMode) => {
    setBusy(true);
    void serial
      .update(port.id, { mode })
      .catch((e) => onError(errorMessage(e)))
      .finally(() => setBusy(false));
  };
  const holder = port.holder?.kind === "client" ? port.holder.client : null;
  return (
    <>
      <div className="serial-mode" role="radiogroup" aria-label={t("串口模式")}>
        {(["takeover", "piping"] as const).map((mode) => (
          <button
            key={mode}
            type="button"
            role="radio"
            aria-checked={port.mode === mode}
            className={port.mode === mode ? "active" : undefined}
            disabled={busy || !!port.job}
            title={
              mode === "takeover"
                ? t("GizMoth 控制串口：读取状态、下发任务和发送数据，外部程序无法占据")
                : t("GizMoth 只记录日志并转发，由一个外部程序通过服务占据串口")
            }
            onClick={() => {
              if (port.mode === mode) return;
              if (mode === "takeover" && holder) setConfirming(true);
              else apply(mode);
            }}
          >
            {mode === "takeover" ? t("接管") : t("透传")}
          </button>
        ))}
      </div>
      {confirming && holder && (
        <Dialog title={t("接管串口")} onClose={() => setConfirming(false)}>
          <p className="dialog-text">
            {t("串口正被 {client} 占据。接管会断开它的连接，之后由 GizMoth 控制此串口；排队的请求继续等待。", {
              client: holder,
            })}
          </p>
          <div className="dialog-actions">
            <Button onClick={() => setConfirming(false)}>{t("取消")}</Button>
            <Button
              variant="primary"
              onClick={() => {
                setConfirming(false);
                apply("takeover");
              }}
            >
              {t("断开并接管")}
            </Button>
          </div>
        </Dialog>
      )}
    </>
  );
}

/** Who holds the port now, shown beside the mode switch. */
function OwnerChip({ port }: { port: SerialPort }) {
  useI18n();
  const holder = port.holder;
  const waiting = port.queue?.length ?? 0;
  const label = holder?.kind === "client" ? holder.client : holder?.kind === "gizmoth" ? "GizMoth" : t("空闲");
  const title =
    holder?.kind === "client"
      ? [t("占据者：{client}", { client: holder.client }), holder.reason].filter(Boolean).join("\n")
      : holder?.kind === "gizmoth"
        ? t("GizMoth 控制此串口")
        : t("没有程序占据此串口");
  return (
    <span className={`serial-owner${holder?.kind === "client" ? " client" : ""}`} title={title}>
      <span className="serial-owner-label">{t("占据者")}</span>
      <span className="serial-ellipsis">{label}</span>
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
  const url = `${pipeBase ?? "ws://127.0.0.1:7420/v1/ports"}/${port.id}/pipe?token=<API Key>&client=<name>&reason=<why>`;
  const run = (action: () => Promise<unknown>) => {
    setBusy(true);
    void action()
      .catch((e) => onError(errorMessage(e)))
      .finally(() => setBusy(false));
  };
  return (
    <section className="card serial-panel serial-occupancy">
      <h2>
        {t("占据")}
        {piping && (
          <label className="serial-auto-grant">
            <span>{t("按顺序自动授予")}</span>
            <input
              type="checkbox"
              role="switch"
              className="switch"
              aria-label={t("按顺序自动授予")}
              checked={port.autoGrant ?? true}
              disabled={busy}
              onChange={(event) => run(() => serial.update(port.id, { autoGrant: event.target.checked }))}
            />
          </label>
        )}
      </h2>
      <dl className="serial-fields">
        <Field label={t("占据者")}>
          {holder ? (
            <>
              <span className="serial-chip">{holder.client}</span>
              <span className="serial-ellipsis" title={holder.reason}>
                {[holder.reason, t("已占据 {time}", { time: since(holder.since, now) })].filter(Boolean).join(" · ")}
              </span>
              <button
                type="button"
                className="text-action danger-text serial-occupancy-action"
                disabled={busy}
                onClick={() => run(() => serial.release(port.id))}
              >
                {t("收回")}
              </button>
            </>
          ) : piping ? (
            t("空闲，等待外部程序占据")
          ) : (
            "GizMoth"
          )}
        </Field>
        {piping && (
          <Field label={t("透传地址")}>
            <code className="serial-ellipsis" title={url}>
              {url}
            </code>
          </Field>
        )}
      </dl>
      {queue.length > 0 && (
        <div className="serial-queue">
          <h3>{t("排队中 · {count}", { count: queue.length })}</h3>
          <ol>
            {queue.map((request, index) => (
              <li key={request.id}>
                <span className="serial-queue-index">{index + 1}</span>
                <div className="serial-queue-copy">
                  <strong className="serial-ellipsis">{request.client}</strong>
                  <span className="serial-ellipsis" title={request.reason}>
                    {request.reason || t("未说明原因")}
                  </span>
                </div>
                <small>{t("已等待 {time}", { time: since(request.since, now) })}</small>
                <Button
                  controlSize="sm"
                  disabled={busy}
                  onClick={() => run(() => serial.answerRequest(port.id, request.id, false))}
                >
                  {t("拒绝")}
                </Button>
                <Button
                  controlSize="sm"
                  variant="primary"
                  disabled={busy || !!port.job}
                  title={
                    piping
                      ? holder
                        ? t("断开 {client}，把串口交给它", { client: holder.client })
                        : t("把串口交给它")
                      : t("切换为透传，把串口交给它")
                  }
                  onClick={() => run(() => serial.answerRequest(port.id, request.id, true))}
                >
                  {t("接受")}
                </Button>
              </li>
            ))}
          </ol>
        </div>
      )}
      <p className="hint">
        {piping
          ? (port.autoGrant ?? true)
            ? t(
                "一次只允许一个程序占据，其余按请求顺序排队，前一个断开后自动授予下一个。GizMoth 记录双向日志，自身不再读取状态或下发任务。",
              )
            : t(
                "一次只允许一个程序占据。新请求在这里等待你接受或拒绝；GizMoth 记录双向日志，自身不再读取状态或下发任务。",
              )
          : t("接管模式下外部请求只能排队；接受请求会切换为透传并把串口交给它。")}
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
  const port = snapshot?.ports.find((p) => p.id === id);
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
  return (
    <>
      <PageHeader
        crumbs={[{ label: t("串口"), onClick: onBack }, { label: port.name }]}
        actions={
          <button type="button" className="text-action danger-text serial-remove" onClick={() => setRemoving(true)}>
            {t("从列表移除")}
          </button>
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
          <OwnerChip port={port} />
          <ModeSwitch port={port} onError={setError} />
          <EnableSwitch port={port} onError={setError} />
        </header>
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
          <LogPanel port={port} running={snapshot.running} instanceId={snapshot.instanceId} />
        ) : (
          <div className="serial-overview" role="tabpanel">
            <div className="serial-overview-inner">
              <div className="serial-overview-grid">
                <section className="card serial-panel">
                  <h2>{t("连接信息")}</h2>
                  <dl className="serial-fields">
                    <Field label={t("路径")}>
                      <code>{port.path}</code>
                      {port.present === false && <span className="serial-chip">{t("未接入")}</span>}
                    </Field>
                    <Field label={t("协议")}>
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
                      {port.protocol === "auto" && port.detected && (
                        <span className="serial-chip">
                          {t("识别为 {protocol}", { protocol: protocolLabel(port.detected) })}
                        </span>
                      )}
                    </Field>
                    <Field label={t("波特率")}>
                      {port.protocol === "h2loader" || port.detected === "h2loader" ? (
                        "460800"
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
                {port.mode === "piping" ? (
                  <OccupancyPanel port={port} pipeBase={snapshot.pipeBase} onError={setError} />
                ) : (
                  <H2LoaderPanel port={port} onError={setError} />
                )}
              </div>
              {port.mode === "takeover" && !!port.queue?.length && (
                <OccupancyPanel port={port} pipeBase={snapshot.pipeBase} onError={setError} />
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
