import { t, useI18n } from "../i18n";
import { lazy, Suspense, useEffect, useId, useMemo, useRef, useState } from "react";
import {
  ArrowsInSimple,
  ArrowsOutSimple,
  CaretDown,
  CaretRight,
  FileCode,
  FilePlus,
  Folder,
  FolderOpen,
  ListBullets,
  MagnifyingGlass,
  PencilSimple,
  TextAlignLeft,
  Trash,
  X,
} from "@phosphor-icons/react";
import { Button, IconButton, SearchInput, Select } from "../ui";
import { projectFileTree, type ProjectFile, type ProjectTreeNode } from "../project-files";
import { detectEditorLanguage, editorLanguages, languageName, type EditorLanguage } from "../editor-language";
import type { EditorPosition } from "../Editor";
import { WorkspaceActions } from "./WorkspaceActions";
import { FileBreadcrumbs } from "./FileBreadcrumbs";
import "./CodeWorkspace.css";
const Editor = lazy(() => import("../Editor").then(({ Editor }) => ({ default: Editor })));

export function CodeWorkspace({
  files,
  tabs,
  active,
  content,
  busy,
  formatting,
  expanded,
  onContent,
  onOpen,
  onCloseTab,
  onCloseAll,
  onNew,
  onRename,
  onDelete,
  onFormat,
  onExpand,
  onHide,
}: {
  files: ProjectFile[];
  tabs: string[];
  active: string;
  content: string;
  busy: boolean;
  formatting: boolean;
  expanded: boolean;
  onContent: (value: string) => void;
  onOpen: (path: string) => Promise<boolean>;
  onCloseTab: (path: string) => void;
  onCloseAll: () => void;
  onNew: () => void;
  onRename: () => void;
  onDelete: () => void;
  onFormat: (language: EditorLanguage) => void;
  onExpand: () => void;
  onHide: () => void;
}) {
  useI18n();
  const [treeOpen, setTreeOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const [languages, setLanguages] = useState<Record<string, EditorLanguage>>({});
  const detected = detectEditorLanguage(active);
  const selectedLanguage = Object.hasOwn(languages, active) ? languages[active] : undefined;
  const language = selectedLanguage ?? detected;
  const canFormat = !!active && language !== "text" && !busy;
  const [searchRequest, setSearchRequest] = useState(0);
  const positions = useRef(new Map<string, EditorPosition>());
  const tabbar = useRef<HTMLDivElement>(null);
  const tree = useRef<HTMLUListElement>(null);
  const prefix = useId();
  const visible = useMemo(
    () => files.filter((file) => file.path.toLocaleLowerCase().includes(filter.trim().toLocaleLowerCase())),
    [files, filter],
  );
  const nodes = useMemo(() => projectFileTree(visible), [visible]);
  useEffect(() => {
    tabbar.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [active]);
  const expandFolder = (path: string, collapse: boolean) =>
    setCollapsed((previous) => {
      const next = new Set(previous);
      if (collapse) next.add(path);
      else next.delete(path);
      return next;
    });
  const renderNodes = (items: ProjectTreeNode[], depth = 0) =>
    items.map((node) => {
      const folder = !!node.children,
        closed = !filter.trim() && collapsed.has(node.path);
      return (
        <li key={node.path} role="none">
          <button
            type="button"
            role="treeitem"
            aria-level={depth + 1}
            aria-expanded={folder ? !closed : undefined}
            aria-selected={folder ? undefined : node.path === active}
            data-path={node.path}
            className={`code-tree-item${node.path === active ? " is-active" : ""}`}
            title={node.path}
            style={{ paddingLeft: 10 + depth * 14 }}
            disabled={busy}
            onClick={() => (folder ? expandFolder(node.path, !closed) : void onOpen(node.path))}
            onKeyDown={(event) => {
              if (event.nativeEvent.isComposing) return;
              const buttons = [
                ...(tree.current?.querySelectorAll<HTMLButtonElement>('[role="treeitem"]') ?? []),
              ].filter((item) => item.getClientRects().length);
              const index = buttons.indexOf(event.currentTarget);
              if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
                event.preventDefault();
                const next =
                  event.key === "Home"
                    ? 0
                    : event.key === "End"
                      ? buttons.length - 1
                      : Math.max(0, Math.min(buttons.length - 1, index + (event.key === "ArrowDown" ? 1 : -1)));
                buttons[next]?.focus();
              } else if (event.key === "ArrowRight" && folder) {
                event.preventDefault();
                expandFolder(node.path, false);
              } else if (event.key === "ArrowLeft") {
                event.preventDefault();
                if (folder && !closed) expandFolder(node.path, true);
                else {
                  const parent = node.path.slice(0, node.path.lastIndexOf("/"));
                  buttons.find((item) => item.dataset.path === parent)?.focus();
                }
              }
            }}
          >
            {folder ? (
              closed ? (
                <CaretRight size={11} />
              ) : (
                <CaretDown size={11} />
              )
            ) : (
              <span className="code-tree-spacer" />
            )}
            {folder ? closed ? <Folder size={15} /> : <FolderOpen size={15} /> : <FileCode size={15} />}
            <span className="code-tree-name">{node.name}</span>
          </button>
          {folder && !closed && <ul role="group">{renderNodes(node.children!, depth + 1)}</ul>}
        </li>
      );
    });
  return (
    <section
      className={`code-workspace${treeOpen ? " tree-open" : ""}`}
      aria-label={t("代码编辑器")}
      onKeyDown={(event) => {
        if (!event.nativeEvent.isComposing && event.altKey && event.shiftKey && event.code === "KeyF") {
          event.preventDefault();
          if (canFormat) onFormat(language);
        }
      }}
    >
      <div className="code-tabbar">
        <div className="code-tabs" role="tablist" aria-label={t("已打开文件")} ref={tabbar}>
          {tabs.map((path, index) => (
            <div
              className={`code-tab${active === path ? " is-active" : ""}`}
              key={path}
              role="presentation"
              onAuxClick={(event) => {
                if (event.button === 1) {
                  event.preventDefault();
                  onCloseTab(path);
                }
              }}
            >
              <button
                type="button"
                role="tab"
                id={`${prefix}-tab-${index}`}
                aria-controls={`${prefix}-editor`}
                aria-selected={active === path}
                aria-label={path}
                tabIndex={active === path ? 0 : -1}
                title={path}
                disabled={busy}
                onClick={() => void onOpen(path)}
                onKeyDown={(event) => {
                  if (event.nativeEvent.isComposing) return;
                  if (event.key === "Delete") {
                    event.preventDefault();
                    onCloseTab(path);
                    return;
                  }
                  if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
                  event.preventDefault();
                  const next =
                    event.key === "Home"
                      ? 0
                      : event.key === "End"
                        ? tabs.length - 1
                        : (index + (event.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length;
                  void onOpen(tabs[next]).then((ok) => {
                    if (ok) tabbar.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus();
                  });
                }}
              >
                <FileCode size={13} />
                <span>{path.split("/").at(-1)}</span>
              </button>
              <button
                type="button"
                className="code-tab-close"
                aria-label={t("关闭文件 {path}", { path })}
                title={t("关闭文件")}
                disabled={busy}
                onClick={() => onCloseTab(path)}
              >
                <X size={12} />
              </button>
            </div>
          ))}
          {!tabs.length && <span className="code-no-tabs">{t("代码")}</span>}
        </div>
        <div className="code-toolbar" aria-label={t("代码工具栏")}>
          <IconButton
            aria-label={t("搜索文件内容")}
            title={t("搜索文件内容")}
            disabled={!active || busy}
            onClick={() => setSearchRequest((value) => value + 1)}
          >
            <MagnifyingGlass size={16} />
          </IconButton>
          <IconButton
            aria-label={t("格式化文件")}
            title={language === "text" ? t("选择代码语言后可格式化") : t("格式化文件（Shift+Alt+F）")}
            aria-busy={formatting}
            disabled={!canFormat}
            onClick={() => onFormat(language)}
          >
            <TextAlignLeft size={16} />
          </IconButton>
          <IconButton
            className={treeOpen ? "is-selected" : undefined}
            aria-label={treeOpen ? t("隐藏文件目录") : t("显示文件目录")}
            aria-pressed={treeOpen}
            aria-controls={`${prefix}-tree`}
            title={t("文件目录")}
            onClick={() => setTreeOpen((value) => !value)}
          >
            <ListBullets size={16} />
          </IconButton>
          <WorkspaceActions
            label={t("文件操作")}
            disabled={busy}
            actions={[
              { label: t("新建文件…"), icon: <FilePlus size={14} />, onSelect: onNew },
              {
                label: t("重命名文件…"),
                icon: <PencilSimple size={14} />,
                disabled: !active || active === "app.json",
                onSelect: onRename,
              },
              {
                label: t("删除文件…"),
                icon: <Trash size={14} />,
                disabled: !active || active === "app.json",
                danger: true,
                onSelect: onDelete,
              },
              { label: t("关闭所有文件"), icon: <X size={14} />, disabled: !tabs.length, onSelect: onCloseAll },
            ]}
          />
          <IconButton
            className="code-expand"
            aria-label={expanded ? t("恢复聊天分栏") : t("展开代码区域")}
            title={expanded ? t("恢复聊天分栏") : t("展开代码区域")}
            onClick={onExpand}
          >
            {expanded ? <ArrowsInSimple size={16} /> : <ArrowsOutSimple size={16} />}
          </IconButton>
          <IconButton aria-label={t("关闭代码面板")} title={t("关闭代码面板")} onClick={onHide}>
            <X size={16} />
          </IconButton>
        </div>
      </div>
      <div className="code-workbench">
        <div className="code-document" id={`${prefix}-editor`} role="tabpanel" aria-label={active || t("文件编辑区")}>
          <div className="code-pathbar">
            <FileBreadcrumbs files={files} path={active} disabled={busy} onOpen={onOpen} />
            <Select
              controlSize="sm"
              className="code-language-select"
              aria-label={t("编辑语言")}
              title={t("选择代码语言")}
              disabled={!active || busy}
              value={selectedLanguage ?? "auto"}
              onValueChange={(value) =>
                setLanguages((previous) => {
                  const next = { ...previous };
                  if (value === "auto") delete next[active];
                  else next[active] = value as EditorLanguage;
                  return next;
                })
              }
              options={[
                { value: "auto", label: t("自动 · {language}", { language: t(languageName(detected)) }) },
                ...editorLanguages.map((item) => ({ value: item.id, label: t(item.name) })),
              ]}
            />
          </div>
          <div className="editor-body">
            {active ? (
              <Suspense fallback={<div className="editor-loading">{t("正在加载编辑器…")}</div>}>
                <Editor
                  key={active}
                  path={active}
                  content={content}
                  language={language}
                  disabled={busy}
                  onChange={onContent}
                  searchRequest={searchRequest}
                  positions={positions.current}
                />
              </Suspense>
            ) : (
              <div className="code-empty">
                <FileCode size={24} />
                <p>{t("从文件目录打开文件，或创建一个新文件。")}</p>
                <div>
                  <Button controlSize="sm" onClick={() => setTreeOpen(true)}>
                    {t("打开文件目录")}
                  </Button>
                  <Button controlSize="sm" disabled={busy} onClick={onNew}>
                    <FilePlus size={14} />
                    {t("新建文件")}
                  </Button>
                </div>
              </div>
            )}
          </div>
        </div>
        {treeOpen && (
          <>
            <button
              type="button"
              className="code-tree-backdrop"
              aria-label={t("关闭文件目录")}
              onClick={() => setTreeOpen(false)}
            />
            <aside className="code-tree" aria-label={t("文件目录")} id={`${prefix}-tree`}>
              <div className="code-tree-search">
                <SearchInput
                  controlSize="sm"
                  aria-label={t("筛选文件")}
                  placeholder={t("筛选文件…")}
                  value={filter}
                  onChange={(event) => setFilter(event.target.value)}
                  onClear={() => setFilter("")}
                />
              </div>
              <div className="code-tree-heading">
                <span>{t("项目文件")}</span>
                <div>
                  <IconButton aria-label={t("新建文件")} title={t("新建文件")} disabled={busy} onClick={onNew}>
                    <FilePlus size={13} />
                  </IconButton>
                </div>
              </div>
              <ul role="tree" aria-label={t("项目目录")} ref={tree}>
                {renderNodes(nodes)}
              </ul>
              {!visible.length && <p className="code-tree-empty">{filter ? t("没有匹配的文件") : t("还没有文件")}</p>}
            </aside>
          </>
        )}
      </div>
    </section>
  );
}
