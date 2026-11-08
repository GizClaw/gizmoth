import { t, useI18n } from "../i18n";
import { Button, SearchInput } from "../ui";
import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import {
  BatteryEmpty as Battery,
  BatteryCharging,
  Bluetooth,
  CaretRight as ChevronRight,
  MagnifyingGlass as Search,
  ShieldCheck,
  WifiSlash as WifiOff,
  FolderSimple as FolderCode,
  Diamond as Gem,
  CircleNotch as LoaderCircle,
  Plus,
  Broadcast as Radio,
  ArrowsClockwise as RefreshCw,
  CellSignalFull as Signal,
  Monitor,
  SpeakerHigh as Volume2,
  WifiHigh as Wifi,
} from "@phosphor-icons/react";
import { bridge, demo, errorMessage, native, type Device, type ProjectSummary } from "../bridge";
import {
  deviceModelOf,
  hardwareNames,
  hardwareSummary,
  levelOf,
  parseApps,
  parseOverview,
  statusText,
  statusTone,
  type DeviceApp,
  type HardwareState,
  type Overview,
} from "../device-model";
import { Dialog, Dot, Empty, PageHeader, Row, relativeTime } from "../ui";
import { DevicePrograms, type ProgramFeedback } from "./DevicePrograms";
import { DeviceControlEffect } from "./DeviceControlEffect";
import { DeviceIdentity } from "./DeviceIdentity";
import { DeviceIcon } from "./DeviceIcon";
import "./DeviceDetail.css";
import { PairDialog } from "./PairDialog";
export { PairDialog } from "./PairDialog";

