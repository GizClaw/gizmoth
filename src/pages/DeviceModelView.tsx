import { t, useI18n } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { Cpu, CircleNotch as LoaderCircle } from "@phosphor-icons/react";
import { loadModelPlayer } from "@gizclaw/models-player";
import "@gizclaw/models-player/style.css";
import type { DeviceModel } from "../device-model";
import { modelPreviewIssue } from "../preview-environment";

/** Interactive 3D model. Geometry comes from the GizClaw CDN, not this repository. */
export function DeviceModelView({ model }: { model?: DeviceModel }) {
  const { language } = useI18n();
  const mount = useRef<HTMLDivElement>(null);
  const viewerLanguage = useRef(language);
  viewerLanguage.current = language;
  const [state, setState] = useState<"loading" | "ready" | "failed">("loading");
  const id = model?.id;
  const environmentIssue = modelPreviewIssue();
  useEffect(() => {
    if (!id || environmentIssue) return;
    let gone = false;
    let destroy: (() => void) | undefined;
    setState("loading");
    loadModelPlayer(id)
      .then(({ createH106Viewer }) => {
        if (gone || !mount.current) return;
        const viewer = createH106Viewer(mount.current, {
          model: id,
          language: viewerLanguage.current,
          embedded: true,
          sound: false,
          interactiveCore: false,
          simGuideButton: false,
        });
        destroy = () => viewer.destroy();
        return viewer.ready;
      })
      .then(() => {
        if (!gone) setState("ready");
      })
      .catch(() => {
        if (!gone) setState("failed");
      });
    return () => {
      gone = true;
      destroy?.();
    };
  }, [id, environmentIssue]);
  const message = id
    ? environmentIssue
      ? t(environmentIssue)
      : state === "loading"
        ? t("正在加载模型…")
        : t("模型加载失败，请检查网络")
    : t("未识别型号");
  return (
    <div className="device-stage">
      <div className="device-model" ref={mount} />
      {(!id || state !== "ready") && (
        <div className="device-model-note" title={message}>
          {id && state === "loading" && !environmentIssue ? (
            <LoaderCircle className="spin" size={18} aria-hidden="true" />
          ) : (
            <Cpu size={22} aria-hidden="true" />
          )}
          <span className="device-model-message">{message}</span>
        </div>
      )}
      <p className="device-model-name">{model?.name ?? t("未知型号")}</p>
    </div>
  );
}
