import type { AgentMessage } from "@mariozechner/pi-agent-core";
import type { ChatCheckpoint, ChatEntry, ChatState, ProjectSummary, Provider } from "./bridge";

// Development-only, in-memory stand-in for the native commands, enabled with
// `?demo=1` in a browser preview. Nothing here touches real devices or keys.
const now = Date.now();
const files: Record<string, Record<string, string>> = {
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
const chats: Record<string, AgentMessage[]> = {
  "breathing-light": [
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
          text: "已写好 breathing_light.lua：亮度每步变化 10，到边界反向。检查通过，可以点「推送并运行」装到设备上。",
        },
      ],
      stopReason: "stop",
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
  ] as AgentMessage[],
};
if (new URLSearchParams(location.search).has("longchat")) {
  chats["breathing-light"] = Array.from({ length: 180 }, (_, i): AgentMessage[] => [
    {
      role: "user",
      content: `第 ${i + 1} 轮：把呼吸灯亮度步长改为 ${(i % 10) + 1}。`,
      timestamp: now - (180 - i) * 60000,
    },
    {
      role: "assistant",
      content: [{ type: "text", text: `第 ${i + 1} 轮已完成，入口文件保留在 breathing_light.lua。` }],
      stopReason: "stop",
      timestamp: now - (180 - i) * 60000,
    } as AgentMessage,
  ]).flat();
}
const chatStates: Record<string, ChatState> = {};
if (new URLSearchParams(location.search).has("longchat")) {
  chatStates["breathing-light"] = {
    epoch: crypto.randomUUID(),
    total: chats["breathing-light"].length,
    checkpoint: {
      through: 200,
      summary:
        "项目是 breathing_light，入口为 breathing_light.lua。用户持续调整呼吸灯的步长；保留亮度在 0 到 100 之间往返变化，推送前检查程序包。以上是前 100 轮对话的演示摘要，完整消息仍可分页查看。",
    },
  };
}
function chatState(id: string): ChatState {
  const state = (chatStates[id] ??= { epoch: crypto.randomUUID(), total: 0, checkpoint: null });
  state.total = (chats[id] ?? []).length;
  return structuredClone(state);
}
const providers: Provider[] = [
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
let deviceApps: { app_id: string; version: string; display_name?: string }[] = [
  { app_id: "breathing_light", version: "0.1.0", display_name: "呼吸灯" },
  { app_id: "snake", version: "1.0.0", display_name: "贪吃小蛇" },
  { app_id: "whack_a_mole", version: "2.1.0", display_name: "Whack-a-Mole" },
  { app_id: "walkie_talkie", version: "0.1.0", display_name: "蓝牙对讲机" },
  { app_id: "catnest", version: "0.1.0" },
  { app_id: "boxbound", version: "0.1.0" },
];
let stopScan: (() => void) | undefined;
const devices = [{ id: "demo-1", name: "客厅的设备", baseUrl: "https://gizclaw.example", icon: null }];
const projectDirectories: Record<string, string> = {
  "breathing-light": "/Users/demo/Documents/GizMoth/breathing_light",
};
const projectSummary = (id: string): ProjectSummary => {
  const meta = JSON.parse(files[id]["app.json"]);
  return {
    id,
    name: meta.display_name,
    appId: meta.app_id,
    version: meta.version,
    updatedAt: now,
    files: Object.keys(files[id]).length,
    directory: projectDirectories[id],
    external: true,
    available: true,
  };
};

export function demoCall(command: string, args: Record<string, unknown> = {}): unknown {
  const id = args.id as string;
  const list = (pid: string) =>
    Object.entries(files[pid] ?? {}).map(([path, c]) => ({ path, size: new TextEncoder().encode(c).length }));
  switch (command) {
    case "list_devices":
      return devices;
    case "device_overview": {
      // `?demo=1&offline=1` previews the offline layout.
      if (new URLSearchParams(location.search).has("offline"))
        return {
          online: false,
          lastSeen: new Date(now - 31 * 60000).toISOString(),
          firmwareVersion: "0.11.7-prod.2+tiga-esp-v4-2",
          errors: [],
          hardware: ["display", "led", "speaker", "battery", "modem", "wifi"].map((hwd) => ({
            instance: { id: `${hwd}.main`, hwd },
            error: "设备离线",
          })),
        };
      return {
        online: true,
        lastSeen: null,
        firmwareVersion: "0.11.7-prod.2+tiga-esp-v4-2",
        errors: [],
        hardware: [
          { instance: { id: "speaker.main", hwd: "speaker" }, value: { volume_percent: 45 } },
          { instance: { id: "display.main", hwd: "display" }, value: { brightness_percent: 70 } },
          { instance: { id: "led.main", hwd: "led" }, value: { brightness_percent: 80 } },
          { instance: { id: "battery.main", hwd: "battery" }, value: { percent: 86, charging: false } },
          { instance: { id: "wifi.main", hwd: "wifi" }, value: { connected: true, ssid: "Home" } },
        ],
      };
    }
    case "list_apps":
      return { apps: deviceApps };
    case "uninstall_app":
      deviceApps = deviceApps.filter((a) => a.app_id !== args.appId);
      return { ok: true };
    case "project_install":
      if (!deviceApps.some((a) => a.app_id === "breathing_light"))
        deviceApps.push({ app_id: "breathing_light", version: "0.1.0", display_name: "呼吸灯" });
      return { app: { app_id: "breathing_light" } };
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
      return Object.keys(projectDirectories).map(projectSummary);
    case "project_choose_directory":
      return "/Users/demo/Projects";
    case "project_create": {
      const id = `project-${Date.now()}`;
      const appId = args.appId as string;
      projectDirectories[id] = `${args.parentDir || "/Users/demo/Documents/GizMoth"}/${appId}`;
      files[id] = {
        "app.json": JSON.stringify({ app_id: appId, display_name: args.name, version: "0.1.0" }),
        [`${appId}.lua`]: '-- New Lua project\nprint("Hello")\n',
      };
      return projectSummary(id);
    }
    case "project_open": {
      const id = "opened-project";
      files[id] ??= {
        "app.json": JSON.stringify({ app_id: "desk_clock", display_name: "桌面时钟", version: "0.1.0" }),
        "desk_clock.lua": 'print("12:00")\n',
      };
      projectDirectories[id] = "/Users/demo/Projects/desk_clock";
      return projectSummary(id);
    }
    case "project_relocate":
      projectDirectories[id] = `/Users/demo/Projects/${id}`;
      return projectSummary(id);
    case "project_reveal":
      return null;
    case "project_dir":
      return projectDirectories[id];
    case "project_delete":
      delete projectDirectories[id];
      delete chats[id];
      return null;
    case "project_files":
      return list(id);
    case "project_read":
      return files[id]?.[args.path as string] ?? "";
    case "project_write":
      files[id][args.path as string] = args.content as string;
      return null;
    case "project_check":
      return { appId: "breathing_light", version: "0.1.0", files: 2, bytes: 412 };
    case "chat_state":
      return chatState(id);
    case "chat_page": {
      const state = chatState(id);
      const records = (chats[id] ?? []).map((message, i): ChatEntry => ({ seq: i + 1, message }));
      const limit = Math.min(100, Number(args.limit) || 40);
      const entries =
        args.after != null
          ? records.filter((entry) => entry.seq > Number(args.after)).slice(0, limit)
          : records.filter((entry) => args.before == null || entry.seq < Number(args.before)).slice(-limit);
      return { epoch: state.epoch, total: state.total, entries: structuredClone(entries) };
    }
    case "chat_append": {
      const state = chatState(id);
      if (args.epoch !== state.epoch || args.expected !== state.total) throw new Error("对话写入冲突");
      chats[id] ??= [];
      chats[id].push(...structuredClone(args.messages as AgentMessage[]));
      return chatState(id);
    }
    case "chat_checkpoint": {
      const state = chatState(id);
      if (args.epoch !== state.epoch) throw new Error("对话已清空");
      chatStates[id].checkpoint = structuredClone(args.value as ChatCheckpoint);
      return chatState(id);
    }
    case "chat_reset":
      if (args.epoch !== chatState(id).epoch) throw new Error("对话已清空");
      chats[id] = [];
      chatStates[id] = { epoch: crypto.randomUUID(), total: 0, checkpoint: null };
      return chatState(id);
    case "providers_list":
      return structuredClone(providers);
    case "provider_save": {
      const next = structuredClone(args.provider as Provider);
      next.hasKey = next.kind === "device" || !!args.apiKey || providers.find((p) => p.id === next.id)?.hasKey || false;
      const index = providers.findIndex((p) => p.id === next.id);
      if (index >= 0) providers[index] = next;
      else providers.push(next);
      return next;
    }
    case "provider_remove": {
      const index = providers.findIndex((p) => p.id === id);
      if (index >= 0) providers.splice(index, 1);
      return null;
    }
    case "provider_key":
      return null;
    case "device_provider_models":
      return {
        data: [
          { id: "chat/fast", display_name: "Fast Chat", context_window: 128000, max_output_tokens: 8192 },
          { id: "chat/coding", display_name: "Coding", context_window: 200000, max_output_tokens: 16384 },
          { id: "chat/reasoning", display_name: "Reasoning", reasoning: true },
        ],
      };
    case "desktop_preferences":
      return { keepInTray: true };
    default:
      throw new Error(`演示模式不支持：${command}`);
  }
}
