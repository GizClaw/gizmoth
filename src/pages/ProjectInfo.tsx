import { t, useI18n } from "../i18n";
import { useEffect, useRef, useState, type RefObject } from "react";
import { Check, CircleNotch, BracketsCurly, LockSimple, PencilSimple, Play, Sparkle, X } from "@phosphor-icons/react";
import { MAX_BRIEF_LENGTH } from "../agent";
import type {
  AgentPermissions,
  Device,
  FileAccess,
  PackageInfo,
  ProjectSummary,
  RunAccess,
  WebAccess,
} from "../bridge";
import { Button, IconButton, Select } from "../ui";
import "./ProjectInfo.css";

const formatBytes = (n: number) => (n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KiB`);
const INSTALL_LIMIT = 512 * 1024;

function permissionSummary({ files }: AgentPermissions) {
  const outside =
    files === "none"
      ? t("只能访问这个项目。")
      : files === "read"
        ? t("项目外指你的用户目录。")
        : t("项目外指你的用户目录；隐藏配置文件和资源库（Library）仍然只读。");
  return t("{scope}凭据目录和 GizMoth 自己的数据始终不可访问。", { scope: outside });
}

/**
 * Read-only facts about the open project, kept out of the composer. Opened from
 * the chat toolbar; a column beside the chat when there is room, else an overlay.
 */
export function ProjectInfo({
  project,
  kindLabel,
  files,
  info,
  device,
  busy,
  canRun,
  codeVisible,
  toggle,
  brief,
  onBriefChange,
  permissions,
  onPermissionsChange,
  onCheck,
  onRun,
  onShowCode,
  onClose,
}: {
  project: ProjectSummary;
  kindLabel: string;
  files: number;
  info: PackageInfo | null;
  device?: Device;
  busy: string;
  canRun: boolean;
  codeVisible: boolean;
  toggle: RefObject<HTMLElement | null>;
  brief: string;
  onBriefChange: (brief: string) => void;
  permissions: AgentPermissions;
  onPermissionsChange: (permissions: AgentPermissions) => void;
  onCheck: () => void;
  onRun: () => void;
  onShowCode: () => void;
  onClose: (restoreFocus: boolean) => void;
}) {
  useI18n();
  const panel = useRef<HTMLElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  // Mounted only when the user opens it, so taking focus here is expected.
  useEffect(() => panel.current?.focus(), []);
  // As a floating overlay (narrow layouts) it behaves like a popover: outside clicks dismiss it.
  useEffect(() => {
    const dismiss = (event: PointerEvent) => {
      const element = panel.current;
      const target = event.target as Node;
      if (!element || element.contains(target) || toggle.current?.contains(target)) return;
      if (getComputedStyle(element).position === "absolute") closeRef.current(false);
    };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [toggle]);
  const appId = info?.appId ?? project.appId;
  const [draft, setDraft] = useState<string | null>(null);
  const editing = draft !== null;
  function saveBrief() {
    if (draft === null) return;
    onBriefChange(draft.trim().slice(0, MAX_BRIEF_LENGTH));
    setDraft(null);
  }
  return (
    <aside
      ref={panel}
      id="workspace-info"
      className="workspace-info"
      aria-label={t("项目信息")}
      tabIndex={-1}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onClose(true);
        }
      }}
    >
      <div className="workspace-info-head">
        <h2>{t("项目信息")}</h2>
        <IconButton controlSize="sm" aria-label={t("关闭项目信息")} title={t("关闭")} onClick={() => onClose(true)}>
          <X size={14} />
        </IconButton>
      </div>
      <div className="workspace-info-body">
        <section aria-labelledby="workspace-info-brief" className="workspace-info-brief">
          <div className="workspace-info-heading">
            <h3 id="workspace-info-brief">{t("项目说明")}</h3>
            {!editing && (
              <Button controlSize="sm" variant="ghost" onClick={() => setDraft(brief)}>
                <PencilSimple size={14} aria-hidden="true" />
                {brief ? t("编辑") : t("填写")}
              </Button>
            )}
          </div>
          {editing ? (
            <form
              className="workspace-info-brief-form"
              onSubmit={(event) => {
                event.preventDefault();
                saveBrief();
              }}
            >
              <textarea
                autoFocus
                aria-labelledby="workspace-info-brief"
                value={draft}
                maxLength={MAX_BRIEF_LENGTH}
                rows={7}
                placeholder={t("这个程序做什么、怎么表现、有哪些约定…")}
                onChange={(event) => setDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    event.stopPropagation();
                    setDraft(null);
                  } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                    event.preventDefault();
                    saveBrief();
                  }
                }}
              />
              <div className="workspace-info-brief-actions">
                <span>
                  {draft.length} / {MAX_BRIEF_LENGTH}
                </span>
                <Button controlSize="sm" onClick={() => setDraft(null)}>
                  {t("取消")}
                </Button>
                <Button controlSize="sm" variant="primary" type="submit">
                  {t("保存")}
                </Button>
              </div>
            </form>
          ) : brief ? (
            <p className="workspace-info-brief-text">{brief}</p>
          ) : (
            <p className="workspace-info-note">
              <Sparkle size={14} aria-hidden="true" />
              <span>{t("助手会在对话中自动总结这个项目的用途和约定。清空对话后它仍会保留，助手按它继续。")}</span>
            </p>
          )}
        </section>
        <section aria-labelledby="workspace-info-project">
          <h3 id="workspace-info-project">{t("项目")}</h3>
          <dl>
            <dt>{t("类型")}</dt>
            <dd>{kindLabel}</dd>
            <dt>{t("程序 ID")}</dt>
            <dd className="mono">{appId || t("未设置")}</dd>
          </dl>
        </section>
        <section aria-labelledby="workspace-info-files">
          <h3 id="workspace-info-files">{t("文件")}</h3>
          <div className="workspace-info-line">
            <span>{t("{count} 个文件", { count: files })}</span>
            {!codeVisible && (
              <Button controlSize="sm" variant="ghost" onClick={onShowCode}>
                <BracketsCurly size={14} aria-hidden="true" />
                {t("显示代码")}
              </Button>
            )}
          </div>
        </section>
        <section aria-labelledby="workspace-info-package">
          <h3 id="workspace-info-package">{t("程序包")}</h3>
          <div className="workspace-info-line">
            <span className={info && info.bytes > INSTALL_LIMIT ? "error-text" : undefined}>
              {info ? t("压缩后 {size}，上限 512 KiB", { size: formatBytes(info.bytes) }) : t("尚未检查")}
            </span>
            <Button controlSize="sm" variant="ghost" disabled={!!busy || !canRun} onClick={onCheck}>
              {busy === "check" ? (
                <CircleNotch size={14} className="spin" aria-hidden="true" />
              ) : (
                <Check size={14} aria-hidden="true" />
              )}
              {info ? t("重新检查") : t("检查")}
            </Button>
          </div>
        </section>
        <section aria-labelledby="workspace-info-target">
          <h3 id="workspace-info-target">{t("运行目标")}</h3>
          <div className="workspace-info-line">
            <span>{device ? device.name : t("运行时选择设备或模拟器")}</span>
            <Button controlSize="sm" variant="ghost" disabled={!!busy || !canRun} onClick={onRun}>
              <Play size={14} aria-hidden="true" />
              {t("运行…")}
            </Button>
          </div>
        </section>
        <section aria-labelledby="workspace-info-scope">
          <h3 id="workspace-info-scope">{t("助手权限")}</h3>
          <div className="workspace-info-permissions">
            <label>
              <span>{t("项目外文件")}</span>
              <Select
                controlSize="sm"
                aria-label={t("项目外文件")}
                value={permissions.files}
                onValueChange={(value) => onPermissionsChange({ ...permissions, files: value as FileAccess })}
                options={[
                  { value: "none", label: t("不可访问") },
                  { value: "read", label: t("只读") },
                  { value: "write", label: t("读写") },
                ]}
              />
            </label>
            <label>
              <span>{t("推送到设备")}</span>
              <Select
                controlSize="sm"
                aria-label={t("推送到设备")}
                value={permissions.run}
                onValueChange={(value) => onPermissionsChange({ ...permissions, run: value as RunAccess })}
                options={[
                  { value: "ask", label: t("每次询问") },
                  { value: "allow", label: t("直接运行") },
                  { value: "deny", label: t("禁止") },
                ]}
              />
            </label>
            <label>
              <span>{t("联网")}</span>
              <Select
                controlSize="sm"
                aria-label={t("联网")}
                value={permissions.web}
                onValueChange={(value) => onPermissionsChange({ ...permissions, web: value as WebAccess })}
                options={[
                  { value: "allow", label: t("允许") },
                  { value: "deny", label: t("禁止") },
                ]}
              />
            </label>
          </div>
          <p className="workspace-info-note">
            <LockSimple size={14} aria-hidden="true" />
            <span>{permissionSummary(permissions)}</span>
          </p>
        </section>
      </div>
    </aside>
  );
}
