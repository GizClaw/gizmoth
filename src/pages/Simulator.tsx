import { t, useI18n } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { Monitor } from "@phosphor-icons/react";
import { errorMessage } from "../bridge";
import { simulatorConnection, type SimulatorAppRequest } from "../simulator-window";
import {
  simulatorProfiles,
  simulatorModels,
  watchSimulatorProfiles,
  type SimulatorProfile,
  type SimulatorPatch,
} from "../simulator-profiles";
import { GamePreview } from "./GamePreview";
import "./Simulator.css";

export function Simulator() {
  const { language } = useI18n();
  const [profile, setProfile] = useState<SimulatorProfile | null>(null);
  const [incomingApp, setIncomingApp] = useState<SimulatorAppRequest | null>(null);
  const [error, setError] = useState("");
  const current = useRef<SimulatorProfile | null>(null);
  current.current = profile;
  useEffect(() => {
    const query = new URLSearchParams(location.search);
    const id = query.get("simulator");
    const token = query.get("channel");
    if (!id || !token) {
      setError("模拟器缺少配置，请从模拟器管理页重新打开。");
      return;
    }
    let gone = false;
    const refresh = () =>
      void simulatorProfiles
        .list()
        .then((all) => {
          if (gone) return;
          const next = all.find((p) => p.id === id);
          if (!next) {
            setProfile(null);
            setError("此模拟器已删除，请关闭窗口。");
            window.close();
            return;
          }
          setProfile((previous) => (JSON.stringify(previous) === JSON.stringify(next) ? previous : next));
        })
        .catch((e) => {
          if (!gone) setError(errorMessage(e));
        });
    let requestSequence = 0;
    const receiveApp = () => {
      const sequence = ++requestSequence;
      void link
        .incomingApp()
        .then((app) => {
          if (!gone && sequence === requestSequence) setIncomingApp(app);
        })
        .catch((e) => {
          if (!gone && sequence === requestSequence) setError(errorMessage(e));
        });
    };
    const link = simulatorConnection(id, token, receiveApp);
    refresh();
    receiveApp();
    const unwatch = watchSimulatorProfiles(refresh);
    const timer = window.setInterval(refresh, 1000);
    return () => {
      gone = true;
      unwatch();
      clearInterval(timer);
      link.dispose();
    };
  }, []);
  useEffect(() => {
    document.title = profile ? t("{name} — GizMoth 模拟器", { name: profile.name }) : t("GizMoth 模拟器");
  }, [profile?.name, language]);
  const save = (patch: SimulatorPatch) => {
    const p = current.current;
    if (!p) return;
    // Send each patch immediately; native commands serialize read/modify/atomic-save.
    const next = { ...p, ...patch };
    current.current = next;
    setProfile(next);
    void simulatorProfiles.update(p.id, patch).catch((e) => setError(errorMessage(e)));
  };
  return (
    <main className={`simulator-window${isTauri() && navigator.userAgent.includes("Mac") ? " simulator-mac" : ""}`}>
      <header className="simulator-titlebar" data-tauri-drag-region>
        <Monitor size={17} />
        <h1 data-tauri-drag-region>{profile?.name ?? t("GizMoth 模拟器")}</h1>
        <span data-tauri-drag-region>{t("模拟器")}</span>
      </header>
      {error && (
        <p className="error banner" role="alert">
          {t(error)}
        </p>
      )}
      {profile ? (
        <GamePreview
          key={`${profile.id}:${profile.model}`}
          deviceModel={simulatorModels.find((m) => m.id === profile.model)}
          incomingApp={incomingApp}
          preferences={profile}
          onPreferences={save}
        />
      ) : (
        !error && (
          <div className="simulator-empty" role="status">
            {t("正在读取模拟器…")}
          </div>
        )
      )}
    </main>
  );
}
