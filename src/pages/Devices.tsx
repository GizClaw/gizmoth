import { useEffect, useRef, useState } from "react";
import {
  Battery,
  Bluetooth,
  Cpu,
  Lightbulb,
  Link2,
  LoaderCircle,
  Package,
  Play,
  Plus,
  Radio,
  RefreshCw,
  Sun,
  Trash2,
  Volume2,
  Wifi,
} from "lucide-react";
import { bridge, errorMessage, native, type Device, type Nearby } from "../bridge";
import {
  hardwareNames,
  hardwareSummary,
  levelOf,
  parseApps,
  parseOverview,
  type DeviceApp,
  type HardwareState,
  type Overview,
} from "../device-model";
import { Dialog, Dot, Empty, PageHeader, Row } from "../ui";

const icons: Record<string, typeof Volume2> = {
  speaker: Volume2,
  display: Sun,
  led: Lightbulb,
  battery: Battery,
  wifi: Wifi,
  ble: Bluetooth,
  mic: Radio,
  modem: Radio,
};
const host = (url: string) => url.replace(/^https?:\/\//, "");

export function statusTone(state?: Overview) {
  return state?.online === true ? "ok" : state?.online === false ? "off" : "idle";
}
export function statusText(state?: Overview) {
  return state?.online === true ? "在线" : state?.online === false ? "离线" : "未检查";
}

export function DevicesPage({
  devices,
  overviews,
  onOpen,
  onPaired,
}: {
  devices: Device[];
  overviews: Record<string, Overview>;
  onOpen: (id: string) => void;
  onPaired: (device: Device) => void;
}) {
  const [pairing, setPairing] = useState(false);
  return (
    <>
      <PageHeader
        crumbs={[{ label: "工作" }, { label: "设备" }]}
        actions={
          <button className="button primary" onClick={() => setPairing(true)}>
            <Plus size={14} />
            配对设备
          </button>
        }
      />
      <div className="page">
        {devices.length ? (
          <section className="card list">
            <div className="list-head">
              <span>已配对</span>
              <span className="muted">{devices.length} 台设备</span>
            </div>
            {devices.map((device) => (
              <Row
                key={device.id}
                icon={<Cpu size={17} />}
                title={device.name}
                meta={host(device.baseUrl)}
                trailing={
                  <span className="status">
                    <Dot tone={statusTone(overviews[device.id])} />
                    {statusText(overviews[device.id])}
                  </span>
                }
                onClick={() => onOpen(device.id)}
              />
            ))}
          </section>
        ) : (
          <Empty
            icon={<Bluetooth size={22} />}
            title="还没有配对设备"
            body="在设备上打开绑定窗口，让它靠近电脑，然后通过蓝牙配对。设备凭据只保存在这台电脑的系统钥匙串里。"
            action={
              <button className="button primary" onClick={() => setPairing(true)}>
                <Bluetooth size={14} />
                配对设备
              </button>
            }
          />
        )}
      </div>
      {pairing && (
        <PairDialog
          onClose={() => setPairing(false)}
          onPaired={(device) => {
            setPairing(false);
            onPaired(device);
          }}
        />
      )}
    </>
  );
}

function PairDialog({ onClose, onPaired }: { onClose: () => void; onPaired: (device: Device) => void }) {
  const [nearby, setNearby] = useState<Nearby[] | null>(null);
  const [url, setUrl] = useState("");
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
  return (
    <Dialog title="配对设备" onClose={onClose} busy={!!busy}>
      <p className="dialog-text">在设备上打开绑定窗口，开启电脑蓝牙，然后查找附近的 GizOS 设备。</p>
      <button
        className="button primary full"
        disabled={!!busy || !native}
        onClick={() => task("正在查找…", async () => setNearby(await bridge.scan()))}
      >
        {busy === "正在查找…" ? <LoaderCircle className="spin" size={14} /> : <Bluetooth size={14} />}
        {busy === "正在查找…" ? busy : "查找附近设备"}
      </button>
      {nearby && (
        <div className="card list inset">
          {nearby.length ? (
            nearby.map((item) => (
              <Row
                key={item.id}
                icon={<Radio size={16} />}
                title={item.name}
                meta="点击读取设备凭据"
                onClick={busy ? undefined : () => task("正在配对…", async () => onPaired(await bridge.bind(item.id)))}
              />
            ))
          ) : (
            <p className="dialog-text pad">没有找到设备。确认绑定窗口已打开后重试。</p>
          )}
        </div>
      )}
      <form
        className="form"
        onSubmit={(e) => {
          e.preventDefault();
          void task("正在添加…", async () => onPaired(await bridge.importDevice(url.trim())));
        }}
      >
        <label htmlFor="credential-url">或粘贴设备凭据链接</label>
        <div className="input-row">
          <input
            id="credential-url"
            type="password"
            autoComplete="off"
            placeholder="https://…/api-keys/…"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            disabled={!!busy || !native}
          />
          <button className="button" type="submit" disabled={!!busy || !native || !url.trim()}>
            <Link2 size={14} />
            添加
          </button>
        </div>
      </form>
      {busy === "正在配对…" && <p className="dialog-text">正在通过蓝牙读取凭据，请保持设备靠近…</p>}
      {!native && <p className="notice">浏览器预览不能访问蓝牙和钥匙串。</p>}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </Dialog>
  );
}

function LevelControl({
  item,
  disabled,
  onApply,
}: {
  item: HardwareState;
  disabled: boolean;
  onApply: (level: number) => void;
}) {
  const confirmed = levelOf(item);
  const [draft, setDraft] = useState(confirmed ?? 0);
  useEffect(() => setDraft(confirmed ?? 0), [confirmed]);
  const Icon = icons[item.instance.hwd] ?? Radio;
  const label = item.instance.description || hardwareNames[item.instance.hwd] || item.instance.hwd;
  const commit = () => {
    if (confirmed !== undefined && draft !== confirmed) onApply(draft);
  };
  return (
    <div className="control-row">
      <Icon size={16} className="control-icon" />
      <label htmlFor={`level-${item.instance.id}`}>{label}</label>
      {confirmed === undefined || item.error ? (
        <span className="muted control-value">{item.error || "不可用"}</span>
      ) : (
        <>
          <input
            id={`level-${item.instance.id}`}
            type="range"
            min={0}
            max={100}
            step={item.instance.hwd === "speaker" ? 1 : 10}
            value={draft}
            disabled={disabled}
            onChange={(e) => setDraft(Number(e.target.value))}
            onPointerUp={commit}
            onKeyUp={commit}
          />
          <span className="control-value">{draft}%</span>
        </>
      )}
    </div>
  );
}

export function DeviceDetail({
  device,
  overview,
  onOverview,
  onBack,
  onRemoved,
}: {
  device: Device;
  overview?: Overview;
  onOverview: (o: Overview | undefined) => void;
  onBack: () => void;
  onRemoved: () => void;
}) {
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [apps, setApps] = useState<DeviceApp[] | null>(null);
  const [removing, setRemoving] = useState(false);
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
  const refresh = () =>
    task("refresh", async () => {
      const next = parseOverview(await bridge.overview(device.id));
      onOverview(next);
      // Listing programs goes through the device itself, so skip it while offline.
      setApps(
        next.online ? parseApps(await bridge.apps(device.id)).sort((a, b) => a.app_id.localeCompare(b.app_id)) : null,
      );
    });
  useEffect(() => {
    setApps(null);
    if (native && !overview) void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [device.id]);
  const online = overview?.online === true;
  const levels = overview?.hardware.filter((h) => ["speaker", "display", "led"].includes(h.instance.hwd)) ?? [];
  const facts =
    overview?.hardware.filter((h) => ["battery", "wifi", "ble", "mic", "modem"].includes(h.instance.hwd)) ?? [];
  async function apply(item: HardwareState, level: number) {
    await task("apply", async () => {
      try {
        const value = await bridge.setLevel(device.id, item.instance, level);
        if (overview)
          onOverview(
            parseOverview({
              ...overview,
              hardware: overview.hardware.map((r) =>
                r.instance.id === item.instance.id ? { instance: r.instance, value } : r,
              ),
            }),
          );
      } catch (e) {
        if (overview)
          onOverview({
            ...overview,
            hardware: overview.hardware.map((r) =>
              r.instance.id === item.instance.id ? { instance: r.instance, error: "结果未确认，请刷新" } : r,
            ),
          });
        throw e;
      }
    });
  }
  return (
    <>
      <PageHeader
        crumbs={[{ label: "设备", onClick: onBack }, { label: device.name }]}
        actions={
          <button className="button" disabled={!!busy || !native} onClick={refresh}>
            {busy === "refresh" ? <LoaderCircle className="spin" size={14} /> : <RefreshCw size={14} />}
            刷新
          </button>
        }
      />
      <div className="page">
        {error && (
          <p className="error banner" role="alert">
            {error}
          </p>
        )}
        <section className="card summary">
          <div className="summary-icon">
            <Cpu size={22} />
          </div>
          <div className="summary-main">
            <h1>{device.name}</h1>
            <p className="muted">{host(device.baseUrl)}</p>
          </div>
          <div className="summary-stats">
            <div>
              <span className="stat-label">状态</span>
              <span className="stat-value">
                <Dot tone={statusTone(overview)} />
                {statusText(overview)}
              </span>
            </div>
            <div>
              <span className="stat-label">固件</span>
              <span className="stat-value">{overview?.firmwareVersion ?? "—"}</span>
            </div>
            {facts.map((item) => (
              <div key={item.instance.id}>
                <span className="stat-label">{item.instance.description || hardwareNames[item.instance.hwd]}</span>
                <span className="stat-value">{hardwareSummary(item)}</span>
              </div>
            ))}
          </div>
        </section>
        {overview?.errors.map((e, i) => (
          <p key={i} className="notice">
            {e}
          </p>
        ))}
        <section className="card list">
          <div className="list-head">
            <span>控制</span>
            {!online && overview && <span className="muted">设备在线时可调节</span>}
          </div>
          {levels.length ? (
            levels.map((item) => (
              <LevelControl
                key={item.instance.id}
                item={item}
                disabled={!!busy || !online}
                onApply={(level) => apply(item, level)}
              />
            ))
          ) : (
            <p className="list-empty">
              {overview ? "设备没有声明可调节的音量、屏幕或灯光。" : "刷新后显示设备支持的控制项。"}
            </p>
          )}
        </section>
        <section className="card list">
          <div className="list-head">
            <span>设备上的程序</span>
            <span className="muted">{apps ? `${apps.length} 个` : ""}</span>
          </div>
          {apps?.length ? (
            apps.map((app) => (
              <Row
                key={app.app_id}
                icon={<Package size={16} />}
                title={app.display_name || app.app_id}
                meta={`${app.app_id} · ${app.version}`}
                trailing={
                  <button
                    className="button small"
                    disabled={!!busy || !online}
                    onClick={() => task("run", async () => void (await bridge.run(device.id, app.app_id)))}
                  >
                    <Play size={12} />
                    运行
                  </button>
                }
              />
            ))
          ) : (
            <p className="list-empty">
              {apps ? "设备上还没有 Lua 程序。在「项目」里编写并推送。" : "刷新后显示已安装的程序。"}
            </p>
          )}
        </section>
        <div className="danger-zone">
          <button className="button ghost danger-text" disabled={!!busy || !native} onClick={() => setRemoving(true)}>
            <Trash2 size={14} />
            移除这台设备
          </button>
        </div>
      </div>
      {removing && (
        <Dialog title="移除设备" onClose={() => setRemoving(false)} busy={!!busy}>
          <p className="dialog-text">
            将删除「{device.name}」在本机的记录和钥匙串凭据。设备上的程序和云端凭据不受影响。
          </p>
          {error && <p className="error">{error}</p>}
          <div className="dialog-actions">
            <button className="button" onClick={() => setRemoving(false)}>
              取消
            </button>
            <button
              className="button danger"
              disabled={!!busy}
              onClick={() =>
                task("remove", async () => {
                  await bridge.removeDevice(device.id);
                  onRemoved();
                })
              }
            >
              移除
            </button>
          </div>
        </Dialog>
      )}
    </>
  );
}
