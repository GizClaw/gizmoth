import { useEffect, useSyncExternalStore } from "react";
import { call, errorMessage, native } from "./bridge";

/** Client of the resident service's compile API (src-tauri/src/compile). */
export type CompileJobStatus =
  "queued" | "booting" | "syncing" | "running" | "collecting" | "succeeded" | "failed" | "cancelled";
export type CompileSlotState = "stopped" | "starting" | "ready" | "busy" | "stopping" | "failed";
export type CompileImageStatus = "missing" | "building" | "ready" | "failed";

export interface CompileExit {
  code: number | null;
  signal: number | null;
  timedOut: boolean;
  cancelled: boolean;
}
export interface CompileSync {
  entries: number;
  files: number;
  bytes: number;
  removed: number;
}
/** Mirrors jobs::Job::summary. */
export interface CompileJob {
  id: string;
  client: string;
  title: string;
  workspace: string;
  command: string[];
  cwd: string;
  artifacts: string[];
  status: CompileJobStatus;
  slot: string | null;
  position: number | null;
  createdAt: number;
  startedAt: number | null;
  finishedAt: number | null;
  exit: CompileExit | null;
  error: string | null;
  artifact: string | null;
  synced: CompileSync | null;
  missing: string[];
  logBytes: number;
}
export interface CompileSlot {
  id: string;
  name: string;
  cpus: number;
  memoryMiB: number;
  diskGiB: number;
  state: CompileSlotState;
  error: string | null;
  job: { id: string; client: string; title: string; status: CompileJobStatus; startedAt: number | null } | null;
  startedAt: number | null;
  lastUsed: number | null;
  disk: { allocated: number; size: number; used: number | null; free: number | null };
}
export interface CompileCache {
  name: string;
  kind: "bazel" | "ccache" | "custom";
  maxBytes: number;
  env: string | null;
  bytes: number;
  files: number;
  hits: number;
  misses: number;
  uploads: number;
}
export interface CompileImage {
  status: CompileImageStatus;
  error: string | null;
  phase: string | null;
  progress: { done: number; total: number } | null;
  info: {
    specHash?: string;
    builtAt?: number;
    packages?: string[];
    aptMirror?: string | null;
    base?: string;
    kernel?: string;
    bazelisk?: string;
    agentVersion?: number;
  } | null;
  /** The image was built from the current package settings. */
  current: boolean;
  allocated: number;
  logBytes: number;
  base: string;
  kernel: string;
  bazelisk: string;
  /** The image's fixed apt packages. */
  packages: string[];
}
export interface CustomCacheSettings {
  name: string;
  env: string;
  maxGib: number;
}
export interface CacheSettings {
  bazel: { maxGib: number };
  ccache: { maxGib: number; localGib: number };
  custom: CustomCacheSettings[];
}
/** Mirrors compile::Settings (serde camelCase). */
export interface CompileSettings {
  slots: { id: string; name: string; cpus: number; memoryMib: number; diskGib: number }[];
  /** The package list is fixed; the mirror is only a download source. */
  image: { aptMirror: string | null };
  caches: CacheSettings;
  network: boolean;
  idleMinutes: number;
  bazelrc: string[];
  env: { name: string; value: string }[];
  excludes: string[];
  artifacts: { keepLast: number; maxGib: number };
}
export type SettingsPatch = Partial<Omit<CompileSettings, "slots">>;
export interface StorageItem {
  id: "image" | "slots" | "caches" | "artifacts" | "packages" | "assets";
  path: string;
  /** Allocated bytes: sparse disks count only what they have written. */
  bytes: number;
  exists: boolean;
}
/** GizMoth's working directory: system image, slots, caches and artifacts. */
export interface CompileStorage {
  root: string;
  /** `<app data>/compile`, used when no other folder is chosen. */
  default: string;
  isDefault: boolean;
  /** False while a chosen folder's disk is not connected. */
  available: boolean;
  free: number | null;
  /** Why the last move failed. */
  error: string | null;
  moving: {
    from: string;
    to: string;
    phase: "stopping" | "moving";
    done: number;
    total: number;
    startedAt: number;
  } | null;
  /** Folders below the working directory, one per kind of data. */
  items: StorageItem[];
}
export interface CompileSnapshot {
  protocol: string;
  version: number;
  available: { ok: boolean; message?: string };
  host: { cpus: number; memoryMiB: number };
  settings: CompileSettings;
  image: CompileImage;
  slots: CompileSlot[];
  queue: CompileJob[];
  jobs: CompileJob[];
  caches: CompileCache[];
  artifacts: { count: number; bytes: number; dir: string };
  storage: CompileStorage;
  packages: CompilePackages;
}
export type PackageState =
  "pending" | "downloading" | "downloaded" | "packing" | "installed" | "failed" | "unsupported";
