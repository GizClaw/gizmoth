import { useRef, useState } from "react";
import { Boxes, Check, Cpu, KeyRound, LoaderCircle, Pencil, Plus, RefreshCw, Search, Trash2 } from "lucide-react";
import {
  bridge,
  errorMessage,
  native,
  type Device,
  type Provider,
  type ProviderApi,
  type ProviderModel,
} from "../bridge";
import {
  apiLabels,
  deviceProviderBase,
  fetchModels,
  mergeModelCatalogue,
  parseModelList,
  pickedModels,
  presetOf,
  presets,
  slug,
} from "../providers";
import { Dialog, Dot, Empty, PageHeader } from "../ui";

export function ProvidersPage({
  providers,
  devices,
  onChanged,
  onOpenDevices,
}: {
  providers: Provider[];
  devices: Device[];
  onChanged: () => Promise<void>;
  onOpenDevices: () => void;
}) {
  const [editing, setEditing] = useState<Provider | "new" | null>(null);
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
        <p className="page-intro">连接一个 Provider，勾选要用的模型。开发时可以在助手顶部切换。</p>
        {providers.length ? (
          providers.map((provider) => {
            const linked = devices.find((d) => d.id === provider.deviceId);
            const deviceSource = provider.kind === "device";
            const selected = pickedModels(provider);
            const ready = deviceSource ? !!linked : provider.hasKey || !presetOf(provider.kind).needsKey;
            return (
              <section className="card provider" key={provider.id}>
                <div className="provider-head">
                  <span className="row-icon">{deviceSource ? <Cpu size={16} /> : <Boxes size={16} />}</span>
                  <div className="row-main">
                    <span className="row-title">
                      {provider.name}
                      <span className="tag">{deviceSource ? "设备" : presetOf(provider.kind).name}</span>
                    </span>
                    <span className="row-meta">
                      {deviceSource ? (linked?.name ?? "设备已移除，请重新选择") : apiLabels[provider.api]}
                    </span>
                  </div>
                  <span className="status">
                    <Dot tone={ready ? "ok" : "warn"} />
                    {deviceSource
                      ? linked
                        ? "使用设备凭据"
                        : "设备不可用"
                      : provider.hasKey
                        ? "已保存 key"
                        : presetOf(provider.kind).needsKey
                          ? "缺少 key"
                          : "无需 key"}
                  </span>
                  <button className="button small" onClick={() => setEditing(provider)}>
                    <Pencil size={12} />
                    选择模型
                  </button>
                </div>
                <div className="provider-models">
                  <span className="small-text muted">
                    已选 {selected.length} / {provider.models.length}
                  </span>
                  <div className="chips">
                    {selected.slice(0, 12).map((m) => (
                      <span className="chip" key={m.id} title={m.id}>
                        {m.name}
                      </span>
                    ))}
                    {selected.length > 12 && <span className="chip muted">+{selected.length - 12}</span>}
                    {!selected.length && <span className="muted small-text">还没有选择模型</span>}
                  </div>
                </div>
              </section>
            );
          })
        ) : (
          <Empty
            icon={<KeyRound size={22} />}
            title="添加一个模型 Provider"
            body="选一台已添加的设备，或连接 magpie 和其他模型服务。"
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
          devices={devices}
          onClose={() => setEditing(null)}
          onOpenDevices={onOpenDevices}
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
  devices,
  onClose,
  onSaved,
  onOpenDevices,
}: {
  provider?: Provider;
  existing: Provider[];
  devices: Device[];
  onClose: () => void;
  onSaved: () => Promise<void>;
  onOpenDevices: () => void;
}) {
  const [kind, setKind] = useState(provider?.kind ?? "");
  const preset = presetOf(kind);
  const [name, setName] = useState(provider?.name ?? "");
  const [api, setApi] = useState<ProviderApi>(provider?.api ?? "openai-completions");
  const [baseUrl, setBaseUrl] = useState(provider?.baseUrl ?? "");
  const [deviceId, setDeviceId] = useState(provider?.deviceId ?? devices[0]?.id ?? "");
  const [apiKey, setApiKey] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [models, setModels] = useState<ProviderModel[]>(provider?.models ?? []);
  const [query, setQuery] = useState("");
  const [manual, setManual] = useState("");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const lock = useRef(false);
  const deviceSource = kind === "device";
  const device = devices.find((d) => d.id === deviceId);
  const endpoint = deviceSource ? (device ? deviceProviderBase(device.baseUrl) : "") : baseUrl.trim();
  const filtered = models.filter((m) => `${m.name} ${m.id}`.toLocaleLowerCase().includes(query.toLocaleLowerCase()));
  const count = models.filter((m) => m.enabled !== false).length;
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
    setName(next === "device" ? (devices[0]?.name ?? "") : next === "custom" ? "" : p.name);
    setApi(p.api);
    setBaseUrl(p.baseUrl);
    setModels([]);
  }
  const id =
    provider?.id ??
    (() => {
      const base = slug(name || preset.name);
      let candidate = base;
      for (let n = 2; existing.some((p) => p.id === candidate); n++) candidate = `${base}-${n}`;
      return candidate;
    })();
  async function readModels() {
    const fetched = deviceSource
      ? parseModelList("openai-completions", await bridge.deviceProviderModels(deviceId))
      : await fetchModels(
          api,
          endpoint,
          apiKey.trim() ||
            (provider?.hasKey && !clearKey ? ((await bridge.providerKey(provider.id)) ?? undefined) : undefined),
        );
    if (!fetched.length) throw new Error("接口没有返回可用模型。可以在下方手动添加模型 ID。");
    setModels((current) => mergeModelCatalogue(current, fetched));
  }
  function toggle(id: string, enabled: boolean) {
    setModels((all) => all.map((m) => (m.id === id ? { ...m, enabled } : m)));
  }
  function selectFiltered(enabled: boolean) {
    const ids = new Set(filtered.map((m) => m.id));
    setModels((all) => all.map((m) => (ids.has(m.id) ? { ...m, enabled } : m)));
  }
  if (!kind)
    return (
      <Dialog title="添加 Provider" onClose={onClose} wide>
        <div className="preset-grid">
          {presets.map((p) => (
            <button
              key={p.kind}
              className={`preset${p.kind === "device" ? " device-preset" : ""}`}
              onClick={() => choose(p.kind)}
            >
              {p.kind === "device" && <Cpu size={18} />}
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
        className="form provider-form"
        onSubmit={(e) => {
          e.preventDefault();
          void task("save", async () => {
            await bridge.saveProvider(
              {
                id,
                kind,
                name: name.trim(),
                api: deviceSource ? "openai-completions" : api,
                baseUrl: endpoint,
                models,
                hasKey: provider?.hasKey ?? false,
                deviceId: deviceSource ? deviceId : null,
              },
              deviceSource ? undefined : clearKey ? "" : apiKey.trim() || undefined,
            );
            await onSaved();
          });
        }}
      >
        <fieldset className="provider-source" disabled={!!busy}>
          <div className="form-pair">
            <label>
              名称
              <input value={name} onChange={(e) => setName(e.target.value)} required maxLength={80} />
            </label>
            {deviceSource ? (
              <label>
                使用设备
                <select
                  value={deviceId}
                  onChange={(e) => {
                    const next = devices.find((d) => d.id === e.target.value);
                    setDeviceId(e.target.value);
                    setModels([]);
                    if (!name || name === device?.name) setName(next?.name ?? "");
                  }}
                  required
                >
                  {!device && <option value="">请选择设备</option>}
                  {devices.map((d) => (
                    <option key={d.id} value={d.id}>
                      {d.name}
                    </option>
                  ))}
                </select>
              </label>
            ) : (
              <label>
                接口类型
                <select
                  value={api}
                  onChange={(e) => {
                    setApi(e.target.value as ProviderApi);
                    setModels([]);
                  }}
                >
                  {(Object.keys(apiLabels) as ProviderApi[]).map((a) => (
                    <option key={a} value={a}>
                      {apiLabels[a]}
                    </option>
                  ))}
                </select>
              </label>
            )}
          </div>
          {deviceSource ? (
            <div className="source-note">
              <KeyRound size={14} />
              <span>
                {device ? "使用这台设备已保存的 API key，无需重复填写。" : "先添加一台设备，再选择它的模型。"}
              </span>
              {!device && (
                <button type="button" className="button small" onClick={onOpenDevices}>
                  添加设备
                </button>
              )}
            </div>
          ) : (
            <>
              <label>
                Base URL
                <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} required spellCheck={false} />
              </label>
              <label>
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
                    provider?.hasKey
                      ? "已保存，留空则保持不变"
                      : preset.needsKey
                        ? "粘贴 API key"
                        : "本机网关通常不需要"
                  }
                />
              </label>
              {provider?.hasKey && (
                <label className="check clear-key">
                  <input type="checkbox" checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} />
                  删除已保存的 key
                </label>
              )}
            </>
          )}
        </fieldset>
        <section className="model-picker" aria-label="选择模型">
          <div className="models-head">
            <span>
              开发模型{" "}
              <small className="muted">
                已选 {count} / {models.length}
              </small>
            </span>
            <button
              type="button"
              className="button small"
              disabled={!!busy || !native || !endpoint}
              onClick={() => task("fetch", readModels)}
            >
              {busy === "fetch" ? <LoaderCircle className="spin" size={12} /> : <RefreshCw size={12} />}读取模型
            </button>
          </div>
          {models.length ? (
            <>
              <div className="model-filter">
                <Search size={14} />
                <input
                  aria-label="搜索模型"
                  placeholder="搜索名称或模型 ID"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                />
                <button
                  type="button"
                  className="text-button"
                  disabled={!!busy || !filtered.length}
                  onClick={() => selectFiltered(true)}
                >
                  全选
                </button>
                <button
                  type="button"
                  className="text-button"
                  disabled={!!busy || !filtered.length}
                  onClick={() => selectFiltered(false)}
                >
                  清空
                </button>
              </div>
              <div className="model-options">
                {filtered.map((m) => (
                  <label key={m.id} className={`model-option${m.enabled !== false ? " selected" : ""}`}>
                    <input
                      type="checkbox"
                      checked={m.enabled !== false}
                      disabled={!!busy}
                      onChange={(e) => toggle(m.id, e.target.checked)}
                    />
                    <span className="model-option-text">
                      <strong>{m.name}</strong>
                      {m.name !== m.id && <small>{m.id}</small>}
                    </span>
                    {m.enabled !== false && <Check size={13} />}
                  </label>
                ))}
                {!filtered.length && <p className="list-empty">没有匹配的模型</p>}
              </div>
            </>
          ) : (
            <p className="model-picker-empty">点“读取模型”，再勾选你想在开发时使用的模型。</p>
          )}
          <div className="model-manual">
            <input
              aria-label="手动模型 ID"
              value={manual}
              onChange={(e) => setManual(e.target.value)}
              placeholder="或手动输入模型 ID"
              spellCheck={false}
            />
            <button
              type="button"
              className="button small"
              disabled={!!busy || !manual.trim() || models.some((m) => m.id === manual.trim())}
              onClick={() => {
                const mid = manual.trim();
                setModels([
                  ...models,
                  {
                    id: mid,
                    name: mid,
                    reasoning: false,
                    images: false,
                    contextWindow: 128000,
                    maxTokens: 16384,
                    enabled: true,
                  },
                ]);
                setManual("");
              }}
            >
              <Plus size={12} />
              添加
            </button>
          </div>
        </section>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        <div className="dialog-actions">
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
          <button
            className="button primary"
            type="submit"
            disabled={!!busy || !native || !name.trim() || (deviceSource && !device)}
          >
            保存{count ? ` ${count} 个模型` : ""}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
