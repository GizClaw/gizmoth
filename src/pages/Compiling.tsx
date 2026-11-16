import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import {
  CaretRight,
  Check,
  CircleNotch,
  Copy,
  FolderOpen,
  Laptop,
  Wrench,
  Package,
  Plus,
  PushPin,
  PushPinSlash,
  UploadSimple,
  X,
} from "@phosphor-icons/react";
import { demo, errorMessage, native } from "../bridge";
import { copyText } from "../clipboard";
import {
  compile,
  compileViews,
  formatDuration,
  formatSize,
  isActive,
  stripAnsi,
  useCompileSnapshot,
  validCacheName,
  validEnvName,
  type CacheSettings,
  type CompileArtifact,
  type CompileCache,
  type CompileJob,
  type CompileJobStatus,
  type CompileSettings,
  type CompileSlot,
  type CompileSlotState,
  type CompileSnapshot,
  type CompileView,
  type SettingsPatch,
} from "../compile";
import { t as translate, useI18n } from "../i18n";
import { serial, useSerialSnapshot, type PackageInfo, type SerialPort } from "../serial";
import {
  Button,
  Dialog,
  Dot,
  IconButton,
  Input,
  PageHeader,
  SearchInput,
  Select,
  relativeTime,
  type InputProps,
} from "../ui";
import { SettingsRow } from "./SettingsRow";
import { PackagesSection } from "./CompilePackages";
import "./Settings.css";
import "./Compiling.css";

const jobLabels: Record<CompileJobStatus, string> = {
  queued: "排队中",
  booting: "启动虚拟机",
  syncing: "同步源码",
  running: "编译中",
  collecting: "收集产物",
  succeeded: "成功",
  failed: "失败",
  cancelled: "已取消",
};
const slotLabels: Record<CompileSlotState, string> = {
  stopped: "已停止",
  starting: "启动中",
  ready: "空闲",
  busy: "编译中",
  stopping: "正在停止",
  failed: "出错",
};

function Status({ label, tone, busy }: { label: string; tone: "ok" | "off" | "warn" | "idle"; busy?: boolean }) {
  return (
    <span className="compile-state">
      {busy ? <CircleNotch size={12} className="spin compile-spinner" aria-hidden="true" /> : <Dot tone={tone} />}
      {label}
    </span>
  );
}
function JobStatus({ job }: { job: Pick<CompileJob, "status" | "position"> }) {
  const { t } = useI18n();
  const tone =
    job.status === "succeeded" ? "ok" : job.status === "failed" ? "warn" : job.status === "cancelled" ? "off" : "idle";
  const label =
    job.status === "queued" && job.position
      ? t("排队第 {position} 位", { position: job.position })
      : t(jobLabels[job.status]);
  return <Status label={label} tone={tone} busy={isActive(job.status) && job.status !== "queued"} />;
}
function SlotStatus({ state }: { state: CompileSlotState }) {
  const { t } = useI18n();
  const tone =
    state === "ready" || state === "busy" ? "ok" : state === "failed" ? "warn" : state === "stopped" ? "off" : "idle";
  return <Status label={t(slotLabels[state])} tone={tone} busy={state === "starting" || state === "stopping"} />;
}

/** Rerenders every second while something is running. */
function useNow(active: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active]);
  return active ? now : Date.now();
}
const basename = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;
const commandLine = (argv: string[]) =>
  argv.map((arg) => (/^[\w./:@=+,-]+$/.test(arg) ? arg : JSON.stringify(arg))).join(" ");
const ratio = (used: number, total: number) => (total > 0 ? Math.min(100, Math.round((used / total) * 100)) : 0);

function Meter({ used, total, label }: { used: number; total: number; label: string }) {
  const percent = ratio(used, total);
  return (
    <div
      className={`compile-meter${percent >= 90 ? " full" : ""}`}
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={percent}
    >
      <span style={{ width: `${total > 0 ? Math.max(percent, used > 0 ? 2 : 0) : 0}%` }} />
    </div>
  );
}

interface Confirmation {
  title: string;
  body: string;
  confirm: string;
  danger?: boolean;
  action: () => Promise<unknown>;
}
function ConfirmDialog({ request, onClose }: { request: Confirmation; onClose: () => void }) {
  const { t } = useI18n();
  const [working, setWorking] = useState(false);
  const [error, setError] = useState("");
  return (
    <Dialog title={request.title} onClose={onClose} busy={working}>
      <p className="dialog-text">{request.body}</p>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <div className="dialog-actions">
        <Button disabled={working} onClick={onClose}>
          {t("取消")}
        </Button>
        <Button
          variant={request.danger ? "danger" : "primary"}
          disabled={working}
          onClick={() => {
            setWorking(true);
            setError("");
            request
              .action()
              .then(onClose)
              .catch((e) => {
                setError(errorMessage(e));
                setWorking(false);
              });
          }}
        >
          {request.confirm}
        </Button>
      </div>
    </Dialog>
  );
}

/** One page per 编译 sidebar entry; `view` comes from the route. */
export function CompilingPage({
  view,
  onView,
  onSerial,
  onSerialPort,
  onStorage,
}: {
  view: CompileView;
  onView: (view: CompileView) => void;
  onSerial: () => void;
  onSerialPort: (id: string) => void;
  /** Settings › Storage, where the working directory is chosen. */
  onStorage: () => void;
}) {
  const { t } = useI18n();
  const { snapshot, error: loadError } = useCompileSnapshot();
  const [error, setError] = useState("");
  const [message, setMessage] = useState<{ text: string; port?: string } | null>(null);
  const [confirming, setConfirming] = useState<Confirmation | null>(null);
  const [highlight, setHighlight] = useState("");
  const viewLabel = compileViews.find((item) => item.id === view)?.label ?? "编译任务";
  const run = useCallback(async (operation: () => Promise<unknown>) => {
    setError("");
    try {
      await operation();
      return true;
    } catch (e) {
      setError(errorMessage(e));
      return false;
    }
  }, []);
  const running = snapshot?.slots.filter((slot) => slot.job).length ?? 0;
  const queued = snapshot?.queue.length ?? 0;
  return (
    <>
      <PageHeader
        crumbs={[{ label: t("编译") }, { label: t(viewLabel) }]}
        actions={
          snapshot && (running || queued) ? (
            <span className="compile-header-status">
              <CircleNotch size={12} className={running ? "spin" : undefined} aria-hidden="true" />
              {t("{running} 个编译中 · {queued} 个排队", { running, queued })}
            </span>
          ) : null
        }
      />
      <div className="page compile-page">
        <div className="page-description">
          <span>
            {t(
              "在本机的 Linux 虚拟机中编译项目。外部 agent 通过 WebSocket 提交编译，断开连接即取消；缓存和产物由所有 slot 共享。",
            )}
          </span>
        </div>
        {demo && <p className="notice">{t("演示模式：编译、缓存和产物仅模拟，不会启动虚拟机或修改本机文件。")}</p>}
        {native && !snapshot && !loadError && (
          <p className="compile-muted compile-loading" role="status">
            {t("正在读取编译服务…")}
          </p>
        )}
        {loadError && !snapshot && (
          <p className="error banner" role="alert">
            {t("无法读取编译服务：{error}", { error: loadError })}
          </p>
        )}
        {snapshot && !snapshot.available.ok && (
          <p className="notice compile-unavailable" role="note">
            <span>{t("此电脑暂时不能运行编译虚拟机。")}</span>
            {snapshot.available.message && <span className="compile-muted">{snapshot.available.message}</span>}
          </p>
        )}
        {error && (
          <p className="error banner compile-banner" role="alert">
            <span>{error}</span>
            <IconButton aria-label={t("关闭")} onClick={() => setError("")}>
              <X size={14} />
            </IconButton>
          </p>
        )}
        {message && (
          <p className="compile-message banner" role="status">
            <span>{message.text}</span>
            {message.port && (
              <Button controlSize="sm" variant="ghost" onClick={() => onSerialPort(message.port!)}>
                {t("查看进度")}
              </Button>
            )}
            <IconButton aria-label={t("关闭")} onClick={() => setMessage(null)}>
              <X size={14} />
            </IconButton>
          </p>
        )}
        {snapshot && (
          <>
            <div className="compile-panel" data-compile-view={view}>
              {view === "builds" && (
                <BuildsTab
                  snapshot={snapshot}
                  run={run}
                  confirm={setConfirming}
                  onArtifact={(id) => {
                    setHighlight(id);
                    onView("artifacts");
                  }}
                />
              )}
              {view === "caches" && (
                <CachesTab snapshot={snapshot} run={run} confirm={setConfirming} onStorage={onStorage} />
              )}
              {view === "artifacts" && (
                <ArtifactsTab
                  snapshot={snapshot}
                  run={run}
                  confirm={setConfirming}
                  highlight={highlight}
                  onInstalled={(text, port) => setMessage({ text, port })}
                  onSerial={onSerial}
                />
              )}
            </div>
          </>
        )}
      </div>
      {confirming && <ConfirmDialog request={confirming} onClose={() => setConfirming(null)} />}
    </>
  );
}

