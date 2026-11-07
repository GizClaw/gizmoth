import { t, useI18n } from "../i18n";
import { useCallback, useEffect, useId, useRef, useState, type CSSProperties } from "react";
import { CircleNotch, Cpu, Monitor, Play, Trash, type Icon as PhosphorIcon } from "@phosphor-icons/react";
import { Button, Dialog, Select } from "../ui";
import { bridge, errorMessage, type ProjectSummary } from "../bridge";
import type { DeviceApp, DeviceModel } from "../device-model";
import type { ProgramFeedback } from "./DevicePrograms";
import { SimulatorProfilePicker } from "./SimulatorManager";
import { openSimulator } from "../simulator-window";
import "./ApplicationActions.css";

export function ApplicationActions({
  app,
  appearance,
  deviceModel,
  busy,
  online,
  running,
  feedback,
  onClose,
  onRun,
  onRemove,
}: {
  app: DeviceApp;
  appearance: { Icon: PhosphorIcon; color: string };
  deviceModel?: DeviceModel;
  busy: boolean;
  online: boolean;
  running: string;
  feedback: ProgramFeedback | null;
  onClose: () => void;
  onRun: (app: DeviceApp) => void;
  onRemove: (app: DeviceApp) => void;
}) {
  useI18n();
  const titleRef = useRef<HTMLHeadingElement>(null);
  const fieldId = useId();
  const [simulatorId, setSimulatorId] = useState("");
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [selectedId, setSelectedId] = useState("");
  const [readError, setReadError] = useState("");
  const [launchError, setLaunchError] = useState("");
  const [launching, setLaunching] = useState(false);
  const [creatingSimulator, setCreatingSimulator] = useState(false);
  const [opened, setOpened] = useState(false);
  const [retry, setRetry] = useState(0);
  const selectSimulator = useCallback((id: string) => {
    setSimulatorId(id);
    setOpened(false);
    setLaunchError("");
  }, []);

  useEffect(() => {
    let active = true;
    setProjects(null);
    setReadError("");
    void bridge
      .projects()
      .then((all) => {
        if (!active) return;
        const matching = all
          .filter((project) => project.appId === app.app_id)
          .sort(
            (a, b) =>
              Number(b.version === app.version) - Number(a.version === app.version) || b.updatedAt - a.updatedAt,
          );
        setProjects(matching);
        setSelectedId(matching[0]?.id ?? "");
      })
      .catch((error) => {
        if (active) {
          setReadError(errorMessage(error));
          setProjects([]);
        }
      });
    return () => {
      active = false;
    };
  }, [app.app_id, app.version, retry]);

  const project = projects?.find((item) => item.id === selectedId);
  const result = feedback?.appId === app.app_id ? feedback : null;
  const { Icon, color } = appearance;
  const name = app.display_name?.trim() || app.app_id;
  const locked = busy || launching || creatingSimulator;
  const requesting = running === app.app_id;
  const launch = () => {
    if (!project || !simulatorId || locked) return;
    setLaunching(true);
    setOpened(false);
    setLaunchError("");
    // Keep this in the click event so the browser fallback can open its window.
    void openSimulator({ profileId: simulatorId, project, deviceModel })
      .then(() => setOpened(true))
      .catch((error) => setLaunchError(errorMessage(error)))
      .finally(() => setLaunching(false));
  };

  return (
    <Dialog
      title={name}
      onClose={onClose}
      className="program-action-dialog"
      busy={locked}
      initialFocus={titleRef}
      heading={
        <div className="program-action-identity">
          <span className="program-icon" style={{ "--program-color": color } as CSSProperties} aria-hidden="true">
            <Icon size={27} />
          </span>
          <div className="program-action-name">
            <h2 ref={titleRef} tabIndex={-1}>
              {name}
            </h2>
            <p>{t("Lua 应用 · {version}", { version: app.version })}</p>
            {name !== app.app_id && <p className="program-action-id">{app.app_id}</p>}
          </div>
        </div>
      }
    >
      {app.description?.trim() && <p className="program-action-description">{app.description.trim()}</p>}

      <section className="program-action-section" aria-labelledby={`${fieldId}-device`}>
        <div className="program-action-row">
          <div className="program-action-copy">
            <h3 id={`${fieldId}-device`}>
              <Cpu size={17} aria-hidden="true" />
              {t("设备")}
            </h3>
            <p>{online ? t("在当前设备上打开") : t("设备离线，连接后可运行。")}</p>
          </div>
          <Button
            variant="primary"
            className="program-run-action"
            aria-label={result?.tone === "error" ? t("重试在设备上运行") : t("在设备上运行")}
            disabled={locked || !online}
            onClick={() => onRun(app)}
          >
            {requesting ? <CircleNotch className="spin" size={14} /> : <Play size={14} weight="fill" />}
            {requesting ? t("请求中…") : result?.tone === "error" ? t("重试") : t("运行")}
          </Button>
        </div>
        {result && (
          <p className={`program-action-feedback ${result.tone}`} role={result.tone === "error" ? "alert" : "status"}>
            {t(result.message, result.values)}
          </p>
        )}
      </section>

      <section className="program-action-section" aria-labelledby={`${fieldId}-simulator`}>
        <div className="program-action-row">
          <div className="program-action-copy">
            <h3 id={`${fieldId}-simulator`}>
              <Monitor size={17} aria-hidden="true" />
              {t("模拟器")}
            </h3>
            <p>{project ? t("使用本机项目，在独立窗口中运行") : t("使用对应的本机项目运行")}</p>
          </div>
          <Button
            className="program-run-action"
            aria-label={t("在模拟器运行")}
            aria-describedby={!project ? `${fieldId}-source-state` : undefined}
            disabled={locked || !project || !simulatorId}
            onClick={launch}
          >
            {launching ? <CircleNotch className="spin" size={14} /> : <Play size={14} />}
            {launching ? t("发送中…") : t("运行")}
          </Button>
        </div>

        <div className="program-simulator-target">
          <SimulatorProfilePicker
            id={`${fieldId}-target`}
            value={simulatorId}
            onChange={selectSimulator}
            selectable={!!project}
            defaultModel={deviceModel?.id}
            disabled={locked}
            onBusyChange={setCreatingSimulator}
            showManage={false}
          />
        </div>

        {projects === null ? (
          <p id={`${fieldId}-source-state`} className="program-source-state" role="status">
            {t("正在查找本机项目…")}
          </p>
        ) : readError ? (
          <div id={`${fieldId}-source-state`} className="program-source-state error" role="alert">
            <p>{t("本机项目读取失败：{error}", { error: readError })}</p>
            <Button controlSize="sm" disabled={locked} onClick={() => setRetry((value) => value + 1)}>
              {t("重新读取")}
            </Button>
          </div>
        ) : project ? (
          <div className="program-source-fields">
            <div className="program-source-field">
              {(projects?.length ?? 0) > 1 ? (
                <>
                  <label htmlFor={`${fieldId}-source`}>{t("来源")}</label>
                  <Select
                    id={`${fieldId}-source`}
                    aria-label={t("选择模拟器项目")}
                    value={selectedId}
                    disabled={locked}
                    onValueChange={(value) => {
                      setSelectedId(value);
                      setOpened(false);
                      setLaunchError("");
                    }}
                    options={(projects ?? []).map((item) => ({
                      value: item.id,
                      label: `${item.name} · ${item.version ?? t("版本未知")}`,
                    }))}
                  />
                </>
              ) : (
                <>
                  <span>{t("来源")}</span>
                  <p>
                    {project.name}
                    {project.version ? ` · ${project.version}` : ""}
                  </p>
                </>
              )}
            </div>
            {project.version !== app.version && (
              <p className="program-source-warning">{t("本机项目与设备上的版本不同。")}</p>
            )}
          </div>
        ) : (
          <p id={`${fieldId}-source-state`} className="program-source-state">
            {t("没有对应的本机项目，暂时无法在模拟器运行此应用。")}
          </p>
        )}
        {launchError && (
          <p className="program-action-feedback error" role="alert">
            {t(launchError)}
          </p>
        )}
        {opened && (
          <p className="program-action-feedback success" role="status">
            {t("已发送至模拟器，请在独立窗口中查看安装和运行状态。")}
          </p>
        )}
      </section>

      <footer className="program-action-footer">
        <Button
          className="program-action-delete"
          disabled={locked || !online}
          onClick={() => {
            onClose();
            onRemove(app);
          }}
        >
          <Trash size={15} />
          {t("删除应用")}
        </Button>
        <span>{t("从当前设备移除")}</span>
      </footer>
    </Dialog>
  );
}
