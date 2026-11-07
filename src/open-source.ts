export const gizmothRepository = "https://github.com/GizClaw/gizmoth";

/** The three projects selected for the Open Source page, in display order. */
export const openSourceProjects = [
  {
    name: "GizOS",
    repository: "gizos",
    description: "面向嵌入式设备、桌面、移动端和 WebAssembly 的可移植固件与应用平台。",
  },
  {
    name: "GizClaw",
    repository: "gizclaw",
    description: "连接设备与智能体的运行时和边缘服务网络，包含 Go 服务端、CLI、SDK 和应用。",
  },
  { name: "GizMoth", repository: "gizmoth", description: "配对和控制 GizOS 设备，用内置编程助手开发 Lua 应用。" },
] as const;

export function projectRepository(repository: (typeof openSourceProjects)[number]["repository"]): string {
  return `https://github.com/GizClaw/${repository}`;
}
