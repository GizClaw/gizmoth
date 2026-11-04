import { useState } from "react";
import { Code2, FolderPlus, Plus } from "lucide-react";
import { bridge, errorMessage, native, type ProjectSummary } from "../bridge";
import { validAppId } from "../project";
import { Dialog, Empty, PageHeader, Row, relativeTime } from "../ui";

export function ProjectsPage({
  projects,
  onOpen,
  onCreated,
}: {
  projects: ProjectSummary[];
  onOpen: (id: string) => void;
  onCreated: (p: ProjectSummary) => void;
}) {
  const [creating, setCreating] = useState(false);
  return (
    <>
      <PageHeader
        crumbs={[{ label: "工作" }, { label: "项目" }]}
        actions={
          <button className="button primary" onClick={() => setCreating(true)}>
            <Plus size={14} />
            新建项目
          </button>
        }
      />
      <div className="page">
        {projects.length ? (
          <section className="card list">
            <div className="list-head">
              <span>Lua 项目</span>
              <span className="muted">保存在本机</span>
            </div>
            {projects.map((p) => (
              <Row
                key={p.id}
                icon={<Code2 size={16} />}
                title={p.name}
                meta={`${p.appId ?? "app.json 待修正"}${p.version ? ` · ${p.version}` : ""} · ${p.files} 个文件`}
                trailing={<span className="muted">{relativeTime(p.updatedAt)}</span>}
                onClick={() => onOpen(p.id)}
              />
            ))}
          </section>
        ) : (
          <Empty
            icon={<FolderPlus size={22} />}
            title="创建第一个 Lua 项目"
            body="和编程助手一起写程序，检查后一键推送到设备上运行。"
            action={
              <button className="button primary" onClick={() => setCreating(true)}>
                <Plus size={14} />
                新建项目
              </button>
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
            onCreated(await bridge.createProject(derived, name.trim()));
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
          />
          <span className="hint">
            小写字母、数字、下划线、点或连字符，最多 32 位。入口文件为 {derived || "<程序 ID>"}.lua。
          </span>
        </label>
        {error && <p className="error">{error}</p>}
        <div className="dialog-actions">
          <button type="button" className="button" onClick={onClose}>
            取消
          </button>
          <button className="button primary" disabled={busy || !native || !name.trim() || !validAppId(derived)}>
            创建
          </button>
        </div>
      </form>
    </Dialog>
  );
}
