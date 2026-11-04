import { useState } from "react";
import { Code2, FolderOpen, FolderPlus, LoaderCircle, Plus } from "lucide-react";
import { bridge, errorMessage, native, type ProjectSummary } from "../bridge";
import { validAppId } from "../project";
import { Dialog, Empty, PageHeader, relativeTime } from "../ui";

export function ProjectsPage({
  projects,
  onOpen,
  onCreated,
  onChanged,
  confirm,
}: {
  projects: ProjectSummary[];
  onOpen: (id: string) => void;
  onCreated: (p: ProjectSummary) => void;
  onChanged: () => Promise<void>;
  confirm: (title: string, body: string) => Promise<boolean>;
}) {
  const [creating, setCreating] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function task(action: () => Promise<void>) {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }
  const openFolder = () =>
    task(async () => {
      const project = await bridge.openProject();
      if (project) onCreated(project);
    });
  return (
    <>
      <PageHeader
        crumbs={[{ label: "工作" }, { label: "项目" }]}
        actions={
          <>
            <button className="button" disabled={busy || !native} onClick={openFolder}>
              <FolderOpen size={14} />
              打开项目
            </button>
            <button className="button primary" onClick={() => setCreating(true)}>
              <Plus size={14} />
              新建项目
            </button>
          </>
        }
      />
      <div className="page">
        <p className="page-intro">项目就是本地文件夹。选择自己的保存位置，也可以打开已有的 Lua 项目。</p>
        {error && (
          <p className="error banner" role="alert">
            {error}
          </p>
        )}
        {projects.length ? (
          <section className="card list project-list">
            <div className="list-head">
              <span>Lua 项目</span>
              <span className="muted">{projects.length} 个项目</span>
            </div>
            {projects.map((p) => (
              <div className="project-row" key={p.id}>
                <button className="project-open" disabled={!p.available || busy} onClick={() => onOpen(p.id)}>
                  <span className="row-icon">
                    <Code2 size={16} />
                  </span>
                  <span className="row-main">
                    <span className="row-title">
                      {p.name}
                      {!p.available && <span className="tag">目录不可用</span>}
                    </span>
                    <span className="row-meta">
                      {p.appId ?? "app.json 待修正"}
                      {p.version ? ` · ${p.version}` : ""} · {p.files} 个文件
                    </span>
                    <span className="project-path" title={p.directory}>
                      {p.directory}
                    </span>
                  </span>
                </button>
                <span className="project-updated muted">{relativeTime(p.updatedAt)}</span>
                {p.available ? (
                  <button
                    className="icon-button"
                    title="打开项目目录"
                    aria-label={`打开 ${p.name} 的目录`}
                    disabled={busy}
                    onClick={() => task(() => bridge.revealProject(p.id))}
                  >
                    <FolderOpen size={15} />
                  </button>
                ) : (
                  <div className="header-actions">
                    <button
                      className="button small"
                      disabled={busy}
                      onClick={() =>
                        task(async () => {
                          const next = await bridge.relocateProject(p.id);
                          if (next) await onChanged();
                        })
                      }
                    >
                      重新定位
                    </button>
                    {p.external && (
                      <button
                        className="button ghost small"
                        disabled={busy}
                        onClick={() =>
                          task(async () => {
                            if (
                              await confirm("移出项目列表", `移出「${p.name}」并清除对话记录？目录中的源码文件会保留。`)
                            ) {
                              await bridge.deleteProject(p.id);
                              await onChanged();
                            }
                          })
                        }
                      >
                        移出列表
                      </button>
                    )}
                  </div>
                )}
              </div>
            ))}
          </section>
        ) : (
          <Empty
            icon={<FolderPlus size={22} />}
            title="从一个文件夹开始"
            body="新建 Lua 项目，或打开包含 app.json 的已有目录。"
            action={
              <div className="header-actions">
                <button className="button" disabled={busy || !native} onClick={openFolder}>
                  <FolderOpen size={14} />
                  打开项目
                </button>
                <button className="button primary" onClick={() => setCreating(true)}>
                  <Plus size={14} />
                  新建项目
                </button>
              </div>
            }
          />
        )}
      </div>
      {creating && (
        <CreateDialog
          onClose={() => setCreating(false)}
          onCreated={(p) => {
            setCreating(false);
            onCreated(p);
          }}
        />
      )}
    </>
  );
}

function CreateDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (p: ProjectSummary) => void }) {
  const [name, setName] = useState("");
  const [appId, setAppId] = useState("");
  const [touched, setTouched] = useState(false);
  const [parentDir, setParentDir] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const derived = touched
    ? appId
    : name
        .toLowerCase()
        .replace(/[^a-z0-9_.-]+/g, "_")
        .replace(/^[._]+/, "")
        .slice(0, 32);
  return (
    <Dialog title="新建项目" onClose={onClose} busy={busy}>
      <form
        className="form"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError("");
          try {
            onCreated(await bridge.createProject(derived, name.trim(), parentDir || undefined));
          } catch (err) {
            setError(errorMessage(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <label>
          项目名称
          <input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="例如 呼吸灯"
            required
            maxLength={128}
            disabled={busy}
          />
        </label>
        <label>
          程序 ID
          <input
            value={derived}
            onChange={(e) => {
              setTouched(true);
              setAppId(e.target.value);
            }}
            placeholder="breathing_light"
            spellCheck={false}
            disabled={busy}
          />
          <span className="hint">
            小写字母、数字、下划线、点或连字符，最多 32 位。入口文件为 {derived || "<程序 ID>"}.lua。
          </span>
        </label>
        <label>
          保存位置
          <div className="directory-picker">
            <span title={parentDir || "Documents/GizMoth"}>{parentDir || "文稿 / GizMoth"}</span>
            <button
              type="button"
              className="button small"
              disabled={busy || !native}
              onClick={async () => {
                setBusy(true);
                setError("");
                try {
                  const dir = await bridge.chooseProjectDirectory();
                  if (dir) setParentDir(dir);
                } catch (e) {
                  setError(errorMessage(e));
                } finally {
                  setBusy(false);
                }
              }}
            >
              <FolderOpen size={13} />
              选择目录
            </button>
          </div>
          <span className="hint">在这里新建 {derived || "<程序 ID>"}/ 文件夹，已有文件不会被覆盖。</span>
        </label>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        <div className="dialog-actions">
          <button type="button" className="button" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button className="button primary" disabled={busy || !native || !name.trim() || !validAppId(derived)}>
            {busy && <LoaderCircle className="spin" size={13} />}创建
          </button>
        </div>
      </form>
    </Dialog>
  );
}
