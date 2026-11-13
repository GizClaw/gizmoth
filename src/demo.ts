import { demoSerial } from "./demo-serial.ts";
import { demoAgents } from "./demo-agents.ts";
import { demoDevEnv } from "./demo-devenv.ts";
import { demoGrants } from "./demo-grants.ts";
// Development-only, in-memory stand-in for the native commands, enabled with
// `?demo=1` in a browser preview. Nothing here touches real devices or keys.
import { byteLength, inspectProject, validAppId, validatePath, type Project } from "./project.ts";
import { buildLuaPackage } from "./lua-package.ts";
import type { Provider } from "./bridge.ts";

const now = Date.now();
/** `?empty`: every list starts empty, for reviewing first-run states. */
export const emptyLists = typeof location !== "undefined" && new URLSearchParams(location.search).has("empty");
const files: Record<string, Record<string, string>> = emptyLists
  ? {}
  : {
      "breathing-light": {
        "app.json":
          JSON.stringify(
            { app_id: "breathing_light", version: "0.1.0", display_name: "呼吸灯", description: "" },
            null,
            2,
          ) + "\n",
        "breathing_light.lua":
          '-- 呼吸灯\nlocal level = 0\nlocal step = 10\n\nfor _ = 1, 20 do\n  level = level + step\n  if level >= 100 or level <= 0 then step = -step end\n  print("level", level)\nend\n',
        "breathing_light/curve.txt": "0 10 30 60 100\n",
      },
    };
const chats: Record<string, string> = {
  "breathing-light": JSON.stringify([
    { role: "user", content: "做一个呼吸灯效果，亮度在 0 到 100 之间来回变化", timestamp: now },
    {
      role: "assistant",
      content: [
        { type: "text", text: "我先看一下项目文件。" },
        { type: "toolCall", id: "t1", name: "list_files", arguments: {} },
        { type: "toolCall", id: "t2", name: "write_file", arguments: { path: "breathing_light.lua" } },
        { type: "toolCall", id: "t3", name: "check_package", arguments: {} },
        {
          type: "text",
          text: "已写好 breathing_light.lua：亮度每步变化 10，到边界反向。检查通过，可以点「运行」，选择硬件或模拟器。",
        },
      ],
      stopReason: "stop",
      usage: {
        input: 9200,
        output: 640,
        cacheRead: 3800,
        cacheWrite: 0,
        totalTokens: 13640,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
      },
      timestamp: now,
    },
    {
      role: "toolResult",
      toolCallId: "t1",
      toolName: "list_files",
      content: [{ type: "text", text: "ok" }],
      isError: false,
      timestamp: now,
    },
    {
      role: "toolResult",
      toolCallId: "t2",
      toolName: "write_file",
      content: [{ type: "text", text: "ok" }],
      isError: false,
      timestamp: now,
    },
    {
      role: "toolResult",
      toolCallId: "t3",
      toolName: "check_package",
      content: [{ type: "text", text: "ok" }],
      isError: false,
      timestamp: now,
    },
  ]),
};
// One conversation per project, stored with the brief that survives clearing it.
chats["breathing-light"] = JSON.stringify({
  format: 2,
  brief:
    "呼吸灯：亮度在 0 到 100 之间来回变化，每步 10，到边界反向。\n目前只用标准 Lua 输出亮度值，还没有接设备的灯光接口。",
  messages: JSON.parse(chats["breathing-light"]),
});
const providers: Provider[] = emptyLists
  ? []
  : [
      {
        id: "magpie",
        kind: "magpie",
        name: "magpie",
        api: "openai-completions",
        baseUrl: "http://127.0.0.1:3425/v1",
        hasKey: false,
        models: ["auto/claude-opus-5-5", "auto/claude-sonnet-5-5", "auto/claude-haiku-5-5", "codex/gpt-6.1-sol"].map(
          (id) => ({ id, name: id, reasoning: true, images: false, contextWindow: 200000, maxTokens: 16384 }),
        ),
      },
      {
        id: "anthropic",
        kind: "anthropic",
        name: "Anthropic",
        api: "anthropic-messages",
        baseUrl: "https://api.anthropic.com",
        hasKey: true,
        models: [
          {
            id: "claude-sonnet-5-5",
            name: "Claude Sonnet 5.5",
            reasoning: true,
            images: true,
            contextWindow: 1000000,
            maxTokens: 64000,
          },
        ],
      },
    ];
const emptyApps = typeof location !== "undefined" && new URLSearchParams(location.search).get("apps") === "empty";
let deviceApps: { app_id: string; version: string; display_name?: string }[] = emptyApps
  ? []
  : [
      { app_id: "breathing_light", version: "0.1.0", display_name: "呼吸灯" },
      { app_id: "snake", version: "1.0.0", display_name: "贪吃小蛇" },
      { app_id: "whack_a_mole", version: "2.1.0", display_name: "Whack-a-Mole" },
      { app_id: "walkie_talkie", version: "0.1.0", display_name: "蓝牙对讲机" },
      { app_id: "catnest", version: "0.1.0" },
      { app_id: "boxbound", version: "0.1.0" },
    ];
