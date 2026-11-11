import { Fragment, useCallback, useEffect, useId, useRef, useState } from "react";
import { ArrowsClockwise, CaretRight, Cpu, FolderOpen, Plus, Toolbox } from "@phosphor-icons/react";
import { demo, errorMessage, native } from "../bridge";
import {
  devenv,
  formatBytes,
  hostLabels,
  type DevEnvBlocked,
  type DevEnvChip,
  type DevEnvEnvironment,
  type DevEnvVersion,
  type DevEnvInstallation,
  type DevEnvJob,
  type DevEnvState,
  type DevEnvToolchain,
  type EntryStatus,
  type RemovalPreview,
  type RemovalTarget,
  type ToolchainCategory,
} from "../devenv";
import { uninstalledVersions, selectedVersion } from "../devenv-versions";
import { t as translate, useI18n } from "../i18n";
import { Button, Dialog, Dot, IconButton, Input, PageHeader, Select, relativeTime } from "../ui";
import "./DevEnv.css";

const STALE_MS = 12 * 60 * 60 * 1000;
// Refresh the registry at most once per app session; installed state is always local.
let refreshedThisSession = false;
const dismissedJobs = new Set<string>();

const categoryLabels: Record<ToolchainCategory, string> = {
  compiler: "编译器",
  "build-tool": "构建工具",
  "post-build": "后处理工具",
  debugger: "调试器",
  utility: "工具",
  runtime: "运行时",
};
const phaseLabels: Record<DevEnvJob["phase"], string> = {
  resolving: "准备中",
  downloading: "下载中",
  extracting: "解压中",
  installing: "写入文件",
  checking: "检查中",
  committing: "保存记录",
  cleaning: "清理中",
  done: "完成",
};

function blockedText(blocked: DevEnvBlocked | null, reason: string | null) {
  switch (blocked?.code) {
    case "planned":
      return translate("配方待补");
    case "unsupported-host":
      return blocked.detail.length
        ? translate("此电脑不支持 · 需要 {hosts}", {
            hosts: blocked.detail.map((host) => hostLabels[host] ?? host).join(" / "),
          })
        : translate("此电脑不支持");
    case "unsupported-operation":
      return translate("需要更新 GizMoth 才能安装");
    case "missing-program":
      return translate("请先在本机安装 {program}", { program: blocked.detail });
    case "invalid":
      return translate(blocked.detail);
  }
  return reason ? translate(reason) : "";
}

function statusTone(status: EntryStatus): "ok" | "warn" | "idle" | "off" {
  return status === "installed" ? "ok" : status === "unsupported" ? "warn" : status === "installing" ? "idle" : "off";
}

function useDevEnv() {
  const [state, setState] = useState<DevEnvState | null>(null);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const run = useCallback(async (key: string, operation: () => Promise<DevEnvState | null>) => {
    setBusy(key);
    setError("");
    try {
      const next = await operation();
      if (alive.current && next) setState(next);
      return next;
    } catch (e) {
      if (alive.current) setError(errorMessage(e));
      return null;
    } finally {
      if (alive.current) setBusy("");
    }
  }, []);

  useEffect(() => {
    void run("loading", devenv.state).then((loaded) => {
      if (!alive.current || !loaded || refreshedThisSession) return;
      if (!loaded.registry || Date.now() - loaded.registry.fetchedAt > STALE_MS) {
        refreshedThisSession = true;
        void run("refresh", devenv.refresh);
      }
    });
  }, [run]);

  const running = state?.job?.state === "running";
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => {
      devenv
        .state()
        .then((next) => alive.current && setState(next))
        .catch(() => {});
    }, 600);
    return () => clearInterval(timer);
  }, [running]);

  const refresh = useCallback(() => {
    refreshedThisSession = true;
    return run("refresh", devenv.refresh);
  }, [run]);
  return { state, setState, busy, error, setError, run, refresh };
}

function RegistryFooter({ state }: { state: DevEnvState }) {
  const { t } = useI18n();
  if (!state.registry) return null;
  return (
    <p className="devenv-registry">
      {t("目录来自 {repository} · {commit} · 更新于 {time}", {
        repository: state.registry.repository.replace("https://github.com/", ""),
        commit: state.registry.commit.slice(0, 7),
        time: relativeTime(state.registry.fetchedAt),
      })}
    </p>
  );
}