type Run = (operation: () => Promise<unknown>) => Promise<boolean>;
type Confirm = (request: Confirmation) => void;

// ---------------------------------------------------------------- builds

function BuildsTab({
  snapshot,
  run,
  confirm,
  onArtifact,
}: {
  snapshot: CompileSnapshot;
  run: Run;
  confirm: Confirm;
  onArtifact: (id: string) => void;
}) {
  const { t } = useI18n();
  const [resizing, setResizing] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const jobs = useMemo(() => {
    const queued = snapshot.queue;
    const ids = new Set(queued.map((job) => job.id));
    const rest = snapshot.jobs.filter((job) => !ids.has(job.id));
    return [...rest.filter((job) => isActive(job.status)), ...queued, ...rest.filter((job) => !isActive(job.status))];
  }, [snapshot.queue, snapshot.jobs]);
  const activeCount = jobs.filter((job) => isActive(job.status)).length;
  const now = useNow(activeCount > 0);
  const slotName = (id: string | null) => snapshot.slots.find((slot) => slot.id === id)?.name ?? id ?? "—";
  return (
    <>
      <section className="compile-section" aria-labelledby="compile-slots-title">
        <div className="compile-section-head">
          <h2 id="compile-slots-title">{t("编译 slot")}</h2>
          <label className="compile-slot-count">
            <span>{t("Slot 数量")}</span>
            <Select
              controlSize="sm"
              aria-label={t("Slot 数量")}
              value={String(snapshot.slots.length)}
              disabled={resizing || Boolean(snapshot.storage.moving)}
              options={Array.from({ length: 17 }, (_, count) => ({ value: String(count), label: String(count) }))}
              onValueChange={(value) => {
                setResizing(true);
                void run(() => compile.setSlotCount(Number(value))).finally(() => setResizing(false));
              }}
            />
          </label>
          <span className="page-list-summary">
            {t("最多同时编译 {count} 个 · 本机 {cpus} 核 {memory} GB 内存", {
              count: snapshot.slots.length,
              cpus: snapshot.host.cpus,
              memory: Math.round(snapshot.host.memoryMiB / 1024),
            })}
          </span>
        </div>
        {snapshot.slots.length ? (
          <SlotMap slots={snapshot.slots}>
            {snapshot.slots.map((slot) => (
              <SlotCard key={slot.id} slot={slot} now={now} onOpenJob={(id) => setOpen(id)} />
            ))}
          </SlotMap>
        ) : (
          <p className="compile-empty">{t("增加 Slot 数量后即可编译。")}</p>
        )}
      </section>
      <section className="compile-section" aria-labelledby="compile-jobs-title">
        <div className="compile-section-head">
          <h2 id="compile-jobs-title">{t("编译任务")}</h2>
          <span className="page-list-summary">
            {t("{active} 个进行中 · 最近 {count} 个", { active: activeCount, count: jobs.length })}
          </span>
        </div>
        {jobs.length ? (
          <div className="card compile-jobs">
            {jobs.map((job) => (
              <JobRow
                key={job.id}
                job={job}
                now={now}
                open={open === job.id}
                slotName={slotName(job.slot)}
                onToggle={() => setOpen((current) => (current === job.id ? null : job.id))}
                onArtifact={onArtifact}
                onCancel={() =>
                  confirm({
                    title: t("取消编译"),
                    body: t("取消「{title}」？提交它的 agent 会收到已取消的结果。", { title: job.title }),
                    confirm: t("取消编译"),
                    danger: true,
                    action: () => compile.cancelJob(job.id),
                  })
                }
              />
            ))}
          </div>
        ) : (
          <p className="compile-empty">
            {t(
              "还没有编译任务。agent 通过 compile_pipe 找到编译 WebSocket 并发送 build 请求后，会在这里排队并显示日志。",
            )}
          </p>
        )}
      </section>
    </>
  );
}

function SlotMap({ slots, children }: { slots: CompileSlot[]; children: ReactNode }) {
  const { t } = useI18n();
  const root = useRef<HTMLDivElement>(null);
  const [layout, setLayout] = useState({ height: 0, targets: [] as number[] });
  const ids = slots.map((slot) => slot.id).join(",");
  useEffect(() => {
    const element = root.current;
    if (!element) return;
    const measure = () => {
      const bounds = element.getBoundingClientRect();
      const targets = [...element.querySelectorAll<HTMLElement>(".compile-slot-machine")].map((node) => {
        const rect = node.getBoundingClientRect();
        return rect.top - bounds.top + rect.height / 2;
      });
      setLayout((previous) =>
        previous.height === bounds.height && previous.targets.join() === targets.join()
          ? previous
          : { height: bounds.height, targets },
      );
    };
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    element.querySelectorAll(".compile-slot").forEach((node) => observer.observe(node));
    measure();
    return () => observer.disconnect();
  }, [ids]);
  return (
    <div className="compile-slot-map" ref={root}>
      <div className="compile-slot-host">
        <Laptop size={30} weight="light" aria-hidden="true" />
        <span>{t("这台电脑")}</span>
      </div>
      {layout.height > 0 && (
        <svg
          className="compile-slot-wires"
          viewBox={`0 0 104 ${layout.height}`}
          preserveAspectRatio="none"
          aria-hidden="true"
        >
          {layout.targets.map((y, index) => (
            <path
              key={slots[index]?.id ?? index}
              className={slots[index]?.state === "busy" ? "is-working" : undefined}
              d={`M 0 ${layout.height / 2} C 48 ${layout.height / 2}, 52 ${y}, 104 ${y}`}
              vectorEffect="non-scaling-stroke"
            />
          ))}
        </svg>
      )}
      <div className="compile-slots">{children}</div>
    </div>
  );
}

/** A quiet workstation at rest; only an occupied slot turns its tools. */
function SlotMachine({ active }: { active: boolean }) {
  return (
    <span className={`compile-slot-machine${active ? " is-working" : ""}`} aria-hidden="true">
      <span className="compile-slot-wrench compile-slot-wrench-first">
        <Wrench size={30} weight="light" />
      </span>
      <span className="compile-slot-wrench compile-slot-wrench-second">
        <Wrench size={30} weight="light" />
      </span>
      <svg className="compile-slot-screw" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.4">
        <path d="M12 3 20 7.5v9L12 21l-8-4.5v-9Z" />
        <path d="m9 9 6 6m0-6-6 6" strokeLinecap="round" />
      </svg>
    </span>
  );
}

