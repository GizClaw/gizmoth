import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import {
  CaretRight,
  Check,
  CircleNotch,
  Copy,
  Cube,
  FolderOpen,
  HardDrives,
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
  type CompileImage,
  type CompileJob,
  type CompileJobStatus,
  type CompilePackage,
  type CompilePackages,
  type CompileSettings,
  type CompileSlot,
  type CompileSlotState,
  type CompileSnapshot,
  type CompileView,
  type SettingsPatch,
} from "../compile";
import { t as translate, useI18n } from "../i18n";
import { serial, useSerialSnapshot, type PackageInfo, type SerialPort } from "../serial";
import { Button, Dialog, Dot, IconButton, Input, PageHeader, SearchInput, Select, relativeTime } from "../ui";
import { SettingsRow } from "./SettingsRow";
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
const GiB = 1024 ** 3;

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
const gib = (bytes: number) => (bytes / GiB).toFixed(bytes >= 10 * GiB || bytes % GiB === 0 ? 0 : 1);
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
  onSettings,
}: {
  view: CompileView;
  onView: (view: CompileView) => void;
  onSerial: () => void;
  onSerialPort: (id: string) => void;
  /** Settings › Storage, where the working directory is chosen. */
  onStorage: () => void;
  /** Settings › 编译: image, packages and build settings. */
  onSettings: () => void;
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
                  onSettings={onSettings}
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

function ImageCard({
  image,
  run,
  compact,
  onSettings,
}: {
  image: CompileImage;
  run: Run;
  compact?: boolean;
  onSettings?: () => void;
}) {
  const { t } = useI18n();
  const [working, setWorking] = useState(false);
  const act = (operation: () => Promise<unknown>) => {
    setWorking(true);
    void run(operation).finally(() => setWorking(false));
  };
  const percent = image.progress ? ratio(image.progress.done, image.progress.total) : null;
  const status =
    image.status === "ready"
      ? { label: t("已就绪"), tone: "ok" as const }
      : image.status === "building"
        ? { label: t("正在准备"), tone: "idle" as const }
        : image.status === "failed"
          ? { label: t("准备失败"), tone: "warn" as const }
          : { label: t("尚未准备"), tone: "off" as const };
  const built = image.info?.builtAt;
  return (
    <section className={`card compile-image${compact ? " compact" : ""}`} aria-label={t("Linux 系统镜像")}>
      <div className="compile-image-head">
        <span className="compile-glyph">
          <HardDrives size={18} aria-hidden="true" />
        </span>
        <div className="compile-identity">
          <h3>{t("Linux 系统镜像")}</h3>
          <span>
            {image.base} · {image.kernel}
          </span>
        </div>
        <Status label={status.label} tone={status.tone} busy={image.status === "building"} />
      </div>
      {image.status === "building" && (
        <div className="compile-image-progress" aria-live="polite">
          <span className="compile-muted">
            {translate(image.phase ?? "正在准备")}
            {image.progress && ` · ${formatSize(image.progress.done)} / ${formatSize(image.progress.total)}`}
          </span>
          <div
            className="compile-progress"
            role="progressbar"
            aria-label={t("镜像准备进度")}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={percent ?? undefined}
          >
            <span className={percent === null ? "indeterminate" : undefined} style={{ width: `${percent ?? 30}%` }} />
          </div>
        </div>
      )}
      {image.status === "ready" && (
        <p className="compile-muted compile-image-meta">
          {t("{packages} 个软件包 · 占用 {size} · 构建于 {time}", {
            packages: image.info?.packages?.length ?? image.packages.length,
            size: formatSize(image.allocated),
            time: built ? relativeTime(built) : "—",
          })}
        </p>
      )}
      {image.status === "missing" && (
        <p className="compile-muted compile-image-meta">
          {t("第一次编译前会自动准备 Ubuntu 系统盘，大约需要 1–2 分钟并需要联网。")}
        </p>
      )}
      {image.error && image.status !== "building" && (
        <p className={image.error === "Cancelled" ? "compile-muted compile-image-meta" : "error"} role="alert">
          {image.error === "Cancelled" ? t("上次准备已取消。") : image.error}
        </p>
      )}
      {image.status === "ready" && !image.current && (
        <p className="notice compile-image-stale">{t("GizMoth 更新了系统镜像，没有编译时会自动重新构建。")}</p>
      )}
      <div className="compile-image-actions">
        {image.status === "building" ? (
          <Button controlSize="sm" disabled={working} onClick={() => act(compile.cancelImage)}>
            {t("取消")}
          </Button>
        ) : (
          <Button
            controlSize="sm"
            variant={image.status === "ready" && image.current ? "default" : "primary"}
            disabled={working}
            onClick={() => act(compile.buildImage)}
          >
            {t(image.status === "ready" ? "重新构建镜像" : image.status === "failed" ? "重试" : "现在准备")}
          </Button>
        )}
        {compact && onSettings && (
          <button type="button" className="text-action" onClick={onSettings}>
            {t("镜像与开发包")}
          </button>
        )}
      </div>
      {!compact && <LogDetails label={t("镜像日志")} source="image" live={image.status === "building"} />}
    </section>
  );
}

function BuildsTab({
  snapshot,
  run,
  confirm,
  onArtifact,
  onSettings,
}: {
  snapshot: CompileSnapshot;
  run: Run;
  confirm: Confirm;
  onArtifact: (id: string) => void;
  onSettings: () => void;
}) {
  const { t } = useI18n();
  const [editing, setEditing] = useState<CompileSlot | "new" | null>(null);
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
  const memory = snapshot.slots.reduce((total, slot) => total + slot.memoryMiB, 0);
  return (
    <>
      {snapshot.image.status !== "ready" && (
        <ImageCard image={snapshot.image} run={run} compact onSettings={onSettings} />
      )}
      <section className="compile-section" aria-labelledby="compile-slots-title">
        <div className="compile-section-head">
          <h2 id="compile-slots-title">{t("编译 slot")}</h2>
          <button
            type="button"
            className="text-action"
            disabled={snapshot.slots.length >= 16}
            onClick={() => setEditing("new")}
          >
            <Plus size={14} aria-hidden="true" />
            {t("添加 slot")}
          </button>
          <span className="page-list-summary">
            {t("最多同时编译 {count} 个 · 本机 {cpus} 核 {memory} GB 内存", {
              count: snapshot.slots.length,
              cpus: snapshot.host.cpus,
              memory: Math.round(snapshot.host.memoryMiB / 1024),
            })}
          </span>
        </div>
        {memory > snapshot.host.memoryMiB && (
          <p className="notice">
            {t("所有 slot 同时运行时共需 {need} GB 内存，超过本机的 {have} GB。", {
              need: Math.round(memory / 1024),
              have: Math.round(snapshot.host.memoryMiB / 1024),
            })}
          </p>
        )}
        {snapshot.slots.length ? (
          <div className="compile-slots">
            {snapshot.slots.map((slot) => (
              <SlotCard
                key={slot.id}
                slot={slot}
                now={now}
                onEdit={() => setEditing(slot)}
                onOpenJob={(id) => setOpen(id)}
                onStop={() => void run(() => compile.stopSlot(slot.id))}
                onReset={() =>
                  confirm({
                    title: t("清空 {name} 的工作盘", { name: slot.name }),
                    body: t(
                      "将停止虚拟机并删除工作盘：源码副本、Bazel 输出目录、本地 ccache 和 home 会在下次编译时重新创建。共享缓存和产物不受影响。",
                    ),
                    confirm: t("清空工作盘"),
                    danger: true,
                    action: () => compile.resetSlot(slot.id),
                  })
                }
                onRemove={() =>
                  confirm({
                    title: t("删除 {name}", { name: slot.name }),
                    body: t("将停止虚拟机并删除这个 slot 的工作盘。共享缓存和产物会保留。"),
                    confirm: t("删除"),
                    danger: true,
                    action: () => compile.removeSlot(slot.id),
                  })
                }
              />
            ))}
          </div>
        ) : (
          <p className="compile-empty">{t("还没有 slot。添加一个 slot 后才能编译。")}</p>
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
      {editing && (
        <SlotDialog slot={editing === "new" ? null : editing} snapshot={snapshot} onClose={() => setEditing(null)} />
      )}
    </>
  );
}

function SlotCard({
  slot,
  now,
  onEdit,
  onOpenJob,
  onStop,
  onReset,
  onRemove,
}: {
  slot: CompileSlot;
  now: number;
  onEdit: () => void;
  onOpenJob: (id: string) => void;
  onStop: () => void;
  onReset: () => void;
  onRemove: () => void;
}) {
  const { t } = useI18n();
  const working = slot.state === "busy" || slot.state === "starting";
  const used = slot.disk.used ?? slot.disk.allocated;
  return (
    <article className={`card compile-slot compile-slot-${slot.state}`} aria-label={slot.name}>
      <div className="compile-slot-head">
        <span className="compile-glyph">
          <Cube size={18} aria-hidden="true" />
        </span>
        <div className="compile-identity">
          <h3 title={slot.name}>{slot.name}</h3>
          <span>
            {t("{cpus} CPU · {memory} GB 内存", { cpus: slot.cpus, memory: gib(slot.memoryMiB * 1024 ** 2) })}
          </span>
        </div>
        <SlotStatus state={slot.state} />
      </div>
      <div className="compile-slot-job">
        {slot.job ? (
          <button type="button" className="compile-slot-current" onClick={() => onOpenJob(slot.job!.id)}>
            <span className="compile-slot-title" title={slot.job.title}>
              {slot.job.title}
            </span>
            <span className="compile-muted">
              {slot.job.client} · {translate(jobLabels[slot.job.status])}
              {slot.job.startedAt ? ` · ${formatDuration(now - slot.job.startedAt)}` : ""}
            </span>
          </button>
        ) : (
          <span className="compile-muted">
            {slot.state === "ready"
              ? t("虚拟机已启动，等待编译{time}", {
                  time: slot.lastUsed ? ` · ${t("上次使用：{time}", { time: relativeTime(slot.lastUsed) })}` : "",
                })
              : slot.state === "stopped"
                ? t("有编译任务时自动启动虚拟机")
                : t(slotLabels[slot.state])}
          </span>
        )}
      </div>
      {slot.error && (
        <p className="error" role="alert">
          {slot.error}
        </p>
      )}
      <div className="compile-slot-disk">
        <div className="compile-slot-disk-label">
          <span>{t("工作盘")}</span>
          <span className="compile-muted">
            {t("{used} / {size}", { used: formatSize(used), size: formatSize(slot.disk.size) })}
          </span>
        </div>
        <Meter used={used} total={slot.disk.size} label={t("{name} 工作盘用量", { name: slot.name })} />
      </div>
      <div className="compile-slot-actions">
        <Button controlSize="sm" variant="ghost" onClick={onEdit}>
          {t("编辑")}
        </Button>
        {(slot.state === "ready" || slot.state === "failed") && (
          <Button controlSize="sm" variant="ghost" onClick={onStop}>
            {t("停止虚拟机")}
          </Button>
        )}
        <Button controlSize="sm" variant="ghost" disabled={working} onClick={onReset}>
          {t("清空工作盘")}
        </Button>
        <Button
          controlSize="sm"
          variant="ghost"
          className="compile-danger"
          disabled={working}
          title={working ? t("先取消这个 slot 上的编译。") : undefined}
          onClick={onRemove}
        >
          {t("删除")}
        </Button>
      </div>
    </article>
  );
}

function SlotDialog({
  slot,
  snapshot,
  onClose,
}: {
  slot: CompileSlot | null;
  snapshot: CompileSnapshot;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const field = useId();
  const template = slot ?? snapshot.slots.at(-1);
  const [name, setName] = useState(slot?.name ?? t("Slot {index}", { index: snapshot.slots.length + 1 }));
  const [cpus, setCpus] = useState(String(template?.cpus ?? Math.min(4, snapshot.host.cpus)));
  const [memory, setMemory] = useState(String(template ? template.memoryMiB / 1024 : 4));
  const [disk, setDisk] = useState(String(template?.diskGiB ?? 64));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const stopped = !slot || slot.state === "stopped" || slot.state === "failed";
  const diskLocked = Boolean(slot && slot.disk.allocated > 0);
  const maxMemory = Math.max(1, Math.floor(snapshot.host.memoryMiB / 1024) - 1);
  const memoryValue = Number(memory);
  const diskValue = Number(disk);
  const invalid =
    !name.trim() || [...name.trim()].length > 40
      ? t("名称需要 1–40 个字符。")
      : !(memoryValue >= 1 && memoryValue <= maxMemory)
        ? t("内存需要在 1–{max} GB 之间，并为 macOS 保留至少 1 GB。", { max: maxMemory })
        : !(Number.isInteger(diskValue) && diskValue >= 8 && diskValue <= 4096)
          ? t("工作盘需要在 8–4096 GB 之间。")
          : "";
  async function save() {
    if (invalid) return;
    setSaving(true);
    setError("");
    const resources = {
      cpus: Number(cpus),
      memoryMiB: Math.round(memoryValue * 1024),
      ...(diskLocked ? {} : { diskGiB: diskValue }),
    };
    try {
      if (slot) {
        const changed =
          resources.cpus !== slot.cpus ||
          resources.memoryMiB !== slot.memoryMiB ||
          (!diskLocked && diskValue !== slot.diskGiB);
        await compile.patchSlot(slot.id, { name: name.trim(), ...(stopped && changed ? resources : {}) });
      } else {
        await compile.addSlot({ name: name.trim(), ...resources, diskGiB: diskValue });
      }
      onClose();
    } catch (e) {
      setError(errorMessage(e));
      setSaving(false);
    }
  }
  return (
    <Dialog title={slot ? t("编辑 {name}", { name: slot.name }) : t("添加 slot")} onClose={onClose} busy={saving}>
      <p className="dialog-text">
        {t(
          "每个 slot 是一台独立的 Linux 虚拟机，同一时间运行一个编译任务。工作盘保存源码副本和 Bazel 输出，跨编译保留。",
        )}
      </p>
      <form
        className="compile-form"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <label htmlFor={`${field}-name`}>{t("名称")}</label>
        <Input
          id={`${field}-name`}
          value={name}
          maxLength={40}
          disabled={saving}
          onChange={(e) => setName(e.target.value)}
        />
        <label htmlFor={`${field}-cpus`}>CPU</label>
        <Select
          id={`${field}-cpus`}
          aria-label="CPU"
          value={cpus}
          disabled={saving || !stopped}
          onValueChange={setCpus}
          options={Array.from({ length: snapshot.host.cpus }, (_, index) => ({
            value: String(index + 1),
            label: t("{count} 核", { count: index + 1 }),
          }))}
        />
        <label htmlFor={`${field}-memory`}>{t("内存")}</label>
        <span className="compile-unit-field">
          <Input
            id={`${field}-memory`}
            type="number"
            inputMode="decimal"
            min={1}
            max={maxMemory}
            step={0.5}
            value={memory}
            disabled={saving || !stopped}
            onChange={(e) => setMemory(e.target.value)}
          />
          <span>GB</span>
        </span>
        <label htmlFor={`${field}-disk`}>{t("工作盘")}</label>
        <span className="compile-unit-field">
          <Input
            id={`${field}-disk`}
            type="number"
            inputMode="numeric"
            min={8}
            max={4096}
            value={disk}
            disabled={saving || !stopped || diskLocked}
            onChange={(e) => setDisk(e.target.value)}
          />
          <span>GB</span>
        </span>
        {slot && !stopped && (
          <p className="compile-form-hint compile-muted">{t("停止虚拟机后才能修改 CPU 和内存。")}</p>
        )}
        {diskLocked && (
          <p className="compile-form-hint compile-muted">{t("工作盘按需占用空间。清空工作盘后才能修改它的大小。")}</p>
        )}
        {(invalid || error) && (
          <p className="error compile-form-hint" role="alert">
            {error || invalid}
          </p>
        )}
        <div className="dialog-actions">
          <Button disabled={saving} onClick={onClose}>
            {t("取消")}
          </Button>
          <Button type="submit" variant="primary" disabled={saving || Boolean(invalid)}>
            {t(slot ? "保存" : "添加")}
          </Button>
        </div>
      </form>
    </Dialog>
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
/** Follows a job or image log page by page; keeps the newest 512 KB. */
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
          const chunk =
            source === "image"
              ? await compile.imageLog(next.current)
              : await compile.jobLog(source, next.current, 256 * 1024);
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

function LogDetails({ label, source, live }: { label: string; source: string; live: boolean }) {
  const [open, setOpen] = useState(live);
  return (
    <details className="compile-details" open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>
        <CaretRight size={12} aria-hidden="true" />
        {label}
      </summary>
      {open && <LogView source={source} live={live} />}
    </details>
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
            ? t("ccache 先查 slot 自己工作盘上的本地缓存，再查所有 slot 共享的缓存。")
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

interface Draft {
  aptMirror: string;
  network: boolean;
  idleMinutes: string;
  bazelrc: string;
  env: string;
  excludes: string;
  keepLast: string;
  artifactsGib: string;
}
const draftOf = (settings: CompileSettings): Draft => ({
  aptMirror: settings.image.aptMirror ?? "",
  network: settings.network,
  idleMinutes: String(settings.idleMinutes),
  bazelrc: settings.bazelrc.join("\n"),
  env: settings.env.map((item) => `${item.name}=${item.value}`).join("\n"),
  excludes: settings.excludes.join("\n"),
  keepLast: String(settings.artifacts.keepLast),
  artifactsGib: String(settings.artifacts.maxGib),
});
const lines = (text: string) =>
  text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);

/** Converts a draft into a settings patch, or the first validation problem. */
function patchOf(draft: Draft): { patch: SettingsPatch } | { error: string } {
  const mirror = draft.aptMirror.trim();
  if (mirror && !/^https?:\/\/\S+$/.test(mirror)) return { error: translate("镜像源需要是 http 或 https 地址。") };
  const idle = Number(draft.idleMinutes);
  if (!(Number.isInteger(idle) && idle >= 1 && idle <= 1440))
    return { error: translate("空闲停止时间需要在 1–1440 分钟之间。") };
  const env: { name: string; value: string }[] = [];
  for (const line of lines(draft.env)) {
    const index = line.indexOf("=");
    const name = index < 0 ? line : line.slice(0, index).trim();
    if (index < 0 || !validEnvName(name)) return { error: translate("环境变量格式应为 NAME=value：{line}", { line }) };
    env.push({ name, value: line.slice(index + 1) });
  }
  const excludes = lines(draft.excludes);
  const badPattern = excludes.find(
    (pattern) => pattern.startsWith("/") || pattern.split("/").some((part) => part === ".." || part === "." || !part),
  );
  if (badPattern) return { error: translate("不同步规则需要是工作区内的相对路径：{pattern}", { pattern: badPattern }) };
  const keepLast = Number(draft.keepLast);
  const maxGib = Number(draft.artifactsGib);
  if (!(Number.isInteger(keepLast) && keepLast >= 1 && keepLast <= 10000))
    return { error: translate("保留数量需要在 1–10000 之间。") };
  if (!(Number.isInteger(maxGib) && maxGib >= 0)) return { error: translate("产物总大小需要是 0 或正整数。") };
  return {
    patch: {
      image: { aptMirror: mirror || null },
      network: draft.network,
      idleMinutes: idle,
      bazelrc: lines(draft.bazelrc),
      env,
      excludes,
      artifacts: { keepLast, maxGib },
    },
  };
}

function FieldBlock({ id, label, hint, children }: { id: string; label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="compile-field">
      <label htmlFor={id}>{label}</label>
      {hint && <span className="compile-muted">{hint}</span>}
      {children}
    </div>
  );
}

function SettingsForm({ snapshot, run }: { snapshot: CompileSnapshot; run: Run }) {
  const { t } = useI18n();
  const field = useId();
  const saved = useMemo(() => draftOf(snapshot.settings), [snapshot.settings]);
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
  const result = patchOf(draft);
  const update = <K extends keyof Draft>(key: K, value: Draft[K]) =>
    setDraft((current) => ({ ...current, [key]: value }));
  return (
    <form
      className="compile-settings"
      onSubmit={(event) => {
        event.preventDefault();
        if (!("patch" in result)) return;
        setSaving(true);
        void run(() => compile.patchSettings(result.patch)).then((ok) => {
          setSaving(false);
          if (ok) setBase(JSON.stringify(draft));
        });
      }}
    >
      <section className="settings-group compile-group">
        <h2>{t("Linux 系统镜像")}</h2>
        <ImageCard image={snapshot.image} run={run} />
        <div className="card compile-card-body">
          <FieldBlock
            id={`${field}-mirror`}
            label={t("Ubuntu 软件源")}
            hint={t("下载镜像软件包的位置。留空使用 Ubuntu 官方源，国内网络可以使用就近的镜像；更换后不需要重新构建。")}
          >
            <Input
              id={`${field}-mirror`}
              controlSize="sm"
              value={draft.aptMirror}
              placeholder="http://ports.ubuntu.com/ubuntu-ports"
              disabled={saving}
              onChange={(event) => update("aptMirror", event.target.value)}
            />
          </FieldBlock>
        </div>
      </section>
      <PackagesSection packages={snapshot.packages} image={snapshot.image} run={run} />
      <section className="settings-group compile-group">
        <h2>{t("虚拟机")}</h2>
        <div className="card settings-list">
          <SettingsRow label={t("虚拟机联网")} description={t("关闭后编译不能下载依赖；源码同步和共享缓存不受影响。")}>
            <input
              type="checkbox"
              role="switch"
              className="switch"
              aria-label={t("虚拟机联网")}
              checked={draft.network}
              disabled={saving}
              onChange={(event) => update("network", event.target.checked)}
            />
          </SettingsRow>
          <SettingsRow
            label={t("空闲后停止虚拟机")}
            description={t("保持虚拟机运行可以复用 Bazel 的分析结果，停止后释放内存。")}
          >
            <span className="compile-unit-field compact">
              <Input
                controlSize="sm"
                type="number"
                inputMode="numeric"
                min={1}
                max={1440}
                aria-label={t("空闲后停止虚拟机")}
                value={draft.idleMinutes}
                disabled={saving}
                onChange={(event) => update("idleMinutes", event.target.value)}
              />
              <span>{t("分钟")}</span>
            </span>
          </SettingsRow>
        </div>
      </section>
      <section className="settings-group compile-group">
        <h2>{t("编译环境")}</h2>
        <div className="card compile-card-body">
          <FieldBlock
            id={`${field}-bazelrc`}
            label={t("Bazel 配置")}
            hint={t("每行一条，追加到虚拟机的 /etc/bazel.bazelrc。")}
          >
            <textarea
              id={`${field}-bazelrc`}
              className="compile-textarea"
              rows={3}
              spellCheck={false}
              placeholder="build --jobs=8"
              value={draft.bazelrc}
              disabled={saving}
              onChange={(event) => update("bazelrc", event.target.value)}
            />
          </FieldBlock>
          <FieldBlock id={`${field}-env`} label={t("环境变量")} hint={t("每行一个 NAME=value，提供给所有编译命令。")}>
            <textarea
              id={`${field}-env`}
              className="compile-textarea"
              rows={3}
              spellCheck={false}
              placeholder="IDF_TOOLS_PATH=/work/home/.espressif"
              value={draft.env}
              disabled={saving}
              onChange={(event) => update("env", event.target.value)}
            />
          </FieldBlock>
          <FieldBlock
            id={`${field}-excludes`}
            label={t("不同步的路径")}
            hint={t("每行一条工作区内的规则，支持 *、? 和 **。根目录的 bazel-* 链接总是跳过。")}
          >
            <textarea
              id={`${field}-excludes`}
              className="compile-textarea"
              rows={3}
              spellCheck={false}
              placeholder="node_modules"
              value={draft.excludes}
              disabled={saving}
              onChange={(event) => update("excludes", event.target.value)}
            />
          </FieldBlock>
        </div>
      </section>
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
          <Button controlSize="sm" type="submit" variant="primary" disabled={saving || "error" in result}>
            {t(saving ? "正在保存…" : "保存")}
          </Button>
        </div>
      )}
    </form>
  );
}

const packageLabels: Record<CompilePackage["state"], string> = {
  pending: "等待下载",
  downloading: "正在下载",
  downloaded: "等待打包",
  packing: "正在打包",
  installed: "已安装",
  failed: "失败",
  unsupported: "暂不支持",
};

/** Registry packages: all downloaded and packed automatically; no choices. */
function PackagesSection({ packages, image, run }: { packages: CompilePackages; image: CompileImage; run: Run }) {
  const { t } = useI18n();
  const [working, setWorking] = useState(false);
  const { registry, pack } = packages;
  const refresh = () => {
    setWorking(true);
    void run(compile.refreshPackages).finally(() => setWorking(false));
  };
  const packText =
    pack.state === "ready"
      ? t("已打包 {size} · {time}", { size: formatSize(pack.bytes), time: relativeTime(pack.builtAt) })
      : pack.state === "packing"
        ? t("正在打包：{phase}", { phase: translate(pack.phase ?? "正在准备") })
        : pack.state === "stale"
          ? image.status === "ready"
            ? t("下载完成后会重新打包")
            : t("系统镜像就绪后打包")
          : pack.state === "failed"
            ? t("打包失败：{error}", { error: pack.error })
            : t("还没有开发包");
  return (
    <section className="settings-group compile-group">
      <h2>{t("开发包")}</h2>
      <div className="card compile-packages-card">
        <div className="compile-packages-head">
          <div className="compile-packages-summary">
            <span>
              {t("{platform} 的全部工具链和 SDK 会自动下载并保持更新，以只读磁盘挂到每个 slot 的 {path}。", {
                platform: packages.platform,
                path: packages.guestPath,
              })}
            </span>
            <span className={pack.state === "failed" ? "error" : "compile-muted"}>
              {registry.refreshing
                ? t("正在检查更新…")
                : registry.error
                  ? t("检查更新失败：{error}", { error: registry.error })
                  : registry.fetchedAt
                    ? t("目录 {commit} · 检查于 {time}", {
                        commit: registry.commit?.slice(0, 7) ?? "",
                        time: relativeTime(registry.fetchedAt),
                      })
                    : t("还没有读取目录")}
              {" · "}
              {packText}
            </span>
          </div>
          <Button controlSize="sm" disabled={working || registry.refreshing} onClick={refresh}>
            {t("检查更新")}
          </Button>
        </div>
        {packages.items.length > 0 && (
          <ul className="compile-package-list" aria-label={t("开发包")}>
            {packages.items.map((item) => {
              const busy = ["downloading", "packing"].includes(item.state);
              const percent =
                item.state === "downloading" && item.total ? Math.round(((item.done ?? 0) / item.total) * 100) : null;
              return (
                <li key={`${item.kind}-${item.id}-${item.version}`} className="compile-package-row">
                  <span className="compile-package-name">
                    {item.name}
                    <code>{item.version}</code>
                    {item.default && <span className="compile-tag">{t("默认")}</span>}
                  </span>
                  <code className="compile-package-path" title={item.path}>
                    {item.path}
                  </code>
                  <span className="compile-package-size">{formatSize(item.download)}</span>
                  <span
                    className={`compile-package-state ${item.state}`}
                    title={item.error}
                    role={item.state === "failed" ? "alert" : undefined}
                  >
                    {busy && <CircleNotch size={12} className="spin" aria-hidden="true" />}
                    {t(packageLabels[item.state])}
                    {percent !== null && ` ${percent}%`}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </section>
  );
}

/** Settings › 编译: the Linux image, packages, VM and build settings. */
export function CompilingSettingsPage() {
  const { t } = useI18n();
  const { snapshot, error: loadError } = useCompileSnapshot();
  const [error, setError] = useState("");
  const [working, setWorking] = useState(false);
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
  const unprepared = snapshot && !snapshot.packages.active && snapshot.image.status === "missing";
  return (
    <>
      <PageHeader crumbs={[{ label: t("设置") }, { label: t("编译") }]} />
      <div className="page settings-page compile-page">
        <div className="page-description">
          <p>{t("编译虚拟机的系统和工具。")}</p>
          <span>
            {t(
              "Linux 系统镜像的软件包是固定的，开发包来自 GizMoth 的工具链目录。两者都会自动下载并保持更新，不需要手动选择。",
            )}
          </span>
        </div>
        {demo && <p className="notice">{t("演示模式：下载和打包仅模拟，不会启动虚拟机或修改本机文件。")}</p>}
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
        {unprepared && snapshot.available.ok && (
          <div className="card compile-prepare">
            <span>{t("现在下载 Linux 系统镜像和开发包，第一次编译就不用等待。之后会自动保持更新。")}</span>
            <Button
              variant="primary"
              controlSize="sm"
              disabled={working}
              onClick={() => {
                setWorking(true);
                void run(compile.prepare).finally(() => setWorking(false));
              }}
            >
              {t("准备编译环境")}
            </Button>
          </div>
        )}
        {snapshot && <SettingsForm snapshot={snapshot} run={run} />}
      </div>
    </>
  );
}