function JobPanel({ job, onCancel }: { job: DevEnvJob | null; onCancel: (id: string) => void }) {
  const { t } = useI18n();
  const [, rerender] = useState(0);
  if (!job || (job.state !== "running" && dismissedJobs.has(job.id))) return null;
  const percent = job.total ? Math.min(100, Math.round((job.received / job.total) * 100)) : null;
  const title =
    job.state === "running"
      ? t("正在安装 {name}", { name: job.name })
      : job.state === "succeeded"
        ? t("{name} 已安装", { name: job.name })
        : job.state === "cancelled"
          ? t("已取消安装 {name}", { name: job.name })
          : t("{name} 安装失败", { name: job.name });
  return (
    <section className={`card devenv-job devenv-job-${job.state}`} aria-live="polite" aria-label={t("安装任务")}>
      <div className="devenv-job-head">
        <Dot tone={job.state === "succeeded" ? "ok" : job.state === "failed" ? "warn" : "idle"} />
        <span className="devenv-job-title">{title}</span>
        {job.state === "running" && (
          <span className="devenv-job-phase">
            {t(phaseLabels[job.phase])}
            {job.total > 0 && ` · ${formatBytes(job.received)} / ${formatBytes(job.total)}`}
          </span>
        )}
        {job.state === "running" ? (
          <Button controlSize="sm" variant="ghost" onClick={() => onCancel(job.id)}>
            {t("取消")}
          </Button>
        ) : (
          <Button
            controlSize="sm"
            variant="ghost"
            onClick={() => {
              dismissedJobs.add(job.id);
              rerender((n) => n + 1);
            }}
          >
            {t("关闭")}
          </Button>
        )}
      </div>
      {job.state === "running" && (
        <div
          className="devenv-progress"
          role="progressbar"
          aria-label={t("安装进度")}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent ?? undefined}
        >
          <span style={{ width: `${percent ?? 0}%` }} />
        </div>
      )}
      {job.error && (
        <p className="error" role="alert">
          {t(job.error)}
        </p>
      )}
      {job.log.length > 0 && (
        <details className="devenv-log">
          <summary>
            <CaretRight size={12} aria-hidden="true" />
            {t("安装日志")}
          </summary>
          <pre tabIndex={0}>{job.log.map((line) => translate(line)).join("\n")}</pre>
        </details>
      )}
    </section>
  );
}

function PageNotices({
  state,
  busy,
  error,
  onRetry,
}: {
  state: DevEnvState | null;
  busy: string;
  error: string;
  onRetry: () => void;
}) {
  const { t } = useI18n();
  return (
    <>
      {demo && <p className="notice">{t("演示模式：安装和删除仅模拟，不会下载或修改本机文件。")}</p>}
      {!native && <p className="devenv-hint">{t("浏览器可查看目录，安装和本地工具链管理请使用桌面应用。")}</p>}
      {native && state && !state.host && (
        <p className="notice">{t("GizMoth 暂不支持在此电脑的平台上安装开发环境。")}</p>
      )}
      {state?.error && (
        <p className="error banner" role="alert">
          {t(state.error)}
        </p>
      )}
      {error && (
        <p className="error banner devenv-error" role="alert">
          <span>{t(error)}</span>
          {!state?.registry && (
            <Button controlSize="sm" variant="ghost" disabled={Boolean(busy)} onClick={onRetry}>
              {t("重试")}
            </Button>
          )}
        </p>
      )}
      {!state?.registry && !error && (
        <p className="devenv-loading" role="status">
          {t(busy === "refresh" ? "正在从 GitHub 获取开发环境目录…" : "正在读取开发环境…")}
        </p>
      )}
    </>
  );
}

function RefreshButton({ busy, onRefresh }: { busy: string; onRefresh: () => void }) {
  const { t } = useI18n();
  return (
    <IconButton
      aria-label={t("刷新开发环境目录")}
      title={t("刷新开发环境目录")}
      disabled={Boolean(busy)}
      onClick={onRefresh}
    >
      <ArrowsClockwise size={15} className={busy === "refresh" ? "spin" : undefined} />
    </IconButton>
  );
}

