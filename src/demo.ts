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
  ]),
};
const providers = [
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
const devices = [{ id: "demo-1", name: "客厅的设备", baseUrl: "https://gizclaw.example", icon: null }];

export function demoCall(command: string, args: Record<string, unknown> = {}): unknown {
  const id = args.id as string;
  const list = (pid: string) =>
    Object.entries(files[pid] ?? {}).map(([path, c]) => ({ path, size: new TextEncoder().encode(c).length }));
  switch (command) {
    case "list_devices":
      return devices;
    case "device_overview":
      return {
        online: true,
        lastSeen: null,
        firmwareVersion: "1.0.6",
        errors: [],
        hardware: [
          { instance: { id: "speaker.main", hwd: "speaker" }, value: { volume_percent: 45 } },
          { instance: { id: "display.main", hwd: "display" }, value: { brightness_percent: 70 } },
          { instance: { id: "led.main", hwd: "led" }, value: { brightness_percent: 80 } },
          { instance: { id: "battery.main", hwd: "battery" }, value: { percent: 86, charging: false } },
          { instance: { id: "wifi.main", hwd: "wifi" }, value: { connected: true, ssid: "Home" } },
        ],
      };
    case "list_apps":
      return { apps: [{ app_id: "breathing_light", version: "0.1.0", display_name: "呼吸灯" }] };
    case "projects_list":
      return Object.keys(files).map((pid) => ({
        id: pid,
        name: "呼吸灯",
        appId: "breathing_light",
        version: "0.1.0",
        updatedAt: now - 120000,
        files: list(pid).length,
      }));
    case "project_files":
      return list(id);
    case "project_read":
      return files[id]?.[args.path as string] ?? "";
    case "project_write":
      files[id][args.path as string] = args.content as string;
      return null;
    case "project_check":
      return { appId: "breathing_light", version: "0.1.0", files: 2, bytes: 412 };
    case "chat_load":
      return chats[id] ?? null;
    case "chat_save":
      chats[id] = args.json as string;
      return null;
    case "providers_list":
      return providers;
    case "desktop_preferences":
      return { keepInTray: true };
    default:
      throw new Error(`演示模式不支持：${command}`);
  }
}
