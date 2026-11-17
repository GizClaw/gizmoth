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
  PushPin,
  PushPinSlash,
  Trash,
  UploadSimple,
  X,
} from "@phosphor-icons/react";
import { demo, errorMessage, native } from "../bridge";
import { copyText } from "../clipboard";
import { CompileTerminal, type CompileTerminalHandle } from "./CompileTerminal";
import { TaskConfiguration } from "./CompileTaskConfig";
import {
  compile,
  compileViews,
  formatDuration,
  formatSize,
  isActive,
  useCompileSnapshot,
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
import { WorkspaceActions } from "./WorkspaceActions";
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
  jobId,
  onJob,
  onView,
  onSerial,
  onSerialPort,
  onStorage,
}: {
  view: CompileView;
  jobId?: string;
  onJob: (id: string) => void;
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
  if (view === "builds" && jobId) {
    const job = snapshot && [...snapshot.jobs, ...snapshot.queue].find((item) => item.id === jobId);
    return (
      <>
        <PageHeader
          crumbs={[
            { label: t("编译") },
            { label: t("编译任务"), onClick: () => onView("builds") },
            { label: job?.title ?? t("编译日志") },
          ]}
          actions={<Button variant="ghost" controlSize="sm" onClick={() => onView("builds")}>{t("返回编译任务")}</Button>}
        />
        {job ? (
          <JobLogPage
            key={job.id}
            job={job}
            slotName={snapshot?.slots.find((slot) => slot.id === job.slot)?.name ?? job.slot ?? "—"}
            onArtifact={() => { setHighlight(job.artifact!); onView("artifacts"); }}
            onCancel={() => setConfirming({
              title: t("取消编译"),
              body: t("取消「{title}」？提交它的 agent 会收到已取消的结果。", { title: job.title }),
              confirm: t("取消编译"),
              danger: true,
              action: () => compile.cancelJob(job.id),
            })}
          />
        ) : (
          <div className="page compile-page">
            <p className={loadError ? "error" : "compile-muted"} role="status">
              {loadError || t(snapshot ? "未找到这个编译任务。" : "正在读取编译服务…")}
            </p>
          </div>
        )}
        {confirming && <ConfirmDialog request={confirming} onClose={() => setConfirming(null)} />}
      </>
    );
  }
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
              "在本机的 Linux 虚拟机中编译项目。通过 gizmoth request 提交 JSON，服务持续管理编译任务；缓存和产物由所有 slot 共享。",
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
                  onJob={onJob}
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
                  onHighlighted={() => setHighlight("")}
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
  onJob,
}: {
  snapshot: CompileSnapshot;
  run: Run;
  onJob: (id: string) => void;
}) {
  const { t } = useI18n();
  const [resizing, setResizing] = useState(false);
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
              <SlotCard key={slot.id} slot={slot} now={now} onOpenJob={onJob} />
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
                slotName={slotName(job.slot)}
                onOpen={() => onJob(job.id)}
              />
            ))}
          </div>
        ) : (
          <p className="compile-empty">
            {t(
              "还没有编译任务。通过 gizmoth request 提交 compile JSON 请求后，会在这里排队并显示日志。",
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

function jobTime(job: CompileJob, now: number) {
  if (isActive(job.status)) {
    return job.startedAt
      ? formatDuration(now - job.startedAt)
      : translate("已等待 {duration}", { duration: formatDuration(now - job.createdAt) });
  }
  return job.finishedAt && job.startedAt ? formatDuration(job.finishedAt - job.startedAt) : "—";
}

function JobRow({ job, now, slotName, onOpen }: {
  job: CompileJob;
  now: number;
  slotName: string;
  onOpen: () => void;
}) {
  return (
    <div className="compile-job">
      <button type="button" className="compile-job-row" onClick={onOpen}>
        <span className="compile-job-status"><JobStatus job={job} /></span>
        <span className="compile-job-main">
          <span className="compile-job-title">{job.title}</span>
          <span className="compile-job-meta">{job.client} · {basename(job.workspace)} · {slotName}</span>
        </span>
        <span className="compile-job-time" title={job.finishedAt ? new Date(job.finishedAt).toLocaleString() : undefined}>
          {jobTime(job, now)}
          {!isActive(job.status) && job.finishedAt && <span className="compile-muted">{relativeTime(job.finishedAt)}</span>}
        </span>
        <CaretRight size={14} className="compile-chevron" aria-hidden="true" />
      </button>
    </div>
  );
}

function JobLogPage({ job, slotName, onArtifact, onCancel }: {
  job: CompileJob;
  slotName: string;
  onArtifact: () => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  const active = isActive(job.status);
  const now = useNow(active);
  const tabsId = useId();
  const [tab, setTab] = useState<"log" | "config">("log");
  const [configurationOpened, setConfigurationOpened] = useState(false);
  const chooseTab = (value: "log" | "config") => {
    setTab(value);
    if (value === "config") setConfigurationOpened(true);
  };
  const fields: [string, ReactNode][] = [
    [t("工作区"), <code key="w">{job.workspace}</code>],
    [t("工作目录"), job.cwd ? <code key="d">{job.cwd}</code> : "—"],
    [t("提交"), new Date(job.createdAt).toLocaleString()],
    [t("结束"), job.finishedAt ? new Date(job.finishedAt).toLocaleString() : "—"],
    [t("退出码"), exitText(job)],
    [t("同步"), job.synced ? t("{files} 个文件更新 · {bytes} · {removed} 个删除", {
      files: job.synced.files, bytes: formatSize(job.synced.bytes), removed: job.synced.removed,
    }) : "—"],
  ];
  return (
    <div className="page compile-job-page">
      <div className="compile-job-heading">
        <div className="compile-job-identity">
          <h1>{job.title}</h1>
          <div className="compile-job-summary">
            <JobStatus job={job} />
            <span>{slotName}</span><span>{jobTime(job, now)}</span><span>{job.client}</span>
          </div>
        </div>
        <div className="compile-job-actions">
          {job.artifact && <Button controlSize="sm" onClick={onArtifact}>{t("查看产物")}</Button>}
          {active && <Button controlSize="sm" variant="ghost" className="compile-danger" onClick={onCancel}>{t("取消编译")}</Button>}
        </div>
      </div>
      <details className="compile-job-info">
        <summary>{t("任务详情")}</summary>
        <div className="compile-job-info-body">
          <dl className="compile-fields">
            {fields.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
            {job.artifacts.length > 0 && <div><dt>{t("产物规则")}</dt><dd>
              {job.artifacts.map((pattern) => <code key={pattern} className={job.missing.includes(pattern) ? "compile-missing" : undefined}>{pattern}</code>)}
              {job.missing.length > 0 && <span className="compile-muted">{t("{count} 条规则没有匹配到文件", { count: job.missing.length })}</span>}
            </dd></div>}
          </dl>
          <div className="compile-job-command"><span>{t("命令")}</span><pre>{job.command.map((arg) => /^[\w./:@=+,-]+$/.test(arg) ? arg : `'${arg.replaceAll("'", "'\\''")}'`).join(" ")}</pre></div>
        </div>
      </details>
      {job.error && <p className="error compile-job-error" role="alert">{translate(job.error)}</p>}
      <div className="compile-job-tabs" role="tablist" aria-label={t("任务详情")}>
        {(["log", "config"] as const).map((item) => (
          <button key={item} type="button" role="tab" data-tab={item}
            id={`${tabsId}-${item}-tab`} aria-controls={`${tabsId}-${item}-panel`}
            aria-selected={tab === item} tabIndex={tab === item ? 0 : -1}
            onClick={() => chooseTab(item)} onKeyDown={(event) => {
              let next: "log" | "config";
              if (event.key === "ArrowLeft" || event.key === "ArrowRight") next = item === "log" ? "config" : "log";
              else if (event.key === "Home") next = "log";
              else if (event.key === "End") next = "config";
              else return;
              event.preventDefault();
              chooseTab(next);
              event.currentTarget.parentElement?.querySelector<HTMLButtonElement>(`[data-tab="${next}"]`)?.focus();
            }}>
            {t(item === "log" ? "日志" : "配置")}
          </button>
        ))}
      </div>
      <div className="compile-job-panel" role="tabpanel" id={`${tabsId}-log-panel`}
        aria-labelledby={`${tabsId}-log-tab`} hidden={tab !== "log"}>
        <LogView key={job.id} source={job.id} live={active} />
      </div>
      <div className="compile-job-panel" role="tabpanel" id={`${tabsId}-config-panel`}
        aria-labelledby={`${tabsId}-config-tab`} hidden={tab !== "config"}>
        {configurationOpened && <TaskConfiguration jobId={job.id} />}
      </div>
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
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [follow, setFollow] = useState(true);
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState("");
  const next = useRef<number | undefined>(undefined);
  const terminal = useRef<CompileTerminalHandle | null>(null);
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
              const combined = previous + chunk.text;
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
      if (!disposed) {
        setLoading(false);
        if (live) timer = window.setTimeout(() => void load(), 1000);
      }
    };
    void load();
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [source, live]);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1800);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <section className="compile-log-wrap" aria-label={t("编译日志")}>
      <div className="compile-log-toolbar">
        <span className="compile-log-label">{t("日志")}</span>
        <SearchInput controlSize="sm" aria-label={t("筛选日志")} placeholder={t("筛选日志")} value={query} onClear={() => setQuery("")} onChange={(event) => setQuery(event.target.value)} />
        <Button controlSize="sm" variant="ghost" aria-pressed={follow} onClick={() => setFollow((value) => !value)}>{t("跟随输出")}</Button>
        <IconButton aria-label={t("复制日志")} title={t("复制日志")} disabled={!text} onClick={() => {
          setCopyFailed("");
          void copyText(terminal.current?.getText() ?? "").then(() => setCopied(true)).catch((error) => setCopyFailed(errorMessage(error)));
        }}>{copied ? <Check size={15} /> : <Copy size={15} />}</IconButton>
      </div>
      {trimmed && <p className="compile-muted compile-log-note-line">{t("只显示最新的日志。")}</p>}
      <div className="compile-terminal-region" role="log" aria-live="off" aria-busy={loading} aria-label={t("日志")}>
        <CompileTerminal
          text={text}
          query={query}
          follow={follow}
          onFollow={setFollow}
          handle={terminal}
          empty={t(loading ? "正在读取日志…" : query.trim() ? "没有匹配的日志。" : live ? "等待输出…" : "没有日志。")}
        />
      </div>
      {failed && (
        <p className="error" role="alert">
          {t("无法读取日志：{error}", { error: failed })}
        </p>
      )}
      {copyFailed && <p className="error" role="alert">{copyFailed}</p>}
    </section>
  );
}

// ---------------------------------------------------------------- caches

const cacheTitles: Record<CompileCache["kind"], string> = {
  bazel: "Bazel 远程缓存",
  ccache: "ccache",
};
function cacheDescription(cache: CompileCache) {
  if (cache.kind === "bazel") return translate("编译动作结果（AC/CAS），各 slot 的 Bazel 自动使用");
  return translate("GCC/Clang 编译结果，所有 slot 直接共用缓存");
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
  const [editing, setEditing] = useState<CompileCache | null>(null);
  const total = snapshot.caches.reduce((sum, cache) => sum + cache.bytes, 0);
  const root = snapshot.storage.root;
  return (
    <>
      <div className="page-list-toolbar">
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
              const label = t(cacheTitles[cache.kind]);
              return (
                <tr key={cache.name}>
                  <th scope="row">
                    <span className="compile-name">{label}</span>
                    <span className="compile-description">{cacheDescription(cache)}</span>
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
      {editing && <CacheDialog cache={editing} settings={snapshot.settings} onClose={() => setEditing(null)} />}
    </>
  );
}

function CacheDialog({
  cache,
  settings,
  onClose,
}: {
  cache: CompileCache;
  settings: CompileSettings;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const field = useId();
  const kind = cache.kind;
  const [maxGib, setMaxGib] = useState(String(settings.caches[kind].maxGib));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const max = Number(maxGib);
  const invalid = !maxGib.trim() || !(Number.isInteger(max) && max >= 0)
    ? t("上限需要是 0 或正整数（0 表示不限）。") : "";
  async function save() {
    if (invalid) return;
    setSaving(true);
    setError("");
    const next: CacheSettings = { ...settings.caches, [kind]: { maxGib: max } };
    try {
      await compile.patchSettings({ caches: next });
      onClose();
    } catch (e) {
      setError(errorMessage(e));
      setSaving(false);
    }
  }
  return (
    <Dialog title={t("{name} 设置", { name: t(cacheTitles[kind]) })} onClose={onClose} busy={saving}>
      <p className="dialog-text">
        {kind === "bazel"
          ? t("各 slot 的 Bazel 通过 --remote_cache 共享编译结果。修改会在空闲虚拟机下次启动时生效。")
          : t("所有 slot 直接使用同一份共享 ccache，不在各 slot 的工作目录保存编译缓存。")}
      </p>
      <form className="compile-form" onSubmit={(event) => { event.preventDefault(); void save(); }}>
        <label htmlFor={`${field}-max`}>{t("上限")}</label>
        <span className="compile-unit-field">
          <Input id={`${field}-max`} type="number" inputMode="numeric" min={0} required value={maxGib}
            disabled={saving} onChange={(e) => setMaxGib(e.target.value)} />
          <span>GB</span>
        </span>
        <p className="compile-form-hint compile-muted">{t("上限为 0 表示不限制；超过上限时删除最久未使用的条目。")}</p>
        {(invalid || error) && <p className="error compile-form-hint" role="alert">{error || invalid}</p>}
        <div className="dialog-actions">
          <Button disabled={saving} onClick={onClose}>{t("取消")}</Button>
          <Button type="submit" variant="primary" disabled={saving || Boolean(invalid)}>{t("保存")}</Button>
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
  onHighlighted,
  onInstalled,
  onSerial,
}: {
  snapshot: CompileSnapshot;
  run: Run;
  confirm: Confirm;
  /** A build log's "View artifact" target, taken once when this tab opens. */
  highlight: string;
  onHighlighted?: () => void;
  onInstalled: (text: string, port: string) => void;
  onSerial: () => void;
}) {
  const { t } = useI18n();
  const [artifacts, setArtifacts] = useState<CompileArtifact[] | null>(null);
  const [failed, setFailed] = useState("");
  const [query, setQuery] = useState("");
  const [limit, setLimit] = useState(ARTIFACT_PAGE);
  const prefix = useId();
  // Opening an artifact from a build expands its details directly. The request
  // is consumed so returning through the sidebar later does not repeat it.
  const [target] = useState(highlight);
  const [revealing, setRevealing] = useState(highlight);
  const [scrollTo, setScrollTo] = useState("");
  useEffect(() => {
    if (target) onHighlighted?.();
  }, [target]);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set(highlight ? [highlight] : []));
  const toggle = (id: string) =>
    setExpanded((previous) => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  const [installing, setInstalling] = useState<CompileArtifact | null>(null);
  const generation = useRef(0);
  const mounted = useRef(false);
  const reload = useCallback(async () => {
    if (!mounted.current) return;
    const current = ++generation.current;
    try {
      const next = await compile.artifacts();
      if (current !== generation.current) return;
      setArtifacts(next);
      setFailed("");
    } catch (e) {
      if (current === generation.current) setFailed(errorMessage(e));
    }
  }, []);
  const key = snapshot.artifacts.revision ?? `${snapshot.artifacts.dir}:${snapshot.artifacts.count}:${snapshot.artifacts.bytes}`;
  useEffect(() => {
    mounted.current = true;
    void reload();
    return () => {
      mounted.current = false;
      generation.current++;
    };
  }, [reload, key]);
  const update = async (operation: () => Promise<unknown>) => {
    // Retire reads started before a pin/delete, including their errors.
    generation.current++;
    try {
      await operation();
    } finally {
      await reload();
    }
  };
  const needle = query.trim().toLowerCase();
  const visible = (artifacts ?? []).filter(
    (artifact) =>
      !needle ||
      [artifact.title, artifact.client, artifact.workspace, ...artifact.files.map((file) => file.path)].some((text) =>
        text.toLowerCase().includes(needle),
      ),
  );
  const shown = visible.slice(0, limit);
  // Once the list arrives, show the page containing the target (it may be past
  // the first 50), expand it, then scroll after that page has rendered.
  const targetIndex = revealing ? visible.findIndex((artifact) => artifact.id === revealing) : -1;
  useEffect(() => {
    if (targetIndex < 0) return;
    setLimit((current) => Math.max(current, Math.ceil((targetIndex + 1) / ARTIFACT_PAGE) * ARTIFACT_PAGE));
    setExpanded((previous) => (previous.has(revealing) ? previous : new Set(previous).add(revealing)));
    setScrollTo(revealing);
    setRevealing("");
  }, [targetIndex, revealing]);
  useEffect(() => {
    if (!scrollTo) return;
    const row = document.getElementById(`artifact-${scrollTo}`);
    if (!row) return;
    row.scrollIntoView({ block: "start" });
    setScrollTo("");
  }, [scrollTo, limit, artifacts]);
  const policy = snapshot.settings.artifacts;
  const mutate = (operation: () => Promise<unknown>) => void run(() => update(operation));
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
      action: () => update(() => compile.deleteArtifact(artifact.id)),
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
            const open = expanded.has(artifact.id);
            const detailsId = `${prefix}-artifact-${artifact.id}-details`;
            return (
              <li
                key={artifact.id}
                id={`artifact-${artifact.id}`}
                className={`compile-artifact-item${target === artifact.id ? " compile-highlight" : ""}`}
                data-pinned={artifact.pinned || undefined}
                data-installable={packages.length > 0 || undefined}
              >
                <button
                  type="button"
                  className="compile-artifact-row"
                  aria-label={[artifact.title, artifact.pinned && t("已固定"), meta, time].filter(Boolean).join("，")}
                  aria-expanded={open}
                  aria-controls={detailsId}
                  onClick={() => toggle(artifact.id)}
                >
                  <span className="compile-artifact-name">
                    <CaretRight size={14} className="compile-artifact-chevron" aria-hidden="true" />
                    <Package size={18} className="compile-artifact-icon" aria-hidden="true" />
                    <strong className="compile-artifact-title" title={artifact.title}>
                      {artifact.title}
                    </strong>
                    {artifact.pinned && (
                      <PushPin size={12} weight="fill" className="compile-pin" aria-hidden="true" />
                    )}
                  </span>
                  <span className="compile-artifact-meta-text">{meta}</span>
                  <span className="compile-artifact-time" title={new Date(artifact.createdAt).toLocaleString()}>
                    {time}
                  </span>
                </button>
                <div className="compile-artifact-actions">
                  <WorkspaceActions
                    label={t("管理 {name}", { name: artifact.title })}
                    actions={[
                      {
                        label: "安装",
                        icon: <UploadSimple size={14} />,
                        disabled: packages.length === 0,
                        title: packages.length ? undefined : t("这个产物没有可安装的固件包（.update.tar.zlib）。"),
                        onSelect: () => setInstalling(artifact),
                      },
                      {
                        label: artifact.pinned ? "取消固定" : "固定",
                        icon: artifact.pinned ? <PushPinSlash size={14} /> : <PushPin size={14} />,
                        onSelect: () => mutate(() => compile.pinArtifact(artifact.id, !artifact.pinned)),
                      },
                      {
                        label: "删除",
                        icon: <Trash size={14} />,
                        danger: true,
                        separator: true,
                        onSelect: () => remove(artifact),
                      },
                    ]}
                  />
                </div>
                <div id={detailsId} className="compile-artifact-details" hidden={!open}>
                  {open && (
                    <ArtifactDetails artifact={artifact} onReveal={() => void run(() => compile.reveal(artifact.dir))} />
                  )}
                </div>
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

/** Paths, checksums and the ID expand inline below their row. */
function ArtifactDetails({ artifact, onReveal }: { artifact: CompileArtifact; onReveal: () => void }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState("");
  const [error, setError] = useState("");
  const copy = (key: string, text: string) =>
    void copyText(text)
      .then(() => setCopied(key))
      .catch((e) => setError(errorMessage(e)));
  return (
    <>
      <div className="compile-artifact-summary">
        <span>
          {artifact.client} · {new Date(artifact.createdAt).toLocaleString()} · {formatSize(artifact.bytes)}
          {artifact.pinned && ` · ${t("已固定")}`}
        </span>
        <button type="button" className="text-action" onClick={onReveal}>
          <FolderOpen size={14} aria-hidden="true" />
          {t("在访达中显示")}
        </button>
      </div>
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
    </>
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