let runningAppId: string | null = null;
const controlLevels = new Map<string, Record<string, number>>();
const projectSnapshot = (id: string): Project => {
  if (!/^[a-z0-9-]{1,64}$/.test(id)) throw new Error("项目 ID 无效。");
  if (!Object.hasOwn(files, id)) throw new Error("项目不存在。");
  return { version: 1, files: Object.entries(files[id]).map(([path, content]) => ({ path, content })) };
};
const deviceFixture =
  typeof location !== "undefined" ? (emptyLists ? "empty" : new URLSearchParams(location.search).get("devices")) : null;
const offline = (deviceId?: unknown) =>
  (typeof location !== "undefined" && new URLSearchParams(location.search).has("offline")) ||
  (deviceFixture === "mixed" && deviceId === "demo-3");
let stopScan: (() => void) | undefined;
// Optional list-state fixtures remain development-only, behind the existing demo bridge.
const devices =
  deviceFixture === "empty"
    ? []
    : [
        { id: "demo-1", name: "客厅的设备", baseUrl: "https://gizclaw.example", icon: null },
        ...(deviceFixture === "mixed"
          ? [
              { id: "demo-2", name: "工作台的设备", baseUrl: "https://studio.gizclaw.example", icon: "h106-zero" },
              { id: "demo-3", name: "卧室的设备", baseUrl: "https://bedroom.gizclaw.example", icon: "h106-orb" },
              { id: "demo-4", name: "新配对的设备", baseUrl: "https://new.gizclaw.example", icon: null },
            ]
          : []),
      ];

// Deliberately synthetic values, confined to the development-only demo store.
const legacyKeys = typeof location !== "undefined" && new URLSearchParams(location.search).get("keys") === "legacy";
const deviceKeys = new Map<string, string>(
  legacyKeys ? [] : devices.map((device) => [device.id, "gizclaw_sk_v1_demo_only"]),
);

