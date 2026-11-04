import { useEffect, useRef, useState, type CSSProperties } from "react";
import {
  Battery,
  Bluetooth,
  Lightbulb,
  Link2,
  LoaderCircle,
  Play,
  Plus,
  Radio,
  RefreshCw,
  Signal,
  Sun,
  Trash2,
  Volume2,
  Wifi,
} from "lucide-react";
import deviceImage from "../assets/device.jpg";
import sceneImage from "../assets/scene.jpg";
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
import { Dialog, Dot, PageHeader, Row } from "../ui";

const hardware: Record<string, { icon: typeof Volume2; tint: string }> = {
  speaker: { icon: Volume2, tint: "#0a84ff" },
  display: { icon: Sun, tint: "#ffb800" },
  led: { icon: Lightbulb, tint: "#ff7a1a" },
  battery: { icon: Battery, tint: "#34c759" },
  wifi: { icon: Wifi, tint: "#5ac8fa" },
  ble: { icon: Bluetooth, tint: "#7d7aff" },
  mic: { icon: Radio, tint: "#ff6482" },
  modem: { icon: Signal, tint: "#30d1a8" },
};
const appTints = ["#ff9f0a", "#30b0c7", "#5e5ce6", "#ff375f", "#34c759", "#0a84ff", "#bf5af2", "#ff6b35"];
const host = (url: string) => url.replace(/^https?:\/\//, "");
const tintOf = (id: string) => appTints[[...id].reduce((sum, c) => (sum * 31 + c.charCodeAt(0)) >>> 0, 0) % 8];

export function statusTone(state?: Overview) {
  return state?.online === true ? "ok" : state?.online === false ? "off" : "idle";
}
export function statusText(state?: Overview) {
  return state?.online === true ? "在线" : state?.online === false ? "离线" : "未检查";
}

/** Full-bleed backdrop. `glow` lights the device's screen in the picture, 0–100. */
function Scene({ glow, dim, soft }: { glow?: number; dim?: boolean; soft?: boolean }) {
  return (
    <div className={`scene${soft ? " soft" : ""}${dim ? " dim" : ""}`} aria-hidden="true">
      <img src={sceneImage} alt="" draggable={false} />
      {glow !== undefined && <i className="scene-lens" style={{ "--glow": glow / 100 } as CSSProperties} />}
    </div>
  );
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
    <div className="scene-page">
      <Scene soft />
      <PageHeader
        glass
        crumbs={[{ label: "工作" }, { label: "设备" }]}
        actions={
          <button className="button glass" onClick={() => setPairing(true)}>
            <Plus size={14} />
            配对设备
          </button>
        }
      />
      <div className="scene-body wide">
        {devices.length ? (
          <div className="device-grid">
            {devices.map((device) => {
              const overview = overviews[device.id];
              const battery = overview?.hardware.find((h) => h.instance.hwd === "battery");
              return (
                <button key={device.id} className="device-card" onClick={() => onOpen(device.id)}>
                  <img src={deviceImage} alt="" draggable={false} />
                  <span className="device-card-text">
                    <strong>{device.name}</strong>
                    <span className="status">
                      <Dot tone={statusTone(overview)} />
                      {statusText(overview)}
                      {battery && ` · ${hardwareSummary(battery)}`}
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
        ) : (
          <div className="hero-empty">
            <img src={deviceImage} alt="" draggable={false} />
            <h2>还没有配对设备</h2>
            <p>在设备上打开绑定窗口，让它靠近电脑，然后通过蓝牙配对。设备凭据只保存在这台电脑的系统钥匙串里。</p>
            <button className="button primary" onClick={() => setPairing(true)}>
              <Bluetooth size={14} />
              配对设备
            </button>
          </div>
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
    </div>
  );
}

function PairDialog({ onClose, onPaired }: { onClose: () => void; onPaired: (device: Device) => void }) {
  const [nearby, setNearby] = useState<Nearby[] | null>(null);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const lock = useRef(false);
  const cancelled = useRef(false);
  // Bluetooth work runs natively; make sure it stops if the dialog goes away.
  useEffect(
    () => () => {
      if (lock.current) void bridge.cancelPairing().catch(() => {});
    },
    [],
  );
  async function task(label: string, action: () => Promise<void>) {
    if (lock.current) return;
    lock.current = true;
    cancelled.current = false;
    setBusy(label);
    setError("");
    try {
      await action();
    } catch (e) {
      if (!cancelled.current) setError(errorMessage(e));
    } finally {
      lock.current = false;
      setBusy("");
    }
  }
  function cancel() {
    cancelled.current = true;
    void bridge.cancelPairing().catch((e) => setError(errorMessage(e)));
  }
  const bluetoothBusy = busy === "正在查找…" || busy === "正在配对…";
  return (
    <Dialog title="配对设备" onClose={onClose}>
      <p className="dialog-text">在设备上打开绑定窗口，开启电脑蓝牙，然后查找附近的 GizOS 设备。</p>
      <button
        className="button primary full"
        disabled={!!busy || !native}
        onClick={() => task("正在查找…", async () => setNearby(await bridge.scan()))}
      >
        {busy === "正在查找…" ? <LoaderCircle className="spin" size={14} /> : <Bluetooth size={14} />}
        {busy === "正在查找…" ? busy : "查找附近设备"}
      </button>
      {bluetoothBusy && (
        <button className="button full pair-cancel" onClick={cancel}>
          取消
        </button>
      )}
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

function LevelTile({
  item,
  disabled,
  onPreview,
  onApply,
}: {
  item: HardwareState;
  disabled: boolean;
  onPreview: (level: number) => void;
  onApply: (level: number) => void;
}) {
  const confirmed = levelOf(item);
  const [draft, setDraft] = useState(confirmed ?? 0);
  useEffect(() => setDraft(confirmed ?? 0), [confirmed]);
  const { icon: Icon, tint } = hardware[item.instance.hwd] ?? { icon: Radio, tint: "#8e8e93" };
  const label = item.instance.description || hardwareNames[item.instance.hwd] || item.instance.hwd;
  const usable = confirmed !== undefined && !item.error;
  const commit = () => {
    if (confirmed !== undefined && draft !== confirmed) onApply(draft);
  };
  return (
    <div
      className={usable && draft > 0 && !disabled ? "tile" : "tile off"}
      style={{ "--tint": tint, "--level": usable ? draft : 0 } as CSSProperties}
    >
      <i className="tile-fill" />
      <span className="tile-icon">
        <Icon size={17} />
      </span>
      <span className="tile-text">
        <strong>{label}</strong>
        <small>{usable ? (draft > 0 ? `${draft}%` : "关闭") : item.error || "不可用"}</small>
      </span>
      {usable && (
        <input
          aria-label={label}
          type="range"
          min={0}
          max={100}
          step={item.instance.hwd === "speaker" ? 1 : 10}
          value={draft}
          disabled={disabled}
          onChange={(e) => {
            setDraft(Number(e.target.value));
            onPreview(Number(e.target.value));
          }}
          onPointerUp={commit}
          onKeyUp={commit}
        />
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
  const [running, setRunning] = useState("");
  const [preview, setPreview] = useState<number>();
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
  const screen = levels.find((h) => h.instance.hwd === "display") ?? levels.find((h) => h.instance.hwd === "led");
  const glow = online ? (preview ?? (screen && levelOf(screen)) ?? 70) : 0;
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
    setPreview(undefined);
  }
  return (
    <div className="scene-page">
      <Scene glow={glow} dim={overview?.online === false} />
      <PageHeader
        glass
        crumbs={[{ label: "设备", onClick: onBack }, { label: device.name }]}
        actions={
          <button className="button glass" disabled={!!busy || !native} onClick={refresh}>
            {busy === "refresh" ? <LoaderCircle className="spin" size={14} /> : <RefreshCw size={14} />}
            刷新
          </button>
        }
      />
      <div className="scene-body">
        <div className="scene-title">
          <h1>{device.name}</h1>
          <p>
            {host(device.baseUrl)}
            {overview?.firmwareVersion && ` · 固件 ${overview.firmwareVersion}`}
          </p>
        </div>
        <div className="pills">
          <span className="pill">
            <Dot tone={statusTone(overview)} />
            <span>
              <strong>状态</strong>
              <small>{statusText(overview)}</small>
            </span>
          </span>
          {facts.map((item) => {
            const Icon = hardware[item.instance.hwd].icon;
            return (
              <span key={item.instance.id} className="pill">
                <Icon size={15} style={{ color: hardware[item.instance.hwd].tint }} />
                <span>
                  <strong>{item.instance.description || hardwareNames[item.instance.hwd]}</strong>
                  <small>{hardwareSummary(item)}</small>
                </span>
              </span>
            );
          })}
        </div>
        {error && (
          <p className="error banner" role="alert">
            {error}
          </p>
        )}
        {overview?.errors.map((e, i) => (
          <p key={i} className="notice">
            {e}
          </p>
        ))}
        <h2 className="scene-heading">
          控制
          {!online && overview && <small>设备在线时可调节</small>}
        </h2>
        {levels.length ? (
          <div className="tiles">
            {levels.map((item) => (
              <LevelTile
                key={item.instance.id}
                item={item}
                disabled={!!busy || !online}
                onPreview={(level) => {
                  if (item === screen) setPreview(level);
                }}
                onApply={(level) => apply(item, level)}
              />
            ))}
          </div>
        ) : (
          <p className="scene-note">
            {overview ? "设备没有声明可调节的音量、屏幕或灯光。" : "刷新后显示设备支持的控制项。"}
          </p>
        )}
        <h2 className="scene-heading">
          程序
          {apps && <small>{apps.length} 个</small>}
        </h2>
        {apps?.length ? (
          <div className="tiles">
            {apps.map((app) => {
              const name = app.display_name || app.app_id;
              return (
                <button
                  key={app.app_id}
                  className="tile app-tile"
                  disabled={!!busy || !online}
                  title={`运行 ${name}`}
                  onClick={() => {
                    setRunning(app.app_id);
                    void task("run", async () => void (await bridge.run(device.id, app.app_id))).finally(() =>
                      setRunning(""),
                    );
                  }}
                >
                  <span className="tile-icon letter" style={{ background: tintOf(app.app_id) }}>
                    {[...name][0].toUpperCase()}
                  </span>
                  <span className="tile-text">
                    <strong>{name}</strong>
                    <small>{app.version}</small>
                  </span>
                  {running === app.app_id ? (
                    <LoaderCircle className="spin tile-action" size={14} />
                  ) : (
                    <Play className="tile-action" size={14} />
                  )}
                </button>
              );
            })}
          </div>
        ) : (
          <p className="scene-note">
            {apps ? "设备上还没有 Lua 程序。在「项目」里编写并推送。" : "刷新后显示已安装的程序。"}
          </p>
        )}
        <div className="danger-zone">
          <button className="button glass" disabled={!!busy || !native} onClick={() => setRemoving(true)}>
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
    </div>
  );
}
