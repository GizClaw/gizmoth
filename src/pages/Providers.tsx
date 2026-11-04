import { useRef, useState } from "react";
import { Boxes, KeyRound, LoaderCircle, Pencil, Plus, RefreshCw, Trash2, X } from "lucide-react";
import { bridge, errorMessage, native, type Provider, type ProviderApi, type ProviderModel } from "../bridge";
import { apiLabels, fetchModels, presetOf, presets, slug } from "../providers";
import { Dialog, Dot, Empty, PageHeader } from "../ui";

export function ProvidersPage({ providers, onChanged }: { providers: Provider[]; onChanged: () => Promise<void> }) {
  const [editing, setEditing] = useState<Provider | "new" | null>(null);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  async function refreshModels(provider: Provider) {
    setBusy(provider.id);
    setError("");
    try {
      const key = provider.hasKey ? ((await bridge.providerKey(provider.id)) ?? undefined) : undefined;
      const models = await fetchModels(provider.api, provider.baseUrl, key);
      await bridge.saveProvider({ ...provider, models });
      await onChanged();
    } catch (e) {
      setError(`${provider.name}：${errorMessage(e)}`);
    } finally {
      setBusy("");
    }
  }
  return (
    <>
      <PageHeader
        crumbs={[{ label: "模型" }, { label: "Providers" }]}
        actions={
          <button className="button primary" onClick={() => setEditing("new")}>
            <Plus size={14} />
            添加 Provider
          </button>
        }
      />
      <div className="page">
        <p className="page-intro">编程助手通过这些 Provider 调用模型。API key 保存在系统钥匙串，只在请求时读取。</p>
        {error && <p className="error banner">{error}</p>}
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
                    <span className="tag">{presetOf(provider.kind).name}</span>
                  </span>
                  <span className="row-meta">
                    {apiLabels[provider.api]} · {provider.baseUrl}
                  </span>
                </div>
                <span className="status">
                  <Dot tone={provider.hasKey || !presetOf(provider.kind).needsKey ? "ok" : "warn"} />
                  {provider.hasKey ? "已保存 key" : presetOf(provider.kind).needsKey ? "缺少 key" : "无需 key"}
                </span>
                <button
                  className="icon-button"
                  title="刷新模型"
                  aria-label="刷新模型"
                  disabled={!!busy || !native}
                  onClick={() => refreshModels(provider)}
                >
                  {busy === provider.id ? <LoaderCircle className="spin" size={15} /> : <RefreshCw size={15} />}
                </button>
                <button className="icon-button" title="编辑" aria-label="编辑" onClick={() => setEditing(provider)}>
                  <Pencil size={15} />
                </button>
              </div>
              <div className="chips">
                {provider.models.slice(0, 18).map((m) => (
                  <span className="chip" key={m.id} title={m.id}>
                    {m.name}
                  </span>
                ))}
                {provider.models.length > 18 && <span className="chip muted">+{provider.models.length - 18}</span>}
                {!provider.models.length && (
                  <span className="muted small-text">还没有模型，点刷新从接口读取，或编辑后手动添加。</span>
                )}
              </div>
            </section>
          ))
        ) : (
          <Empty
            icon={<KeyRound size={22} />}
            title="添加一个模型 Provider"
            body="可以用本机的 magpie 网关，也可以直接填写 Anthropic、OpenAI、Gemini、OpenRouter、DeepSeek 或任意兼容接口。"
            action={
              <button className="button primary" onClick={() => setEditing("new")}>
                <Plus size={14} />
                添加 Provider
              </button>
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
    setName(next === "custom" ? "" : p.name);
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
      <Dialog title="添加 Provider" onClose={onClose} wide>
        <div className="preset-grid">
          {presets.map((p) => (
            <button key={p.kind} className="preset" onClick={() => choose(p.kind)}>
              <strong>{p.name}</strong>
              <span>{p.hint}</span>
            </button>
          ))}
        </div>
      </Dialog>
    );

  return (
    <Dialog title={provider ? `编辑 ${provider.name}` : `添加 ${preset.name}`} onClose={onClose} wide busy={!!busy}>
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
          名称
          <input value={name} onChange={(e) => setName(e.target.value)} required maxLength={80} />
        </label>
        <label>
          接口类型
          <select value={api} onChange={(e) => setApi(e.target.value as ProviderApi)}>
            {(Object.keys(apiLabels) as ProviderApi[]).map((a) => (
              <option key={a} value={a}>
                {apiLabels[a]}
              </option>
            ))}
          </select>
        </label>
        <label className="span">
          Base URL
          <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} required spellCheck={false} />
        </label>
        <label className="span">
          API key{preset.needsKey ? "" : "（可选）"}
          <input
            type="password"
            autoComplete="off"
            value={apiKey}
            onChange={(e) => {
              setApiKey(e.target.value);
              setClearKey(false);
            }}
            placeholder={
              provider?.hasKey ? "已保存，留空则保持不变" : preset.needsKey ? "粘贴 API key" : "本机网关通常不需要"
            }
          />
          {provider?.hasKey && (
            <span className="check">
              <input type="checkbox" checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} />
              删除已保存的 key
            </span>
          )}
        </label>
        <div className="span models-editor">
          <div className="models-head">
            <span>模型</span>
            <button
              type="button"
              className="button small"
              disabled={!!busy || !native || !baseUrl.trim()}
              onClick={() =>
                task("fetch", async () => setModels(await fetchModels(api, baseUrl.trim(), await keyForRequest())))
              }
            >
              {busy === "fetch" ? <LoaderCircle className="spin" size={12} /> : <RefreshCw size={12} />}
              从接口读取
            </button>
          </div>
          <div className="chips editable">
            {models.map((m) => (
              <span className="chip" key={m.id}>
                {m.id}
                <button
                  type="button"
                  aria-label={`移除 ${m.id}`}
                  onClick={() => setModels(models.filter((x) => x.id !== m.id))}
                >
                  <X size={11} />
                </button>
              </span>
            ))}
            {!models.length && <span className="muted small-text">读取接口返回的模型，或在下方手动添加模型 ID。</span>}
          </div>
          <div className="input-row">
            <input
              value={manual}
              onChange={(e) => setManual(e.target.value)}
              placeholder="手动添加模型 ID，例如 claude-sonnet-5-5"
              spellCheck={false}
            />
            <button
              type="button"
              className="button"
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
              添加
            </button>
          </div>
        </div>
        {error && <p className="error span">{error}</p>}
        <div className="dialog-actions span">
          {provider && (
            <button
              type="button"
              className="button ghost danger-text push-left"
              disabled={!!busy}
              onClick={() =>
                task("remove", async () => {
                  await bridge.removeProvider(provider.id);
                  await onSaved();
                })
              }
            >
              <Trash2 size={14} />
              删除
            </button>
          )}
          <button type="button" className="button" onClick={onClose} disabled={!!busy}>
            取消
          </button>
          <button className="button primary" type="submit" disabled={!!busy || !native || !name.trim()}>
            保存
          </button>
        </div>
      </form>
    </Dialog>
  );
}
