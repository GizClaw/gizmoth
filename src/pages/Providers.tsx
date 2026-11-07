import { t, useI18n } from "../i18n";
import { Button, IconButton, Input, Select } from "../ui";
import { useRef, useState } from "react";
import {
  Cube as Boxes,
  Key as KeyRound,
  CircleNotch as LoaderCircle,
  PencilSimple as Pencil,
  Plus,
  ArrowsClockwise as RefreshCw,
  Trash as Trash2,
  X,
} from "@phosphor-icons/react";
import { bridge, errorMessage, native, type Provider, type ProviderApi, type ProviderModel } from "../bridge";
import { apiLabels, fetchModels, presetOf, presets, slug } from "../providers";
import { Dialog, Dot, Empty, PageHeader } from "../ui";

function providerErrorText(message: string): string {
  const http = /^读取模型列表失败：HTTP (\d+)$/.exec(message);
  return http ? t("读取模型列表失败：HTTP {status}", { status: http[1] }) : t(message);
}

export function ProvidersPage({ providers, onChanged }: { providers: Provider[]; onChanged: () => Promise<void> }) {
  useI18n();
  const [editing, setEditing] = useState<Provider | "new" | null>(null);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState<{ provider: string; message: string } | null>(null);
  async function refreshModels(provider: Provider) {
    setBusy(provider.id);
    setError(null);
    try {
      const key = provider.hasKey ? ((await bridge.providerKey(provider.id)) ?? undefined) : undefined;
      const models = await fetchModels(provider.api, provider.baseUrl, key);
      await bridge.saveProvider({ ...provider, models });
      await onChanged();
    } catch (e) {
      setError({ provider: provider.name, message: errorMessage(e) });
    } finally {
      setBusy("");
    }
  }
  return (
    <>
      <PageHeader
        crumbs={[{ label: t("模型") }, { label: "Providers" }]}
        actions={
          <Button variant="primary" onClick={() => setEditing("new")}>
            <Plus size={14} />
            {t("添加 Provider")}
          </Button>
        }
      />
      <div className="page">
        <p className="page-intro">
          {t("编程助手通过这些 Provider 调用模型。API key 保存在系统钥匙串，只在请求时读取。")}
        </p>
        {error && (
          <p className="error banner">
            {t("{name}：{error}", { name: error.provider, error: providerErrorText(error.message) })}
          </p>
        )}
        {providers.length ? (
          providers.map((provider) => (
            <section className="card provider" key={provider.id}>
              <div className="provider-head">
                <span className="row-icon">
                  <Boxes size={16} />
                </span>
                <div className="row-main">
                  <span className="row-title">
                    {provider.name}
                    <span className="tag">{t(presetOf(provider.kind).name)}</span>
                  </span>
                  <span className="row-meta">
                    {apiLabels[provider.api]} · {provider.baseUrl}
                  </span>
                </div>
                <span className="status">
                  <Dot tone={provider.hasKey || !presetOf(provider.kind).needsKey ? "ok" : "warn"} />
                  {provider.hasKey ? t("已保存 key") : presetOf(provider.kind).needsKey ? t("缺少 key") : t("无需 key")}
                </span>
                <IconButton
                  title={t("刷新模型")}
                  aria-label={t("刷新模型")}
                  disabled={!!busy || !native}
                  onClick={() => refreshModels(provider)}
                >
                  {busy === provider.id ? <LoaderCircle className="spin" size={15} /> : <RefreshCw size={15} />}
                </IconButton>
                <IconButton title={t("编辑")} aria-label={t("编辑")} onClick={() => setEditing(provider)}>
                  <Pencil size={15} />
                </IconButton>
              </div>
              <div className="chips">
                {provider.models.slice(0, 18).map((m) => (
                  <span className="chip" key={m.id} title={m.id}>
                    {m.name}
                  </span>
                ))}
                {provider.models.length > 18 && <span className="chip muted">+{provider.models.length - 18}</span>}
                {!provider.models.length && (
                  <span className="muted small-text">{t("还没有模型，点刷新从接口读取，或编辑后手动添加。")}</span>
                )}
              </div>
            </section>
          ))
        ) : (
          <Empty
            icon={<KeyRound size={22} />}
            title={t("添加一个模型 Provider")}
            body={t(
              "可以用本机的 magpie 网关，也可以直接填写 Anthropic、OpenAI、Gemini、OpenRouter、DeepSeek 或任意兼容接口。",
            )}
            action={
              <Button variant="primary" onClick={() => setEditing("new")}>
                <Plus size={14} />
                {t("添加 Provider")}
              </Button>
            }
          />
        )}
      </div>
      {editing && (
        <ProviderDialog
          provider={editing === "new" ? undefined : editing}
          existing={providers}
          onClose={() => setEditing(null)}
          onSaved={async () => {
            setEditing(null);
            await onChanged();
          }}
        />
      )}
    </>
  );
}