function InstallationLocation({ path, disabled, onOpen }: { path: string; disabled: boolean; onOpen: () => void }) {
  const { t } = useI18n();
  return (
    <div className="devenv-location">
      <div className="devenv-location-copy">
        <span className="devenv-location-label">{t("安装位置")}</span>
        <code className="devenv-location-path">{path}</code>
      </div>
      <Button controlSize="sm" variant="ghost" disabled={disabled} onClick={onOpen} title={path}>
        <FolderOpen size={15} aria-hidden="true" />
        {t("打开文件夹")}
      </Button>
    </div>
  );
}

function ReinstallDialog({
  target,
  name,
  path,
  onClose,
  onStarted,
}: {
  target: RemovalTarget;
  name: string;
  path: string;
  onClose: () => void;
  onStarted: (state: DevEnvState) => void;
}) {
  const { t } = useI18n();
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState("");
  async function reinstall() {
    setStarting(true);
    setError("");
    try {
      onStarted(await devenv.reinstall(target));
    } catch (e) {
      setError(errorMessage(e));
      setStarting(false);
    }
  }
  return (
    <Dialog title={t("重新安装 {name}", { name })} onClose={onClose} busy={starting}>
      <p className="dialog-text">{t("将重新安装当前版本。新副本校验成功后才替换旧安装；失败或取消会保留旧安装。")}</p>
      {target.kind === "chip" && (
        <p className="dialog-text">{t("只重新安装此芯片的 SDK，共享工具链和本地工具链会保留。")}</p>
      )}
      <p className="devenv-location-label">{t("当前安装位置")}</p>
      <code className="devenv-reinstall-path">{path}</code>
      <p className="devenv-removal-note devenv-muted">{t("旧目录中的手动修改会在替换后删除。新安装使用独立目录。")}</p>
      {error && (
        <p className="error" role="alert">
          {t(error)}
        </p>
      )}
      <div className="dialog-actions">
        <Button disabled={starting} onClick={onClose}>
          {t("取消")}
        </Button>
        <Button variant="primary" disabled={starting} onClick={() => void reinstall()}>
          {t(starting ? "正在开始…" : "重新安装")}
        </Button>
      </div>
    </Dialog>
  );
}

function RemovalDialog({
  target,
  name,
  onClose,
  onRemoved,
}: {
  target: RemovalTarget;
  name: string;
  onClose: () => void;
  onRemoved: (state: DevEnvState) => void;
}) {
  const { t } = useI18n();
  const [preview, setPreview] = useState<RemovalPreview | null>(null);
  const [error, setError] = useState("");
  const [removing, setRemoving] = useState(false);
  useEffect(() => {
    let current = true;
    devenv
      .removePreview(target)
      .then((value) => current && setPreview(value))
      .catch((e) => current && setError(errorMessage(e)));
    return () => {
      current = false;
    };
  }, [target]);
  async function confirm() {
    if (!preview) return;
    setRemoving(true);
    setError("");
    try {
      onRemoved(await devenv.remove(target, preview.confirm));
    } catch (e) {
      setError(errorMessage(e));
      setRemoving(false);
    }
  }
  const list = (items: DevEnvInstallation[]) => (
    <ul className="devenv-removal-list">
      {items.map((item) => (
        <li key={item.installationId}>
          <span>
            {item.name} <span className="devenv-muted">{item.version}</span>
          </span>
          <code title={item.path}>{item.path}</code>
        </li>
      ))}
    </ul>
  );
  const title = target.kind === "chip" ? t("删除 {name}", { name }) : t("移除 {name}", { name });
  return (
    <Dialog title={title} onClose={onClose} busy={removing}>
      {!preview && !error && <p className="dialog-text">{t("正在计算要删除的内容…")}</p>}
      {preview?.action === "blocked" && (
        <p className="dialog-text">
          {preview.chips.length + (preview.environments?.length ?? 0)
            ? t("{items} 仍在使用它，请先移除使用它的芯片或环境。", {
                items: [...preview.chips, ...(preview.environments ?? [])].join("、"),
              })
            : t("其他工具链仍依赖它，请先移除：{items}", {
                items: preview.dependants.map((item) => `${item.name} ${item.version}`).join("、"),
              })}
        </p>
      )}
      {preview?.action === "unregister" && (
        <>
          <p className="dialog-text">{t("只取消登记，不会删除你的文件：")}</p>
          {list(preview.keep)}
        </>
      )}
      {preview?.action === "delete" && (
        <>
          <p className="dialog-text">
            {t(
              preview.delete.length
                ? "将删除以下由 GizMoth 安装的内容："
                : "将移除此环境的安装记录，共享或单独安装的组件会保留。",
            )}
          </p>
          {list(preview.delete)}
          {preview.keep.length > 0 && (
            <>
              <p className="dialog-text">{t("以下内容仍被其他环境使用或由你单独安装，会保留：")}</p>
              {list(preview.keep)}
            </>
          )}
          <p className="devenv-muted devenv-removal-note">
            {t("在这些目录中手动修改过的文件也会一并删除。下载缓存保留。")}
          </p>
        </>
      )}
      {error && (
        <p className="error" role="alert">
          {t(error)}
        </p>
      )}
      <div className="dialog-actions">
        <Button disabled={removing} onClick={onClose}>
          {t(preview?.action === "blocked" ? "关闭" : "取消")}
        </Button>
        {preview && preview.action !== "blocked" && (
          <Button variant="danger" disabled={removing} onClick={() => void confirm()}>
            {removing ? t("正在删除…") : t(preview.action === "unregister" ? "取消登记" : "删除")}
          </Button>
        )}
      </div>
    </Dialog>
  );
}

