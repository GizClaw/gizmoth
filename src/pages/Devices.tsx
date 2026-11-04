import { useEffect, useRef, useState, type CSSProperties } from "react";
import {
  Battery,
  Bluetooth,
  Cpu,
  Download,
  FolderCode,
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
import { bridge, errorMessage, native, type Device, type Nearby, type ProjectSummary } from "../bridge";
import {
  deviceModelOf,
  hardwareNames,
  hardwareSummary,
  levelOf,
  parseApps,
  parseOverview,
  type DeviceApp,
  type HardwareState,
  type Overview,
} from "../device-model";
import { Dialog, Dot, Empty, PageHeader, Row, relativeTime } from "../ui";
import { DeviceModelView } from "./DeviceModelView";

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
          <div className="tiles">
            {devices.map((device) => {
              const overview = overviews[device.id];
              const battery = overview?.hardware.find((h) => h.instance.hwd === "battery");
              const model = deviceModelOf(overview?.firmwareVersion, device.icon);
              return (
                <button
                  key={device.id}
                  className="tile app-tile"
                  style={{ "--tint": "#48484a" } as CSSProperties}
                  onClick={() => onOpen(device.id)}
                >
                  <span className="tile-icon">
                    <Cpu size={18} />
                  </span>
                  <span className="tile-text">
                    <strong>{device.name}</strong>
                    <small>{model?.name ?? "未知型号"}</small>
                  </span>
                  <span className="status">
                    <Dot tone={statusTone(overview)} />
                    {statusText(overview)}
                    {battery && ` · ${hardwareSummary(battery)}`}
                  </span>
                </button>
              );
            })}
          </div>
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
  onApply,
}: {
  item: HardwareState;
  disabled: boolean;
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
          onChange={(e) => setDraft(Number(e.target.value))}
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
  const [removing, setRemoving] = useState(false);
  const [installing, setInstalling] = useState<ProjectSummary[] | null>(null);
  const [uninstalling, setUninstalling] = useState<DeviceApp | null>(null);
  const lock = useRef(false);
  const installingId = useRef("");
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
  const loadApps = async () =>
    setApps(parseApps(await bridge.apps(device.id)).sort((a, b) => a.app_id.localeCompare(b.app_id)));
  const refresh = () =>
    task("refresh", async () => {
      const next = parseOverview(await bridge.overview(device.id));
      onOverview(next);
      // Listing programs goes through the device itself, so skip it while offline.
      if (next.online) await loadApps();
      else setApps(null);
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
  const model = deviceModelOf(overview?.firmwareVersion, device.icon);
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
      <div className="device-page">
        <div className="device-main">
          <div className="device-title">
            <h1>{device.name}</h1>
            <p>
              {model?.name ?? "未知型号"} · {host(device.baseUrl)}
              {overview?.firmwareVersion && ` · 固件 ${overview.firmwareVersion}`}
            </p>
          </div>
          <section className="status-strip">
            <div className="status-cell lead">
              <span className={`status-orb orb-${statusTone(overview)}`} />
              <span>
                <strong>{statusText(overview)}</strong>
                <small>
                  {online
                    ? "已连接"
                    : overview?.lastSeen && Date.parse(overview.lastSeen)
                      ? `最后在线 ${relativeTime(Date.parse(overview.lastSeen))}`
                      : overview
                        ? "设备离线"
                        : "尚未刷新"}
                </small>
              </span>
            </div>
            {facts.map((item) => {
              const { icon: Icon, tint } = hardware[item.instance.hwd];
              return (
                <div key={item.instance.id} className="status-cell" style={{ "--tint": tint } as CSSProperties}>
                  <span className="status-icon">
                    <Icon size={15} />
                  </span>
                  <span>
                    <strong>{online && !item.error ? hardwareSummary(item) : "—"}</strong>
                    <small>{item.instance.description || hardwareNames[item.instance.hwd]}</small>
                  </span>
                </div>
              );
            })}
          </section>
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
          <h2 className="device-heading">
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
                  onApply={(level) => apply(item, level)}
                />
              ))}
            </div>
          ) : (
            <p className="device-note">
              {overview ? "设备没有声明可调节的音量、屏幕或灯光。" : "刷新后显示设备支持的控制项。"}
            </p>
          )}
          <h2 className="device-heading">
            程序
            {apps && <small>{apps.length} 个</small>}
            <button
              className="button small push-right"
              disabled={!!busy || !online}
              onClick={() => task("projects", async () => setInstalling(await bridge.projects()))}
            >
              <Download size={13} />
              从项目安装
            </button>
          </h2>
          {apps?.length ? (
            <div className="tiles">
              {apps.map((app) => {
                const name = app.display_name || app.app_id;
                return (
                  <div key={app.app_id} className="tile app-tile">
                    <span className="tile-icon letter" style={{ background: tintOf(app.app_id) }}>
                      {[...name][0].toUpperCase()}
                    </span>
                    <span className="tile-text">
                      <strong>{name}</strong>
                      <small>
                        {app.app_id} · {app.version}
                      </small>
                    </span>
                    <button
                      className="icon-button tile-remove"
                      aria-label={`删除 ${name}`}
                      title="删除"
                      disabled={!!busy || !online}
                      onClick={() => setUninstalling(app)}
                    >
                      <Trash2 size={14} />
                    </button>
                    <button
                      className="tile-run"
                      aria-label={`运行 ${name}`}
                      title="运行"
                      disabled={!!busy || !online}
                      onClick={() => {
                        setRunning(app.app_id);
                        void task("run", async () => void (await bridge.run(device.id, app.app_id))).finally(() =>
                          setRunning(""),
                        );
                      }}
                    >
                      {running === app.app_id ? <LoaderCircle className="spin" size={13} /> : <Play size={13} />}
                    </button>
                  </div>
                );
              })}
            </div>
          ) : (
            <p className="device-note">
              {apps ? "设备上还没有 Lua 程序。点「从项目安装」把项目装到设备上。" : "设备在线时显示已安装的程序。"}
            </p>
          )}
          <div className="danger-zone">
            <button className="button ghost danger-text" disabled={!!busy || !native} onClick={() => setRemoving(true)}>
              <Trash2 size={14} />
              移除这台设备
            </button>
          </div>
        </div>
        <DeviceModelView model={model} />
      </div>
      {installing && (
        <Dialog title="安装到设备" onClose={() => setInstalling(null)} busy={busy === "install"}>
          <p className="dialog-text">选择一个项目，打包检查后安装到「{device.name}」。同 ID 的程序会被覆盖。</p>
          {installing.length ? (
            <div className="card list install-list">
              {installing.map((project) => (
                <Row
                  key={project.id}
                  icon={<FolderCode size={16} />}
                  title={project.name}
                  meta={[project.appId, project.version, relativeTime(project.updatedAt)].filter(Boolean).join(" · ")}
                  trailing={
                    busy === "install" &&
                    installingId.current === project.id && <LoaderCircle className="spin" size={14} />
                  }
                  onClick={
                    busy
                      ? undefined
                      : () => {
                          installingId.current = project.id;
                          void task("install", async () => {
                            await bridge.install(project.id, device.id);
                            setInstalling(null);
                            await loadApps();
                          });
                        }
                  }
                />
              ))}
            </div>
          ) : (
            <p className="device-note">还没有项目。先在「项目」里新建一个。</p>
          )}
          {error && <p className="error">{error}</p>}
        </Dialog>
      )}
      {uninstalling && (
        <Dialog title="删除程序" onClose={() => setUninstalling(null)} busy={busy === "uninstall"}>
          <p className="dialog-text">
            将从「{device.name}」上删除「{uninstalling.display_name || uninstalling.app_id}」。本机的项目文件不受影响。
          </p>
          {error && <p className="error">{error}</p>}
          <div className="dialog-actions">
            <button className="button" onClick={() => setUninstalling(null)}>
              取消
            </button>
            <button
              className="button danger"
              disabled={!!busy}
              onClick={() =>
                task("uninstall", async () => {
                  await bridge.uninstall(device.id, uninstalling.app_id);
                  setUninstalling(null);
                  await loadApps();
                })
              }
            >
              删除
            </button>
          </div>
        </Dialog>
      )}
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