const permissions: Record<string, Record<string, string>> = {};
const search: { provider: string | null; hasKey: boolean } = { provider: null, hasKey: false };
export function demoCall(command: string, args: Record<string, unknown> = {}): unknown {
  if (command.startsWith("system_permission") || command.startsWith("notification_")) return demoGrants(command, args);
  if (command.startsWith("agents_")) return demoAgents(command, args);
  if (command.startsWith("devenv_")) return demoDevEnv(command, args);
  if (command.startsWith("serial_") || command.startsWith("gateway_")) return demoSerial(command, args);
  const id = args.id as string;
  const list = (pid: string) =>
    Object.entries(files[pid] ?? {}).map(([path, c]) => ({ path, size: new TextEncoder().encode(c).length }));
  switch (command) {
    case "list_devices":
      return devices;
    case "rename_device": {
      const name = typeof args.name === "string" ? args.name.trim() : "";
      if (!name || [...name].length > 40 || /[\u0000-\u001f\u007f-\u009f]/.test(name))
        throw new Error("设备名称须为 1–40 个字符，且不能包含换行。");
      const index = devices.findIndex((device) => device.id === id);
      if (index < 0) throw new Error("设备未绑定");
      const device = { ...devices[index], name };
      devices[index] = device;
      return device;
    }
    case "device_key_status":
    case "device_key":
    case "device_save_key":
    case "device_migrate_key": {
      if (!devices.some((device) => device.id === id)) throw new Error("设备未绑定。");
      const saved = deviceKeys.get(id);
      if (command === "device_key_status") return { hasKey: saved !== undefined, needsMigration: saved === undefined };
      if (command === "device_key") return saved ?? null;
      if (command === "device_migrate_key") {
        if (saved) return { legacyRemoved: false };
        deviceKeys.set(id, "gizclaw_sk_v1_demo_migrated");
        return { legacyRemoved: true };
      }
      const key = args.apiKey;
      if (typeof key !== "string" || !/^gizclaw_sk_v1_[A-Za-z0-9_-]{1,81}$/.test(key))
        throw new Error("设备 API key 格式无效。");
      deviceKeys.set(id, key);
      return null;
    }
    case "device_overview": {
      if (deviceFixture === "mixed" && args.deviceId === "demo-4")
        return { online: null, lastSeen: null, firmwareVersion: null, errors: [], hardware: [] };
      const board =
        args.deviceId === "demo-2" ? "zero-esp-v3-0" : args.deviceId === "demo-3" ? "orb-esp-v1-0" : "tiga-esp-v4-2";
      const firmwareVersion = `0.11.7-prod.2+${board}`;
      // `?demo=1&offline=1` previews the offline layout.
      if (offline(args.deviceId))
        return {
          online: false,
          lastSeen: new Date(now - 31 * 60000).toISOString(),
          firmwareVersion,
          errors: [],
          hardware: ["display", "led", "speaker", "battery", "modem", "wifi"].map((hwd) => ({
            instance: { id: `${hwd}.main`, hwd },
            error: "设备离线",
          })),
        };
      return {
        online: true,
        lastSeen: null,
        firmwareVersion,
        errors: [],
        hardware: [
          {
            instance: { id: "speaker.main", hwd: "speaker" },
            value: { volume_percent: controlLevels.get(String(args.deviceId))?.["speaker.main"] ?? 45 },
          },
          {
            instance: { id: "display.main", hwd: "display" },
            value: { brightness_percent: controlLevels.get(String(args.deviceId))?.["display.main"] ?? 70 },
          },
          {
            instance: { id: "led.main", hwd: "led" },
            value: { brightness_percent: controlLevels.get(String(args.deviceId))?.["led.main"] ?? 80 },
          },
          {
            instance: { id: "battery.main", hwd: "battery" },
            value: { percent: args.deviceId === "demo-2" ? 8 : 86, charging: false },
          },
          {
            instance: { id: "wifi.main", hwd: "wifi" },
            value: { connected: true, ssid: args.deviceId === "demo-2" ? "Studio" : "Home" },
          },
        ],
      };
    }
    case "set_device_level": {
      if (!devices.some((device) => device.id === args.deviceId)) throw new Error("设备不存在。");
      if (offline(args.deviceId)) throw new Error("设备离线，请连接后再调节。");
      const instance = args.instance as { id?: string; hwd?: string } | undefined;
      const level = args.level;
      if (
        !instance ||
        typeof instance.id !== "string" ||
        typeof instance.hwd !== "string" ||
        !["speaker", "display", "led"].includes(instance.hwd) ||
        instance.id !== `${instance.hwd}.main`
      )
        throw new Error("设备未声明此控制项。");
      if (
        typeof level !== "number" ||
        !Number.isInteger(level) ||
        level < 0 ||
        level > 100 ||
        (instance.hwd !== "speaker" && level % 10 !== 0)
      )
        throw new Error("数值无效，亮度需要使用 10% 档位。");
      const deviceId = String(args.deviceId);
      controlLevels.set(deviceId, { ...controlLevels.get(deviceId), [instance.id]: level });
      return instance.hwd === "speaker" ? { volume_percent: level } : { brightness_percent: level };
    }
    case "list_apps":
      return { apps: deviceApps, running_app_id: runningAppId };
    case "run_app": {
      if (!devices.some((device) => device.id === args.deviceId)) throw new Error("设备不存在。");
      if (offline(args.deviceId)) throw new Error("设备离线，请连接后再运行。");
      const app = deviceApps.find((item) => item.app_id === args.appId);
      if (!app) throw new Error("设备尚未安装此应用，请先安装当前项目。");
      runningAppId = app.app_id;
      return { app_id: app.app_id, running: true, simulated: true };
    }
    case "uninstall_app":
      deviceApps = deviceApps.filter((a) => a.app_id !== args.appId);
      if (runningAppId === args.appId) runningAppId = null;
      return { ok: true };
    case "project_install": {
      if (!devices.some((device) => device.id === args.deviceId)) throw new Error("设备不存在。");
      if (offline(args.deviceId)) throw new Error("设备离线，请连接后再安装。");
      return buildLuaPackage(projectSnapshot(id)).then(({ config }) => {
        const app = { app_id: config.app_id, version: config.version, display_name: config.display_name };
        deviceApps = [...deviceApps.filter((item) => item.app_id !== app.app_id), app];
        return { app, simulated: true };
      });
    }
    case "scan_devices":
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => resolve([{ id: "demo-ble", name: "GizOS-1F3A" }]), 5000);
        stopScan = () => {
          clearTimeout(timer);
          reject(new Error("已取消配对"));
        };
      });
    case "cancel_pairing":
      stopScan?.();
      return null;
    case "projects_list":
      return Object.keys(files).map((pid) => {
        const config = inspectProject(projectSnapshot(pid)).config;
        return {
          id: pid,
          name: config?.display_name ?? pid,
          appId: config?.app_id ?? null,
          version: config?.version ?? null,
          updatedAt: now - 120000,
          files: list(pid).length,
        };
      });
    case "project_create": {
      const appId = String(args.appId ?? "");
      const name = String(args.name ?? "").trim();
      if (!validAppId(appId)) throw new Error("程序 ID 需为 1–32 位小写字母、数字、下划线、点或连字符。");
      if (!name || byteLength(name) > 128) throw new Error("项目名称需为 1–128 字节。");
      const base = appId.replace(/[_.]/g, "-");
      let projectId = base;
      for (let index = 1; Object.hasOwn(files, projectId); index++) projectId = `${base}-${index}`;
      Object.defineProperty(files, projectId, {
        value: {
          "app.json":
            JSON.stringify({ app_id: appId, version: "0.1.0", display_name: name, description: "" }, null, 2) + "\n",
          [`${appId}.lua`]: `-- ${name}\n-- Runs on the device after it is installed and started.\n\nlocal message = "Hello from ${appId}"\nprint(message)\n\nreturn { message = message }\n`,
        },
        configurable: true,
        enumerable: true,
        writable: true,
      });
      return { id: projectId, name, appId, version: "0.1.0", updatedAt: Date.now(), files: 2 };
    }
    case "project_delete":
      if (!Object.hasOwn(files, id)) throw new Error("项目不存在。");
      delete files[id];
      delete chats[id];
      return null;
    case "project_files":
      return list(id);
    case "project_read":
      if (!Object.hasOwn(files, id) || !Object.hasOwn(files[id], String(args.path))) throw new Error("文件不存在。");
      return files[id][String(args.path)];
    case "project_write": {
      if (!Object.hasOwn(files, id)) throw new Error("项目不存在。");
      const path = String(args.path ?? "");
      const pathError = validatePath(path);
      if (pathError) throw new Error(pathError);
      const content = String(args.content ?? "");
      const existing = Object.keys(files[id]);
      const lower = path.toLowerCase();
      if (
        existing.some(
          (other) =>
            (other.toLowerCase() === lower && other !== path) ||
            other.toLowerCase().startsWith(`${lower}/`) ||
            lower.startsWith(`${other.toLowerCase()}/`),
        )
      )
        throw new Error("文件路径重复（不区分大小写）或与目录冲突。");
      if (!Object.hasOwn(files[id], path) && existing.length >= 65) throw new Error("每个项目最多 65 个文件。");
      if (
        existing.reduce((sum, key) => sum + (key === path ? 0 : byteLength(files[id][key])), byteLength(content)) >
        2 * 1024 * 1024
      )
        throw new Error("项目内容超过 2 MiB 限制。");
      Object.defineProperty(files[id], path, { value: content, configurable: true, enumerable: true, writable: true });
      return null;
    }
    case "project_remove_file": {
      const path = String(args.path ?? "");
      if (path === "app.json") throw new Error("app.json 不能删除。");
      if (!Object.hasOwn(files, id) || !Object.hasOwn(files[id], path)) throw new Error("文件不存在。");
      delete files[id][path];
      return null;
    }
    case "project_rename_file": {
      const from = String(args.from ?? "");
      const to = String(args.to ?? "");
      if (from === "app.json" || to === "app.json") throw new Error("app.json 不能重命名。");
      const content = demoCall("project_read", { id, path: from });
      if (Object.keys(files[id]).some((path) => path.toLowerCase() === to.toLowerCase()))
        throw new Error("目标文件已存在。");
      demoCall("project_write", { id, path: to, content });
      demoCall("project_remove_file", { id, path: from });
      return null;
    }
    case "project_check":
      return buildLuaPackage(projectSnapshot(id)).then(({ config, files, bytes }) => ({
        appId: config.app_id,
        version: config.version,
        files,
        bytes: bytes.length,
      }));
    case "agent_permissions_get":
      return { files: "read", run: "ask", web: "allow", ...permissions[id] };
    case "agent_permissions_set":
      permissions[id] = args.permissions as Record<string, string>;
      return null;
    case "search_settings_get":
      return { ...search };
    case "search_settings_save":
      search.provider = (args.provider as string | null) ?? null;
      if (!search.provider) search.hasKey = false;
      else if (args.apiKey) search.hasKey = true;
      return { ...search };
    case "home_list":
    case "home_read":
    case "home_write":
    case "web_fetch":
    case "web_search":
      throw new Error("演示模式不会访问你的文件或网络。");
    case "chat_load":
      return chats[id] ?? null;
    case "chat_save":
      chats[id] = args.json as string;
      return null;
    case "providers_list":
      return structuredClone(providers);
    case "provider_save": {
      const provider = structuredClone(args.provider as Provider);
      const index = providers.findIndex((item) => item.id === provider.id);
      provider.hasKey = args.apiKey == null ? (providers[index]?.hasKey ?? false) : Boolean(args.apiKey);
      if (index < 0) providers.push(provider);
      else providers[index] = provider;
      return structuredClone(provider);
    }
    case "desktop_preferences":
      return { keepInTray: true };
    default:
      throw new Error(`演示模式不支持：${command}`);
  }
}
