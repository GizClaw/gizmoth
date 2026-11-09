import { t, useI18n } from "../i18n";
import { Button, IconButton, Input } from "../ui";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import {
  Check,
  DownloadSimple as Download,
  CircleNotch as LoaderCircle,
  BracketsCurly,
  Info,
  Play,
  Trash as Trash2,
  Warning,
} from "@phosphor-icons/react";
import {
  bridge,
  demo,
  errorMessage,
  native,
  type Device,
  type PackageInfo,
  type ProjectSummary,
  type Provider,
} from "../bridge";
import { validatePath, type Project } from "../project";
import { Dialog, PageHeader } from "../ui";
import { AgentPanel } from "./AgentPanel";
import { disposeProjectSession, projectSession } from "../agent-session";
import type { ConfirmLabels } from "../agent";
import type { DeviceModel } from "../device-model";
import { beforeAppUpdate } from "../update-preparation";
import { projectKindOf, projectKinds } from "../project-kinds";
import "./WorkspaceNavigation.css";
import { RunTargetDialog } from "./RunTargetDialog";
import { WorkspaceActions } from "./WorkspaceActions";
import { clearSimulatorTarget, openSimulator, registerSimulatorProjectReader } from "../simulator-window";

import { CodeWorkspace } from "./CodeWorkspace";
import { CHAT_DEFAULT, CHAT_MIN, PaneSplitter } from "./PaneSplitter";
import { ProjectInfo } from "./ProjectInfo";
import type { CSSProperties } from "react";
import { formatEditorCode } from "../editor-format";
import { languageName, type EditorLanguage } from "../editor-language";

const chatWidthKey = "gizmoth.workspace.chatWidth";

