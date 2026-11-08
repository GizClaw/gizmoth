import { t, useI18n } from "../i18n";
import { useId, useState } from "react";
import {
  BatteryCharging,
  BatteryEmpty,
  BatteryFull,
  BatteryHigh,
  BatteryLow,
  BatteryMedium,
  Bluetooth,
  BluetoothConnected,
  BluetoothSlash,
  CaretDown,
  CellSignalSlash,
  CellTower,
  CircleNotch,
  Info,
  Microphone,
  MicrophoneSlash,
  Trash,
  WifiHigh,
  WifiSlash,
  type Icon,
} from "@phosphor-icons/react";
import type { Device } from "../bridge";
import {
  batteryLevel,
  hardwareNames,
  hardwareSummary,
  offlineVitalsText,
  statusText,
  statusTone,
  vitalsOf,
  type BatteryLevel,
  type DeviceModel,
  type HardwareState,
  type Overview,
} from "../device-model";
import { Button, Dot, relativeTime } from "../ui";
import { DeviceKeyEditor } from "./DeviceKeyEditor";
import { DeviceNameEditor } from "./DeviceNameEditor";
import { DeviceModelView } from "./DeviceModelView";
import "./DeviceIdentity.css";

const batteryIcons: Record<BatteryLevel, Icon> = {
  charging: BatteryCharging,
  full: BatteryFull,
  high: BatteryHigh,
  medium: BatteryMedium,
  low: BatteryLow,
  empty: BatteryEmpty,
};
const categoryIcons: Record<string, Icon> = {
  battery: BatteryEmpty,
  wifi: WifiHigh,
  modem: CellTower,
  ble: Bluetooth,
  mic: Microphone,
};

/** Glyphs show only what a reading states: battery fill and on/off links, never an invented signal strength. */
function presentVital(item: HardwareState): { Icon: Icon; text: string; tone?: "low" | "muted" } {
  const value = item.value;
  const fallback = categoryIcons[item.instance.hwd] ?? Info;
  if (item.error || !value) return { Icon: fallback, text: t("未读取"), tone: "muted" };
  const text = hardwareSummary(item, t);
  switch (item.instance.hwd) {
    case "battery": {
      if (typeof value.percent !== "number") return { Icon: fallback, text, tone: "muted" };
      const charging = value.charging === true;
      return {
        Icon: batteryIcons[batteryLevel(value.percent, charging)],
        text: `${Math.round(value.percent)}%`,
        tone: value.percent <= 20 && !charging ? "low" : undefined,
      };
    }
    case "wifi":
      return value.connected === false
        ? { Icon: WifiSlash, text, tone: "muted" }
        : { Icon: WifiHigh, text, tone: value.connected === true ? undefined : "muted" };
    case "modem":
      return value.connected === false
        ? { Icon: CellSignalSlash, text, tone: "muted" }
        : { Icon: CellTower, text, tone: value.connected === true ? undefined : "muted" };
    case "ble":
      return value.connected === true
        ? { Icon: BluetoothConnected, text }
        : value.advertising === true
          ? { Icon: Bluetooth, text }
          : { Icon: BluetoothSlash, text, tone: "muted" };
    case "mic":
      return value.enabled === true && value.available !== false
        ? { Icon: Microphone, text }
        : { Icon: MicrophoneSlash, text, tone: "muted" };
    default:
      return { Icon: fallback, text };
  }
}

/** Keeps Latin product words such as “Wi-Fi” whole when a Chinese sentence wraps. */
function keepWords(text: string) {
  return text.split(/([A-Za-z][A-Za-z0-9-]*)/).map((part, index) =>
    index % 2 ? (
      <span key={index} className="device-identity-word">
        {part}
      </span>
    ) : (
      part
    ),
  );
}