export function EnvironmentsPage({ onToolchains }: { onToolchains: () => void }) {
  const { t } = useI18n();
  const { state, setState, busy, error, run, refresh } = useDevEnv();
  const [removing, setRemoving] = useState<DevEnvEnvironment | null>(null);
  const running = state?.job?.state === "running";
  return (
    <>
      <PageHeader
        crumbs={[{ label: t("开发环境") }, { label: t("通用环境") }]}
        actions={<RefreshButton busy={busy} onRefresh={() => void refresh()} />}
      />
      <div className="page devenv-page">
        <div className="page-description">
          <span>{t("一次准备开发环境需要的全部工具。相同版本自动复用，不同版本可以并存。")}</span>
        </div>
        <PageNotices state={state} busy={busy} error={error} onRetry={() => void refresh()} />
        <JobPanel job={state?.job ?? null} onCancel={(id) => void run("cancel", () => devenv.cancel(id))} />
        <section className="devenv-chips" aria-label={t("通用环境")}>
          {(state?.environments ?? []).map((environment) => (
            <article className="card devenv-chip" key={environment.id} aria-label={environment.name}>
              <div className="devenv-chip-head">
                <span className="devenv-chip-glyph" aria-hidden="true">
                  <Toolbox size={20} />
                </span>
                <div className="devenv-chip-identity">
                  <h3>{environment.name}</h3>
                </div>
                {environment.status === "available" && (
                  <Button
                    controlSize="sm"
                    variant="primary"
                    disabled={!native || Boolean(busy) || running}
                    title={!native ? t("安装需桌面应用") : undefined}
                    aria-label={`${t("安装")} · ${environment.name}`}
                    onClick={() =>
                      void run(`install:${environment.id}`, () => devenv.install("environment", environment.id))
                    }
                  >
                    {t("安装")}
                  </Button>
                )}
                {environment.status === "installed" && (
                  <Button
                    controlSize="sm"
                    variant="ghost"
                    disabled={Boolean(busy) || running}
                    aria-label={`${t("删除")} · ${environment.name}`}
                    onClick={() => setRemoving(environment)}
                  >
                    {t("删除")}
                  </Button>
                )}
              </div>
              <p className="devenv-chip-description">{environment.description}</p>
              <ul className="devenv-packages" aria-label={t("{name} 包含的组件", { name: environment.name })}>
                {environment.packages.map((component) => (
                  <li key={`${component.id}@${component.version}`}>
                    <span>{component.name}</span>
                    <span className="devenv-muted">
                      {component.version} · {t(component.installed ? "已有" : "将安装")}
                    </span>
                  </li>
                ))}
              </ul>
              <span className="devenv-state">
                <Dot tone={statusTone(environment.status)} />
                {environment.status === "installed"
                  ? t("已安装")
                  : environment.status === "installing"
                    ? t("安装中…")
                    : environment.status === "available"
                      ? t(native ? "可安装" : "安装需桌面应用")
                      : blockedText(environment.blocked, environment.reason)}
              </span>
            </article>
          ))}
        </section>
        {state?.registry && (
          <p className="devenv-hint">
            {t("每个组件的版本、安装位置和使用方可在工具链中查看。")}
            <button type="button" className="text-action devenv-link" onClick={onToolchains}>
              {t("查看工具链")}
              <CaretRight size={13} aria-hidden="true" />
            </button>
          </p>
        )}
        {state && <RegistryFooter state={state} />}
      </div>
      {removing && (
        <RemovalDialog
          target={{ kind: "environment", id: removing.id }}
          name={removing.name}
          onClose={() => setRemoving(null)}
          onRemoved={(next) => {
            setState(next);
            setRemoving(null);
          }}
        />
      )}
    </>
  );
}

