import { invoke, isTauri } from "@tauri-apps/api/core";

export const demo = import.meta.env.DEV && !isTauri() && new URLSearchParams(location.search).has("demo");
/** True in the desktop app, and in the development-only `?demo=1` browser preview. */
export const native = isTauri() || demo;

export interface Device {
  id: string;
  name: string;
  baseUrl: string;
  icon?: string | null;
}
export interface DeviceKeyStatus {
  hasKey: boolean;
  needsMigration: boolean;
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
  /** Reasoning efforts the provider advertised for this model, lowest first. */
  reasoningLevels?: string[];
  images: boolean;
  contextWindow: number;
  maxTokens: number;
}
/** What the assistant may do beyond its project; enforced by the native layer. */
export type FileAccess = "none" | "read" | "write";
export type RunAccess = "ask" | "allow" | "deny";
export type WebAccess = "allow" | "deny";
export interface AgentPermissions {
  files: FileAccess;
  run: RunAccess;
  web: WebAccess;
}
export const defaultPermissions: AgentPermissions = { files: "read", run: "ask", web: "allow" };
export interface HomeEntry {
  name: string;
  kind: "dir" | "file" | "link";
  size: number;
}
export interface FetchedPage {
  status: number;
  url: string;
  contentType: string;
  location: string | null;
  body: string;
  truncated: boolean;
}
export interface SearchResult {
  title: string;
  url: string;
  snippet: string;
}
export type SearchProvider = "brave" | "tavily";
export interface SearchSettings {
  provider: SearchProvider | null;
  hasKey: boolean;
}
export interface Provider {
  id: string;
  kind: string;
  name: string;
  api: ProviderApi;
  baseUrl: string;
  models: ProviderModel[];
  hasKey: boolean;
}

export async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (demo) return (await import("./demo")).demoCall(command, args) as T;
  if (!native) throw new Error("请在 GizMoth 桌面应用中使用此功能。");
  return invoke<T>(command, args);
}

export const bridge = {
  devices: () => call<Device[]>("list_devices"),
  importDevice: (url: string) => call<Device>("import_device", { url }),
  removeDevice: (id: string) => call<void>("remove_device", { id }),
  renameDevice: (id: string, name: string) => call<Device>("rename_device", { id, name }),
  deviceKeyStatus: (id: string) => call<DeviceKeyStatus>("device_key_status", { id }),
  deviceKey: (id: string) => call<string | null>("device_key", { id }),
  saveDeviceKey: (id: string, apiKey: string) => call<void>("device_save_key", { id, apiKey }),
  migrateDeviceKey: (id: string) => call<{ legacyRemoved: boolean }>("device_migrate_key", { id }),
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
  createProject: (appId: string, name: string) => call<ProjectSummary>("project_create", { appId, name }),
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
  chatLoad: (id: string) => call<string | null>("chat_load", { id }),
  chatSave: (id: string, json: string) => call<void>("chat_save", { id, json }),
  agentPermissions: (id: string) => call<AgentPermissions>("agent_permissions_get", { id }),
  setAgentPermissions: (id: string, permissions: AgentPermissions) =>
    call<void>("agent_permissions_set", { id, permissions }),
  homeList: (id: string, path: string) => call<HomeEntry[]>("home_list", { id, path }),
  homeRead: (id: string, path: string) => call<string>("home_read", { id, path }),
  homeWrite: (id: string, path: string, content: string) => call<void>("home_write", { id, path, content }),
  webFetch: (id: string, url: string) => call<FetchedPage>("web_fetch", { id, url }),
  webSearch: (id: string, query: string) => call<SearchResult[]>("web_search", { id, query }),
  searchSettings: () => call<SearchSettings>("search_settings_get"),
  saveSearchSettings: (provider: SearchProvider | null, apiKey?: string) =>
    call<SearchSettings>("search_settings_save", { provider, apiKey: apiKey ?? null }),

  providers: () => call<Provider[]>("providers_list"),
  saveProvider: (provider: Provider, apiKey?: string) =>
    call<Provider>("provider_save", { provider, apiKey: apiKey ?? null }),
  removeProvider: (id: string) => call<void>("provider_remove", { id }),
  providerKey: (id: string) => call<string | null>("provider_key", { id }),

  preferences: () => call<{ keepInTray: boolean }>("desktop_preferences"),
  setPreferences: (keepInTray: boolean) => call<void>("set_desktop_preferences", { keepInTray }),
};

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return "操作失败，请重试。";
}
