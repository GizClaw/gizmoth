export interface HardwareState {
  instance: { id: string; hwd: string; description?: string | null };
  value?: Record<string, unknown>;
  error?: string;
}
export interface Overview {
  online: boolean | null;
  lastSeen: string | null;
  firmwareVersion: string | null;
  hardware: HardwareState[];
  errors: string[];
}
export interface DeviceApp {
  app_id: string;
  version: string;
  display_name?: string;
  description?: string;
}
export interface DeviceModel {
  id: "h106-tiga" | "h106-zero" | "h106-orb" | "h106-zeta";
  name: string;
}
const deviceModels: Record<string, DeviceModel> = {
  tiga: { id: "h106-tiga", name: "H106 Tiga" },
  zero: { id: "h106-zero", name: "H106 Zero" },
  orb: { id: "h106-orb", name: "H106 Orb" },
  zeta: { id: "h106-zeta", name: "H106 Zeta" },
};
/**
 * The firmware build names the board (`0.11.7+tiga-esp-v4-2`), so it is the
 * reliable source; the pairing link's `icon` covers devices not read yet.
 */
export function deviceModelOf(firmwareVersion?: string | null, icon?: string | null): DeviceModel | undefined {
  const board = firmwareVersion?.split("+")[1]?.split(/[-_]/)[0]?.toLowerCase();
  const key = icon
    ?.trim()
    .toLowerCase()
    .replace(/_/g, "-")
    .replace(/^h106-/, "");
  return (board && deviceModels[board]) || (key === "h106" ? deviceModels.zero : key ? deviceModels[key] : undefined);
}
export const hardwareNames: Record<string, string> = {
  speaker: "音量",
  display: "屏幕亮度",
  led: "灯光亮度",
  wifi: "Wi-Fi",
  ble: "蓝牙",
  battery: "电池",
  modem: "蜂窝网络",
  mic: "麦克风",
};
const object = (value: unknown): value is Record<string, unknown> =>
  !!value && typeof value === "object" && !Array.isArray(value);
export function parseOverview(raw: unknown): Overview {
  if (
    !object(raw) ||
    (raw.online !== null && typeof raw.online !== "boolean") ||
    !Array.isArray(raw.hardware) ||
    !Array.isArray(raw.errors) ||
    raw.errors.some((e) => typeof e !== "string")
  )
    throw new Error("设备状态格式不匹配，请重新刷新。");
  const ids = new Set<string>();
  const hardware = raw.hardware.map((row: unknown): HardwareState => {
    if (
      !object(row) ||
      !object(row.instance) ||
      typeof row.instance.id !== "string" ||
      typeof row.instance.hwd !== "string" ||
      ids.has(row.instance.id)
    )
      throw new Error("设备能力信息无效或重复。");
    ids.add(row.instance.id);
    if (row.value !== undefined && !object(row.value)) throw new Error("设备设置值无效。");
    if (row.value) {
      for (const field of ["volume_percent", "brightness_percent", "percent"]) {
        const value = row.value[field];
        if (value !== undefined && (typeof value !== "number" || !Number.isFinite(value) || value < 0 || value > 100))
          throw new Error("设备返回的百分比无效。");
      }
      for (const field of ["connected", "charging", "enabled", "muted", "advertising", "available"])
        if (row.value[field] !== undefined && typeof row.value[field] !== "boolean")
          throw new Error("设备返回的开关状态无效。");
    }
    return {
      instance: {
        id: row.instance.id,
        hwd: row.instance.hwd,
        description: typeof row.instance.description === "string" ? row.instance.description : undefined,
      },
      value: row.value as Record<string, unknown> | undefined,
      error: typeof row.error === "string" ? row.error : undefined,
    };
  });
  return {
    online: raw.online as boolean | null,
    lastSeen: typeof raw.lastSeen === "string" ? raw.lastSeen : null,
    firmwareVersion: typeof raw.firmwareVersion === "string" ? raw.firmwareVersion : null,
    hardware,
    errors: raw.errors as string[],
  };
}
export function parseApps(raw: unknown): DeviceApp[] {
  if (!object(raw) || !Array.isArray(raw.apps)) throw new Error("设备程序列表格式不匹配。");
  return raw.apps.map((app: unknown) => {
    if (!object(app) || typeof app.app_id !== "string" || typeof app.version !== "string")
      throw new Error("设备程序信息无效。");
    return {
      app_id: app.app_id,
      version: app.version,
      display_name: typeof app.display_name === "string" ? app.display_name : undefined,
      ...(typeof app.description === "string" ? { description: app.description } : {}),
    };
  });
}
export function levelOf(item: HardwareState): number | undefined {
  const field = item.instance.hwd === "speaker" ? "volume_percent" : "brightness_percent";
  const level = item.value?.[field];
  return typeof level === "number" && Number.isFinite(level) && level >= 0 && level <= 100 ? level : undefined;
}
type Translate = (message: string, values?: Record<string, string | number>) => string;
const identity: Translate = (message) => message;