function SlotCard({ slot, now, onOpenJob }: { slot: CompileSlot; now: number; onOpenJob: (id: string) => void }) {
  return (
    <article className={`compile-slot compile-slot-${slot.state}`} aria-label={slot.name}>
      <SlotMachine active={slot.state === "busy"} />
      <div className="compile-slot-body">
        <div className="compile-slot-head">
          <h3 title={slot.name}>{slot.name}</h3>
          <SlotStatus state={slot.state} />
        </div>
        {slot.job && (
          <button type="button" className="compile-slot-current" onClick={() => onOpenJob(slot.job!.id)}>
            <span className="compile-slot-title" title={slot.job.title}>
              {slot.job.title}
            </span>
            <span className="compile-muted">
              {slot.job.client} · {translate(jobLabels[slot.job.status])}
              {slot.job.startedAt ? ` · ${formatDuration(now - slot.job.startedAt)}` : ""}
            </span>
          </button>
        )}
        {slot.error && (
          <p className="error" role="alert">
            {slot.error}
          </p>
        )}
      </div>
    </article>
  );
}

function exitText(job: CompileJob) {
  if (!job.exit) return "—";
  if (job.exit.timedOut) return translate("超时");
  if (job.exit.code !== null) return String(job.exit.code);
  if (job.exit.signal !== null) return translate("信号 {signal}", { signal: job.exit.signal });
  return "—";
}