export function ChipsPage({ onToolchains }: { onToolchains: () => void }) {
  const { t } = useI18n();
  const { state, setState, busy, error, run, refresh } = useDevEnv();
  const [removing, setRemoving] = useState<DevEnvChip | null>(null);
  const [reinstalling, setReinstalling] = useState<DevEnvChip | null>(null);
  const jobRunning = state?.job?.state === "running";
  return (
    <>
      <PageHeader
        crumbs={[{ label: t("开发环境") }, { label: t("芯片") }]}
        actions={<RefreshButton busy={busy} onRefresh={() => void refresh()} />}
      />
      <div className="page devenv-page">
        <div className="page-description">
          <span>{t("选择要开发的芯片。安装时会自动准备对应的 SDK、工具链和构建工具。")}</span>
        </div>
        <PageNotices state={state} busy={busy} error={error} onRetry={() => void refresh()} />
        <JobPanel job={state?.job ?? null} onCancel={(id) => void run("cancel", () => devenv.cancel(id))} />
        {state && state.chips.length > 0 && (
          <section className="devenv-chips" aria-label={t("芯片")}>
            {state.chips.map((chip) => (
              <ChipCard
                key={chip.id}
                chip={chip}
                disabled={!native || Boolean(busy) || jobRunning}
                installing={busy === `install:${chip.id}`}
                onInstall={() => void run(`install:${chip.id}`, () => devenv.install("chip", chip.id))}
                onRemove={() => setRemoving(chip)}
                onReinstall={() => setReinstalling(chip)}
                onOpen={() => {
                  if (chip.installationId)
                    void run(`open:${chip.installationId}`, async () => {
                      await devenv.openFolder(chip.installationId!);
                      return null;
                    });
                }}
              />
            ))}
          </section>
        )}
        {state?.registry && (
          <p className="devenv-hint">
            {t("工具链可以单独安装，也可以登记自己下载的版本。")}
            <button type="button" className="text-action devenv-link" onClick={onToolchains}>
              {t("查看工具链")}
              <CaretRight size={13} aria-hidden="true" />
            </button>
          </p>
        )}
        {state && <RegistryFooter state={state} />}
      </div>
      {reinstalling?.path && (
        <ReinstallDialog
          target={{ kind: "chip", id: reinstalling.id }}
          name={`${reinstalling.name} · ${reinstalling.sdk} ${reinstalling.version ?? ""}`}
          path={reinstalling.path}
          onClose={() => setReinstalling(null)}
          onStarted={(next) => {
            setState(next);
            setReinstalling(null);
          }}
        />
      )}
      {removing && (
        <RemovalDialog
          target={{ kind: "chip", id: removing.id }}
          name={removing.name}
          onClose={() => setRemoving(null)}
          onRemoved={(next) => {
            setState(next);
            setRemoving(null);
          }}
        />
      )}
    </>
  );
}