export function hardwareSummary(item: HardwareState, translate: Translate = identity): string {
  if (item.error) return translate(item.error);
  const value = item.value;
  if (!value) return translate("尚未读取");
  const number = value.percent;
  switch (item.instance.hwd) {
    case "battery":
      return typeof number === "number"
        ? `${Math.round(number)}%${value.charging === true ? translate(" · 正在充电") : ""}`
        : translate("电量未知");
    case "wifi":
      return value.connected === true
        ? typeof value.ssid === "string"
          ? value.ssid
          : translate("已连接")
        : value.connected === false
          ? translate("未连接")
          : translate("状态未知");
    case "modem":
      return value.connected === true
        ? translate("已连接")
        : value.connected === false
          ? translate("未连接")
          : translate("状态未知");
    case "ble":
      return value.connected === true
        ? translate("已连接")
        : value.advertising === true
          ? translate("广播中")
          : translate("未连接");
    case "mic":
      return value.available === false
        ? translate("不可用")
        : value.enabled === true
          ? translate("已启用")
          : value.enabled === false
            ? translate("已关闭")
            : translate("状态未知");
    default:
      return translate("已读取");
  }
}
/** Connection wording shared by the device list and the device identity card. */
export function statusTone(state?: Overview) {
  return state?.online === true ? "ok" : state?.online === false ? "off" : "idle";
}
export function statusText(state?: Overview, translate: Translate = identity) {
  return translate(state?.online === true ? "在线" : state?.online === false ? "离线" : "未检查");
}
const vitalOrder = ["battery", "wifi", "modem", "ble", "mic"];
/** Read-only readings shown on the identity card: battery first, then connectivity. */
export function vitalsOf(state?: Overview): HardwareState[] {
  return (state?.hardware ?? [])
    .filter((item) => vitalOrder.includes(item.instance.hwd))
    .sort((a, b) => vitalOrder.indexOf(a.instance.hwd) - vitalOrder.indexOf(b.instance.hwd));
}
export type BatteryLevel = "charging" | "full" | "high" | "medium" | "low" | "empty";
/** Coarse glyph level only; the exact percentage stays in the visible text. */
export function batteryLevel(percent: number, charging = false): BatteryLevel {
  if (charging) return "charging";
  if (percent >= 85) return "full";
  if (percent >= 60) return "high";
  if (percent >= 35) return "medium";
  return percent > 10 ? "low" : "empty";
}
/** Names the declared readings that return once an offline device reconnects. */
export function offlineVitalsText(items: HardwareState[], translate?: Translate): string {
  const names = [...new Set(items.map((item) => hardwareNames[item.instance.hwd] ?? item.instance.hwd))];
  if (translate) {
    const translated = names.map((name) => translate(name));
    const list =
      translated.length > 1
        ? `${translated.slice(0, -1).join(translate("、"))}${translate("和")}${translated.at(-1)}`
        : (translated[0] ?? translate("设备"));
    return translate("上线后显示{names}状态", { names: list });
  }
  const list = names.length > 1 ? `${names.slice(0, -1).join("、")}和${names.at(-1)}` : (names[0] ?? "设备");
  return `上线后显示${list}状态`.replace(/([一-鿿])([A-Za-z])/g, "$1 $2").replace(/([A-Za-z])([一-鿿])/g, "$1 $2");
}
