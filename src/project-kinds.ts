import type { ProjectSummary } from "./bridge";

export type ProjectKind = "lua" | "giz" | "rom";
export const projectKindOrder: ProjectKind[] = ["lua", "giz", "rom"];

export const projectKinds: Record<ProjectKind, { label: string; description: string; available: boolean }> = {
  lua: {
    label: "Lua 应用",
    description: "Lua 应用基于 GizOS Lua Runtime，可在浏览器、手机和 MCU 等环境中运行。",
    available: true,
  },
  giz: { label: "Giz 固件", description: "面向 GizOS 的固件项目，编译与 loader 安装流程尚未接入。", available: false },
  rom: { label: "原生固件", description: "使用 ESP-IDF 等工具链直接构建和烧录的固件，尚未接入。", available: false },
};

export function projectKindOf(project: ProjectSummary): ProjectKind {
  if ("kind" in project && (project.kind === "giz" || project.kind === "rom")) return project.kind;
  // The existing native project store contains Lua applications without a kind field.
  return "lua";
}