function JobRow({
  job,
  now,
  open,
  slotName,
  onToggle,
  onArtifact,
  onCancel,
}: {
  job: CompileJob;
  now: number;
  open: boolean;
  slotName: string;
  onToggle: () => void;
  onArtifact: (id: string) => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  const active = isActive(job.status);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (open) ref.current?.scrollIntoView({ block: "nearest" });
  }, [open]);
  const time = active
    ? job.startedAt
      ? formatDuration(now - job.startedAt)
      : t("已等待 {duration}", { duration: formatDuration(now - job.createdAt) })
    : job.finishedAt && job.startedAt
      ? formatDuration(job.finishedAt - job.startedAt)
      : "—";
  const fields: [string, ReactNode][] = [
    [t("工作区"), <code key="w">{job.workspace}</code>],
    [t("命令"), <code key="c">{commandLine(job.command)}</code>],
    [t("工作目录"), job.cwd ? <code key="d">{job.cwd}</code> : "—"],
    [t("Slot"), job.slot ? slotName : "—"],
    [t("提交"), `${new Date(job.createdAt).toLocaleString()}`],
    [t("结束"), job.finishedAt ? new Date(job.finishedAt).toLocaleString() : "—"],
    [t("退出码"), exitText(job)],
    [
      t("同步"),
      job.synced
        ? t("{files} 个文件更新 · {bytes} · {removed} 个删除", {
            files: job.synced.files,
            bytes: formatSize(job.synced.bytes),
            removed: job.synced.removed,
          })
        : "—",
    ],
  ];
  return (
    <div className={`compile-job${open ? " open" : ""}`} ref={ref}>
      <button type="button" className="compile-job-row" aria-expanded={open} onClick={onToggle}>
        <span className="compile-job-status">
          <JobStatus job={job} />
        </span>
        <span className="compile-job-main">
          <span className="compile-job-title">{job.title}</span>
          <span className="compile-job-meta">
            {job.client} · {basename(job.workspace)} · <code>{commandLine(job.command)}</code>
          </span>
        </span>
        <span
          className="compile-job-time"
          title={job.finishedAt ? new Date(job.finishedAt).toLocaleString() : undefined}
        >
          {time}
          {!active && job.finishedAt && <span className="compile-muted">{relativeTime(job.finishedAt)}</span>}
        </span>
        <CaretRight size={14} className="compile-chevron" aria-hidden="true" />
      </button>
      {open && (
        <div className="compile-job-detail">
          <dl className="compile-fields">
            {fields.map(([label, value]) => (
              <div key={label}>
                <dt>{label}</dt>
                <dd>{value}</dd>
              </div>
            ))}
            {job.artifacts.length > 0 && (
              <div>
                <dt>{t("产物规则")}</dt>
                <dd>
                  {job.artifacts.map((pattern) => (
                    <code key={pattern} className={job.missing.includes(pattern) ? "compile-missing" : undefined}>
                      {pattern}
                    </code>
                  ))}
                  {job.missing.length > 0 && (
                    <span className="compile-muted">
                      {t("{count} 条规则没有匹配到文件", { count: job.missing.length })}
                    </span>
                  )}
                </dd>
              </div>
            )}
          </dl>
          {job.error && (
            <p className="error" role="alert">
              {translate(job.error)}
            </p>
          )}
          <div className="compile-job-actions">
            {job.artifact && (
              <Button controlSize="sm" onClick={() => onArtifact(job.artifact!)}>
                {t("查看产物")}
              </Button>
            )}
            {active && (
              <Button controlSize="sm" variant="ghost" className="compile-danger" onClick={onCancel}>
                {t("取消编译")}
              </Button>
            )}
          </div>
          <LogView source={job.id} live={active} />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- logs

const LOG_LIMIT = 512 * 1024;
/** Follows a job log page by page; keeps the newest 512 KB. */
function LogView({ source, live }: { source: string; live: boolean }) {
  const { t } = useI18n();
  const [text, setText] = useState("");
  const [failed, setFailed] = useState("");
  const [trimmed, setTrimmed] = useState(false);
  const next = useRef<number | undefined>(undefined);
  const pre = useRef<HTMLPreElement>(null);
  const pinned = useRef(true);
  useEffect(() => {
    let disposed = false;
    let timer = 0;
    const load = async () => {
      try {
        for (let page = 0; page < 8 && !disposed; page++) {
          const chunk = await compile.jobLog(source, next.current, 256 * 1024);
          if (disposed) return;
          const more = chunk.next !== next.current && chunk.text.length > 0;
          if (chunk.truncated) setTrimmed(true);
          next.current = chunk.next;
          if (chunk.text) {
            setText((previous) => {
              const combined = previous + stripAnsi(chunk.text);
              if (combined.length <= LOG_LIMIT) return combined;
              setTrimmed(true);
              const cut = combined.indexOf("\n", combined.length - LOG_LIMIT);
              return combined.slice(cut < 0 ? combined.length - LOG_LIMIT : cut + 1);
            });
          }
          setFailed("");
          if (!more || chunk.text.length < 64 * 1024) break;
        }
      } catch (e) {
        if (!disposed) setFailed(errorMessage(e));
      }
      if (!disposed && live) timer = window.setTimeout(() => void load(), 1000);
    };
    void load();
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [source, live]);
  useEffect(() => {
    const element = pre.current;
    if (element && pinned.current) element.scrollTop = element.scrollHeight;
  }, [text]);
  const lines = text.endsWith("\n") ? text.slice(0, -1).split("\n") : text.split("\n");
  return (
    <div className="compile-log-wrap">
      {trimmed && <p className="compile-muted compile-log-note-line">{t("只显示最新的日志。")}</p>}
      <pre
        ref={pre}
        className="compile-log"
        tabIndex={0}
        aria-label={t("日志")}
        onScroll={(event) => {
          const element = event.currentTarget;
          pinned.current = element.scrollHeight - element.scrollTop - element.clientHeight < 24;
        }}
      >
        {text
          ? lines.map((line, index) => (
              <span key={index} className={line.startsWith("[gizmoth]") ? "compile-log-note" : undefined}>
                {line}
                {"\n"}
              </span>
            ))
          : t(live ? "等待输出…" : "没有日志。")}
      </pre>
      {failed && (
        <p className="error" role="alert">
          {t("无法读取日志：{error}", { error: failed })}
        </p>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- caches

const cacheTitles: Record<CompileCache["kind"], string> = {
  bazel: "Bazel 远程缓存",
  ccache: "ccache",
  custom: "自定义缓存",
};
function cacheDescription(cache: CompileCache, settings: CompileSettings) {
  if (cache.kind === "bazel") return translate("编译动作结果（AC/CAS），各 slot 的 Bazel 自动使用");
  if (cache.kind === "ccache")
    return translate("GCC/Clang 编译结果，所有 slot 共享 · 每个 slot 另有 {local} GB 本地缓存", {
      local: settings.caches.ccache.localGib,
    });
  return translate("编译命令通过环境变量 {env} 获得缓存地址", { env: cache.env ?? "" });
}

function CachesTab({
  snapshot,
  run,
  confirm,
  onStorage,
}: {
  snapshot: CompileSnapshot;
  run: Run;
  confirm: Confirm;
  onStorage: () => void;
}) {
  const { t } = useI18n();
  const [editing, setEditing] = useState<CompileCache | "new" | null>(null);
  const caches = snapshot.settings.caches;
  const total = snapshot.caches.reduce((sum, cache) => sum + cache.bytes, 0);
  const root = snapshot.storage.root;
  return (
    <>
      <div className="page-list-toolbar">
        <button
          type="button"
          className="text-action"
          disabled={caches.custom.length >= 16}
          onClick={() => setEditing("new")}
        >
          <Plus size={14} aria-hidden="true" />
          {t("添加自定义缓存")}
        </button>
        <button type="button" className="text-action" onClick={() => void run(() => compile.reveal(`${root}/cache`))}>
          <FolderOpen size={14} aria-hidden="true" />
          {t("在访达中显示")}
        </button>
        <span className="page-list-summary">
          {t("共 {size}，保存在工作目录", { size: formatSize(total) })}
          <button type="button" className="link-button compile-storage-link" onClick={onStorage}>
            {t("更改位置")}
          </button>
        </span>
      </div>
      <div className="card compile-table-wrap">
        <table className="compile-table compile-cache-table">
          <thead>
            <tr>
              <th scope="col">{t("缓存")}</th>
              <th scope="col">{t("已用 / 上限")}</th>
              <th scope="col">{t("文件")}</th>
              <th scope="col">{t("命中率")}</th>
              <th scope="col">
                <span className="compile-sr-only">{t("操作")}</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {snapshot.caches.map((cache) => {
              const lookups = cache.hits + cache.misses;
              const label = cache.kind === "custom" ? cache.name : t(cacheTitles[cache.kind]);
              return (
                <tr key={cache.name}>
                  <th scope="row">
                    <span className="compile-name">{label}</span>
                    <span className="compile-description">{cacheDescription(cache, snapshot.settings)}</span>
                  </th>
                  <td className="compile-usage-cell">
                    <span className="compile-number">
                      {formatSize(cache.bytes)} / {cache.maxBytes ? formatSize(cache.maxBytes) : t("不限")}
                    </span>
                    {cache.maxBytes > 0 && (
                      <Meter used={cache.bytes} total={cache.maxBytes} label={t("{name} 用量", { name: label })} />
                    )}
                  </td>
                  <td className="compile-number">{cache.files.toLocaleString()}</td>
                  <td
                    className="compile-number"
                    title={t("命中 {hits} · 未命中 {misses} · 上传 {uploads}", {
                      hits: cache.hits,
                      misses: cache.misses,
                      uploads: cache.uploads,
                    })}
                  >
                    {lookups ? `${Math.round((cache.hits / lookups) * 100)}%` : "—"}
                  </td>
                  <td className="compile-actions">
                    <div className="compile-row-actions">
                      <Button
                        controlSize="sm"
                        variant="ghost"
                        aria-label={`${t("设置")} · ${label}`}
                        onClick={() => setEditing(cache)}
                      >
                        {t("设置")}
                      </Button>
                      <Button
                        controlSize="sm"
                        variant="ghost"
                        disabled={!cache.bytes}
                        aria-label={`${t("清空")} · ${label}`}
                        onClick={() =>
                          confirm({
                            title: t("清空 {name}", { name: label }),
                            body: t("将删除这个缓存的全部内容（{size}）。之后的编译会重新生成并上传结果。", {
                              size: formatSize(cache.bytes),
                            }),
                            confirm: t("清空"),
                            danger: true,
                            action: () => compile.clearCache(cache.name),
                          })
                        }
                      >
                        {t("清空")}
                      </Button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      <p className="compile-hint">
        {t(
          "缓存通过各虚拟机内的 127.0.0.1:7421 访问，数据保存在本机，不会上传到其他电脑。命中次数从服务本次启动开始统计；超过上限时自动删除最久未使用的条目。",
        )}
      </p>
      {editing && (
        <CacheDialog
          cache={editing === "new" ? null : editing}
          settings={snapshot.settings}
          onClose={() => setEditing(null)}
          onRemove={(name) =>
            confirm({
              title: t("移除 {name}", { name }),
              body: t("将删除这个缓存的全部内容并从设置中移除。使用它的环境变量不会再提供给编译命令。"),
              confirm: t("移除"),
              danger: true,
              action: async () => {
                await compile.clearCache(name);
                await compile.patchSettings({
                  caches: { ...caches, custom: caches.custom.filter((item) => item.name !== name) },
                });
              },
            })
          }
        />
      )}
    </>
  );
}

function CacheDialog({
  cache,
  settings,
  onClose,
  onRemove,
}: {
  cache: CompileCache | null;
  settings: CompileSettings;
  onClose: () => void;
  onRemove: (name: string) => void;
}) {
  const { t } = useI18n();
  const field = useId();
  const kind = cache?.kind ?? "custom";
  const current = settings.caches;
  const custom = cache?.kind === "custom" ? current.custom.find((item) => item.name === cache.name) : undefined;
  const limit =
    kind === "bazel" ? current.bazel.maxGib : kind === "ccache" ? current.ccache.maxGib : (custom?.maxGib ?? 10);
  const [name, setName] = useState(custom?.name ?? "");
  const [env, setEnv] = useState(custom?.env ?? "");
  const [maxGib, setMaxGib] = useState(String(limit));
  const [localGib, setLocalGib] = useState(String(current.ccache.localGib));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const max = Number(maxGib);
  const local = Number(localGib);
  const invalid =
    kind === "custom" && !validCacheName(name)
      ? t("名称只能使用小写字母、数字和连字符，最多 32 个字符。")
      : kind === "custom" && !cache && ["bazel", "ccache", ...current.custom.map((item) => item.name)].includes(name)
        ? t("已有同名缓存。")
        : kind === "custom" && !validEnvName(env)
          ? t("环境变量名只能使用字母、数字和下划线，且不能以数字开头。")
          : !(Number.isInteger(max) && max >= 0)
            ? t("上限需要是 0 或正整数（0 表示不限）。")
            : kind === "ccache" && !(Number.isInteger(local) && local >= 1)
              ? t("本地缓存至少需要 1 GB。")
              : "";
  async function save() {
    if (invalid) return;
    setSaving(true);
    setError("");
    const next: CacheSettings = structuredClone(current);
    if (kind === "bazel") next.bazel = { maxGib: max };
    else if (kind === "ccache") next.ccache = { maxGib: max, localGib: local };
    else {
      const item = { name, env, maxGib: max };
      next.custom = cache
        ? next.custom.map((entry) => (entry.name === cache.name ? item : entry))
        : [...next.custom, item];
    }
    try {
      await compile.patchSettings({ caches: next });
      onClose();
    } catch (e) {
      setError(errorMessage(e));
      setSaving(false);
    }
  }
  const title = cache
    ? t("{name} 设置", { name: cache.kind === "custom" ? cache.name : t(cacheTitles[cache.kind]) })
    : t("添加自定义缓存");
  return (
    <Dialog title={title} onClose={onClose} busy={saving}>
      <p className="dialog-text">
        {kind === "bazel"
          ? t("各 slot 的 Bazel 通过 --remote_cache 共享编译结果。修改会在空闲虚拟机下次启动时生效。")
          : kind === "ccache"
            ? t("ccache 先查 slot 宿主工作目录中的本地缓存，再查所有 slot 共享的缓存。")
            : t(
                "GizMoth 为编译命令提供一个 HTTP 键值缓存，地址通过你指定的环境变量传入，可用于 sccache、Gradle 等工具。",
              )}
      </p>
      <form
        className="compile-form"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        {kind === "custom" && (
          <>
            <label htmlFor={`${field}-name`}>{t("名称")}</label>
            <Input
              id={`${field}-name`}
              value={name}
              maxLength={32}
              placeholder="sccache"
              disabled={saving || Boolean(cache)}
              onChange={(e) => setName(e.target.value.trim())}
            />
            <label htmlFor={`${field}-env`}>{t("变量名")}</label>
            <Input
              id={`${field}-env`}
              value={env}
              maxLength={128}
              placeholder="SCCACHE_WEBDAV_ENDPOINT"
              disabled={saving}
              onChange={(e) => setEnv(e.target.value.trim())}
            />
          </>
        )}
        <label htmlFor={`${field}-max`}>{t("上限")}</label>
        <span className="compile-unit-field">
          <Input
            id={`${field}-max`}
            type="number"
            inputMode="numeric"
            min={0}
            value={maxGib}
            disabled={saving}
            onChange={(e) => setMaxGib(e.target.value)}
          />
          <span>GB</span>
        </span>
        {kind === "ccache" && (
          <>
            <label htmlFor={`${field}-local`}>{t("每个 slot")}</label>
            <span className="compile-unit-field">
              <Input
                id={`${field}-local`}
                type="number"
                inputMode="numeric"
                min={1}
                value={localGib}
                disabled={saving}
                onChange={(e) => setLocalGib(e.target.value)}
              />
              <span>GB</span>
            </span>
          </>
        )}
        <p className="compile-form-hint compile-muted">{t("上限为 0 表示不限制；超过上限时删除最久未使用的条目。")}</p>
        {(invalid || error) && (
          <p className="error compile-form-hint" role="alert">
            {error || invalid}
          </p>
        )}
        <div className="dialog-actions">
          {cache?.kind === "custom" && (
            <Button
              className="compile-danger compile-dialog-remove"
              variant="ghost"
              disabled={saving}
              onClick={() => {
                onClose();
                onRemove(cache.name);
              }}
            >
              {t("移除")}
            </Button>
          )}
          <Button disabled={saving} onClick={onClose}>
            {t("取消")}
          </Button>
          <Button type="submit" variant="primary" disabled={saving || Boolean(invalid)}>
            {t(cache ? "保存" : "添加")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

// ---------------------------------------------------------------- artifacts

const isPackage = (path: string) => path.endsWith(".update.tar.zlib");

const ARTIFACT_PAGE = 50;

function ArtifactsTab({
  snapshot,
  run,
  confirm,
  highlight,
  onInstalled,
  onSerial,
}: {
  snapshot: CompileSnapshot;
  run: Run;
  confirm: Confirm;
  highlight: string;
  onInstalled: (text: string, port: string) => void;
  onSerial: () => void;
}) {
  const { t } = useI18n();
  const [artifacts, setArtifacts] = useState<CompileArtifact[] | null>(null);
  const [failed, setFailed] = useState("");
  const [query, setQuery] = useState("");
  const [limit, setLimit] = useState(ARTIFACT_PAGE);
  // Opening an artifact from a build shows its details directly.
  const [detail, setDetail] = useState<string | null>(highlight || null);
  const [installing, setInstalling] = useState<CompileArtifact | null>(null);
  const reload = useCallback(async () => {
    try {
      setArtifacts(await compile.artifacts());
      setFailed("");
    } catch (e) {
      setFailed(errorMessage(e));
    }
  }, []);
  const key = `${snapshot.artifacts.count}:${snapshot.artifacts.bytes}`;
  useEffect(() => {
    void reload();
  }, [reload, key]);
  useEffect(() => {
    if (!highlight || !artifacts) return;
    document.getElementById(`artifact-${highlight}`)?.scrollIntoView({ block: "center" });
  }, [highlight, artifacts]);
  const needle = query.trim().toLowerCase();
  const visible = (artifacts ?? []).filter(
    (artifact) =>
      !needle ||
      [artifact.title, artifact.client, artifact.workspace, ...artifact.files.map((file) => file.path)].some((text) =>
        text.toLowerCase().includes(needle),
      ),
  );
  const shown = visible.slice(0, limit);
  const selected = detail ? artifacts?.find((artifact) => artifact.id === detail) : undefined;
  const policy = snapshot.settings.artifacts;
  const mutate = (operation: () => Promise<unknown>) =>
    void run(async () => {
      await operation();
      await reload();
    });
  const remove = (artifact: CompileArtifact) =>
    confirm({
      title: t("删除产物"),
      body: t("将删除「{title}」的 {count} 个文件（{size}）。", {
        title: artifact.title,
        count: artifact.files.length,
        size: formatSize(artifact.bytes),
      }),
      confirm: t("删除"),
      danger: true,
      action: async () => {
        await compile.deleteArtifact(artifact.id);
        await reload();
      },
    });
  return (
    <>
      <div className="page-list-toolbar">
        <button
          type="button"
          className="text-action"
          onClick={() => void run(() => compile.reveal(snapshot.artifacts.dir))}
        >
          <FolderOpen size={14} aria-hidden="true" />
          {t("在访达中显示")}
        </button>
        <div className="page-list-tools">
          <SearchInput
            className="compile-search"
            controlSize="sm"
            aria-label={t("搜索产物")}
            placeholder={t("搜索产物")}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setLimit(ARTIFACT_PAGE);
            }}
            onClear={() => setQuery("")}
          />
        </div>
        <span
          className="page-list-summary"
          title={t("自动保留最近 {keep} 个、最多 {size}；固定的产物不会被自动删除。", {
            keep: policy.keepLast,
            size: policy.maxGib ? `${policy.maxGib} GB` : t("不限"),
          })}
        >
          {t("{count} 个 · {size}", { count: snapshot.artifacts.count, size: formatSize(snapshot.artifacts.bytes) })}
        </span>
      </div>
      {failed && (
        <p className="error banner" role="alert">
          {t("无法读取产物：{error}", { error: failed })}
        </p>
      )}
      {artifacts && !artifacts.length && (
        <p className="compile-empty">
          {t("还没有产物。编译请求带上 artifacts 规则（例如 bazel-bin/**/*.update.tar.zlib），成功后会在这里保存。")}
        </p>
      )}
      {shown.length > 0 && (
        <ul className="card compile-artifact-list" aria-label={t("产物")}>
          {shown.map((artifact) => {
            const packages = artifact.files.filter((file) => isPackage(file.path));
            const primary = packages[0] ?? artifact.files[0];
            const more = artifact.files.length - 1;
            const meta = [
              primary && `${basename(primary.path)}${more > 0 ? ` +${more}` : ""}`,
              formatSize(artifact.bytes),
            ]
              .filter(Boolean)
              .join(" · ");
            const time = relativeTime(artifact.createdAt);
            return (
              <li
                key={artifact.id}
                id={`artifact-${artifact.id}`}
                className={highlight === artifact.id ? "compile-highlight" : undefined}
              >
                <button
                  type="button"
                  className="compile-artifact-row"
                  aria-label={`${artifact.title}，${meta}，${time}`}
                  onClick={() => setDetail(artifact.id)}
                >
                  <Package size={18} aria-hidden="true" />
                  <strong className="project-row-name" title={artifact.title}>
                    {artifact.title}
                  </strong>
                  {artifact.pinned && (
                    <PushPin size={12} weight="fill" className="compile-pin" aria-label={t("已固定")} />
                  )}
                  <span className="project-row-meta">{meta}</span>
                  <span className="project-row-time">{time}</span>
                </button>
                <span className="compile-artifact-action">
                  {packages.length > 0 && (
                    <Button
                      controlSize="sm"
                      aria-label={t("安装 {title}", { title: artifact.title })}
                      onClick={() => setInstalling(artifact)}
                    >
                      <UploadSimple size={14} aria-hidden="true" />
                      <span>{t("安装")}</span>
                    </Button>
                  )}
                </span>
              </li>
            );
          })}
        </ul>
      )}
      {visible.length > shown.length && (
        <button type="button" className="text-action compile-more" onClick={() => setLimit((n) => n + ARTIFACT_PAGE)}>
          {t("再显示 {count} 个", { count: Math.min(ARTIFACT_PAGE, visible.length - shown.length) })}
        </button>
      )}
      {artifacts && artifacts.length > 0 && !visible.length && <p className="compile-empty">{t("没有匹配的产物。")}</p>}
      {selected && (
        <ArtifactDialog
          artifact={selected}
          onClose={() => setDetail(null)}
          onInstall={() => {
            setDetail(null);
            setInstalling(selected);
          }}
          onPin={() => mutate(() => compile.pinArtifact(selected.id, !selected.pinned))}
          onReveal={() => void run(() => compile.reveal(selected.dir))}
          onDelete={() => {
            setDetail(null);
            remove(selected);
          }}
        />
      )}
      {installing && (
        <InstallDialog
          artifact={installing}
          onClose={() => setInstalling(null)}
          onSerial={onSerial}
          onStarted={(text, port) => {
            setInstalling(null);
            onInstalled(text, port);
          }}
        />
      )}
    </>
  );
}

/** Paths, checksums and the ID stay here, out of the list. */
function ArtifactDialog({
  artifact,
  onClose,
  onInstall,
  onPin,
  onReveal,
  onDelete,
}: {
  artifact: CompileArtifact;
  onClose: () => void;
  onInstall: () => void;
  onPin: () => void;
  onReveal: () => void;
  onDelete: () => void;
}) {
  const { t } = useI18n();
  const [copied, setCopied] = useState("");
  const [error, setError] = useState("");
  const copy = (key: string, text: string) =>
    void copyText(text)
      .then(() => setCopied(key))
      .catch((e) => setError(errorMessage(e)));
  const packages = artifact.files.filter((file) => isPackage(file.path));
  return (
    <Dialog title={artifact.title} onClose={onClose} wide className="compile-artifact-dialog">
      <p className="compile-artifact-summary">
        {artifact.client} · {new Date(artifact.createdAt).toLocaleString()} · {formatSize(artifact.bytes)}
        {artifact.pinned && ` · ${t("已固定")}`}
      </p>
      <ul className="compile-artifact-files" aria-label={t("文件")}>
        {artifact.files.map((file) => (
          <li key={file.path}>
            <span className="compile-artifact-file" title={`${file.absolutePath}\nsha256 ${file.sha256}`}>
              {file.path}
            </span>
            <span className="compile-number">{formatSize(file.size)}</span>
            <IconButton
              aria-label={t("复制 {file} 的完整路径", { file: basename(file.path) })}
              title={t(copied === file.path ? "已复制" : "复制完整路径")}
              onClick={() => copy(file.path, file.absolutePath)}
            >
              {copied === file.path ? <Check size={14} /> : <Copy size={14} />}
            </IconButton>
          </li>
        ))}
      </ul>
      <dl className="compile-artifact-meta">
        <div>
          <dt>{t("产物 ID")}</dt>
          <dd>
            <code>{artifact.id}</code>
            <IconButton
              aria-label={t("复制产物 ID")}
              title={t(copied === artifact.id ? "已复制" : "复制产物 ID")}
              onClick={() => copy(artifact.id, artifact.id)}
            >
              {copied === artifact.id ? <Check size={14} /> : <Copy size={14} />}
            </IconButton>
          </dd>
        </div>
        <div>
          <dt>{t("工作区")}</dt>
          <dd>
            <code>{artifact.workspace}</code>
          </dd>
        </div>
        <div>
          <dt>{t("命令")}</dt>
          <dd>
            <code>{commandLine(artifact.command)}</code>
          </dd>
        </div>
      </dl>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <div className="dialog-actions compile-artifact-dialog-actions">
        <Button variant="ghost" className="compile-danger push-left" onClick={onDelete}>
          {t("删除")}
        </Button>
        <Button onClick={onReveal}>
          <FolderOpen size={14} aria-hidden="true" />
          {t("在访达中显示")}
        </Button>
        <Button onClick={onPin}>
          {artifact.pinned ? <PushPinSlash size={14} aria-hidden="true" /> : <PushPin size={14} aria-hidden="true" />}
          {t(artifact.pinned ? "取消固定" : "固定")}
        </Button>
        {packages.length > 0 && (
          <Button variant="primary" onClick={onInstall}>
            <UploadSimple size={14} aria-hidden="true" />
            {t("安装到设备…")}
          </Button>
        )}
      </div>
    </Dialog>
  );
}

function portProblem(port: SerialPort) {
  if (port.remote?.stale) return "所在 GizMoth 未连接";
  if (port.connection !== "connected") return "未连接";
  if (port.detected !== "h2loader") return "不是 H2Loader 设备";
  if (port.job) return "正在执行任务";
  return "";
}

function InstallDialog({
  artifact,
  onClose,
  onSerial,
  onStarted,
}: {
  artifact: CompileArtifact;
  onClose: () => void;
  onSerial: () => void;
  onStarted: (text: string, port: string) => void;
}) {
  const { t } = useI18n();
  const field = useId();
  const { snapshot } = useSerialSnapshot();
  const packages = artifact.files.filter((file) => isPackage(file.path));
  const ports = useMemo(
    () => [...(snapshot?.ports ?? []), ...(snapshot?.nodes ?? []).flatMap((node) => node.ports)],
    [snapshot],
  );
  const [file, setFile] = useState(packages[0]?.absolutePath ?? "");
  const [portId, setPortId] = useState("");
  const [info, setInfo] = useState<PackageInfo | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    if (!portId) setPortId(ports.find((port) => !portProblem(port))?.id ?? ports[0]?.id ?? "");
  }, [ports, portId]);
  useEffect(() => {
    let current = true;
    setInfo(null);
    if (!file) return;
    serial
      .inspect(file)
      .then((value) => current && setInfo(value))
      .catch((e) => current && setError(errorMessage(e)));
    return () => {
      current = false;
    };
  }, [file]);
  const port = ports.find((candidate) => candidate.id === portId);
  const problem = port ? portProblem(port) : "";
  const status = port?.h2loader;
  const mismatch = info && status && (info.board !== status.board || info.target !== status.target);
  async function start() {
    if (!port) return;
    setStarting(true);
    setError("");
    try {
      await serial.job(port.id, "install", file);
      onStarted(t("已开始在「{port}」上安装 {file}。", { port: port.name, file: basename(file) }), port.id);
    } catch (e) {
      setError(errorMessage(e));
      setStarting(false);
    }
  }
  return (
    <Dialog title={t("安装到设备")} onClose={onClose} busy={starting}>
      <p className="dialog-text">{t("通过 GizMoth 管理的串口安装产物中的固件包。过程中请勿断开设备。")}</p>
      {ports.length ? (
        <div className="compile-form">
          {packages.length > 1 && (
            <>
              <label htmlFor={`${field}-file`}>{t("固件包")}</label>
              <Select
                id={`${field}-file`}
                aria-label={t("固件包")}
                value={file}
                disabled={starting}
                onValueChange={setFile}
                options={packages.map((item) => ({ value: item.absolutePath, label: item.path }))}
              />
            </>
          )}
          <label htmlFor={`${field}-port`}>{t("目标串口")}</label>
          <Select
            id={`${field}-port`}
            aria-label={t("目标串口")}
            value={portId}
            disabled={starting}
            onValueChange={setPortId}
            options={ports.map((item) => {
              const reason = portProblem(item);
              const board = item.h2loader ? ` · ${item.h2loader.board} ${item.h2loader.target}` : "";
              return {
                value: item.id,
                label: `${item.name}${board}${reason ? ` · ${t(reason)}` : ""}`,
                disabled: Boolean(reason),
              };
            })}
          />
          <dl className="compile-fields compile-form-hint">
            <div>
              <dt>{t("固件包")}</dt>
              <dd>{info?.name ?? basename(file)}</dd>
            </div>
            <div>
              <dt>{t("类型与版本")}</dt>
              <dd>{info ? `${info.role === "loader" ? "Loader" : "App"} ${info.version}` : "—"}</dd>
            </div>
            <div>
              <dt>{t("适用设备")}</dt>
              <dd>{info ? `${info.board} · ${info.target}` : "—"}</dd>
            </div>
          </dl>
          {mismatch && (
            <p className="error compile-form-hint" role="alert">
              {t("固件包适用于 {board} {target}，与设备 {device} 不匹配。", {
                board: info.board,
                target: info.target,
                device: `${status.board} ${status.target}`,
              })}
            </p>
          )}
          {problem && (
            <p className="compile-form-hint compile-muted">
              {t("这个串口暂时不能安装：{reason}。", { reason: t(problem) })}
            </p>
          )}
          {error && (
            <p className="error compile-form-hint" role="alert">
              {error}
            </p>
          )}
        </div>
      ) : (
        <p className="dialog-text compile-muted">{t("还没有可用的串口。在串口页面添加设备后再安装。")}</p>
      )}
      <div className="dialog-actions">
        {!ports.length ? (
          <>
            <Button onClick={onClose}>{t("取消")}</Button>
            <Button variant="primary" onClick={onSerial}>
              {t("前往串口")}
            </Button>
          </>
        ) : (
          <>
            <Button disabled={starting} onClick={onClose}>
              {t("取消")}
            </Button>
            <Button
              variant="primary"
              disabled={starting || !port || Boolean(problem) || Boolean(mismatch) || !file}
              onClick={() => void start()}
            >
              {t(starting ? "正在开始…" : "开始安装")}
            </Button>
          </>
        )}
      </div>
    </Dialog>
  );
}

// ---------------------------------------------------------------- settings

function UnitInput({ unit, ...props }: InputProps & { unit: string }) {
  return (
    <span className="compile-inset-unit">
      <Input controlSize="sm" {...props} />
      {unit && <span aria-hidden="true">{unit}</span>}
    </span>
  );
}

interface Draft {
  autoStopIdle: boolean;
  vmCpus: string;
  vmMemory: string;
  idleMinutes: string;
  keepLast: string;
  artifactsGib: string;
}
const draftOf = (settings: CompileSettings, host: CompileSnapshot["host"]): Draft => ({
  autoStopIdle: settings.autoStopIdle ?? true,
  vmCpus: String(settings.vmResources?.cpus ?? host.cpus),
  vmMemory: String((settings.vmResources?.memoryMiB ?? host.memoryMiB / 2) / 1024),
  idleMinutes: String(settings.idleMinutes),
  keepLast: String(settings.artifacts.keepLast),
  artifactsGib: String(settings.artifacts.maxGib),
});
/** Converts a draft into a settings patch, or the first validation problem. */
function patchOf(
  draft: Draft,
  host: CompileSnapshot["host"],
  budgetsAvailable: boolean,
  autoStopAvailable: boolean,
): { patch: SettingsPatch } | { error: string } {
  const vmCpus = Number(draft.vmCpus);
  const vmMemory = Number(draft.vmMemory);
  if (!Number.isInteger(vmCpus) || vmCpus < 1 || vmCpus > host.cpus)
    return { error: translate("全部 VM 的虚拟核总预算需要在 1–{max} 之间。", { max: host.cpus }) };
  if (!Number.isInteger(vmMemory) || vmMemory < 2 || vmMemory * 1024 > host.memoryMiB)
    return {
      error: translate("全部 VM 的内存总预算需要在 2–{max} GB 之间。", { max: Math.floor(host.memoryMiB / 1024) }),
    };
  const idle = Number(draft.idleMinutes);
  if (!(Number.isInteger(idle) && idle >= 1 && idle <= 1440))
    return { error: translate("空闲停止时间需要在 1–1440 分钟之间。") };
  const keepLast = Number(draft.keepLast);
  const maxGib = Number(draft.artifactsGib);
  if (!(Number.isInteger(keepLast) && keepLast >= 1 && keepLast <= 10000))
    return { error: translate("保留数量需要在 1–10000 之间。") };
  if (!(Number.isInteger(maxGib) && maxGib >= 0)) return { error: translate("产物总大小需要是 0 或正整数。") };
  return {
    patch: {
      ...(budgetsAvailable ? { vmResources: { cpus: vmCpus, memoryMiB: vmMemory * 1024 } } : {}),
      ...(autoStopAvailable ? { autoStopIdle: draft.autoStopIdle } : {}),
      idleMinutes: idle,
      artifacts: { keepLast, maxGib },
    },
  };
}

function SettingsForm({ snapshot, run }: { snapshot: CompileSnapshot; run: Run }) {
  const { t } = useI18n();
  const saved = useMemo(
    () => draftOf(snapshot.settings, snapshot.host),
    [snapshot.settings, snapshot.host.cpus, snapshot.host.memoryMiB],
  );
  const savedKey = JSON.stringify(saved);
  const [draft, setDraft] = useState<Draft>(saved);
  const [base, setBase] = useState(savedKey);
  const [saving, setSaving] = useState(false);
  const dirty = JSON.stringify(draft) !== base;
  // Adopt remote changes while the form has no local edits.
  useEffect(() => {
    if (!dirty) {
      setDraft(JSON.parse(savedKey) as Draft);
      setBase(savedKey);
    }
  }, [savedKey]);
  const usage = snapshot.resources;
  const budgetBlocked =
    usage && (Number(draft.vmCpus) < usage.allocated.cpus || Number(draft.vmMemory) * 1024 < usage.allocated.memoryMiB);
  const autoStopAvailable = typeof snapshot.settings.autoStopIdle === "boolean";
  const result = patchOf(draft, snapshot.host, Boolean(usage), autoStopAvailable);
  const update = <K extends keyof Draft>(key: K, value: Draft[K]) =>
    setDraft((current) => ({ ...current, [key]: value }));
  return (
    <form
      className="compile-settings"
      onSubmit={(event) => {
        event.preventDefault();
        if (!("patch" in result) || budgetBlocked) return;
        setSaving(true);
        void run(() => compile.patchSettings(result.patch)).then((ok) => {
          setSaving(false);
          if (ok) setBase(JSON.stringify(draft));
        });
      }}
    >
      <section className="settings-group compile-group">
        <h2>{t("虚拟机")}</h2>
        <p className="compile-muted">
          {t("以下是所有构建与准备 VM 共享的总预算，不是每个 slot 的额度；资源不足时编译会排队。")}
        </p>
        <div className="card settings-list compile-vm-list">
          <SettingsRow
            label={t("全部 VM 虚拟核总预算")}
            description={t("限制所有 VM 的虚拟核合计，由 GizMoth 自动安排各 VM 的容量，不是每个 slot 都获得这个数值。")}
          >
            <UnitInput
              unit={t("核")}
              type="number"
              inputMode="numeric"
              min={1}
              max={snapshot.host.cpus}
              aria-label={t("全部 VM 虚拟核总预算")}
              value={draft.vmCpus}
              disabled={saving || !usage}
              onChange={(e) => update("vmCpus", e.target.value)}
            />
          </SettingsRow>
          <SettingsRow
            label={t("全部 VM 内存总预算")}
            description={t(
              "所有运行、启动和热备 VM 的来宾内存容量合计不超过此值，包括准备 VM。实际按需占用，VM 进程自身开销另计；不是进程 RSS 的精确硬上限。",
            )}
          >
            <UnitInput
              unit="GB"
              type="number"
              inputMode="numeric"
              min={2}
              max={Math.floor(snapshot.host.memoryMiB / 1024)}
              aria-label={t("全部 VM 内存总预算")}
              value={draft.vmMemory}
              disabled={saving || !usage}
              onChange={(e) => update("vmMemory", e.target.value)}
            />
          </SettingsRow>
          <SettingsRow
            label={t("自动停止空闲 VM")}
            description={t("开启后按设定时间停止空闲 VM 并释放内存；关闭后保持热备，仍会保留内存和预算。")}
          >
            <input
              type="checkbox"
              role="switch"
              className="switch"
              aria-label={t("自动停止空闲 VM")}
              checked={draft.autoStopIdle}
              disabled={saving || !autoStopAvailable}
              onChange={(e) => update("autoStopIdle", e.target.checked)}
            />
          </SettingsRow>
          <SettingsRow label={t("空闲等待时间")} description={t("编译任务结束后，等待这段时间再自动停止空闲 VM。")}>
            <UnitInput
              unit={t("分钟")}
              type="number"
              inputMode="numeric"
              min={1}
              max={1440}
              aria-label={t("空闲停止等待时间")}
              value={draft.idleMinutes}
              disabled={saving || !draft.autoStopIdle}
              onChange={(e) => update("idleMinutes", e.target.value)}
            />
          </SettingsRow>
          <SettingsRow
            label={
              usage
                ? t("已分配容量 {memory} / {limit} GB · {cpus} / {cpuLimit} 虚拟核", {
                    memory: (usage.allocated.memoryMiB / 1024).toFixed(1),
                    limit: usage.limit.memoryMiB / 1024,
                    cpus: usage.allocated.cpus,
                    cpuLimit: usage.limit.cpus,
                  })
                : t("资源服务更新中…")
            }
            description={t("这里显示已分配的来宾容量，不是实时内存用量。热备 VM 仍占用容量，停止后才归还预算。")}
          >
            <button
              type="button"
              className="text-action compile-stop-idle"
              disabled={saving || !usage || !snapshot.slots.some((s) => s.state === "ready")}
              onClick={() => void run(compile.stopIdleSlots)}
            >
              {t("停止空闲 VM")}
            </button>
          </SettingsRow>
        </div>
        {budgetBlocked && (
          <p className="notice">
            {t("新预算低于当前已分配容量。请先停止空闲 VM；正在编译的 VM 需等任务结束后再停止。")}
          </p>
        )}
      </section>
      <PackagesSection snapshot={snapshot} run={run} />
      <section className="settings-group compile-group">
        <h2>{t("产物保留")}</h2>
        <div className="card settings-list">
          <SettingsRow label={t("保留最近的产物")} description={t("超过数量时删除最早的产物，固定的产物除外。")}>
            <span className="compile-unit-field compact">
              <Input
                controlSize="sm"
                type="number"
                inputMode="numeric"
                min={1}
                max={10000}
                aria-label={t("保留最近的产物")}
                value={draft.keepLast}
                disabled={saving}
                onChange={(event) => update("keepLast", event.target.value)}
              />
              <span>{t("个")}</span>
            </span>
          </SettingsRow>
          <SettingsRow label={t("产物总大小上限")} description={t("0 表示不限制大小。")}>
            <span className="compile-unit-field compact">
              <Input
                controlSize="sm"
                type="number"
                inputMode="numeric"
                min={0}
                aria-label={t("产物总大小上限")}
                value={draft.artifactsGib}
                disabled={saving}
                onChange={(event) => update("artifactsGib", event.target.value)}
              />
              <span>GB</span>
            </span>
          </SettingsRow>
        </div>
      </section>
      {dirty && (
        <div className="compile-save-bar" role="region" aria-label={t("未保存的更改")}>
          <span className={"error" in result ? "error" : "compile-muted"}>
            {"error" in result ? result.error : t("有未保存的更改。")}
          </span>
          <Button controlSize="sm" disabled={saving} onClick={() => setDraft(JSON.parse(base) as Draft)}>
            {t("还原")}
          </Button>
          <Button
            controlSize="sm"
            type="submit"
            variant="primary"
            disabled={saving || "error" in result || Boolean(budgetBlocked)}
          >
            {t(saving ? "正在保存…" : "保存")}
          </Button>
        </div>
      )}
    </form>
  );
}

/** Settings › 编译: boot resources, formula downloads, VM and build settings. */
export function CompilingSettingsPage() {
  const { t } = useI18n();
  const { snapshot, error: loadError } = useCompileSnapshot();
  const [error, setError] = useState("");
  const run = useCallback(async (operation: () => Promise<unknown>) => {
    setError("");
    try {
      await operation();
      return true;
    } catch (e) {
      setError(errorMessage(e));
      return false;
    }
  }, []);
  return (
    <>
      <PageHeader crumbs={[{ label: t("设置") }, { label: t("编译") }]} />
      <div className="page settings-page compile-page">
        {demo && <p className="notice">{t("演示模式：下载和运行环境准备仅模拟，不会启动虚拟机或修改本机文件。")}</p>}
        {native && !snapshot && !loadError && (
          <p className="compile-muted compile-loading" role="status">
            {t("正在读取编译服务…")}
          </p>
        )}
        {loadError && !snapshot && (
          <p className="error banner" role="alert">
            {t("无法读取编译服务：{error}", { error: loadError })}
          </p>
        )}
        {snapshot && !snapshot.available.ok && (
          <p className="notice compile-unavailable" role="note">
            <span>{t("此电脑暂时不能运行编译虚拟机。")}</span>
            {snapshot.available.message && <span className="compile-muted">{snapshot.available.message}</span>}
          </p>
        )}
        {error && (
          <p className="error banner compile-banner" role="alert">
            <span>{error}</span>
            <IconButton aria-label={t("关闭")} onClick={() => setError("")}>
              <X size={14} />
            </IconButton>
          </p>
        )}
        {snapshot && <SettingsForm snapshot={snapshot} run={run} />}
      </div>
    </>
  );
}