const hardware: Record<string, { icon: typeof Volume2 }> = {
  speaker: { icon: Volume2 },
  display: { icon: Monitor },
  led: { icon: Gem },
  battery: { icon: Battery },
  wifi: { icon: Wifi },
  ble: { icon: Bluetooth },
  mic: { icon: Radio },
  modem: { icon: Signal },
};
const host = (url: string) => url.replace(/^https?:\/\//, "");

export { statusText, statusTone } from "../device-model";

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
  useI18n();
  const [pairing, setPairing] = useState(false);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<"all" | "online" | "offline" | "unknown">("all");
  const entries = devices.map((device) => {
    const overview = overviews[device.id];
    const model = deviceModelOf(overview?.firmwareVersion, device.icon);
    const state = overview?.online === true ? "online" : overview?.online === false ? "offline" : "unknown";
    const address = host(device.baseUrl).split("/")[0];
    return { device, overview, model, state, address };
  });
  const filters = [
    { key: "all", label: t("全部") },
    { key: "online", label: t("在线") },
    { key: "offline", label: t("离线") },
    { key: "unknown", label: t("未检查") },
  ] as const;
  const search = query.trim().toLocaleLowerCase();
  const visible = entries.filter(
    ({ device, model, state, address }) =>
      (filter === "all" || state === filter) &&
      `${device.name} ${model?.name ?? ""} ${address}`.toLocaleLowerCase().includes(search),
  );
  return (
    <>
      <PageHeader crumbs={[{ label: t("工作") }, { label: t("设备") }]} />
      <div className="page devices-page">
        <p className="devices-description">{t("查看设备状态，调整控制项和管理应用。")}</p>
        {devices.length > 0 && (
          <div className="devices-toolbar">
            <div className="device-filters" role="group" aria-label={t("按连接状态筛选")}>
              {filters.map(({ key, label }) => (
                <button
                  key={key}
                  className={filter === key ? "filter active" : "filter"}
                  aria-pressed={filter === key}
                  onClick={() => setFilter(key)}
                >
                  {label}
                  <span>{key === "all" ? entries.length : entries.filter((entry) => entry.state === key).length}</span>
                </button>
              ))}
            </div>
            <SearchInput
              className="device-search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onClear={() => setQuery("")}
              clearLabel={t("清除设备搜索")}
              placeholder={t("搜索名称、型号或地址")}
              aria-label={t("搜索设备")}
            />
          </div>
        )}
        <div className="page-list-toolbar">
          <button type="button" className="text-action" disabled={!native} onClick={() => setPairing(true)}>
            <Plus size={14} aria-hidden="true" />{t("连接设备")}
          </button>
          <span className="page-list-summary">{t("{count} 台设备", { count: visible.length })}</span>
        </div>
        <section className="device-directory" aria-label={t("已配对设备")}>
          {devices.length ? (
            <>
              {visible.length ? (
                <div className="device-list" role="list">
                  {visible.map(({ device, overview, model, state, address }) => {
                    const battery = overview?.hardware.find((item) => item.instance.hwd === "battery");
                    const networks =
                      overview?.hardware.filter((item) => ["wifi", "modem", "ble"].includes(item.instance.hwd)) ?? [];
                    const network = networks.find((item) => item.value?.connected === true) ?? networks[0];
                    const networkText =
                      state === "offline"
                        ? t("设备离线")
                        : network && !network.error
                          ? hardwareSummary(network, t)
                          : t("未读取");
                    const batteryText =
                      state === "online" && battery && !battery.error ? hardwareSummary(battery, t) : t("未读取");
                    const percent = state === "online" && !battery?.error ? battery?.value?.percent : undefined;
                    const lowBattery = typeof percent === "number" && percent <= 20;
                    const hasNetwork = !!network && !network.error;
                    const hasBattery = !!battery && !battery.error;
                    const NetworkIcon =
                      state === "offline"
                        ? WifiOff
                        : network?.instance.hwd === "modem"
                          ? Signal
                          : network?.instance.hwd === "ble"
                            ? Bluetooth
                            : Wifi;
                    const BatteryIcon =
                      battery?.value?.charging === true && state === "online" ? BatteryCharging : Battery;
                    const seen = overview?.lastSeen ? Date.parse(overview.lastSeen) : NaN;
                    return (
                      <div role="listitem" key={device.id}>
                        <button
                          className={`device-list-row is-${state}`}
                          onClick={() => onOpen(device.id)}
                          title={t("查看「{name}」详情", { name: device.name })}
                          aria-label={[
                            device.name,
                            model?.name,
                            statusText(overview, t),
                            address,
                            state === "online" && hasNetwork ? networkText : undefined,
                            state === "online" && hasBattery ? t("电量：{level}", { level: batteryText }) : undefined,
                          ]
                            .filter(Boolean)
                            .join(" · ")}
                        >
                          <span className="device-emblem">
                            <DeviceIcon model={model?.id} />
                            <Dot tone={statusTone(overview)} />
                          </span>
                          <span className="device-row-copy">
                            <strong className="device-row-name">{device.name}</strong>
                            <span className="device-row-meta">
                              <span className="device-model-label">{model?.name ?? t("型号待识别")}</span>
                              <span className="device-address">{address}</span>
                            </span>
                          </span>
                          <span className="device-row-vitals">
                            {state === "online" && (hasNetwork || hasBattery) ? (
                              <>
                                <span className="device-vital device-vital-state">{statusText(overview, t)}</span>
                                {hasNetwork && (
                                  <span className="device-vital device-row-network" title={networkText}>
                                    <NetworkIcon size={15} aria-hidden="true" />
                                    <span>{networkText}</span>
                                  </span>
                                )}
                                {hasBattery && (
                                  <span
                                    className={`device-vital device-row-battery${lowBattery ? " battery-low" : ""}`}
                                    aria-label={t("电量：{level}", { level: batteryText })}
                                  >
                                    <BatteryIcon size={16} aria-hidden="true" />
                                    <span>{batteryText}</span>
                                  </span>
                                )}
                              </>
                            ) : (
                              <span className={`device-vital device-connection ${state}`}>
                                <span>{statusText(overview, t)}</span>
                                {state === "offline" && Number.isFinite(seen) && (
                                  <span className="device-last-seen">
                                    {t("{time}在线", { time: relativeTime(seen) })}
                                  </span>
                                )}
                              </span>
                            )}
                          </span>
                          <ChevronRight className="device-row-action" size={15} aria-hidden="true" />
                        </button>
                      </div>
                    );
                  })}
                </div>
              ) : (
                <div className="devices-empty-results">
                  <Search size={24} aria-hidden="true" />
                  <h2>
                    {search
                      ? t("没有找到匹配的设备")
                      : filter === "online"
                        ? t("暂无在线设备")
                        : filter === "offline"
                          ? t("暂无离线设备")
                          : t("暂无未检查设备")}
                  </h2>
                  <p>
                    {search
                      ? t("试试设备名称、型号，或清除当前筛选条件。")
                      : t("设备状态变化后，可使用标题栏的刷新按钮重新读取。")}
                  </p>
                  <Button
                    onClick={() => {
                      setQuery("");
                      setFilter("all");
                    }}
                  >
                    {t("显示全部设备")}
                  </Button>
                </div>
              )}
            </>
          ) : (
            <div className="devices-first-run">
              <Empty
                icon={<Bluetooth size={26} />}
                title={t("连接你的第一台设备")}
                body={t("在 GizOS 设备上打开绑定窗口，让它靠近电脑。通过蓝牙配对，或使用设备凭据链接添加。")}
              />
            </div>
          )}
        </section>
        <div className="devices-list-foot">
          <span>
            <ShieldCheck size={14} aria-hidden="true" />
            {t("设备密钥保存在本机，可在设备信息中查看和修改")}
          </span>
          {devices.length > 0 && (
            <span>
              {search || filter !== "all"
                ? t("显示 {count} / {total} 台设备", { count: visible.length, total: devices.length })
                : t("点击设备查看详情与控制项")}
            </span>
          )}
        </div>
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

function LevelTile({
  item,
  disabled,
  offline,
  onApply,
}: {
  item: HardwareState;
  disabled: boolean;
  offline: boolean;
  onApply: (level: number) => void;
}) {
  useI18n();
  const confirmed = levelOf(item);
  const [draft, setDraft] = useState(confirmed ?? 0);
  const [expanded, setExpanded] = useState(false);
  const [origin, setOrigin] = useState<CSSProperties>({});
  const dragging = useRef(false);
  useEffect(() => setDraft(confirmed ?? 0), [confirmed]);
  const { icon: Icon } = hardware[item.instance.hwd] ?? { icon: Radio };
  const label =
    item.instance.description ||
    (hardwareNames[item.instance.hwd] ? t(hardwareNames[item.instance.hwd]) : item.instance.hwd);
  const usable = confirmed !== undefined && !item.error && !offline;
  const tone = item.instance.hwd === "speaker" ? "is-audio" : item.instance.hwd === "display" ? "is-screen" : "is-gem";
  const commit = (value: number) => {
    if (!disabled && usable && value !== confirmed) onApply(value);
  };
  const cancel = () => {
    dragging.current = false;
    setDraft(confirmed ?? 0);
  };
  const close = () => {
    cancel();
    setExpanded(false);
  };
  return (
    <>
      <button
        className={`device-control-tile ${tone}${!usable ? " unavailable" : ""}`}
        disabled={disabled || !usable}
        aria-label={t("调整{label}", { label })}
        aria-haspopup="dialog"
        onClick={(event) => {
          const bounds = event.currentTarget.getBoundingClientRect();
          setOrigin({
            "--control-origin-x": `${bounds.x + bounds.width / 2 - window.innerWidth / 2}px`,
            "--control-origin-y": `${bounds.y + bounds.height / 2 - window.innerHeight / 2}px`,
            "--control-origin-scale": Math.min(bounds.width / 360, bounds.height / 480),
          } as CSSProperties);
          setExpanded(true);
        }}
      >
        <span className="device-control-symbol">
          <Icon size={24} aria-hidden="true" />
        </span>
        <span className="device-control-copy">
          <span className="device-control-name" title={label}>
            {label}
          </span>
          <span className="device-control-reading">{usable ? `${confirmed}%` : offline ? t("离线") : t("未读取")}</span>
        </span>
      </button>
      {expanded && (
        <Dialog title={label} onClose={close} className={`device-control-dialog ${tone}`} style={origin}>
          <div
            className={`device-adjuster ${tone}`}
            style={
              {
                "--level": `${usable ? draft : 0}%`,
                "--intensity": usable ? draft / 100 : 0,
                "--beam-scale": 0.4 + (usable ? draft : 0) * 0.006,
              } as CSSProperties
            }
          >
            <p className="device-adjuster-value">
              {usable ? (
                <>
                  {draft}
                  <small>%</small>
                </>
              ) : offline ? (
                t("离线")
              ) : (
                t("未读取")
              )}
            </p>
            <div className={`control-effect${draft === 0 ? " is-zero" : ""}`}>
              <DeviceControlEffect kind={item.instance.hwd} />
              {usable && (
                <input
                  autoFocus
                  aria-label={label}
                  aria-orientation="vertical"
                  aria-valuetext={`${draft}%`}
                  aria-describedby={`control-hint-${item.instance.id}`}
                  type="range"
                  min={0}
                  max={100}
                  step={item.instance.hwd === "speaker" ? 1 : 10}
                  value={draft}
                  disabled={disabled}
                  onChange={(event) => setDraft(Number(event.target.value))}
                  onPointerDown={(event) => {
                    dragging.current = true;
                    event.currentTarget.setPointerCapture(event.pointerId);
                  }}
                  onPointerUp={(event) => {
                    if (!dragging.current) return;
                    dragging.current = false;
                    commit(Number(event.currentTarget.value));
                  }}
                  onKeyUp={(event) => commit(Number(event.currentTarget.value))}
                  onBlur={(event) => {
                    if (dragging.current) cancel();
                    else commit(Number(event.currentTarget.value));
                  }}
                  onPointerCancel={cancel}
                  onLostPointerCapture={() => {
                    if (dragging.current) cancel();
                  }}
                />
              )}
            </div>
            {item.error && (
              <p className="error" role="alert">
                {t("{error}。请关闭后刷新设备。", { error: t(item.error) })}
              </p>
            )}
            <p className="device-adjuster-hint" id={`control-hint-${item.instance.id}`}>
              {disabled
                ? t("正在应用…")
                : offline
                  ? t("设备离线")
                  : tone === "is-audio"
                    ? t("向上增大音量 · 向下减小音量")
                    : t("向上调亮 · 向下调暗")}
            </p>
            <p className="device-adjuster-dismiss">{t("点击外部任意位置关闭")}</p>
          </div>
        </Dialog>
      )}
    </>
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
  useI18n();
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [apps, setApps] = useState<DeviceApp[] | null>(null);
  const [appsLoading, setAppsLoading] = useState(false);
  const [feedback, setFeedback] = useState<{ message: string; name: string } | null>(null);
  const [programFeedback, setProgramFeedback] = useState<ProgramFeedback | null>(null);
  const appsRequest = useRef(0);
  const online = overview?.online === true;
  const [running, setRunning] = useState("");
  const [removing, setRemoving] = useState(false);
  const [installing, setInstalling] = useState<ProjectSummary[] | null>(null);
  const [uninstalling, setUninstalling] = useState<DeviceApp | null>(null);
  const lock = useRef(false);
  const installingId = useRef("");
  async function task(label: string, action: () => Promise<void>, onError?: (message: string) => void) {
    if (lock.current) return;
    lock.current = true;
    setBusy(label);
    setError("");
    setFeedback(null);
    setProgramFeedback(null);
    try {
      await action();
    } catch (e) {
      (onError ?? setError)(errorMessage(e));
    } finally {
      lock.current = false;
      setBusy("");
    }
  }
  const loadApps = useCallback(async () => {
    const request = ++appsRequest.current;
    setAppsLoading(true);
    try {
      const next = parseApps(await bridge.apps(device.id)).sort((a, b) =>
        (a.display_name || a.app_id).localeCompare(b.display_name || b.app_id),
      );
      if (request === appsRequest.current) setApps(next);
    } finally {
      if (request === appsRequest.current) setAppsLoading(false);
    }
  }, [device.id]);
  useEffect(() => {
    setApps(null);
    if (!native || !online) {
      setAppsLoading(false);
      return;
    }
    let active = true;
    void loadApps().catch((e) => {
      if (active) setError(errorMessage(e));
    });
    return () => {
      active = false;
      appsRequest.current++;
    };
  }, [device.id, online, loadApps]);
  const refresh = () =>
    task("refresh", async () => {
      const next = parseOverview(await bridge.overview(device.id));
      onOverview(next);
      // Listing programs goes through the device itself, so skip it while offline.
      if (next.online && online) await loadApps();
      else if (!next.online) setApps(null);
    });
  useEffect(() => {
    setApps(null);
    if (native && !overview) void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [device.id]);
  const levels = overview?.hardware.filter((h) => ["speaker", "display", "led"].includes(h.instance.hwd)) ?? [];
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
        crumbs={[{ label: t("设备"), onClick: onBack }, { label: device.name }]}
        actions={
          <Button disabled={!!busy || !native} onClick={refresh}>
            {busy === "refresh" ? <LoaderCircle className="spin" size={14} /> : <RefreshCw size={14} />}
            {t("刷新")}
          </Button>
        }
      />
      <div className="device-page">
        <div className="device-main">
          {(error || !!overview?.errors.length) && (
            <div className="device-alerts">
              {error && (
                <p className="error banner" role="alert">
                  {t(error)}
                </p>
              )}
              {overview?.errors.map((message, index) => (
                <p key={index} className="notice">
                  {message}
                </p>
              ))}
            </div>
          )}
          <div className="device-console">
            <DeviceIdentity
              key={device.id}
              device={device}
              overview={overview}
              model={model}
              busy={!!busy}
              refreshing={busy === "refresh"}
              canRemove={native}
              onRemove={() => setRemoving(true)}
              onKeySaved={() => void refresh()}
            />
            <section className="device-controls-section" aria-label={t("设备控制")}>
              <div className="section-head">
                <h2>{t("设备控制")}</h2>
                <span>{online ? t("点按调节") : t("设备在线时可调节")}</span>
              </div>
              {levels.length ? (
                <div className="device-controls">
                  {levels.map((item) => (
                    <LevelTile
                      key={item.instance.id}
                      item={item}
                      disabled={!!busy || !online}
                      offline={!online}
                      onApply={(level) => apply(item, level)}
                    />
                  ))}
                </div>
              ) : (
                <p className="section-empty">
                  {overview ? t("设备没有声明可调节的音量、屏幕或灯光。") : t("刷新后显示设备支持的控制项。")}
                </p>
              )}
            </section>
          </div>
          <DevicePrograms
            key={device.id}
            apps={apps}
            loading={appsLoading}
            online={online}
            busy={!!busy}
            running={running}
            feedback={programFeedback}
            notice={feedback ? t(feedback.message, { name: feedback.name }) : ""}
            deviceModel={model}
            onInstall={() => void task("projects", async () => setInstalling(await bridge.projects()))}
            onRemove={setUninstalling}
            onRun={(app) => {
              if (lock.current) return;
              setRunning(app.app_id);
              void task(
                "run",
                async () => {
                  await bridge.run(device.id, app.app_id);
                  setProgramFeedback({
                    appId: app.app_id,
                    tone: "success",
                    message: demo ? "演示：已模拟打开" : "已发送打开请求",
                  });
                },
                (message) =>
                  setProgramFeedback({
                    appId: app.app_id,
                    tone: "error",
                    message: "未能打开：{error}",
                    values: { error: message },
                  }),
              ).finally(() => setRunning(""));
            }}
          />
        </div>
      </div>
      {installing && (
        <Dialog title={t("安装到设备")} onClose={() => setInstalling(null)} busy={busy === "install"}>
          <p className="dialog-text">
            {t("选择一个项目，打包检查后安装到「{name}」。同 ID 的程序会被覆盖。", { name: device.name })}
          </p>
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
                            setFeedback({
                              message: demo ? "演示：已模拟安装「{name}」" : "已安装「{name}」",
                              name: project.name,
                            });
                          });
                        }
                  }
                />
              ))}
            </div>
          ) : (
            <p className="section-empty">{t("还没有项目。先在「项目」里新建一个。")}</p>
          )}
          {error && <p className="error">{t(error)}</p>}
        </Dialog>
      )}
      {uninstalling && (
        <Dialog title={t("删除程序")} onClose={() => setUninstalling(null)} busy={busy === "uninstall"}>
          <p className="dialog-text">
            {t("将从「{device}」上删除「{app}」。本机的项目文件不受影响。", {
              device: device.name,
              app: uninstalling.display_name || uninstalling.app_id,
            })}
          </p>
          {error && <p className="error">{t(error)}</p>}
          <div className="dialog-actions">
            <Button onClick={() => setUninstalling(null)}>{t("取消")}</Button>
            <Button
              variant="danger"
              disabled={!!busy}
              onClick={() =>
                task("uninstall", async () => {
                  await bridge.uninstall(device.id, uninstalling.app_id);
                  setUninstalling(null);
                  await loadApps();
                  setFeedback({
                    message: demo ? "演示：已模拟删除「{name}」" : "已删除「{name}」",
                    name: uninstalling.display_name || uninstalling.app_id,
                  });
                })
              }
            >
              {t("删除")}
            </Button>
          </div>
        </Dialog>
      )}
      {removing && (
        <Dialog title={t("移除设备")} onClose={() => setRemoving(false)} busy={!!busy}>
          <p className="dialog-text">
            {t("将删除「{name}」在本机的记录和本地 API key。设备上的程序和云端凭据不受影响。", { name: device.name })}
          </p>
          {error && <p className="error">{t(error)}</p>}
          <div className="dialog-actions">
            <Button onClick={() => setRemoving(false)}>{t("取消")}</Button>
            <Button
              variant="danger"
              disabled={!!busy}
              onClick={() =>
                task("remove", async () => {
                  await bridge.removeDevice(device.id);
                  onRemoved();
                })
              }
            >
              {t("移除")}
            </Button>
          </div>
        </Dialog>
      )}
    </>
  );
}
