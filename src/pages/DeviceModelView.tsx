import { useEffect, useRef, useState } from "react";
import { Cpu, LoaderCircle } from "lucide-react";
import { loadModelPlayer } from "@gizclaw/models-player";
import "@gizclaw/models-player/style.css";
import type { DeviceModel } from "../device-model";

/** Interactive 3D model. Geometry comes from the GizClaw CDN, not this repository. */
export function DeviceModelView({ model }: { model?: DeviceModel }) {
  const mount = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<"loading" | "ready" | "failed">("loading");
  const id = model?.id;
  useEffect(() => {
    if (!id) return;
    let gone = false;
    let destroy: (() => void) | undefined;
    setState("loading");
    loadModelPlayer(id)
      .then(({ createH106Viewer }) => {
        if (gone || !mount.current) return;
        const viewer = createH106Viewer(mount.current, {
          model: id,
          language: "zh-CN",
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
  }, [id]);
  return (
    <aside className="device-stage">
      <div className="device-model" ref={mount} />
      {(!id || state !== "ready") && (
        <div className="device-model-note">
          {id && state === "loading" ? <LoaderCircle className="spin" size={18} /> : <Cpu size={22} />}
          {id ? (state === "loading" ? "正在加载模型…" : "模型加载失败，请检查网络") : "未识别型号"}
        </div>
      )}
      <p className="device-model-name">{model?.name ?? "未知型号"}</p>
    </aside>
  );
}
