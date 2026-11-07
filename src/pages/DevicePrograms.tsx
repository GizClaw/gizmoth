import { t, useI18n } from "../i18n";
import { Button, SearchInput } from "../ui";
import { useState, type CSSProperties } from "react";
import {
  AppWindow,
  Cube as Boxes,
  Cat,
  Check,
  WarningCircle as CircleAlert,
  DownloadSimple as Download,
  GameController as Gamepad2,
  Lightbulb,
  CircleNotch as LoaderCircle,
  Package as PackagePlus,
  Broadcast as Radio,
  MagnifyingGlass as Search,
  Target,
  WifiSlash as WifiOff,
} from "@phosphor-icons/react";
import type { DeviceApp, DeviceModel } from "../device-model";
import { ApplicationActions } from "./ApplicationActions";

// Local fallback artwork: the device API supplies no app icon assets.
const appearances: Record<string, { Icon: typeof AppWindow; color: string }> = {
  breathing_light: { Icon: Lightbulb, color: "#b67918" },
  snake: { Icon: Gamepad2, color: "#328958" },
  whack_a_mole: { Icon: Target, color: "#c44e69" },
  walkie_talkie: { Icon: Radio, color: "#238b91" },
  catnest: { Icon: Cat, color: "#8059c5" },
  boxbound: { Icon: Boxes, color: "#436bce" },
};
const fallbackAppearance = { Icon: AppWindow, color: "#64758e" };

export type ProgramFeedback = {
  appId: string;
  tone: "success" | "error";
  message: string;
  values?: Record<string, string | number>;
};

export function DevicePrograms({
  apps,
  loading,
  online,
  busy,
  running,
  feedback,
  notice,
  deviceModel,
  onRun,
  onRemove,
  onInstall,
}: {
  apps: DeviceApp[] | null;
  loading: boolean;
  online: boolean;
  busy: boolean;
  running: string;
  feedback: ProgramFeedback | null;
  notice: string;
  deviceModel?: DeviceModel;
  onRun: (app: DeviceApp) => void;
  onRemove: (app: DeviceApp) => void;
  onInstall: () => void;
}) {
  useI18n();
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<DeviceApp | null>(null);
  const search = query.trim().toLocaleLowerCase();
  const visible =
    apps?.filter((app) =>
      `${app.display_name ?? ""} ${app.app_id} ${app.description ?? ""}`.toLocaleLowerCase().includes(search),
    ) ?? [];
  return (
    <section className="program-library" aria-labelledby="program-library-title" aria-busy={loading}>
      <header className="program-library-head">
        <div className="program-library-title">
          <h2 id="program-library-title">{t("应用库")}</h2>
          <p>
            {apps
              ? t("{count} 个应用", { count: search ? `${visible.length} / ${apps.length}` : apps.length })
              : t("设备上的 Lua 应用")}
          </p>
        </div>
        <div className="program-library-tools">
          {!!apps?.length && (
            <SearchInput
              className="program-search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onClear={() => setQuery("")}
              clearLabel={t("清除应用搜索")}
              placeholder={t("搜索应用")}
              aria-label={t("搜索已安装应用")}
            />
          )}
          <Button className="program-install" disabled={busy || !online} onClick={onInstall}>
            <Download size={14} />
            {t("从项目安装")}
          </Button>
        </div>
      </header>
      {notice && (
        <p className="program-notice" role="status">
          <Check size={14} aria-hidden="true" />
          {t(notice)}
        </p>
      )}
      {!online ? (
        <div className="program-library-state">
          <WifiOff size={26} aria-hidden="true" />
          <p>{t("设备在线后，可查看和管理已安装的应用。")}</p>
        </div>
      ) : loading && !apps ? (
        <div className="program-library-state" role="status">
          <LoaderCircle className="spin" size={24} aria-hidden="true" />
          <p>{t("正在读取设备上的应用…")}</p>
        </div>
      ) : apps === null ? (
        <div className="program-library-state">
          <AppWindow size={26} aria-hidden="true" />
          <p>{t("暂时无法读取应用列表，请刷新后重试。")}</p>
        </div>
      ) : !apps.length ? (
        <div className="program-library-state">
          <PackagePlus size={34} aria-hidden="true" />
          <h3>{t("把你的第一个应用装到设备上")}</h3>
          <p>{t("选择本机 Lua 项目，检查并安装后即可打开。")}</p>
          <Button disabled={busy} onClick={onInstall}>
            {t("从项目安装")}
          </Button>
        </div>
      ) : !visible.length ? (
        <div className="program-library-state">
          <Search size={26} aria-hidden="true" />
          <p>{t("没有找到匹配的应用。")}</p>
          <Button controlSize="sm" onClick={() => setQuery("")}>
            {t("显示全部应用")}
          </Button>
        </div>
      ) : (
        <ul className="program-grid" id="installed-apps">
          {visible.map((app) => {
            const name = app.display_name?.trim() || app.app_id;
            const { Icon, color } = appearances[app.app_id] ?? fallbackAppearance;
            const result = feedback?.appId === app.app_id ? feedback : null;
            const opening = running === app.app_id;
            return (
              <li className="program-item" key={app.app_id} data-app-id={app.app_id}>
                <button
                  className="program-launch"
                  style={{ "--program-color": color } as CSSProperties}
                  disabled={busy || loading || !online}
                  aria-label={t("查看 {name}", { name })}
                  aria-haspopup="dialog"
                  aria-busy={opening}
                  title={[name, t("版本 {version}", { version: app.version }), app.app_id, app.description?.trim()]
                    .filter(Boolean)
                    .join("\n")}
                  onClick={() => setSelected(app)}
                >
                  <span className="program-icon" aria-hidden="true">
                    {opening ? <LoaderCircle className="spin" size={32} /> : <Icon size={36} />}
                    {result && !opening && (
                      <span className={`program-result-mark ${result.tone}`}>
                        {result.tone === "error" ? <CircleAlert size={15} /> : <Check size={14} />}
                      </span>
                    )}
                  </span>
                  <span className="program-name">{name}</span>
                </button>
                {result && (
                  <p className={`program-feedback ${result.tone}`} role={result.tone === "error" ? "alert" : "status"}>
                    {t(result.message, result.values)}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {selected && (
        <ApplicationActions
          key={selected.app_id}
          app={selected}
          appearance={appearances[selected.app_id] ?? fallbackAppearance}
          deviceModel={deviceModel}
          busy={busy}
          online={online}
          running={running}
          feedback={feedback}
          onClose={() => setSelected(null)}
          onRun={onRun}
          onRemove={onRemove}
        />
      )}
    </section>
  );
}