function ChipCard({
  chip,
  disabled,
  installing,
  onInstall,
  onRemove,
  onReinstall,
  onOpen,
}: {
  chip: DevEnvChip;
  disabled: boolean;
  installing: boolean;
  onInstall: () => void;
  onRemove: () => void;
  onReinstall: () => void;
  onOpen: () => void;
}) {
  const { t } = useI18n();
  const headingId = useId();
  const status =
    chip.status === "installed"
      ? t("已安装")
      : chip.status === "installing"
        ? t("安装中…")
        : chip.status === "available"
          ? t(native ? "可安装" : "安装需桌面应用")
          : blockedText(chip.blocked, chip.reason);
  return (
    <article className={`card devenv-chip devenv-chip-${chip.status}`} aria-labelledby={headingId}>
      <div className="devenv-chip-head">
        <span className="devenv-chip-glyph" aria-hidden="true">
          <Cpu size={20} />
        </span>
        <div className="devenv-chip-identity">
          <h3 id={headingId}>{chip.name}</h3>
          {chip.vendor && <span>{chip.vendor}</span>}
        </div>
        {chip.status === "available" && (
          <Button
            controlSize="sm"
            variant="primary"
            disabled={disabled}
            onClick={onInstall}
            aria-label={`${t("安装")} · ${chip.name}`}
          >
            {installing ? t("正在开始…") : t("安装")}
          </Button>
        )}
        {chip.status === "installed" && (
          <Button
            controlSize="sm"
            variant="ghost"
            disabled={disabled}
            onClick={onRemove}
            aria-label={`${t("删除")} · ${chip.name}`}
          >
            {t("删除")}
          </Button>
        )}
      </div>
      {chip.description && <p className="devenv-chip-description">{chip.description}</p>}
      <div className="devenv-chip-foot">
        <span className="devenv-chip-sdk">
          {chip.sdk}
          {chip.version && <span className="devenv-muted"> {chip.version}</span>}
        </span>
        <span className="devenv-state" title={chip.notes ?? undefined}>
          <Dot tone={statusTone(chip.status)} />
          {status}
        </span>
      </div>
      {chip.packages.length > 0 && (
        <ul className="devenv-packages" aria-label={t("{name} 包含的组件", { name: chip.name })}>
          {chip.packages.map((item) => (
            <li key={`${item.kind}:${item.id}:${item.version}`}>
              <span>{item.name}</span>
              <span className="devenv-muted">
                {item.version}
                {chip.status !== "installed" && ` · ${t(item.installed ? "已有" : "将安装")}`}
              </span>
            </li>
          ))}
        </ul>
      )}
      {chip.status === "installed" && chip.path && chip.installationId && (
        <>
          <InstallationLocation path={chip.path} disabled={disabled} onOpen={onOpen} />
          <div className="devenv-chip-actions">
            <Button
              controlSize="sm"
              disabled={disabled || !chip.reinstallable}
              onClick={onReinstall}
              title={!chip.reinstallable ? t("当前目录未提供可重新安装的相同版本。") : undefined}
            >
              {t("重新安装")}
            </Button>
          </div>
        </>
      )}
    </article>
  );
}

type Row =
  | { kind: "installation"; item: DevEnvInstallation }
  | { kind: "catalog"; item: DevEnvToolchain; versions: DevEnvVersion[]; release: DevEnvVersion | DevEnvToolchain };

