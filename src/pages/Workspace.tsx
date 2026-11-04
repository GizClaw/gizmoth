import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import {
  Check,
  Download,
  FileCode2,
  FileJson,
  FilePlus2,
  FileText,
  FolderOpen,
  RefreshCw,
  LoaderCircle,
  Pencil,
  Play,
  Trash2,
  Upload,
} from "lucide-react";
import {
  bridge,
  errorMessage,
  native,
  type Device,
  type FileEntry,
  type PackageInfo,
  type ProjectSummary,
  type Provider,
} from "../bridge";
import { validatePath } from "../project";
import { Dialog, PageHeader } from "../ui";
import { AgentPanel } from "./AgentPanel";

const Editor = lazy(() => import("../Editor").then(({ Editor }) => ({ default: Editor })));

const fileIcon = (path: string) => (path.endsWith(".lua") ? FileCode2 : path.endsWith(".json") ? FileJson : FileText);
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
}: {
  project: ProjectSummary;
  devices: Device[];
  deviceId: string;
  onDevice: (id: string) => void;
  providers: Provider[];
  confirm: (title: string, body: string) => Promise<boolean>;
  onBack: () => void;
  onChanged: () => void;
  onDeleted: () => void;
  onOpenProviders: () => void;
}) {
  const id = project.id;
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [active, setActive] = useState("");
  const [content, setContent] = useState("");
  const [saved, setSaved] = useState("");
  const [status, setStatus] = useState<{ text: string; error?: boolean } | null>(null);
  const [busy, setBusy] = useState("");
  const [info, setInfo] = useState<PackageInfo | null>(null);
  const [dialog, setDialog] = useState<"new" | "rename" | "delete-file" | "delete-project" | null>(null);
  const [pathInput, setPathInput] = useState("");
  const lock = useRef(false);
  const activeRef = useRef(active);
  activeRef.current = active;
  const dirtyRef = useRef(false);
  dirtyRef.current = content !== saved;
  const device = devices.find((d) => d.id === deviceId);
  const entry = files.find((f) => /^[^/]+\.lua$/.test(f.path) && f.path === `${project.appId}.lua`)?.path;

  const loadFiles = useCallback(async () => {
    const list = await bridge.files(id);
    setFiles(list);
    return list;
  }, [id]);

  const open = useCallback(
    async (path: string) => {
      const text = await bridge.read(id, path);
      setActive(path);
      setContent(text);
      setSaved(text);
    },
    [id],
  );

  useEffect(() => {
    if (!native) return;
    setActive("");
    setInfo(null);
    void loadFiles()
      .then((list) => {
        const first = list.find((f) => f.path.endsWith(".lua")) ?? list[0];
        if (first) return open(first.path);
      })
      .catch((e) => setStatus({ text: errorMessage(e), error: true }));
  }, [id, loadFiles, open]);

  // Autosave the active file shortly after typing stops.
  useEffect(() => {
    if (!active || content === saved) return;
    const timer = setTimeout(() => {
      const path = active;
      bridge
        .write(id, path, content)
        .then(() => {
          if (activeRef.current === path) setSaved(content);
          setInfo(null);
          void loadFiles();
          onChanged();
        })
        .catch((e) => setStatus({ text: errorMessage(e), error: true }));
    }, 500);
    return () => clearTimeout(timer);
  }, [active, content, saved, id, loadFiles, onChanged]);

  const onFilesChanged = useCallback(
    (path?: string) => {
      setInfo(null);
      void loadFiles().then((list) => {
        const current = activeRef.current;
        if (!list.some((f) => f.path === current)) {
          const next = path ?? list[0]?.path;
          if (next) void open(next);
        } else if (!dirtyRef.current && (!path || path === current)) void open(current);
        else if (path && !current) void open(path);
      });
      onChanged();
    },
    [loadFiles, open, onChanged],
  );

  async function task(label: string, action: () => Promise<void>) {
    if (lock.current) return;
    lock.current = true;
    setBusy(label);
    setStatus(null);
    try {
      if (dirtyRef.current) {
        await bridge.write(id, activeRef.current, content);
        setSaved(content);
      }
      await action();
    } catch (e) {
      setStatus({ text: errorMessage(e), error: true });
    } finally {
      lock.current = false;
      setBusy("");
    }
  }

  const check = () =>
    task("check", async () => {
      const next = await bridge.check(id);
      setInfo(next);
      setStatus({ text: `检查通过：${next.files} 个文件，压缩后 ${formatBytes(next.bytes)}` });
    });

  const push = () =>
    task("push", async () => {
      if (!device) throw new Error("请先选择目标设备");
      const next = await bridge.check(id);
      setInfo(next);
      await bridge.install(id, device.id);
      await bridge.run(device.id, next.appId);
      setStatus({ text: `已推送 ${next.appId} ${next.version} 到「${device.name}」并开始运行` });
    });

  function commitPath() {
    const path = pathInput.trim();
    const problem = validatePath(path);
    if (problem) return setStatus({ text: problem, error: true });
    void task("file", async () => {
      if (dialog === "rename") {
        await bridge.renameFile(id, active, path);
      } else {
        await bridge.write(id, path, "");
      }
      setDialog(null);
      await loadFiles();
      await open(path);
      onChanged();
    });
  }

  return (
    <div className="workspace">
      <PageHeader
        crumbs={[{ label: "项目", onClick: onBack }, { label: project.name }]}
        actions={
          <>
            <select
              className="device-select"
              value={deviceId}
              onChange={(e) => onDevice(e.target.value)}
              aria-label="目标设备"
            >
              {!devices.length && <option value="">没有配对设备</option>}
              {devices.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.name}
                </option>
              ))}
            </select>
            <button className="button" disabled={!!busy || !native} onClick={check}>
              {busy === "check" ? <LoaderCircle className="spin" size={14} /> : <Check size={14} />}
              检查
            </button>
            <button
              className="button"
              title="导出 .lua-app.tar.zlib"
              aria-label="导出程序包"
              disabled={!!busy || !native}
              onClick={() =>
                task("export", async () => {
                  const path = await bridge.exportPackage(id);
                  if (path) setStatus({ text: `已导出：${path}` });
                })
              }
            >
              <Download size={14} />
            </button>
            <button className="button primary" disabled={!!busy || !native || !device} onClick={push}>
              {busy === "push" ? <LoaderCircle className="spin" size={14} /> : <Upload size={14} />}
              推送并运行
            </button>
          </>
        }
      />
      <div className="workspace-location">
        <FolderOpen size={13} />
        <span title={project.directory}>{project.directory}</span>
        <button
          className="text-button"
          disabled={!!busy}
          onClick={() => task("reveal", () => bridge.revealProject(id))}
        >
          打开目录
        </button>
        <button
          className="icon-button"
          aria-label="刷新项目文件"
          title="刷新外部修改"
          disabled={!!busy}
          onClick={() =>
            task("reload", async () => {
              const list = await loadFiles();
              const next = list.find((f) => f.path === activeRef.current) ?? list[0];
              if (next) await open(next.path);
              onChanged();
            })
          }
        >
          <RefreshCw size={13} />
        </button>
      </div>
      <div className="workspace-body">
        <nav className="files" aria-label="项目文件">
          <div className="files-head">
            <span>文件</span>
            <button
              className="icon-button"
              aria-label="新建文件"
              title="新建文件"
              onClick={() => {
                setPathInput(project.appId ? `${project.appId}/` : "");
                setDialog("new");
              }}
            >
              <FilePlus2 size={14} />
            </button>
          </div>
          {files.map((f) => {
            const Icon = fileIcon(f.path);
            return (
              <button
                key={f.path}
                className={f.path === active ? "file active" : "file"}
                onClick={() => task("open", () => open(f.path))}
                title={f.path}
              >
                <Icon size={14} />
                <span>{f.path}</span>
                {f.path === entry && <Play size={10} className="entry-mark" aria-label="入口" />}
              </button>
            );
          })}
          <div className="files-foot">
            <button className="button ghost small danger-text" onClick={() => setDialog("delete-project")}>
              <Trash2 size={12} />
              {project.external ? "移出项目列表" : "删除项目"}
            </button>
          </div>
        </nav>
        <section className="editor-pane">
          <div className="editor-bar">
            <span className="editor-path">{active || "—"}</span>
            {active && active !== "app.json" && (
              <span className="editor-actions">
                <button
                  className="icon-button"
                  aria-label="重命名"
                  title="重命名"
                  onClick={() => {
                    setPathInput(active);
                    setDialog("rename");
                  }}
                >
                  <Pencil size={13} />
                </button>
                <button
                  className="icon-button"
                  aria-label="删除文件"
                  title="删除文件"
                  onClick={() => setDialog("delete-file")}
                >
                  <Trash2 size={13} />
                </button>
              </span>
            )}
          </div>
          <div className="editor-body">
            {active ? (
              <Suspense fallback={<div className="editor-loading">正在加载编辑器…</div>}>
                <Editor key={active} path={active} content={content} onChange={setContent} />
              </Suspense>
            ) : (
              <div className="editor-loading">{native ? "选择一个文件" : "浏览器预览不能读取本地项目"}</div>
            )}
          </div>
          <div className={status?.error ? "editor-status error-text" : "editor-status"}>
            <span>
              {status?.text ??
                (content !== saved
                  ? "正在保存…"
                  : info
                    ? `压缩后 ${formatBytes(info.bytes)} · 直接安装上限 512 KiB`
                    : "已保存在本机")}
            </span>
          </div>
        </section>
        <AgentPanel
          host={{ projectId: id, onFilesChanged, device: () => device, confirm }}
          providers={providers}
          onOpenProviders={onOpenProviders}
        />
      </div>
      {(dialog === "new" || dialog === "rename") && (
        <Dialog title={dialog === "new" ? "新建文件" : "重命名文件"} onClose={() => setDialog(null)} busy={!!busy}>
          <form
            className="form"
            onSubmit={(e) => {
              e.preventDefault();
              commitPath();
            }}
          >
            <label>
              文件路径
              <input autoFocus value={pathInput} onChange={(e) => setPathInput(e.target.value)} spellCheck={false} />
              <span className="hint">资源文件需放在 {project.appId ?? "<程序 ID>"}/ 目录下。</span>
            </label>
            <div className="dialog-actions">
              <button type="button" className="button" onClick={() => setDialog(null)}>
                取消
              </button>
              <button className="button primary" disabled={!!busy || !pathInput.trim()}>
                {dialog === "new" ? "创建" : "重命名"}
              </button>
            </div>
          </form>
        </Dialog>
      )}
      {(dialog === "delete-file" || dialog === "delete-project") && (
        <Dialog
          title={dialog === "delete-file" ? "删除文件" : project.external ? "移出项目列表" : "删除项目"}
          onClose={() => setDialog(null)}
          busy={!!busy}
        >
          <p className="dialog-text">
            {dialog === "delete-file"
              ? `从项目中删除 ${active}？`
              : project.external
                ? `从 GizMoth 移出「${project.name}」并清除对话记录？${project.directory} 中的文件会保留。`
                : `删除「${project.name}」的全部文件和对话记录？此操作不能撤销，设备上已安装的程序不受影响。`}
          </p>
          <div className="dialog-actions">
            <button className="button" onClick={() => setDialog(null)}>
              取消
            </button>
            <button
              className="button danger"
              disabled={!!busy}
              onClick={() =>
                dialog === "delete-file"
                  ? void task("delete", async () => {
                      await bridge.removeFile(id, active);
                      setDialog(null);
                      const list = await loadFiles();
                      if (list[0]) await open(list[0].path);
                      onChanged();
                    })
                  : void task("delete", async () => {
                      await bridge.deleteProject(id);
                      setDialog(null);
                      onDeleted();
                    })
              }
            >
              {dialog === "delete-project" && project.external ? "移出列表" : "删除"}
            </button>
          </div>
        </Dialog>
      )}
    </div>
  );
}
