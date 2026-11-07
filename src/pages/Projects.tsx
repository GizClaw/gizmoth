import { t, useI18n } from "../i18n";
import { Button, Input, SearchInput } from "../ui";
import { useState } from "react";
import {
  Code as Code2,
  Cpu,
  FolderPlus,
  Plus,
  MagnifyingGlass as Search,
  Circuitry as CircuitBoard,
  CaretRight,
} from "@phosphor-icons/react";
import { bridge, errorMessage, native, type ProjectSummary } from "../bridge";
import { validAppId } from "../project";
import { projectKindOf, projectKindOrder, projectKinds, type ProjectKind } from "../project-kinds";
import { Dialog, Empty, PageHeader, relativeTime } from "../ui";

const kindIcons = { lua: Code2, giz: Cpu, rom: CircuitBoard };
export function ProjectsPage({
  projects,
  kind,
  onShowAll,
  onOpen,
  onCreated,
}: {
  projects: ProjectSummary[];
  kind?: ProjectKind;
  onShowAll: () => void;
  onOpen: (id: string) => void;
  onCreated: (p: ProjectSummary) => void;
}) {
  useI18n();
  const [creating, setCreating] = useState(false);
  const [query, setQuery] = useState("");
  const available = !kind || projectKinds[kind].available;
  const visible = projects.filter(
    (p) =>
      (!kind || projectKindOf(p) === kind) &&
      `${p.name} ${p.appId ?? ""}`.toLowerCase().includes(query.trim().toLowerCase()),
  );
  return (
    <>
      <PageHeader
        crumbs={
          kind
            ? [{ label: t("项目"), onClick: onShowAll }, { label: t(projectKinds[kind].label) }]
            : [{ label: t("工作") }, { label: t("项目") }]
        }
        actions={
          <Button
            variant="primary"
            disabled={!available || !native}
            title={kind && !available ? t("{kind}创建尚未接入", { kind: t(projectKinds[kind].label) }) : undefined}
            onClick={() => setCreating(true)}
          >
            <Plus size={14} />
            {t("新建项目")}
          </Button>
        }
      />
      <div className="page projects-page">
        <div className="page-description">
          {!kind && <p>{t("从一个想法，到设备上的新功能。")}</p>}
          <span>
            {kind === "lua" ? t(projectKinds.lua.description) : t("项目保存在本机，Lua 应用可由 pi-agent 协助开发。")}
          </span>
        </div>
        {available && (
          <div className="project-toolbar">
            <span className="project-list-summary">
              {query ? t("{count} 个搜索结果", { count: visible.length }) : t("{count} 个项目", { count: visible.length })}
            </span>
            <SearchInput
              className="project-search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onClear={() => setQuery("")}
              clearLabel={t("清除项目搜索")}
              placeholder={t("搜索项目")}
              aria-label={t("搜索项目")}
            />
          </div>
        )}
        {!available && kind ? (
          <Empty
            icon={kind === "giz" ? <Cpu size={24} /> : <CircuitBoard size={24} />}
            title={t("{kind}尚未接入", { kind: t(projectKinds[kind].label) })}
            body={t(projectKinds[kind].description)}
            action={<Button onClick={onShowAll}>{t("查看全部项目")}</Button>}
          />
        ) : visible.length ? (
          <ul className="card project-list" aria-label={t("本机项目")}>
            {visible.map((p) => {
              const projectKind = projectKindOf(p);
              const Icon = kindIcons[projectKind];
              const meta = `${p.appId ?? t("app.json 待修正")}${p.version ? ` · ${p.version}` : ""} · ${t("{count} 个文件", { count: p.files })}`;
              const updated = relativeTime(p.updatedAt);
              return (
                <li key={p.id}>
                  <button
                    className="project-row"
                    title={`${p.name} · ${t(projectKinds[projectKind].label)} · ${meta} · ${updated}`}
                    aria-label={`${p.name}，${t(projectKinds[projectKind].label)}，${meta}，${updated}`}
                    onClick={() => onOpen(p.id)}
                  >
                    <Icon size={18} aria-hidden="true" />
                    <strong className="project-row-name">{p.name}</strong>
                    <span className="project-row-kind">{t(projectKinds[projectKind].label)}</span>
                    <span className="project-row-meta">{meta}</span>
                    <span className="project-row-time">{updated}</span>
                    <CaretRight size={13} aria-hidden="true" />
                  </button>
                </li>
              );
            })}
          </ul>
        ) : query ? (
          <Empty
            icon={<Search size={22} />}
            title={t("没有匹配的项目")}
            body={t("换一个项目名称或程序 ID 试试。")}
            action={<Button onClick={() => setQuery("")}>{t("清除搜索")}</Button>}
          />
        ) : (
          <Empty
            icon={<FolderPlus size={24} />}
            title={t("创建第一个 Lua 应用")}
            body={t("和 pi-agent 一起编写程序，再安装到你的设备上。")}
            action={
              <Button variant="primary" onClick={() => setCreating(true)}>
                <Plus size={14} />
                {t("新建项目")}
              </Button>
            }
          />
        )}
      </div>
      {creating && (
        <CreateDialog
          initialKind={kind ?? "lua"}
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

function CreateDialog({
  initialKind,
  onClose,
  onCreated,
}: {
  initialKind: ProjectKind;
  onClose: () => void;
  onCreated: (p: ProjectSummary) => void;
}) {
  useI18n();
  const [kind, setKind] = useState<ProjectKind>(initialKind);
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
  const available = projectKinds[kind].available;
  return (
    <Dialog title={t("新建项目")} onClose={onClose} busy={busy}>
      <div className="project-kind-options" role="group" aria-label={t("选择项目类型")}>
        {projectKindOrder.map((value) => {
          const Icon = kindIcons[value];
          return (
            <button
              type="button"
              key={value}
              className={kind === value ? "kind-option selected" : "kind-option"}
              aria-pressed={kind === value}
              disabled={busy}
              onClick={() => setKind(value)}
            >
              <Icon size={18} />
              <span>{t(projectKinds[value].label)}</span>
            </button>
          );
        })}
      </div>
      <p className="kind-description">{t(projectKinds[kind].description)}</p>
      <form
        className="form"
        onSubmit={async (e) => {
          e.preventDefault();
          if (!available || busy || !name.trim() || !validAppId(derived)) return;
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
        {available && (
          <>
            <label>
              {t("项目名称")}
              <Input
                autoFocus
                value={name}
                disabled={busy}
                onChange={(e) => setName(e.target.value)}
                placeholder={t("例如 呼吸灯")}
                required
                maxLength={128}
              />
            </label>
            <label>
              {t("程序 ID")}
              <Input
                value={derived}
                disabled={busy}
                onChange={(e) => {
                  setTouched(true);
                  setAppId(e.target.value);
                }}
                placeholder="breathing_light"
                spellCheck={false}
              />
              <span className="hint">
                {t("最多 32 位小写字母、数字、下划线、点或连字符。入口为 {entry}。", { entry: `${derived || t("<程序 ID>")}.lua` })}
              </span>
            </label>
          </>
        )}
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        <div className="dialog-actions">
          <Button type="button" disabled={busy} onClick={onClose}>
            {t("取消")}
          </Button>
          <Button
            variant="primary"
            type="submit"
            disabled={!available || busy || !native || !name.trim() || !validAppId(derived)}
          >
            {available ? (busy ? t("正在创建…") : t("创建项目")) : t("尚未支持")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
