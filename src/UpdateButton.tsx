import {
  ArrowCircleUp as ArrowUpCircle,
  CircleNotch as LoaderCircle,
  ArrowCounterClockwise as RotateCcw,
} from "@phosphor-icons/react";
import { downloadPercent, type UpdateState } from "./updater";
import { useUpdates } from "./updates";
import { t, useI18n } from "./i18n";

function label(state: Exclude<UpdateState, { phase: "hidden" }>) {
  switch (state.phase) {
    case "available":
      return t("更新");
    case "downloading": {
      const percent = downloadPercent(state);
      return percent === undefined
        ? t("下载中 {size} MB", { size: (state.received / 1048576).toFixed(1) })
        : t("下载中 {percent}%", { percent });
    }
    case "verifying":
      return t("正在校验…");
    case "downloaded":
      return t("准备安装…");
    case "installing":
      return t("正在安装…");
    case "restarting":
      return t("正在重启…");
    case "installed":
      return t("更新已安装");
    case "error":
      return t(state.retry === "download" ? "重试下载" : state.retry === "install" ? "重试安装" : "重试重启");
  }
}
export function UpdateButton() {
  const { t } = useI18n();
  const { state, activate } = useUpdates();
  if (state.phase === "hidden" || state.phase === "installed") return null;
  const text = label(state);
  const busy = ["downloading", "verifying", "downloaded", "installing", "restarting"].includes(state.phase);
  const description =
    state.phase === "error"
      ? t(state.message)
      : state.phase === "available"
        ? t("发现 GizMoth v{version}，点击下载并更新；安装前会保存当前项目", { version: state.version })
        : state.phase === "downloaded"
          ? t("签名校验通过，正在保存当前项目并准备安装")
          : `GizMoth v${state.version}`;
  const Icon = busy ? LoaderCircle : state.phase === "error" ? RotateCcw : ArrowUpCircle;
  return (
    <button
      type="button"
      className="mds-pill mds-pill--responsive update-button"
      data-tone={state.phase === "error" ? "error" : undefined}
      data-update-state={state.phase}
      disabled={busy}
      onClick={activate}
      title={description}
      aria-label={t("{action}。{description}", { action: text, description })}
      aria-busy={busy}
    >
      <Icon size={13} className="mds-icon" aria-hidden="true" />
      <span className="mds-pill-label" role="status" aria-live="polite">
        {text}
      </span>
    </button>
  );
}