/** Device identity card: model tile, name and removal, then live readings with the details disclosure. */
export function DeviceIdentity({
  device,
  overview,
  model,
  busy,
  refreshing,
  canRemove,
  onRemove,
  onKeySaved,
  onRenamed,
}: {
  device: Device;
  overview?: Overview;
  model?: DeviceModel;
  busy: boolean;
  refreshing: boolean;
  canRemove: boolean;
  onRemove: () => void;
  onKeySaved: () => void;
  onRenamed: (device: Device) => void;
}) {
  useI18n();
  const id = useId();
  const [open, setOpen] = useState(false);
  const vitals = vitalsOf(overview);
  const offline = overview?.online === false;
  const seen = offline && overview.lastSeen ? Date.parse(overview.lastSeen) : NaN;
  const reading = refreshing && !offline && overview?.online !== true;
  const note = offline
    ? offlineVitalsText(vitals, t)
    : overview?.online === true
      ? vitals.length
        ? ""
        : t("设备未提供电量和网络状态")
      : reading
        ? t("正在读取设备状态…")
        : t("刷新后显示设备状态");
  return (
    <section className="device-identity" aria-labelledby={`${id}-name`}>
      <div className={`device-identity-model${offline ? " is-offline" : ""}`}>
        <DeviceModelView model={model} />
      </div>
      <div className="device-identity-head">
        <div className="device-identity-title">
          <DeviceNameEditor device={device} headingId={`${id}-name`} disabled={busy} onSaved={onRenamed} />
          <p className="device-identity-meta">
            <span>{model?.name ?? t("未知型号")}</span>
            <span className="device-identity-status">
              <Dot tone={statusTone(overview)} />
              {statusText(overview, t)}
            </span>
            {Number.isFinite(seen) && (
              <span className="device-identity-seen">{t("{time}在线", { time: relativeTime(seen) })}</span>
            )}
          </p>
        </div>
        <Button
          controlSize="sm"
          variant="ghost"
          className="device-remove"
          aria-label={t("移除设备")}
          title={t("移除这台设备")}
          disabled={busy || !canRemove}
          onClick={onRemove}
        >
          <Trash size={15} aria-hidden="true" />
          <span>{t("移除设备")}</span>
        </Button>
      </div>
      <div className="device-identity-strip">
        {note ? (
          <p className="device-vitals-note">
            {reading && <CircleNotch className="spin" size={14} aria-hidden="true" />}
            <span>{keepWords(note)}</span>
          </p>
        ) : (
          <ul className="device-vitals" aria-label={t("设备状态")}>
            {vitals.map((item) => {
              const { Icon, text, tone } = presentVital(item);
              const name =
                item.instance.description ||
                (hardwareNames[item.instance.hwd] ? t(hardwareNames[item.instance.hwd]) : item.instance.hwd);
              const detail = hardwareSummary(item, t);
              return (
                <li
                  key={item.instance.id}
                  className={`device-vital${tone ? ` is-${tone}` : ""}`}
                  title={`${name} · ${detail}`}
                >
                  <Icon size={16} aria-hidden="true" />
                  <span aria-hidden="true">{text}</span>
                  <span className="device-identity-sr">{`${name} ${detail}`}</span>
                </li>
              );
            })}
          </ul>
        )}
        <button
          type="button"
          className="device-info-toggle"
          aria-label={t("设备信息")}
          title={open ? t("收起设备信息") : t("设备地址、固件版本和 API key")}
          aria-expanded={open}
          aria-controls={open ? `${id}-details` : undefined}
          onClick={() => setOpen((value) => !value)}
        >
          <CaretDown size={16} className="device-info-caret" aria-hidden="true" />
        </button>
      </div>
      {open && (
        <dl className="device-identity-details" id={`${id}-details`}>
          <div>
            <dt title={t("设备地址")}>{t("设备地址")}</dt>
            <dd className="device-identity-value" title={device.baseUrl}>
              {device.baseUrl}
            </dd>
          </div>
          <div>
            <dt title={t("固件版本")}>{t("固件版本")}</dt>
            <dd className="device-identity-value" title={overview?.firmwareVersion || t("尚未读取")}>
              {overview?.firmwareVersion || t("尚未读取")}
            </dd>
          </div>
          <div className="device-identity-key">
            <dt title={t("设备 API key")}>{t("设备 API key")}</dt>
            <dd>
              <DeviceKeyEditor deviceId={device.id} disabled={busy} onSaved={onKeySaved} />
            </dd>
          </div>
        </dl>
      )}
    </section>
  );
}