function ProviderDialog({
  provider,
  existing,
  onClose,
  onSaved,
}: {
  provider?: Provider;
  existing: Provider[];
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  useI18n();
  const [kind, setKind] = useState(provider?.kind ?? "");
  const preset = presetOf(kind);
  const [name, setName] = useState(provider?.name ?? "");
  const [api, setApi] = useState<ProviderApi>(provider?.api ?? "openai-completions");
  const [baseUrl, setBaseUrl] = useState(provider?.baseUrl ?? "");
  const [apiKey, setApiKey] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [models, setModels] = useState<ProviderModel[]>(provider?.models ?? []);
  const [manual, setManual] = useState("");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const lock = useRef(false);
  async function task(label: string, action: () => Promise<void>) {
    if (lock.current) return;
    lock.current = true;
    setBusy(label);
    setError("");
    try {
      await action();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      lock.current = false;
      setBusy("");
    }
  }
  function choose(next: string) {
    const p = presetOf(next);
    setKind(next);
    setName(next === "custom" ? "" : t(p.name));
    setApi(p.api);
    setBaseUrl(p.baseUrl);
  }
  const id =
    provider?.id ??
    (() => {
      const base = slug(name || preset.name);
      let candidate = base;
      for (let n = 2; existing.some((p) => p.id === candidate); n++) candidate = `${base}-${n}`;
      return candidate;
    })();
  const draft = (): Provider => ({
    id,
    kind,
    name: name.trim(),
    api,
    baseUrl: baseUrl.trim(),
    models,
    hasKey: provider?.hasKey ?? false,
  });
  const keyForRequest = async () =>
    apiKey.trim() ||
    (provider?.hasKey && !clearKey ? ((await bridge.providerKey(provider.id)) ?? undefined) : undefined);

  if (!kind)
    return (
      <Dialog title={t("添加 Provider")} onClose={onClose} wide>
        <div className="preset-grid">
          {presets.map((p) => (
            <button key={p.kind} className="preset" onClick={() => choose(p.kind)}>
              <strong>{t(p.name)}</strong>
              <span>{t(p.hint)}</span>
            </button>
          ))}
        </div>
      </Dialog>
    );

  return (
    <Dialog
      title={provider ? t("编辑 {name}", { name: provider.name }) : t("添加 {name}", { name: t(preset.name) })}
      onClose={onClose}
      wide
      busy={!!busy}
    >
      <form
        className="form grid"
        onSubmit={(e) => {
          e.preventDefault();
          void task("save", async () => {
            await bridge.saveProvider(draft(), clearKey ? "" : apiKey.trim() || undefined);
            await onSaved();
          });
        }}
      >
        <label>
          {t("名称")}
          <Input value={name} onChange={(e) => setName(e.target.value)} required maxLength={80} />
        </label>
        <label>
          {t("接口类型")}
          <Select
            aria-label={t("接口类型")}
            value={api}
            onValueChange={(value) => setApi(value as ProviderApi)}
            options={(Object.keys(apiLabels) as ProviderApi[]).map((a) => ({ value: a, label: apiLabels[a] }))}
          />
        </label>
        <label className="span">
          Base URL
          <Input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} required spellCheck={false} />
        </label>
        <label className="span">
          API key{preset.needsKey ? "" : t("（可选）")}
          <Input
            type="password"
            autoComplete="off"
            value={apiKey}
            onChange={(e) => {
              setApiKey(e.target.value);
              setClearKey(false);
            }}
            placeholder={
              provider?.hasKey
                ? t("已保存，留空则保持不变")
                : preset.needsKey
                  ? t("粘贴 API key")
                  : t("本机网关通常不需要")
            }
          />
          {provider?.hasKey && (
            <span className="check">
              <input type="checkbox" checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} />
              {t("删除已保存的 key")}
            </span>
          )}
        </label>
        <div className="span models-editor">
          <div className="models-head">
            <span>{t("模型")}</span>
            <Button
              controlSize="sm"
              type="button"
              disabled={!!busy || !native || !baseUrl.trim()}
              onClick={() =>
                task("fetch", async () => setModels(await fetchModels(api, baseUrl.trim(), await keyForRequest())))
              }
            >
              {busy === "fetch" ? <LoaderCircle className="spin" size={12} /> : <RefreshCw size={12} />}
              {t("从接口读取")}
            </Button>
          </div>
          <div className="chips editable">
            {models.map((m) => (
              <span className="chip" key={m.id}>
                {m.id}
                <button
                  type="button"
                  aria-label={t("移除 {name}", { name: m.id })}
                  onClick={() => setModels(models.filter((x) => x.id !== m.id))}
                >
                  <X size={11} />
                </button>
              </span>
            ))}
            {!models.length && (
              <span className="muted small-text">{t("读取接口返回的模型，或在下方手动添加模型 ID。")}</span>
            )}
          </div>
          <div className="input-row">
            <Input
              value={manual}
              onChange={(e) => setManual(e.target.value)}
              placeholder={t("手动添加模型 ID，例如 claude-sonnet-5-5")}
              spellCheck={false}
            />
            <Button
              type="button"
              disabled={!manual.trim() || models.some((m) => m.id === manual.trim())}
              onClick={() => {
                const mid = manual.trim();
                setModels([
                  ...models,
                  { id: mid, name: mid, reasoning: false, images: false, contextWindow: 128000, maxTokens: 16384 },
                ]);
                setManual("");
              }}
            >
              {t("添加")}
            </Button>
          </div>
        </div>
        {error && <p className="error span">{providerErrorText(error)}</p>}
        <div className="dialog-actions span">
          {provider && (
            <Button
              variant="ghost"
              type="button"
              className="danger-text push-left"
              disabled={!!busy}
              onClick={() =>
                task("remove", async () => {
                  await bridge.removeProvider(provider.id);
                  await onSaved();
                })
              }
            >
              <Trash2 size={14} />
              {t("删除")}
            </Button>
          )}
          <Button type="button" onClick={onClose} disabled={!!busy}>
            {t("取消")}
          </Button>
          <Button variant="primary" type="submit" disabled={!!busy || !native || !name.trim()}>
            {t("保存")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
