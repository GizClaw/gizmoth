import { useEffect, useState } from "react";
import { bridge, errorMessage, native } from "../bridge";
import { PageHeader, Select } from "../ui";
import { useI18n } from "../i18n";
import { useTheme } from "../use-theme";
import { saveLanguagePreference, saveThemePreference } from "../appearance";
import type { LanguagePreference } from "../language";
import type { ThemePreference } from "../theme";
import { WebSearchSettings } from "./WebSearchSettings";
import { setCompanionEnabled, useCompanionEnabled } from "../companion-preferences";
import { SettingsRow } from "./SettingsRow";
import "./Settings.css";

export function SettingsPage() {
  const { t, preference: languagePreference } = useI18n();
  const { preference: themePreference } = useTheme();
  const companionEnabled = useCompanionEnabled();
  const [saving, setSaving] = useState<"theme" | "language" | "window" | "companion" | "">("");
  const [keepInTray, setKeepInTray] = useState<boolean | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    if (native)
      void bridge
        .preferences()
        .then((p) => setKeepInTray(p.keepInTray))
        .catch((e) => setError(errorMessage(e)));
  }, []);
  async function save(field: "theme" | "language" | "window" | "companion", action: () => Promise<void>) {
    if (saving) return;
    setSaving(field);
    setError("");
    try {
      await action();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSaving("");
    }
  }
  return (
    <>
      <PageHeader crumbs={[{ label: t("设置") }, { label: t("常规") }]} />
      <div className="page settings-page">
        {error && (
          <p className="error banner" role="alert">
            {t(error)}
          </p>
        )}
        <section className="card settings-list" aria-label={t("常规")}>
          <SettingsRow label={t("主题")} description={t("应用于所有窗口，支持随系统自动切换。")}>
            <Select
              className="settings-preference-select"
              controlSize="sm"
              aria-label={t("主题")}
              disabled={!!saving}
              value={themePreference}
              onValueChange={(value) => void save("theme", () => saveThemePreference(value as ThemePreference))}
              options={[
                { value: "system", label: t("跟随系统") },
                { value: "light", label: t("浅色") },
                { value: "dark", label: t("深色") },
              ]}
            />
          </SettingsRow>
          <SettingsRow label={t("语言")} description={t("应用界面与文档使用同一语言。")}>
            <Select
              className="settings-preference-select"
              controlSize="sm"
              aria-label={t("语言")}
              disabled={!!saving}
              value={languagePreference}
              onValueChange={(value) =>
                void save("language", () => saveLanguagePreference(value as LanguagePreference))
              }
              options={[
                { value: "system", label: t("跟随系统") },
                { value: "zh-CN", label: "简体中文" },
                { value: "en", label: "English" },
              ]}
            />
          </SettingsRow>
          <SettingsRow
            label={t("关闭窗口后继续运行")}
            description={t("从菜单栏图标重新打开；选择「退出 GizMoth」完全退出。")}
          >
            <input
              type="checkbox"
              role="switch"
              className="switch"
              aria-label={t("关闭窗口后继续运行")}
              checked={keepInTray ?? false}
              disabled={keepInTray === null || !!saving}
              onChange={(event) => {
                const value = event.target.checked;
                void save("window", async () => {
                  await bridge.setPreferences(value);
                  setKeepInTray(value);
                });
              }}
            />
          </SettingsRow>
          <SettingsRow
            label={t("页面小人")}
            description={t("在页面边缘散步和跳跃，也可以拖动调整位置。")}
          >
            <input
              type="checkbox"
              role="switch"
              className="switch"
              aria-label={t("页面小人")}
              checked={companionEnabled}
              disabled={!!saving}
              onChange={(event) => {
                const enabled = event.target.checked;
                void save("companion", async () => setCompanionEnabled(enabled));
              }}
            />
          </SettingsRow>
        </section>
        <WebSearchSettings />
      </div>
    </>
  );
}