const formatBytes = (n: number) => (n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KiB`);

export function Workspace({
  project,
  devices,
  deviceId,
  onDevice,
  providers,
  confirm,
  onBack,
  onChanged,
  onDeleted,
  onOpenProviders,
  previewModel,
  onBeforeLeave,
}: {
  onBeforeLeave: (prepare: () => Promise<void>) => () => void;
  previewModel?: DeviceModel;
  project: ProjectSummary;
  devices: Device[];
  deviceId: string;
  onDevice: (id: string) => void;
  providers: Provider[];
  confirm: (title: string, body: string, labels?: ConfirmLabels) => Promise<boolean>;
  onBack: () => void;
  onChanged: () => void;
  onDeleted: () => void;
  onOpenProviders: () => void;
}) {
  useI18n();
  const id = project.id;
  const kind = projectKindOf(project);
  const [showCode, setShowCode] = useState(false);
  const [codeExpanded, setCodeExpanded] = useState(false);
  const panesRef = useRef<HTMLDivElement>(null);
  const [chatWidth, setChatWidth] = useState(() => {
    const stored = Number(localStorage.getItem(chatWidthKey));
    return Number.isFinite(stored) && stored >= CHAT_MIN ? stored : CHAT_DEFAULT;
  });
  const commitChatWidth = useCallback((width: number) => {
    setChatWidth(width);
    localStorage.setItem(chatWidthKey, String(width));
  }, []);
  // Opened on demand each visit so a project never opens with focus in the panel.
  const [infoOpen, setInfoOpen] = useState(false);
  const [chatSlot, setChatSlot] = useState<HTMLSpanElement | null>(null);
  // The project's single conversation and its brief outlive this page.
  const session = useMemo(() => projectSession(id), [id]);
  useSyncExternalStore(session.subscribe, session.getVersion);
  const brief = session.brief;
  const setBrief = useCallback((value: string) => session.setBrief(value), [session]);
  const infoToggle = useRef<HTMLButtonElement>(null);
  const closeInfo = useCallback((restoreFocus: boolean) => {
    setInfoOpen(false);
    if (restoreFocus) infoToggle.current?.focus();
  }, []);
  const [tabs, setTabs] = useState<string[]>([]);
  const [diskFiles, setDiskFiles] = useState<Record<string, string>>({});
  const diskRef = useRef(diskFiles);
  diskRef.current = diskFiles;
  const fileLoadSequence = useRef(0);
  const openSequence = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      fileLoadSequence.current++;
    };
  }, []);
  const [choosingTarget, setChoosingTarget] = useState(false);
  const [active, setActive] = useState("");
  const [content, setContent] = useState("");
  const [saved, setSaved] = useState("");
  const [status, setStatus] = useState<{
    text: string;
    values?: Record<string, string | number>;
    error?: boolean;
  } | null>(null);
  const [busy, setBusy] = useState("");
  const [info, setInfo] = useState<PackageInfo | null>(null);
  const [dialog, setDialog] = useState<"new" | "rename" | "delete-file" | "delete-project" | null>(null);
  const [pathInput, setPathInput] = useState("");
  const lock = useRef(false);
  const activeRef = useRef(active);
  activeRef.current = active;
  const dirtyRef = useRef(false);
  dirtyRef.current = content !== saved;
  const editorRef = useRef({ active, content });
  editorRef.current = { active, content };
  const pendingSave = useRef<Promise<void>>(Promise.resolve());
  const recordSaved = useCallback((path: string, text: string) => {
    if (!alive.current) return;
    diskRef.current = { ...diskRef.current, [path]: text };
    setDiskFiles(diskRef.current);
    if (activeRef.current === path) {
      if (editorRef.current.content === text) dirtyRef.current = false;
      setSaved(text);
    }
  }, []);
  const saveBeforeLeave = useCallback(async () => {
    if (lock.current) throw new Error(t("项目操作尚未完成，请稍后重试。"));
    await pendingSave.current.catch(() => {});
    const current = editorRef.current;
    if (current.active && dirtyRef.current) {
      await bridge.write(id, current.active, current.content);
      recordSaved(current.active, current.content);
      onChanged();
    }
  }, [id, onChanged, recordSaved]);
  useEffect(() => beforeAppUpdate(saveBeforeLeave), [saveBeforeLeave]);
  useEffect(() => onBeforeLeave(saveBeforeLeave), [onBeforeLeave, saveBeforeLeave]);
  const device = devices.find((d) => d.id === deviceId);
  const fileStates = useMemo(
    () =>
      Object.keys(diskFiles)
        .sort()
        .map((path) => ({ path })),
    [diskFiles],
  );

  const loadFiles = useCallback(async () => {
    const sequence = ++fileLoadSequence.current;
    const list = await bridge.files(id);
    const contents = Object.fromEntries(list.map((file) => [file.path, ""]));
    if (alive.current && sequence === fileLoadSequence.current) {
      diskRef.current = contents;
      setDiskFiles(contents);
    }
    return list;
  }, [id]);

  const open = useCallback(
    async (path: string) => {
      const request = ++openSequence.current;
      const text = await bridge.read(id, path);
      if (!alive.current || request !== openSequence.current || (path === activeRef.current && dirtyRef.current))
        return;
      activeRef.current = path;
      editorRef.current = { active: path, content: text };
      dirtyRef.current = false;
      setActive(path);
      setContent(text);
      setSaved(text);
      setTabs((previous) => (previous.includes(path) ? previous : [...previous, path]));
    },
    [id],
  );

  useEffect(() => {
    if (!native) return;
    let gone = false;
    setInfo(null);
    void loadFiles()
      .then((list) => {
        if (gone) return;
        const first = list.find((f) => f.path.endsWith(".lua")) ?? list[0];
        if (first) return open(first.path);
      })
      .catch((e) => {
        if (!gone) setStatus({ text: errorMessage(e), error: true });
      });
    return () => {
      gone = true;
    };
  }, [id, loadFiles, open]);

  // Autosave the active file shortly after typing stops.
  useEffect(() => {
    if (!active || content === saved) return;
    const timer = setTimeout(() => {
      const path = active;
      const writing = pendingSave.current
        .catch(() => {})
        .then(() => bridge.write(id, path, content))
        .then(() => {
          recordSaved(path, content);
          setInfo(null);
          onChanged();
        });
      pendingSave.current = writing;
      void writing.catch((e) => setStatus({ text: errorMessage(e), error: true }));
    }, 500);
    return () => clearTimeout(timer);
  }, [active, content, saved, id, recordSaved, onChanged]);

  const onFilesChanged = useCallback(
    (path?: string) => {
      setInfo(null);
      void loadFiles()
        .then((list) => {
          const current = activeRef.current;
          if (!alive.current || dirtyRef.current) return;
          const existing = new Set(list.map((file) => file.path));
          setTabs((previous) => previous.filter((file) => existing.has(file)));
          if (current && !existing.has(current)) {
            if (list[0]) void open(list[0].path);
            else clearEditor();
          } else if (current && (!path || path === current)) void open(current);
        })
        .catch((e) => setStatus({ text: errorMessage(e), error: true }));
      onChanged();
    },
    [loadFiles, open, onChanged],
  );

  async function task(label: string, action: () => Promise<void>, flush = true): Promise<boolean> {
    if (lock.current) return false;
    lock.current = true;
    setBusy(label);
    setStatus(null);
    try {
      await pendingSave.current.catch(() => {});
      const current = editorRef.current;
      if (flush && current.active && dirtyRef.current) {
        await bridge.write(id, current.active, current.content);
        recordSaved(current.active, current.content);
        onChanged();
      }
      await action();
      return true;
    } catch (e) {
      setStatus({ text: errorMessage(e), error: true });
      return false;
    } finally {
      lock.current = false;
      setBusy("");
    }
  }

  const selectFile = (path: string): Promise<boolean> =>
    path === active ? Promise.resolve(true) : task("open", () => open(path));

  function clearEditor() {
    activeRef.current = "";
    editorRef.current = { active: "", content: "" };
    dirtyRef.current = false;
    setActive("");
    setContent("");
    setSaved("");
  }
  function closeTab(path: string) {
    void task("close", async () => {
      const remaining = tabs.filter((tab) => tab !== path);
      if (path === active) {
        const next = remaining[Math.min(tabs.indexOf(path), remaining.length - 1)];
        if (next) await open(next);
        else clearEditor();
      }
      setTabs(remaining);
    });
  }
  function formatActive(language: EditorLanguage) {
    if (!active) return;
    void task("format", async () => {
      const source = { ...editorRef.current };
      const formatted = await formatEditorCode(source.content, language);
      if (!alive.current || editorRef.current.active !== source.active || editorRef.current.content !== source.content)
        return;
      setContent(formatted);
      editorRef.current = { active: source.active, content: formatted };
      dirtyRef.current = formatted !== source.content;
      await bridge.write(id, source.active, formatted);
      recordSaved(source.active, formatted);
      setInfo(null);
      onChanged();
      setStatus({ text: "已格式化并保存 · {language}", values: { language: t(languageName(language)) } });
    });
  }

  const check = () =>
    task("check", async () => {
      const next = await bridge.check(id);
      setInfo(next);
      setStatus({
        text: "检查通过：{count} 个文件，压缩后 {size}",
        values: { count: next.files, size: formatBytes(next.bytes) },
      });
    });

  const selectHardware = (target: Device) => {
    setChoosingTarget(false);
    clearSimulatorTarget(id);
    onDevice(target.id);
  };

  const push = (target: Device) =>
    task("push", async () => {
      const next = await bridge.check(id);
      setInfo(next);
      await bridge.install(id, target.id);
      await bridge.run(target.id, next.appId);
      setStatus({
        text: demo ? "演示：已模拟推送 {appId} 到「{name}」并模拟运行" : "已推送 {appId} 到「{name}」并发送运行请求",
        values: { appId: next.appId, name: target.name },
      });
    });

  const runInSimulator = (profileId: string) => {
    if (lock.current) return;
    lock.current = true;
    setBusy("simulator");
    setStatus(null);
    // Open synchronously from the target click so browser fallback keeps user activation.
    void openSimulator({ profileId, project, deviceModel: previewModel })
      .then(() => setStatus({ text: "已向模拟器发送临时应用请求，安装与运行状态请查看模拟器窗口。" }))
      .catch((error) => setStatus({ text: errorMessage(error), error: true }))
      .finally(() => {
        lock.current = false;
        setBusy("");
      });
  };

  const readPreviewProject = useCallback(async (): Promise<Project> => {
    const { active: selectedPath, content: currentContent } = editorRef.current;
    const list = await bridge.files(id);
    return {
      version: 1,
      files: await Promise.all(
        list.map(async (file) => ({
          path: file.path,
          content: file.path === selectedPath ? currentContent : await bridge.read(id, file.path),
        })),
      ),
    };
  }, [id]);
  useEffect(() => registerSimulatorProjectReader(id, readPreviewProject), [id, readPreviewProject]);

  function openDialog(next: Exclude<typeof dialog, null>) {
    setStatus(null);
    setDialog(next);
  }

  function commitPath() {
    const path = pathInput.trim();
    const problem = validatePath(path);
    if (problem) return setStatus({ text: t(problem), error: true });
    void task("file", async () => {
      if (dialog === "rename") {
        await bridge.renameFile(id, active, path);
        setTabs((previous) => previous.map((tab) => (tab === active ? path : tab)));
      } else {
        await bridge.write(id, path, "");
      }
      setDialog(null);
      await loadFiles();
      await open(path);
      setShowCode(true);
      onChanged();
    });
  }

  // Results fade after a few seconds; errors stay until the next action.
  useEffect(() => {
    if (!status || status.error) return;
    const timer = setTimeout(() => setStatus((current) => (current === status ? null : current)), 6000);
    return () => clearTimeout(timer);
  }, [status]);
  const dirty = content !== saved;
  const busyLabels: Record<string, string> = {
    check: t("正在检查程序包…"),
    push: t("正在推送到设备…"),
    simulator: t("正在打开模拟器…"),
    export: t("正在导出程序包…"),
    format: t("正在格式化并保存…"),
    file: t("正在保存文件…"),
    delete: t("正在删除…"),
    open: t("正在打开文件…"),
    close: t("正在关闭文件…"),
  };
  const statusText = status?.error
    ? status.text
    : busy
      ? (busyLabels[busy] ?? t("正在处理…"))
      : dirty
        ? t("正在保存…")
        : status
          ? t(status.text, status.values)
          : "";
  const statusBusy = !status?.error && (!!busy || dirty);

  return (
    <div className="workspace project-workspace">
      <PageHeader
        crumbs={[{ label: t(projectKinds[kind].label), onClick: onBack }, { label: project.name }]}
        actions={
          statusText ? (
            <span className={`editor-status${status?.error ? " error-text" : ""}`} role="status" title={statusText}>
              {statusBusy ? (
                <LoaderCircle className="spin" size={13} aria-hidden="true" />
              ) : status?.error ? (
                <Warning size={13} aria-hidden="true" />
              ) : (
                <Check size={13} aria-hidden="true" />
              )}
              <span>{statusText}</span>
            </span>
          ) : undefined
        }
      />
      <div className="workspace-body">
        <section className="workspace-authoring" aria-label={t("项目开发")}>
          <div className={`workspace-row${showCode ? " code-visible" : ""}`}>
            <div
              ref={panesRef}
              className={`workspace-panes${showCode ? " code-visible" : ""}${showCode && codeExpanded ? " code-expanded" : ""}`}
              style={{ "--chat-width": `${chatWidth}px` } as CSSProperties}
            >
              <div className="workspace-assistant">
                <div className="workspace-chat-toolbar" aria-label={t("聊天操作")}>
                  <span className="workspace-chat-label">{t("对话")}</span>
                  <div className="workspace-chat-actions">
                    <span className="workspace-chat-slot" ref={setChatSlot} />
                    <IconButton
                      ref={infoToggle}
                      className="workspace-info-toggle"
                      aria-label={t("项目信息")}
                      title={infoOpen ? t("隐藏项目信息") : t("显示项目信息")}
                      aria-expanded={infoOpen}
                      aria-controls={infoOpen ? "workspace-info" : undefined}
                      onClick={() => (infoOpen ? closeInfo(false) : setInfoOpen(true))}
                    >
                      <Info size={17} />
                    </IconButton>
                    <IconButton
                      aria-label={t("运行")}
                      title={t("运行项目")}
                      disabled={!!busy || !native}
                      onClick={() => setChoosingTarget(true)}
                    >
                      {busy === "push" || busy === "simulator" ? (
                        <LoaderCircle className="spin" size={16} />
                      ) : (
                        <Play size={16} />
                      )}
                    </IconButton>
                    <IconButton
                      className="workspace-code-toggle"
                      aria-label={showCode ? t("隐藏代码") : t("显示代码")}
                      title={showCode ? t("隐藏文件与代码") : t("显示文件与代码")}
                      aria-expanded={showCode}
                      aria-controls="workspace-code"
                      onClick={() => {
                        setShowCode((visible) => !visible);
                        setCodeExpanded(false);
                      }}
                    >
                      <BracketsCurly size={17} />
                    </IconButton>
                    <WorkspaceActions
                      label={t("项目操作")}
                      disabled={!!busy || (showCode && codeExpanded)}
                      actions={[
                        {
                          label: t("检查程序"),
                          icon: <Check size={14} />,
                          disabled: !native,
                          onSelect: () => void check(),
                        },
                        {
                          label: t("导出程序包…"),
                          icon: <Download size={14} />,
                          disabled: !native,
                          onSelect: () =>
                            void task("export", async () => {
                              const path = await bridge.exportPackage(id);
                              if (path) setStatus({ text: "已导出：{path}", values: { path } });
                            }),
                        },
                        {
                          label: t("删除项目…"),
                          icon: <Trash2 size={14} />,
                          disabled: !native,
                          danger: true,
                          separator: true,
                          onSelect: () => openDialog("delete-project"),
                        },
                      ]}
                    />
                  </div>
                </div>
                <AgentPanel
                  host={{ projectId: id, onFilesChanged, device: () => device, confirm }}
                  providers={providers}
                  onOpenProviders={onOpenProviders}
                  toolbar={chatSlot}
                />
              </div>
              {showCode && !codeExpanded && (
                <PaneSplitter
                  container={panesRef}
                  width={chatWidth}
                  controls="workspace-code"
                  onResize={setChatWidth}
                  onCommit={commitChatWidth}
                />
              )}
              <div className="workspace-code" id="workspace-code">
                <CodeWorkspace
                  files={fileStates}
                  tabs={tabs}
                  active={active}
                  content={content}
                  formatting={busy === "format"}
                  busy={!!busy || !native || !showCode}
                  expanded={codeExpanded}
                  onContent={setContent}
                  onOpen={selectFile}
                  onCloseTab={closeTab}
                  onCloseAll={() =>
                    void task("close", async () => {
                      setTabs([]);
                      clearEditor();
                    })
                  }
                  onNew={() => {
                    setPathInput(project.appId ? `${project.appId}/` : "");
                    openDialog("new");
                  }}
                  onRename={() => {
                    setPathInput(active);
                    openDialog("rename");
                  }}
                  onDelete={() => openDialog("delete-file")}
                  onFormat={formatActive}
                  onExpand={() => setCodeExpanded((value) => !value)}
                  onHide={() => {
                    setShowCode(false);
                    setCodeExpanded(false);
                  }}
                />
              </div>
            </div>
            {infoOpen && (
              <ProjectInfo
                project={project}
                kindLabel={t(projectKinds[kind].label)}
                files={fileStates.length || project.files}
                info={info}
                device={device}
                busy={busy}
                canRun={native}
                codeVisible={showCode}
                toggle={infoToggle}
                brief={brief}
                onBriefChange={setBrief}
                permissions={session.permissions}
                onPermissionsChange={(next) => void session.setPermissions(next)}
                onCheck={() => void check()}
                onRun={() => setChoosingTarget(true)}
                onShowCode={() => setShowCode(true)}
                onClose={closeInfo}
              />
            )}
          </div>
        </section>
      </div>
      {choosingTarget && (
        <RunTargetDialog
          devices={devices}
          onClose={() => setChoosingTarget(false)}
          onDevice={(target) => {
            selectHardware(target);
            void push(target);
          }}
          onSelectDevice={(target) => {
            selectHardware(target);
            setStatus({ text: "已选择硬件「{name}」，未安装或运行。", values: { name: target.name } });
          }}
          onSimulator={(profileId) => {
            setChoosingTarget(false);
            runInSimulator(profileId);
          }}
        />
      )}
      {(dialog === "new" || dialog === "rename") && (
        <Dialog
          title={dialog === "new" ? t("新建文件") : t("重命名文件")}
          onClose={() => setDialog(null)}
          busy={!!busy}
        >
          <form
            className="form"
            onSubmit={(e) => {
              e.preventDefault();
              commitPath();
            }}
          >
            <label>
              {t("文件路径")}
              <Input autoFocus value={pathInput} onChange={(e) => setPathInput(e.target.value)} spellCheck={false} />
              <span className="hint">
                {t("资源文件需放在 {path}/ 目录下。", { path: project.appId ?? t("<程序 ID>") })}
              </span>
            </label>
            {status?.error && (
              <p className="error" role="alert">
                {status.text}
              </p>
            )}
            <div className="dialog-actions">
              <Button type="button" onClick={() => setDialog(null)}>
                {t("取消")}
              </Button>
              <Button variant="primary" type="submit" disabled={!!busy || !pathInput.trim()}>
                {dialog === "new" ? t("创建") : t("重命名")}
              </Button>
            </div>
          </form>
        </Dialog>
      )}
      {(dialog === "delete-file" || dialog === "delete-project") && (
        <Dialog
          title={dialog === "delete-file" ? t("删除文件") : t("删除项目")}
          onClose={() => setDialog(null)}
          busy={!!busy}
        >
          <p className="dialog-text">
            {dialog === "delete-file"
              ? t("从项目中删除 {path}？", { path: active })
              : t("删除「{name}」的全部文件和对话记录？此操作不能撤销，设备上已安装的程序不受影响。", {
                  name: project.name,
                })}
          </p>
          {status?.error && (
            <p className="error" role="alert">
              {status.text}
            </p>
          )}
          <div className="dialog-actions">
            <Button onClick={() => setDialog(null)}>{t("取消")}</Button>
            <Button
              variant="danger"
              disabled={!!busy}
              onClick={() =>
                dialog === "delete-file"
                  ? void task("delete", async () => {
                      await bridge.removeFile(id, active);
                      setDialog(null);
                      await loadFiles();
                      const remaining = tabs.filter((path) => path !== active);
                      if (remaining.length) await open(remaining[Math.min(tabs.indexOf(active), remaining.length - 1)]);
                      else clearEditor();
                      setTabs(remaining);
                      onChanged();
                    })
                  : void task("delete", async () => {
                      await bridge.deleteProject(id);
                      disposeProjectSession(id);
                      setDialog(null);
                      onDeleted();
                    })
              }
            >
              {t("删除")}
            </Button>
          </div>
        </Dialog>
      )}
    </div>
  );
}