/** One linux-arm64 release of the GizClaw/gizmoth-devenv registry. */
export interface CompilePackage {
  kind: "toolchain" | "sdk";
  id: string;
  name: string;
  version: string;
  /** On PATH in builds. */
  default: boolean;
  download: number;
  /** Where builds find it. */
  path: string;
  state: PackageState;
  done?: number;
  total?: number;
  error?: string;
}
/** Every registry package is downloaded and packed for the VMs automatically. */
export interface CompilePackages {
  active: boolean;
  busy: boolean;
  registry: {
    repository: string;
    commit: string | null;
    fetchedAt: number | null;
    refreshing: boolean;
    error: string | null;
  };
  platform: string;
  guestPath: string;
  /** The packed read-only disk in the working directory. */
  file: string;
  items: CompilePackage[];
  pack:
    | { state: "none" }
    | { state: "packing"; phase: string | null }
    | { state: "ready"; bytes: number; builtAt: number }
    | { state: "failed"; error: string }
    | { state: "stale"; bytes: number | null };
}
export interface ArtifactFile {
  path: string;
  size: number;
  sha256: string;
  absolutePath: string;
}
export interface CompileArtifact {
  id: string;
  jobId: string;
  client: string;
  title: string;
  workspace: string;
  command: string[];
  createdAt: number;
  pinned: boolean;
  files: ArtifactFile[];
  bytes: number;
  dir: string;
}
export interface LogChunk {
  text: string;
  next: number;
  truncated: boolean;
}
export interface SlotInput {
  name?: string;
  cpus?: number;
  memoryMiB?: number;
  diskGiB?: number;
}

const segment = (value: string) => encodeURIComponent(value);
const request = <T>(method: "GET" | "POST" | "PATCH" | "DELETE", path: string, body?: unknown) =>
  call<T>("compile_request", { method, path, body: body ?? null });
const logPath = (base: string, after?: number, limit?: number) => {
  const query = new URLSearchParams();
  if (after !== undefined) query.set("after", String(after));
  if (limit !== undefined) query.set("limit", String(limit));
  const text = query.toString();
  return text ? `${base}?${text}` : base;
};

/** Mutations that return the full snapshot publish it to every subscriber. */
async function publish(operation: Promise<CompileSnapshot>) {
  const next = await operation;
  setCompileSnapshot(next);
  return next;
}
/** Mutations with another result refresh the shared snapshot afterwards. */
async function thenRefresh<T>(operation: Promise<T>) {
  const result = await operation;
  void refreshCompile().catch(() => {});
  return result;
}

export const compile = {
  snapshot: () => request<CompileSnapshot>("GET", "/v1/compile"),
  watch: (since: number) => call<CompileSnapshot>("compile_watch", { since }),
  patchSettings: (patch: SettingsPatch) => publish(request("PATCH", "/v1/compile/settings", patch)),
  addSlot: (slot: SlotInput) => publish(request("POST", "/v1/compile/slots", slot)),
  patchSlot: (id: string, patch: SlotInput) => publish(request("PATCH", `/v1/compile/slots/${segment(id)}`, patch)),
  removeSlot: (id: string) => publish(request("DELETE", `/v1/compile/slots/${segment(id)}`)),
  stopSlot: (id: string) => publish(request("POST", `/v1/compile/slots/${segment(id)}/stop`)),
  resetSlot: (id: string) => publish(request("POST", `/v1/compile/slots/${segment(id)}/reset`)),
  buildImage: () => publish(request("POST", "/v1/compile/image/build")),
  cancelImage: () => thenRefresh(request<{ cancelling: boolean }>("POST", "/v1/compile/image/cancel")),
  imageLog: (after?: number) => request<LogChunk>("GET", logPath("/v1/compile/image/log", after)),
  clearCache: (name: string) => publish(request("DELETE", `/v1/compile/caches/${segment(name)}`)),
  artifacts: () => request<CompileArtifact[]>("GET", "/v1/compile/artifacts"),
  pinArtifact: (id: string, pinned: boolean) =>
    thenRefresh(request<CompileArtifact>("PATCH", `/v1/compile/artifacts/${segment(id)}`, { pinned })),
  deleteArtifact: (id: string) =>
    thenRefresh(request<{ deleted: string }>("DELETE", `/v1/compile/artifacts/${segment(id)}`)),
  jobLog: (id: string, after?: number, limit?: number) =>
    request<LogChunk>("GET", logPath(`/v1/compile/jobs/${segment(id)}/log`, after, limit)),
  cancelJob: (id: string) => thenRefresh(request<CompileJob>("POST", `/v1/compile/jobs/${segment(id)}/cancel`)),
  reveal: (path: string) => call<void>("compile_reveal", { path }),
  chooseDirectory: (current?: string) => call<string | null>("compile_choose_directory", { current: current ?? null }),
  /** Moves the working directory; `null` returns to the default location. */
  moveStorage: (path: string | null) =>
    publish(request("POST", "/v1/compile/storage", path === null ? { default: true } : { path })),
  cancelStorageMove: () => publish(request("POST", "/v1/compile/storage/cancel")),
  /** Starts downloading the image and packages before the first build. */
  prepare: () => publish(request("POST", "/v1/compile/prepare")),
  /** Checks the registry now and retries anything that failed. */
  refreshPackages: () => publish(request("POST", "/v1/compile/packages/refresh")),
};

