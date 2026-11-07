import { CircleNotch } from "@phosphor-icons/react";
import { version } from "../../package.json";
import { useI18n } from "../i18n";
import { Button, Mark, PageHeader } from "../ui";
import { UpdateButton } from "../UpdateButton";
import { useUpdates } from "../updates";
import "./Settings.css";
import "./About.css";

export function AboutPage() {
  const { t } = useI18n();
  const { state, checkState, supported, check } = useUpdates();
  const checking = checkState.phase === "checking" || supported === null;
  const hasUpdate = state.phase !== "hidden" && state.phase !== "installed";
  const status =
    !supported && supported !== null
      ? t("仅桌面应用支持自动更新。")
      : hasUpdate
        ? t("发现新版本 {version}", { version: `v${state.version}` })
        : checking
          ? t("正在检查更新…")
          : state.phase === "installed" || checkState.phase === "current"
            ? t("已是最新版本")
            : checkState.phase === "error"
              ? t("检查更新失败，请重试。")
              : "";
  return (
    <>
      <PageHeader crumbs={[{ label: t("设置") }, { label: t("关于") }]} />
      <div className="page settings-page about-page">
        <div className="about-identity">
          <Mark size={44} />
          <div>
            <h1>GizMoth</h1>
            <p>MIT License</p>
          </div>
        </div>
        <section className="card settings-list" aria-label={t("关于 GizMoth")}>
          <div className="settings-row">
            <span className="settings-row-label">{t("当前版本")}</span>
            <span className="about-version">v{version}</span>
          </div>
          <div className="settings-row about-update-row">
            <span className="settings-row-label">{t("软件更新")}</span>
            <div className="about-update-actions">
              <span
                className={`about-update-status${checkState.phase === "error" ? " error-text" : ""}`}
                role="status"
                title={checkState.phase === "error" ? checkState.message : undefined}
              >
                {status}
              </span>
              {!hasUpdate ? (
                <Button controlSize="sm" disabled={!supported || checking} onClick={check}>
                  {checking && <CircleNotch size={13} className="spin" aria-hidden="true" />}
                  {t("检查更新")}
                </Button>
              ) : (
                <UpdateButton />
              )}
            </div>
          </div>
        </section>
        <p className="about-update-note">{t("启动时、每小时和网络恢复后自动检查更新。")}</p>
      </div>
    </>
  );
}
