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
    };
  });
}
export function levelOf(item: HardwareState): number | undefined {
  const field = item.instance.hwd === "speaker" ? "volume_percent" : "brightness_percent";
  const level = item.value?.[field];
  return typeof level === "number" && Number.isFinite(level) && level >= 0 && level <= 100 ? level : undefined;
}
export function hardwareSummary(item: HardwareState): string {
  if (item.error) return item.error;
  const value = item.value;
  if (!value) return "尚未读取";
  const number = value.percent;
  switch (item.instance.hwd) {
    case "battery":
      return typeof number === "number"
        ? `${Math.round(number)}%${value.charging === true ? " · 正在充电" : ""}`
        : "电量未知";
    case "wifi":
      return value.connected === true
        ? typeof value.ssid === "string"
          ? value.ssid
          : "已连接"
        : value.connected === false
          ? "未连接"
          : "状态未知";
    case "modem":
      return value.connected === true ? "已连接" : value.connected === false ? "未连接" : "状态未知";
    case "ble":
      return value.connected === true ? "已连接" : value.advertising === true ? "广播中" : "未连接";
    case "mic":
      return value.available === false
        ? "不可用"
        : value.enabled === true
          ? "已启用"
          : value.enabled === false
            ? "已关闭"
            : "状态未知";
    default:
      return "已读取";
  }
}
