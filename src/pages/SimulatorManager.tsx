import { t, useI18n } from "../i18n";
import { useEffect, useId, useRef, useState } from "react";
import { Monitor, Plus, PencilSimple, Trash, ArrowSquareOut } from "@phosphor-icons/react";
import { Button, IconButton, Input, Select, Dialog, PageHeader, Empty } from "../ui";
import { errorMessage } from "../bridge";
import {
  simulatorModels,
  simulatorProfiles,
  watchSimulatorProfiles,
  showSimulatorManager,
  type SimulatorProfile,
} from "../simulator-profiles";
import { openSimulator, closeSimulator } from "../simulator-window";
import "./SimulatorManager.css";

export function SimulatorProfilePicker({
  value,
  onChange,
  disabled = false,
  selectable = true,
  defaultModel = "h106-tiga",
  onBusyChange,
  showManage = true,
  onManage = showSimulatorManager,
  id,
}: {
  value: string;
  onChange: (id: string) => void;
  disabled?: boolean;
  selectable?: boolean;
  defaultModel?: SimulatorProfile["model"];
  onBusyChange?: (busy: boolean) => void;
  showManage?: boolean;
  onManage?: () => void;
  id?: string;
}) {
  useI18n();
  const fieldId = useId();
  const saveLock = useRef(false);
  const [profiles, setProfiles] = useState<SimulatorProfile[] | null>(null);
  const [readError, setReadError] = useState("");
  const [retry, setRetry] = useState(0);
  const [creating, setCreating] = useState(false);
  const [saving, setSaving] = useState(false);
  const [name, setName] = useState<string | null>(null);
  const resolvedName = name ?? t("我的模拟器");
  const [model, setModel] = useState(defaultModel);
  const [saveError, setSaveError] = useState("");
  const [createdName, setCreatedName] = useState("");
  useEffect(() => {
    let active = true;
    let sequence = 0;
    const refresh = () => {
      const request = ++sequence;
      void simulatorProfiles
        .list()
        .then((all) => {
          if (!active || request !== sequence) return;
          setProfiles(all);
          setReadError("");
          if (!all.some((p) => p.id === value)) onChange(all.length === 1 ? all[0].id : "");
        })
        .catch((e) => {
          if (active && request === sequence) setReadError(errorMessage(e));
        });
    };
    refresh();
    const unwatch = watchSimulatorProfiles(refresh);
    return () => {
      active = false;
      unwatch();
    };
  }, [value, onChange, retry]);
  async function create() {
    if (disabled || saveLock.current || !resolvedName.trim()) return;
    saveLock.current = true;
    setSaving(true);
    onBusyChange?.(true);
    setSaveError("");
    try {
      const profile = await simulatorProfiles.create(resolvedName.trim(), model);
      setProfiles((all) => [...(all ?? []).filter((item) => item.id !== profile.id), profile]);
      onChange(profile.id);
      setCreatedName(profile.name);
      setCreating(false);
    } catch (e) {
      setSaveError(errorMessage(e));
    } finally {
      saveLock.current = false;
      setSaving(false);
      onBusyChange?.(false);
    }
  }
  if (creating)
    return (
      <form
        className="simulator-picker-create"
        aria-label={t("创建模拟器")}
        onSubmit={(event) => {
          event.preventDefault();
          void create();
        }}
      >
        <label htmlFor={`${fieldId}-name`}>{t("名称")}</label>
        <Input
          id={`${fieldId}-name`}
          aria-label={t("模拟器名称")}
          maxLength={80}
          value={resolvedName}
          disabled={disabled || saving}
          onChange={(event) => setName(event.target.value)}
          autoFocus
        />
        <label htmlFor={`${fieldId}-model`}>{t("型号")}</label>
        <Select
          id={`${fieldId}-model`}
          aria-label={t("模拟器型号")}
          value={model}
          disabled={disabled || saving}
          onValueChange={(value) => setModel(value as SimulatorProfile["model"])}
          options={simulatorModels.map((item) => ({ value: item.id, label: item.name }))}
        />
        {saveError && (
          <p className="simulator-picker-error" role="alert">
            {t(saveError)}
          </p>
        )}
        <div className="simulator-picker-create-actions">
          <Button
            controlSize="sm"
            disabled={disabled || saving}
            onClick={() => {
              setCreating(false);
              setSaveError("");
            }}
          >
            {t("取消")}
          </Button>
          <Button
            controlSize="sm"
            type="submit"
            variant="primary"
            aria-label={t("确认创建模拟器")}
            disabled={disabled || saving || !resolvedName.trim()}
          >
            {saving ? t("创建中…") : t("创建")}
          </Button>
        </div>
      </form>
    );
  if (readError)
    return (
      <div className="simulator-picker-error" role="alert">
        <p>{t("无法读取模拟器：{error}", { error: readError })}</p>
        <Button controlSize="sm" disabled={disabled} onClick={() => setRetry((current) => current + 1)}>
          {t("重新读取模拟器")}
        </Button>
      </div>
    );
  if (profiles === null)
    return (
      <p className="simulator-picker-note" role="status">
        {t("正在读取模拟器…")}
      </p>
    );
  if (profiles.length && !selectable)
    return createdName ? (
      <p className="simulator-picker-note" role="status">
        {t("已创建「{name}」，可在左侧「模拟器」中打开。", { name: createdName })}
      </p>
    ) : null;
  return (
    <div className="simulator-picker">
      {profiles.length ? (
        <div className="simulator-picker-target">
          <label htmlFor={id ?? fieldId}>{t("目标")}</label>
          <Select
            id={id ?? fieldId}
            aria-label={t("选择模拟器")}
            value={value}
            disabled={disabled}
            onValueChange={onChange}
            options={[{ value: "", label: t("选择模拟器") }, ...profiles.map((p) => ({ value: p.id, label: p.name }))]}
          />
        </div>
      ) : (
        <div className="simulator-picker-empty">
          <span>{t("还没有模拟器")}</span>
          <Button disabled={disabled} onClick={() => setCreating(true)}>
            <Plus size={14} />
            {t("创建模拟器")}
          </Button>
        </div>
      )}
      {!!profiles.length && showManage && (
        <IconButton title={t("管理模拟器")} aria-label={t("管理模拟器")} disabled={disabled} onClick={onManage}>
          <Monitor size={15} />
        </IconButton>
      )}
    </div>
  );
}
export function SimulatorManager() {
  useI18n();
  const [profiles, setProfiles] = useState<SimulatorProfile[]>([]);
  const [editing, setEditing] = useState<SimulatorProfile | "new" | null>(null);
  const [name, setName] = useState<string | null>(null);
  const resolvedName = name ?? t("我的模拟器");
  const [model, setModel] = useState<SimulatorProfile["model"]>("h106-tiga");
  const [removing, setRemoving] = useState<SimulatorProfile | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    const refresh = () =>
      void simulatorProfiles
        .list()
        .then(setProfiles)
        .catch((e) => setError(errorMessage(e)));
    refresh();
    return watchSimulatorProfiles(refresh);
  }, []);
  const edit = (profile: SimulatorProfile | "new") => {
    setEditing(profile);
    setName(profile === "new" ? null : profile.name);
    setModel(profile === "new" ? "h106-tiga" : profile.model);
    setError("");
  };
  async function save() {
    if (!editing) return;
    setBusy(true);
    setError("");
    try {
      if (editing === "new") await simulatorProfiles.create(resolvedName, model);
      else
        await simulatorProfiles.update(editing.id, {
          name: resolvedName.trim(),
          model,
          ...(model !== editing.model ? { firmwareId: "", view: "model" as const } : {}),
        });
      setEditing(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <PageHeader
        crumbs={[{ label: t("工作") }, { label: t("模拟器") }]}
        actions={
          <Button variant="primary" onClick={() => edit("new")}>
            <Plus size={14} />
            {t("创建模拟器")}
          </Button>
        }
      />
      <div className="page simulator-manager">
        <div className="page-description">
          <p>{t("你的模拟设备，随时继续。")}</p>
          <span>{t("型号、固件选择和视图保存在本机，关闭窗口后仍然保留。")}</span>
        </div>
        {error && !editing && !removing && (
          <p role="alert" className="error">
            {t(error)}
          </p>
        )}
        {profiles.length ? (
          <ul className="card simulator-profile-list" aria-label={t("模拟器列表")}>
            {profiles.map((p) => (
              <li key={p.id}>
                <Monitor size={18} />
                <div className="simulator-profile-copy">
                  <strong className="project-row-name" title={p.name}>
                    {p.name}
                  </strong>
                  <span className="project-row-meta">{simulatorModels.find((m) => m.id === p.model)?.name}</span>
                  <span className="simulator-profile-status">{p.firmwareId ? t("已选择固件") : t("尚未选择固件")}</span>
                </div>
                <div className="simulator-profile-actions">
                  <Button
                    controlSize="sm"
                    className="simulator-profile-open"
                    aria-label={t("打开 {name}", { name: p.name })}
                    title={t("打开 {name}", { name: p.name })}
                    onClick={() => void openSimulator({ profileId: p.id }).catch((e) => setError(errorMessage(e)))}
                  >
                    <ArrowSquareOut size={14} />
                    <span>{t("打开")}</span>
                  </Button>
                  <IconButton aria-label={t("编辑 {name}", { name: p.name })} onClick={() => edit(p)}>
                    <PencilSimple size={15} />
                  </IconButton>
                  <IconButton
                    aria-label={t("删除 {name}", { name: p.name })}
                    onClick={() => {
                      setRemoving(p);
                      setError("");
                    }}
                  >
                    <Trash size={15} />
                  </IconButton>
                </div>
              </li>
            ))}
          </ul>
        ) : (
          <Empty
            icon={<Monitor size={26} />}
            title={t("创建你的第一个模拟器")}
            body={t("设置名称和型号，打开独立窗口安装固件。")}
            action={
              <Button onClick={() => edit("new")}>
                <Plus size={14} />
                {t("创建模拟器")}
              </Button>
            }
          />
        )}
        {editing && (
          <form
            className="simulator-profile-editor"
            onSubmit={(e) => {
              e.preventDefault();
              void save();
            }}
          >
            <h2>{editing === "new" ? t("创建模拟器") : t("编辑模拟器")}</h2>
            <label>
              {t("名称")}
              <Input
                aria-label={t("模拟器名称")}
                maxLength={80}
                value={resolvedName}
                onChange={(e) => setName(e.target.value)}
                autoFocus
              />
            </label>
            <label>
              {t("型号")}
              <Select
                aria-label={t("模拟器型号")}
                value={model}
                onValueChange={(value) => setModel(value as SimulatorProfile["model"])}
                options={simulatorModels.map((m) => ({ value: m.id, label: m.name }))}
              />
            </label>
            {error && (
              <p role="alert" className="error">
                {t(error)}
              </p>
            )}
            <div>
              <Button disabled={busy} onClick={() => setEditing(null)}>
                {t("取消")}
              </Button>
              <Button type="submit" variant="primary" disabled={busy || !resolvedName.trim()}>
                {busy ? t("保存中…") : t("保存模拟器")}
              </Button>
            </div>
          </form>
        )}
      </div>
      {removing && (
        <Dialog title={t("删除模拟器")} busy={busy} onClose={() => setRemoving(null)}>
          <p className="dialog-text">
            {t("删除「{name}」及其偏好设置？它的窗口将关闭，本机项目不会删除。", { name: removing.name })}
          </p>
          {error && (
            <p role="alert" className="error">
              {t(error)}
            </p>
          )}
          <div className="dialog-actions">
            <Button disabled={busy} onClick={() => setRemoving(null)}>
              {t("取消")}
            </Button>
            <Button
              disabled={busy}
              variant="danger"
              onClick={() => {
                setBusy(true);
                void simulatorProfiles
                  .delete(removing.id)
                  .then(() => {
                    closeSimulator(removing.id);
                    setRemoving(null);
                  })
                  .catch((e) => setError(errorMessage(e)))
                  .finally(() => setBusy(false));
              }}
            >
              {t("删除模拟器")}
            </Button>
          </div>
        </Dialog>
      )}
    </>
  );
}