export function ToolchainsPage() {
  const { t } = useI18n();
  const { state, setState, busy, error, run, refresh } = useDevEnv();
  const [removing, setRemoving] = useState<DevEnvInstallation | null>(null);
  const [reinstalling, setReinstalling] = useState<DevEnvInstallation | null>(null);
  const [adding, setAdding] = useState(false);
  const [versionChoices, setVersionChoices] = useState<Record<string, string>>({});
  const jobRunning = state?.job?.state === "running";
  const rows: Row[] = state
    ? [
        ...[...state.installations]
          .sort(
            (a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version, undefined, { numeric: true }),
          )
          .map((item) => ({ kind: "installation" as const, item })),
        ...state.toolchains.flatMap<Row>((item) => {
          const versions = uninstalledVersions(item, state.installations);
          if (item.versions.length && !versions.length) return [];
          return [
            {
              kind: "catalog",
              item,
              versions,
              release: selectedVersion(versions, versionChoices[item.id], item.version) ?? item,
            },
          ];
        }),
      ]
    : [];
  const origin = (item: DevEnvInstallation) =>
    item.origin === "local" ? t("本地路径") : item.requestedByUser ? t("单独安装") : t("自动依赖");
  return (
    <>
      <PageHeader
        crumbs={[{ label: t("开发环境") }, { label: t("工具链") }]}
        actions={<RefreshButton busy={busy} onRefresh={() => void refresh()} />}
      />
      <div className="page devenv-page">
        <div className="page-description">
          <span>{t("管理编译器、Python 和构建工具。每个版本独立安装，开发环境自动复用需要的版本。")}</span>
        </div>
        <PageNotices state={state} busy={busy} error={error} onRetry={() => void refresh()} />
        <JobPanel job={state?.job ?? null} onCancel={(id) => void run("cancel", () => devenv.cancel(id))} />
        {state?.registry && (
          <div className="page-list-toolbar">
            <button
              type="button"
              className="text-action"
              disabled={!native || Boolean(busy) || jobRunning || !state.host}
              onClick={() => setAdding(true)}
            >
              <Plus size={14} aria-hidden="true" />
              {t("添加本地工具链")}
            </button>
            <span className="page-list-summary">{t("{count} 个已安装", { count: state.installations.length })}</span>
          </div>
        )}
        {rows.length > 0 && (
          <div className="card devenv-table-wrap">
            <table className="devenv-table">
              <thead>
                <tr>
                  <th scope="col">{t("名称")}</th>
                  <th scope="col">{t("版本")}</th>
                  <th scope="col">{t("类型")}</th>
                  <th scope="col">{t("使用方")}</th>
                  <th scope="col">{t("来源")}</th>
                  <th scope="col">{t("状态")}</th>
                  <th scope="col">
                    <span className="devenv-sr-only">{t("操作")}</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row) =>
                  row.kind === "installation" ? (
                    <Fragment key={row.item.installationId}>
                      <tr>
                        <th scope="row">
                          <span className="devenv-name">{row.item.name}</span>
                        </th>
                        <td className="devenv-version">{row.item.version}</td>
                        <td>{row.item.category ? t(categoryLabels[row.item.category]) : "—"}</td>
                        <td>{[...row.item.chips, ...(row.item.environments ?? [])].join("、") || "—"}</td>
                        <td>{origin(row.item)}</td>
                        <td>
                          <span className="devenv-state">
                            <Dot tone={row.item.healthy ? "ok" : "warn"} />
                            {t(row.item.healthy ? "已安装" : "文件缺失")}
                          </span>
                        </td>
                        <td className="devenv-actions">
                          <div className="devenv-row-actions">
                            {row.item.origin === "managed" && (
                              <Button
                                controlSize="sm"
                                variant="ghost"
                                disabled={!native || Boolean(busy) || jobRunning || !row.item.reinstallable}
                                aria-label={`${t("重新安装")} · ${row.item.name} ${row.item.version}`}
                                title={!row.item.reinstallable ? t("当前目录未提供可重新安装的相同版本。") : undefined}
                                onClick={() => setReinstalling(row.item)}
                              >
                                {t("重新安装")}
                              </Button>
                            )}
                            <Button
                              controlSize="sm"
                              variant="ghost"
                              disabled={!native || Boolean(busy) || jobRunning}
                              aria-label={`${t(row.item.origin === "local" ? "取消登记" : "删除")} · ${row.item.name} ${row.item.version}`}
                              onClick={() => setRemoving(row.item)}
                            >
                              {t(row.item.origin === "local" ? "取消登记" : "删除")}
                            </Button>
                          </div>
                        </td>
                      </tr>
                      <tr className="devenv-install-details">
                        <td colSpan={7}>
                          <InstallationLocation
                            path={row.item.path}
                            disabled={!native || Boolean(busy)}
                            onOpen={() =>
                              void run(`open:${row.item.installationId}`, async () => {
                                await devenv.openFolder(row.item.installationId);
                                return null;
                              })
                            }
                          />
                        </td>
                      </tr>
                    </Fragment>
                  ) : (
                    <tr key={`catalog:${row.item.id}`} className="devenv-catalog-row">
                      <th scope="row">
                        <span className="devenv-name">{row.item.name}</span>
                        <span className="devenv-path devenv-description">{row.item.description}</span>
                      </th>
                      <td className="devenv-version">
                        {row.versions.length > 1 ? (
                          <Select
                            controlSize="sm"
                            className="devenv-version-select"
                            aria-label={`${t("选择版本")} · ${row.item.name}`}
                            value={row.release.version ?? ""}
                            disabled={Boolean(busy) || jobRunning}
                            onValueChange={(version) =>
                              setVersionChoices((values) => ({ ...values, [row.item.id]: version }))
                            }
                            options={row.versions.map((release) => ({
                              value: release.version,
                              label: release.version,
                            }))}
                          />
                        ) : (
                          (row.release.version ?? "—")
                        )}
                      </td>
                      <td>{t(categoryLabels[row.item.category])}</td>
                      <td>—</td>
                      <td>—</td>
                      <td>
                        <span className="devenv-state" title={row.item.notes ?? undefined}>
                          <Dot tone={statusTone(row.release.status)} />
                          {row.release.status === "available"
                            ? t(native ? "未安装" : "安装需桌面应用")
                            : row.release.status === "installing"
                              ? t("安装中…")
                              : blockedText(row.release.blocked, row.release.reason)}
                        </span>
                      </td>
                      <td className="devenv-actions">
                        {row.release.status === "available" && (
                          <Button
                            controlSize="sm"
                            disabled={!native || Boolean(busy) || jobRunning}
                            aria-label={`${t("安装")} · ${row.item.name}`}
                            title={!native ? t("安装需桌面应用") : undefined}
                            onClick={() =>
                              void run(`install:${row.item.id}@${row.release.version}`, () =>
                                devenv.install("toolchain", row.item.id, row.release.version ?? undefined),
                              )
                            }
                          >
                            {busy === `install:${row.item.id}@${row.release.version}` ? t("正在开始…") : t("安装")}
                          </Button>
                        )}
                      </td>
                    </tr>
                  ),
                )}
              </tbody>
            </table>
          </div>
        )}
        {state && <RegistryFooter state={state} />}
      </div>
      {reinstalling && (
        <ReinstallDialog
          target={{ kind: "installation", id: reinstalling.installationId }}
          name={`${reinstalling.name} ${reinstalling.version}`}
          path={reinstalling.path}
          onClose={() => setReinstalling(null)}
          onStarted={(next) => {
            setState(next);
            setReinstalling(null);
          }}
        />
      )}
      {removing && (
        <RemovalDialog
          target={{ kind: "installation", id: removing.installationId }}
          name={`${removing.name} ${removing.version}`}
          onClose={() => setRemoving(null)}
          onRemoved={(next) => {
            setState(next);
            setRemoving(null);
          }}
        />
      )}
      {adding && state && (
        <AddLocalDialog
          toolchains={state.toolchains}
          onClose={() => setAdding(false)}
          onAdded={(next) => {
            setState(next);
            setAdding(false);
          }}
        />
      )}
    </>
  );
}