/** Second-level sidebar entries of 编译; its settings live in Settings › 编译. */
export type CompileView = "builds" | "caches" | "artifacts";
export const compileViews: readonly { id: CompileView; label: string }[] = [
  { id: "builds", label: "编译任务" },
  { id: "caches", label: "缓存" },
  { id: "artifacts", label: "产物" },
];

export const activeStatuses: readonly CompileJobStatus[] = ["queued", "booting", "syncing", "running", "collecting"];
export const isActive = (status: CompileJobStatus) => activeStatuses.includes(status);

/** Removes terminal escape sequences (the agent dims its own notes). */
export function stripAnsi(text: string) {
  return text.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "").replace(/\r(?!\n)/g, "\n");
}
export function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(bytes < 10 * 1024 ** 3 ? 2 : 1)} GB`;
}
export function formatDuration(ms: number) {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${String(seconds % 60).padStart(2, "0")}s`;
  return `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m`;
}
/** Valid Debian package names and version pins, as compile::Settings::validate accepts. */
export const validPackage = (name: string) => /^[a-z0-9][a-z0-9.+\-:=]{0,99}$/.test(name);
export const validCacheName = (name: string) => /^[a-z0-9-]{1,32}$/.test(name);
export const validEnvName = (name: string) => /^[A-Za-z_][A-Za-z0-9_]{0,127}$/.test(name);

// One long-poll loop shared by the page and the sidebar count.
let snapshot: CompileSnapshot | null = null;
let failure = "";
const listeners = new Set<() => void>();
let demand = 0;
let loop = 0;

function emit() {
  for (const listener of listeners) listener();
}
export function setCompileSnapshot(next: CompileSnapshot) {
  // Long-poll results are authoritative; mutation replies never step backwards.
  if (snapshot && next.version < snapshot.version) return;
  snapshot = next;
  failure = "";
  emit();
}
export async function refreshCompile() {
  const next = await compile.snapshot();
  setCompileSnapshot(next);
  return next;
}
async function run(id: number) {
  while (id === loop) {
    try {
      const next = snapshot ? await compile.watch(snapshot.version) : await compile.snapshot();
      if (id !== loop) return;
      // A restarted service starts its version again; accept the watch result.
      snapshot = next;
      failure = "";
      emit();
    } catch (e) {
      if (id !== loop) return;
      failure = errorMessage(e);
      emit();
      await new Promise((resolve) => setTimeout(resolve, 3000));
    }
  }
}
function acquire() {
  demand++;
  if (demand === 1 && native) void run(++loop);
}
function release() {
  demand = Math.max(0, demand - 1);
  if (!demand) loop++;
}
function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}
const read = () => snapshot;
const readFailure = () => failure;
const readActive = () => (snapshot ? snapshot.queue.length + snapshot.slots.filter((slot) => slot.job).length : 0);
const readArtifacts = () => snapshot?.artifacts.count ?? 0;

/** The Compiling page keeps the snapshot live; mounting it may start the service. */
export function useCompileSnapshot() {
  useEffect(() => {
    acquire();
    return release;
  }, []);
  const value = useSyncExternalStore(subscribe, read, read);
  const error = useSyncExternalStore(subscribe, readFailure, readFailure);
  return { snapshot: value, error };
}

/** Sidebar counts follow a service that is already running, so the sidebar
 * never starts the service on its own. */
function useFollow(serviceRunning: boolean) {
  useEffect(() => {
    if (!serviceRunning) return;
    acquire();
    return release;
  }, [serviceRunning]);
}
/** Running and queued builds. */
export function useCompileActiveCount(serviceRunning: boolean) {
  useFollow(serviceRunning);
  return useSyncExternalStore(subscribe, readActive, readActive);
}
/** Stored build artifacts. */
export function useCompileArtifactCount(serviceRunning: boolean) {
  useFollow(serviceRunning);
  return useSyncExternalStore(subscribe, readArtifacts, readArtifacts);
}
