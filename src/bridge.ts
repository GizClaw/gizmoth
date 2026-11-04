import type { AgentMessage } from "@mariozechner/pi-agent-core";
import { invoke, isTauri } from "@tauri-apps/api/core";

const demo = import.meta.env.DEV && !isTauri() && new URLSearchParams(location.search).has("demo");
/** True in the desktop app, and in the development-only `?demo=1` browser preview. */
export const native = isTauri() || demo;

export interface Device {
  id: string;
  name: string;
  baseUrl: string;
  icon?: string | null;
}
export interface Nearby {
  id: string;
  name: string;
}
export interface PackageInfo {
  appId: string;
  version: string;
  files: number;
  bytes: number;
}
export interface ProjectSummary {
  id: string;
  name: string;
  appId?: string | null;
  version?: string | null;
  updatedAt: number;
  files: number;
  directory: string;
  external: boolean;
  available: boolean;
}
export interface ChatEntry {
  seq: number;
  message: AgentMessage;
}
export interface ChatCheckpoint {
  through: number;
  summary: string;
}
export interface ChatState {
  epoch: string;
  total: number;
  checkpoint: ChatCheckpoint | null;
}
export interface ChatPage {
  epoch: string;
  total: number;
  entries: ChatEntry[];
}
export interface FileEntry {
  path: string;
  size: number;
}
export type ProviderApi = "openai-completions" | "openai-responses" | "anthropic-messages" | "google-generative-ai";
export interface ProviderModel {
  id: string;
  name: string;
  reasoning: boolean;
  images: boolean;
  contextWindow: number;
  maxTokens: number;
  /** Old provider files without a selection flag keep their current models enabled. */
  enabled?: boolean;
}
export interface Provider {
  id: string;
  kind: string;
  name: string;
  api: ProviderApi;
  baseUrl: string;
  models: ProviderModel[];
  hasKey: boolean;
  deviceId?: string | null;
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (demo) return (await import("./demo")).demoCall(command, args) as T;
  if (!native) throw new Error("请在 GizMoth 桌面应用中使用此功能。");
  return invoke<T>(command, args);
}

export const bridge = {
  devices: () => call<Device[]>("list_devices"),
  importDevice: (url: string) => call<Device>("import_device", { url }),
  removeDevice: (id: string) => call<void>("remove_device", { id }),
  scan: () => call<Nearby[]>("scan_devices"),
  bind: (id: string) => call<Device>("bind_device", { id }),
  cancelPairing: () => call<void>("cancel_pairing"),
  overview: (deviceId: string) => call<unknown>("device_overview", { deviceId }),
  setLevel: (deviceId: string, instance: { id: string; hwd: string }, level: number) =>
    call<unknown>("set_device_level", { deviceId, instance, level }),
  apps: (deviceId: string) => call<unknown>("list_apps", { deviceId }),
  run: (deviceId: string, appId: string) => call<unknown>("run_app", { deviceId, appId }),
  uninstall: (deviceId: string, appId: string) => call<unknown>("uninstall_app", { deviceId, appId }),

  projects: () => call<ProjectSummary[]>("projects_list"),
  createProject: (appId: string, name: string, parentDir?: string) =>
    call<ProjectSummary>("project_create", { appId, name, parentDir: parentDir ?? null }),
  chooseProjectDirectory: () => call<string | null>("project_choose_directory"),
  openProject: () => call<ProjectSummary | null>("project_open"),
  relocateProject: (id: string) => call<ProjectSummary | null>("project_relocate", { id }),
  revealProject: (id: string) => call<void>("project_reveal", { id }),
  deleteProject: (id: string) => call<void>("project_delete", { id }),
  files: (id: string) => call<FileEntry[]>("project_files", { id }),
  read: (id: string, path: string) => call<string>("project_read", { id, path }),
  write: (id: string, path: string, content: string) => call<void>("project_write", { id, path, content }),
  removeFile: (id: string, path: string) => call<void>("project_remove_file", { id, path }),
  renameFile: (id: string, from: string, to: string) => call<void>("project_rename_file", { id, from, to }),
  projectDir: (id: string) => call<string>("project_dir", { id }),
  check: (id: string) => call<PackageInfo>("project_check", { id }),
  exportPackage: (id: string) => call<string | null>("project_export", { id }),
  install: (id: string, deviceId: string) => call<unknown>("project_install", { id, deviceId }),
  chatState: (id: string) => call<ChatState>("chat_state", { id }),
  chatPage: (id: string, options: { before?: number; after?: number; limit?: number } = {}) =>
    call<ChatPage>("chat_page", {
      id,
      before: options.before ?? null,
      after: options.after ?? null,
      limit: options.limit ?? 40,
    }),
  chatAppend: (id: string, epoch: string, expected: number, messages: AgentMessage[]) =>
    call<ChatState>("chat_append", { id, epoch, expected, messages }),
  chatCheckpoint: (id: string, epoch: string, value: ChatCheckpoint) =>
    call<ChatState>("chat_checkpoint", { id, epoch, value }),
  chatReset: (id: string, epoch: string) => call<ChatState>("chat_reset", { id, epoch }),

  providers: () => call<Provider[]>("providers_list"),
  saveProvider: (provider: Provider, apiKey?: string) =>
    call<Provider>("provider_save", { provider, apiKey: apiKey ?? null }),
  removeProvider: (id: string) => call<void>("provider_remove", { id }),
  providerKey: (id: string) => call<string | null>("provider_key", { id }),
  deviceProviderModels: (deviceId: string) => call<unknown>("device_provider_models", { deviceId }),

  preferences: () => call<{ keepInTray: boolean }>("desktop_preferences"),
  setPreferences: (keepInTray: boolean) => call<void>("set_desktop_preferences", { keepInTray }),
};

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return "操作失败，请重试。";
}