function AddLocalDialog({
  toolchains,
  onClose,
  onAdded,
}: {
  toolchains: DevEnvToolchain[];
  onClose: () => void;
  onAdded: (state: DevEnvState) => void;
}) {
  const { t } = useI18n();
  const fieldId = useId();
  const [toolchain, setToolchain] = useState(toolchains[0]?.id ?? "");
  const [version, setVersion] = useState(toolchains[0]?.version ?? "");
  const [picking, setPicking] = useState(false);
  const [error, setError] = useState("");
  async function pick() {
    setPicking(true);
    setError("");
    try {
      const next = await devenv.addLocal(toolchain, version.trim());
      if (next) onAdded(next);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setPicking(false);
    }
  }
  return (
    <Dialog title={t("添加本地工具链")} onClose={onClose} busy={picking}>
      <p className="dialog-text">
        {t("登记你自己下载的工具链。GizMoth 只记录路径并检查能否使用，不会修改或删除其中的文件。")}
      </p>
      <form
        className="devenv-form"
        onSubmit={(event) => {
          event.preventDefault();
          void pick();
        }}
      >
        <label htmlFor={`${fieldId}-toolchain`}>{t("工具链")}</label>
        <Select
          id={`${fieldId}-toolchain`}
          aria-label={t("工具链")}
          value={toolchain}
          disabled={picking}
          onValueChange={(value) => {
            setToolchain(value);
            setVersion(toolchains.find((item) => item.id === value)?.version ?? "");
          }}
          options={toolchains.map((item) => ({ value: item.id, label: item.name }))}
        />
        <label htmlFor={`${fieldId}-version`}>{t("版本")}</label>
        <Input
          id={`${fieldId}-version`}
          value={version}
          maxLength={96}
          placeholder="10.3.1"
          disabled={picking}
          onChange={(event) => setVersion(event.target.value)}
        />
        <p className="devenv-muted devenv-form-hint">
          {t("芯片只会使用版本完全一致的工具链。目录中有这个版本时，会按配方检查所选文件夹。")}
        </p>
        {error && (
          <p className="error" role="alert">
            {t(error)}
          </p>
        )}
        <div className="dialog-actions">
          <Button disabled={picking} onClick={onClose}>
            {t("取消")}
          </Button>
          <Button type="submit" variant="primary" disabled={picking || !toolchain || !version.trim()}>
            <FolderOpen size={15} aria-hidden="true" />
            {picking ? t("等待选择…") : t("选择文件夹…")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
