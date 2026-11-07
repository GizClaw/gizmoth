import { t, useI18n } from "../i18n";
import { useEffect, useState } from "react";
import { bridge, errorMessage, native, type SearchProvider, type SearchSettings } from "../bridge";
import { Button, Input, Select } from "../ui";

const providerNames: Record<SearchProvider, string> = { brave: "Brave Search", tavily: "Tavily" };

/** Search service the assistant's web_search tool uses; the key stays in the OS credential store. */
export function WebSearchSettings() {
  useI18n();
  const [settings, setSettings] = useState<SearchSettings | null>(null);
  const [provider, setProvider] = useState<SearchProvider | "">("");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ text: string; values?: Record<string, string>; error?: boolean } | null>(
    null,
  );
  useEffect(() => {
    if (!native) return;
    void bridge
      .searchSettings()
      .then((value) => {
        setSettings(value);
        setProvider(value.provider ?? "");
      })
      .catch((e) => setMessage({ text: errorMessage(e), error: true }));
  }, []);
  const changed = provider !== (settings?.provider ?? "") || !!key.trim();
  const needsKey = !!provider && !key.trim() && !(settings?.hasKey && settings.provider === provider);

  async function save(next: SearchProvider | null) {
    setBusy(true);
    setMessage(null);
    try {
      const value = await bridge.saveSearchSettings(next, key.trim() || undefined);
      setSettings(value);
      setProvider(value.provider ?? "");
      setKey("");
      setMessage({
        text: value.provider ? "已保存，助手会用 {provider} 搜索。" : "已关闭联网搜索并删除密钥。",
        values: value.provider ? { provider: providerNames[value.provider] } : undefined,
      });
    } catch (e) {
      setMessage({ text: errorMessage(e), error: true });
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="card list web-search-settings" aria-labelledby="web-search-title">
      <div className="list-head">
        <span id="web-search-title">{t("联网搜索")}</span>
      </div>
      <div className="row">
        <span className="row-main">
          <span className="row-title">{t("搜索服务")}</span>
          <span className="row-meta">{t("供助手的「联网搜索」工具使用；打开网页不需要配置。")}</span>
        </span>
        <Select
          controlSize="sm"
          aria-label={t("搜索服务")}
          value={provider}
          disabled={!settings || busy}
          onValueChange={(value) => setProvider(value as SearchProvider | "")}
          options={[
            { value: "", label: t("不使用") },
            { value: "brave", label: "Brave Search" },
            { value: "tavily", label: "Tavily" },
          ]}
        />
      </div>
      {provider && (
        <div className="row">
          <span className="row-main">
            <span className="row-title">API Key</span>
            <span className="row-meta">
              {settings?.hasKey && settings.provider === provider
                ? t("已保存在系统钥匙串。留空保持不变，填写则替换。")
                : t("保存在系统钥匙串，不会写进配置文件。")}
            </span>
          </span>
          <Input
            controlSize="sm"
            type="password"
            aria-label={t("搜索服务 API Key")}
            autoComplete="off"
            spellCheck={false}
            placeholder={settings?.hasKey && settings.provider === provider ? "••••••••" : t("粘贴 API Key")}
            value={key}
            disabled={busy}
            onChange={(event) => setKey(event.target.value)}
          />
        </div>
      )}
      <div className="row web-search-actions">
        <span className={message?.error ? "error-text" : "row-meta"} role="status">
          {message ? t(message.text, message.values) : ""}
        </span>
        {settings?.provider && (
          <Button controlSize="sm" disabled={busy} onClick={() => void save(null)}>
            {t("关闭并删除密钥")}
          </Button>
        )}
        <Button
          controlSize="sm"
          variant="primary"
          disabled={busy || !settings || !changed || needsKey}
          onClick={() => void save(provider || null)}
        >
          {t("保存")}
        </Button>
      </div>
    </section>
  );
}
