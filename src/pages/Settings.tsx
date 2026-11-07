import { useEffect, useState } from "react";
import { bridge, errorMessage, native } from "../bridge";
import { Mark, PageHeader, Select } from "../ui";
import { useI18n } from "../i18n";
import { useTheme } from "../use-theme";
import { saveLanguagePreference, saveThemePreference } from "../appearance";
import type { LanguagePreference } from "../language";
import type { ThemePreference } from "../theme";
import { WebSearchSettings } from "./WebSearchSettings";

export function SettingsPage() {
  const { t, preference: languagePreference } = useI18n();
  const { preference: themePreference } = useTheme();
  const [saving, setSaving] = useState<"theme" | "language" | "">("");
  const [keepInTray, setKeepInTray] = useState<boolean | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    if (native)
      void bridge
        .preferences()
        .then((p) => setKeepInTray(p.keepInTray))
        .catch((e) => setError(errorMessage(e)));
  }, []);
  async function save(field: "theme" | "language", action: () => Promise<void>) {
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
      <div className="page narrow">
        {error && (
          <p className="error banner" role="alert">
            {t(error)}
          </p>
        )}
        <section className="card list">
          <div className="list-head">
            <span>{t("外观与语言")}</span>
          </div>
          <div className="row">
            <span className="row-main">
              <span className="row-title">{t("主题")}</span>
              <span className="row-meta">{t("应用于所有窗口，支持随系统自动切换。")}</span>
            </span>
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
          </div>
          <div className="row">
            <span className="row-main">
              <span className="row-title">{t("语言")}</span>
              <span className="row-meta">{t("应用界面与文档使用同一语言。")}</span>
            </span>
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
          </div>
        </section>
        <section className="card list">
          <div className="list-head">
            <span>{t("窗口")}</span>
          </div>
          <div className="row">
            <span className="row-main">
              <span className="row-title">{t("关闭窗口后继续在菜单栏运行")}</span>
              <span className="row-meta">{t("从菜单栏图标重新打开；选择「退出 GizMoth」完全退出。")}</span>
            </span>
            <input
              type="checkbox"
              className="switch"
              aria-label={t("关闭窗口后继续运行")}
              checked={keepInTray ?? false}
              disabled={keepInTray === null}
              onChange={(event) => {
                const value = event.target.checked;
                bridge
                  .setPreferences(value)
                  .then(() => setKeepInTray(value))
                  .catch((err) => setError(errorMessage(err)));
              }}
            />
          </div>
        </section>
        <WebSearchSettings />
        <section className="card list">
          <div className="list-head">
            <span>{t("本机数据")}</span>
          </div>
          <div className="row">
            <span className="row-main">
              <span className="row-title">{t("设备凭据与 API key")}</span>
              <span className="row-meta">{t("保存在系统钥匙串（macOS Keychain / Windows 凭据管理器）。")}</span>
            </span>
          </div>
          <div className="row">
            <span className="row-main">
              <span className="row-title">{t("项目、对话记录与配置")}</span>
              <span className="row-meta">
                {t("保存在应用数据目录：设备列表、Provider 配置、Lua 项目目录和每个项目的助手对话。")}
              </span>
            </span>
          </div>
        </section>
        <section className="card about">
          <span className="about-mark">
            <Mark size={30} />
          </span>
          <div>
            <h2>GizMoth 0.1.0</h2>
            <p className="muted">
              {t("配对、控制 GizOS 设备，并用编程助手开发 Lua 程序。MIT License · github.com/GizClaw/gizmoth")}
            </p>
          </div>
        </section>
      </div>
    </>
  );
}
