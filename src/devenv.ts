import { call, native } from "./bridge";

/** Why an entry cannot be installed on this computer; mirrors the native `Blocked`. */
export type DevEnvBlocked =
  | { code: "planned" }
  | { code: "unsupported-host"; detail: string[] }
  | { code: "unsupported-operation"; detail: string }
  | { code: "missing-program"; detail: string }
  | { code: "invalid"; detail: string };

export type EntryStatus = "planned" | "unsupported" | "available" | "installing" | "installed";

export interface DevEnvPackage {
  kind: "sdk" | "toolchain";
  id: string;
  name: string;
  version: string;
  installed: boolean;
}
export interface DevEnvChip {
  id: string;
  name: string;
  vendor: string;
  description: string;
  homepage: string | null;
  sdk: string;
  notes: string | null;
  status: EntryStatus;
  blocked: DevEnvBlocked | null;
  reason: string | null;
  version: string | null;
  packages: DevEnvPackage[];
  installationId: string | null;
  path: string | null;
  reinstallable: boolean;
}
export type ToolchainCategory = "compiler" | "build-tool" | "post-build" | "debugger" | "utility";
export interface DevEnvToolchain {
  id: string;
  name: string;
  description: string;
  category: ToolchainCategory;
  homepage: string;
  notes: string | null;
  status: EntryStatus;
  blocked: DevEnvBlocked | null;
  reason: string | null;
  version: string | null;
}
export interface DevEnvInstallation {
  installationId: string;
  kind: "sdk" | "toolchain";
  id: string;
  name: string;
  version: string;
  category: ToolchainCategory | null;
  origin: "managed" | "local";
  requestedByUser: boolean;
  path: string;
  chips: string[];
  healthy: boolean;
  reinstallable: boolean;
}
export type JobPhase = "resolving" | "downloading" | "installing" | "checking" | "committing" | "cleaning" | "done";
export interface DevEnvJob {
  id: string;
  kind: "chip" | "toolchain";
  target: string;
  name: string;
  phase: JobPhase;
  state: "running" | "succeeded" | "failed" | "cancelled";
  received: number;
  total: number;
  log: string[];
  error: string | null;
  startedAt: number;
  finishedAt: number | null;
}
export interface DevEnvState {
  host: string | null;
  registry: { repository: string; commit: string; fetchedAt: number } | null;
  error: string | null;
  chips: DevEnvChip[];
  toolchains: DevEnvToolchain[];
  installations: DevEnvInstallation[];
  job: DevEnvJob | null;
}
export type RemovalTarget = { kind: "chip"; id: string } | { kind: "installation"; id: string };
export interface RemovalPreview {
  action: "delete" | "unregister" | "blocked";
  delete: DevEnvInstallation[];
  keep: DevEnvInstallation[];
  chips: string[];
  dependants: DevEnvInstallation[];
  /** Installation IDs to send back on confirmation; the backend refuses a changed plan. */
  confirm: string[];
}

export const devenv = {
  state: () =>
    native
      ? call<DevEnvState>("devenv_state")
      : import("./devenv-browser").then(({ browserCatalog }) => browserCatalog.state()),
  refresh: () =>
    native
      ? call<DevEnvState>("devenv_refresh")
      : import("./devenv-browser").then(({ browserCatalog }) => browserCatalog.refresh()),
  install: (kind: "chip" | "toolchain", id: string) => call<DevEnvState>("devenv_install", { target: { kind, id } }),
  cancel: (jobId: string) => call<DevEnvState>("devenv_cancel", { jobId }),
  openFolder: (installationId: string) => call<void>("devenv_open_folder", { installationId }),
  reinstall: (target: RemovalTarget) => call<DevEnvState>("devenv_reinstall", { target }),
  removePreview: (target: RemovalTarget) => call<RemovalPreview>("devenv_remove_preview", { target }),
  remove: (target: RemovalTarget, confirm: string[]) => call<DevEnvState>("devenv_remove", { target, confirm }),
  addLocal: (toolchain: string, version: string) =>
    call<DevEnvState | null>("devenv_add_local", { toolchain, version }),
};

export const hostLabels: Record<string, string> = {
  "macos-arm64": "macOS Apple Silicon",
  "macos-x86_64": "macOS Intel",
  "linux-arm64": "Linux arm64",
  "linux-x86_64": "Linux x86_64",
  "windows-arm64": "Windows arm64",
  "windows-x86_64": "Windows x64",
};

export function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
