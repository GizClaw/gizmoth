import { useEffect, useMemo, useSyncExternalStore } from "react";
import { call, native } from "./bridge";
import { SerialLogReader, SerialSnapshotOrder } from "./serial-state";

/** Client of the resident serial daemon (src-tauri/src/serial). */
export type SerialProtocol = "auto" | "h2loader" | "serial";
/** takeover: GizMoth control. piping: one WS connection has exclusive communication rights.
 * GizMoth's service opens and manages the physical serial port in both modes. */
export type SerialMode = "takeover" | "piping";
/** WS ownership survives physical disconnection; read `connection` for device availability. */
export type SerialHolder =
  { kind: "gizmoth" } | { kind: "client"; id: number; client: string; title: string; reason: string; since: number };
/** A pipe client waiting for the port; its WebSocket stays open until granted or rejected. */
export interface SerialRequest {
  id: number;
  client: string;
  /** Required task title, up to 120 characters. */
  title: string;
  reason: string;
  since: number;
}
export type SerialConnection = "off" | "waiting" | "connecting" | "connected" | "occupied" | "error";
export type SerialActivity =
  "idle" | "handshaking" | "sending" | "transferring" | "rebooting" | "verifying" | "reconnecting";
export type SerialJobKind = "refresh" | "stage" | "install" | "rebootApp" | "rebootLoader" | "rebootUpgrade";
export type SerialJobState = "queued" | "running" | "succeeded" | "failed" | "cancelled";

export interface UsbIdentity {
  vid: number;
  pid: number;
  serial?: string | null;
  product?: string | null;
  manufacturer?: string | null;
}
export interface PackageInfo {
  board: string;
  target: string;
  version: string;
  role: "app" | "loader";
  sha256: string;
  bytes: number;
  format: number;
  name?: string | null;
}
export interface SerialJob {
  id: string;
  kind: SerialJobKind;
  state: SerialJobState;
  phase: string;
  acknowledged: number;
  total: number;
  source: "app" | "mcp" | "gateway" | "ws";
  createdAt: number;
  finishedAt?: number | null;
  error?: string | null;
  package?: PackageInfo | null;
}
export interface H2LoaderMetadata {
  valid: boolean;
  packageSize: number;
  imageSize: number;
  packageChecksum: string;
  imageChecksum: string;
  version: string;
  board: string;
  target: string;
  role: "app" | "loader" | "unknown";
}
export interface H2LoaderStatus {
  board: string;
  target: string;
  chip: string;
  deviceUid: string;
  capabilities: number;
  commandAvailability: number;
  /** Decoded by the serial service from commandAvailability. */
  commands?: string[];
  transports?: string[];
  active: { role: "app" | "loader" | "unknown"; version: string; imageChecksum: string; imageSize: number };
  runningPartition: number;
  nextPartition: number;
  bootIntent: "auto" | "loader";
  stage: H2LoaderMetadata;
  partition1: H2LoaderMetadata;
  partition2: H2LoaderMetadata;
  lastResult: number;
}
export interface SerialPort {
  id: string;
  name: string;
  path: string;
  protocol: SerialProtocol;
  baud: number;
  enabled: boolean;
  mode: SerialMode;
  holder?: SerialHolder | null;
  queue?: SerialRequest[];
  /** Grant exclusive WS requests automatically when idle, in either mode. */
  autoGrant?: boolean;
  usb?: UsbIdentity | null;
  createdAt: number;
  present: boolean | null;
  connection: SerialConnection;
  detected?: "h2loader" | "serial" | null;
  activity: SerialActivity;
  error?: string | null;
  rxBytes?: number;
  txBytes?: number;
  lastRxAt?: number | null;
  lastTxAt?: number | null;
  connectedAt?: number | null;
  h2loader?: H2LoaderStatus | null;
  h2loaderAt?: number | null;
  job?: SerialJob | null;
  preparing?: boolean;
  history?: SerialJob[];
  logSeq?: number;
  logGeneration?: number;
  /** Set on ports imported from another GizMoth: the owning node's ID. */
  node?: string;
  remote?: RemotePortState;
}
/** This GizMoth's own exclusive session on an imported port. */
export interface RemoteControl {
  state: "none" | "opening" | "queued" | "granted";
  position?: number;
  connectionId?: number;
  holder?: string;
  reason?: string;
}
export interface RemotePortState {
  /** The port's ID on its owning node. */
  portId: string;
  /** The node is not connected; the port shows its last known state. */
  stale: boolean;
  control: RemoteControl;
}
export type RemoteNodeStatus = "connecting" | "online" | "offline" | "error";
/** Another GizMoth whose serial ports this one imports (one level). */
export interface RemoteNode {
  id: string;
  name: string;
  url: string;
  status: RemoteNodeStatus;
  error?: string | null;
  bootId?: string | null;
  /** Boot whose first complete topology has arrived; absent during the handshake. */
  topologyBootId?: string | null;
  version?: string | null;
  lastSeenAt?: number | null;
  ports: SerialPort[];
}
/** Node information returned by `serial_remote_probe`. */
export interface RemoteProbe {
  url: string;
  node?: { id: string; name: string; version: string };
  self?: boolean;
  ports?: number | null;
}
export interface GatewaySummary {
  enabled: boolean;
  scope: "local" | "lan";
  port: number;
  error?: string | null;
}
export interface GatewayStatus extends GatewaySummary {
  apiKey: string;
  listening: boolean;
  boundPort: number;
  urls: string[];
  node?: { id: string; name: string };
}
export interface SerialSnapshot {
  /** Forwarded preparation or an owned remote session blocks safe retirement. */
  remoteBusy?: boolean;
  serviceUpdateError?: string | null;
  gateway?: GatewaySummary;
  pipeBase?: string;
  running: boolean;
  /** Desktop IPC identity epoch; advances when the verified discovery connection changes. */
  ownerEpoch: number;
  /** Desktop IPC observation sequence, allocated after verifying the current discovery owner. */
  observationEpoch: number;
  instanceId?: string | null;
  /** Persisted daemon startup generation; 0 when offline or connected to a legacy daemon. */
  instanceGeneration: number;
  version: number;
  h2loaderCore: boolean;
  ports: SerialPort[];
  /** This service's identity as a cascade node. */
  node?: { id: string; name: string };
  nodes?: RemoteNode[];
}
export interface SystemPort {
  path: string;
  name: string;
  usb?: UsbIdentity | null;
}
export interface LogEntry {
  seq: number;
  at: number;
  source: "rx" | "tx" | "system";
  text: string;
}
export interface LogPage {
  instanceId?: string;
  generation?: number;
  entries: LogEntry[];
  next: number;
  truncated: boolean;
}
export interface NewSerialPort {
  path: string;
  name?: string;
  protocol: SerialProtocol;
  baud: number;
  usb?: UsbIdentity | null;
  enabled: boolean;
}

async function refreshSnapshot(): Promise<SerialSnapshot> {
  const next = await serial.overview();
  setSerialSnapshot(next);
  return next;
}

/** Keep the command result even when synchronizing subscribers fails. */
async function mutate<T>(
  command: string,
  args: Record<string, unknown>,
  refresh: "await" | "background" = "await",
): Promise<T> {
  const result = await call<T>(command, args);
  const pending = refreshSnapshot().catch((e) => {
    error = String(e instanceof Error ? e.message : e);
    emit();
  });
  if (refresh === "await") await pending;
  else void pending;
  return result;
}

export const serial = {
  overview: () => call<SerialSnapshot>("serial_overview"),
  refreshSnapshot,
  watch: (since: number, instanceId?: string | null) => call<SerialSnapshot>("serial_watch", { since, instanceId }),
  systemPorts: () => call<SystemPort[]>("serial_system_ports"),
  add: (port: NewSerialPort) => mutate<SerialPort>("serial_add", { port }, "background"),
  update: (
    id: string,
    patch: Partial<Pick<SerialPort, "name" | "protocol" | "baud" | "enabled" | "autoGrant">> & { mode?: "takeover" },
  ) => mutate<SerialPort>("serial_update", { id, patch }),
  remove: (id: string) => mutate<unknown>("serial_remove", { id }),
  logs: (id: string, after?: number, limit?: number) => call<LogPage>("serial_logs", { id, after, limit }),
  clearLogs: (id: string) => call<unknown>("serial_clear_logs", { id }),
  send: (id: string, text: string, lineEnding: "lf" | "crlf" | "cr" | "none") =>
    call<unknown>("serial_send", { id, text, lineEnding }),
  choosePackage: () => call<string | null>("serial_choose_package"),
  inspect: (path: string) => call<PackageInfo>("serial_inspect", { path }),
  job: (id: string, kind: SerialJobKind, path?: string) => call<SerialJob>("serial_job", { id, kind, path }),
  cancelJob: (jobId: string) => call<unknown>("serial_cancel_job", { jobId }),
  answerRequest: (id: string, request: number, accept: boolean) =>
    mutate<SerialPort>("serial_answer_request", { id, request, accept }),
  release: (id: string, connection: number, pauseAutomatic = false) =>
    mutate<SerialPort>("serial_release", { id, connection, pauseAutomatic }),
  stopService: () => call<void>("serial_stop_service"),
  exportLogs: (id: string, name: string) => call<string | null>("serial_export_logs", { id, name }),
  remoteProbe: (url: string, apiKey: string) => call<RemoteProbe>("serial_remote_probe", { url, apiKey }),
  remoteAdd: (url: string, apiKey: string) =>
    mutate<{ id: string; name: string; url: string }>("serial_remote_add", { url, apiKey }),
  remoteRemove: (id: string) => mutate<unknown>("serial_remote_remove", { id }),
  gateway: () => call<GatewayStatus>("gateway_status"),
  updateGateway: (patch: { enabled?: boolean; scope?: "local" | "lan"; port?: number; regenerateKey?: boolean }) =>
    mutate<GatewayStatus>("gateway_update", { patch }, "background"),
};

// One long-poll loop shared by every subscriber (sidebar, list, detail).
let snapshot: SerialSnapshot | null = null;
let error = "";
const listeners = new Set<() => void>();
const snapshotOrder = new SerialSnapshotOrder();
let loop = 0;

function emit() {
  for (const listener of listeners) listener();
}
function setSerialSnapshot(next: SerialSnapshot) {
  if (!snapshotOrder.accepts(next)) return;
  snapshot = next;
  error = "";
  emit();
}
async function run(id: number) {
  try {
    const next = await serial.overview();
    if (id !== loop) return;
    setSerialSnapshot(next);
  } catch (e) {
    error = String(e instanceof Error ? e.message : e);
    emit();
  }
  while (id === loop) {
    try {
      const next = await serial.watch(
        snapshot?.running ? snapshot.version : 0,
        snapshot?.running ? snapshot.instanceId : undefined,
      );
      if (id !== loop) return;
      setSerialSnapshot(next);
    } catch (e) {
      if (id !== loop) return;
      error = String(e instanceof Error ? e.message : e);
      emit();
      await new Promise((resolve) => setTimeout(resolve, 2000));
    }
  }
}
function subscribe(listener: () => void) {
  listeners.add(listener);
  if (listeners.size === 1 && native) void run(++loop);
  return () => {
    listeners.delete(listener);
    if (!listeners.size) loop++;
  };
}
const read = () => snapshot;
const readError = () => error;
const readPortCount = () =>
  (snapshot?.ports.length ?? 0) + (snapshot?.nodes ?? []).reduce((count, node) => count + node.ports.length, 0);

/** A local or imported port, with the node that owns an imported one. */
export function findSerialPort(
  snapshot: SerialSnapshot | null | undefined,
  id: string,
): { port: SerialPort; node?: RemoteNode } | null {
  const local = snapshot?.ports.find((p) => p.id === id);
  if (local) return { port: local };
  for (const node of snapshot?.nodes ?? []) {
    const port = node.ports.find((p) => p.id === id);
    if (port) return { port, node };
  }
  return null;
}

/** Traffic updates do not rerender the entire project workspace. */
export function useSerialPortCount() {
  return useSyncExternalStore(subscribe, readPortCount, readPortCount);
}

export function useSerialSnapshot() {
  const value = useSyncExternalStore(subscribe, read, read);
  const failure = useSyncExternalStore(subscribe, readError, readError);
  return { snapshot: value, error: failure };
}

/** Incremental reads survive forward watermark updates; process/log generations
 * invalidate pending work and clear the old history for every mounted reader. */
export function useSerialLogs(
  id: string,
  logSeq: number | undefined,
  running: boolean,
  instanceId?: string | null,
  logGeneration?: number,
) {
  const reader = useMemo(() => new SerialLogReader(serial.logs), [id, instanceId]);
  const state = useSyncExternalStore(reader.subscribe, reader.read, reader.read);
  useEffect(() => {
    reader.update({ id, seq: logSeq, running, instanceId, generation: logGeneration });
  }, [reader, id, logSeq, running, instanceId, logGeneration]);
  return { ...state, reset: reader.reset };
}

/** Device commands available right now (h2loader::COMMANDS bit order). */
const COMMAND_BITS: [number, string][] = [
  [0, "rebootApp"],
  [1, "rebootLoader"],
  [2, "help"],
  [3, "status"],
  [4, "stats"],
  [5, "memory"],
  [8, "coredumpStatus"],
  [9, "coredumpDump"],
  [10, "coredumpErase"],
  [11, "stagePayload"],
  [12, "stageAbort"],
  [13, "stageUrl"],
  [16, "wifiScan"],
  [17, "wifiConnect"],
  [18, "wifiDisconnect"],
  [19, "rebootUpgrade"],
  [20, "wifiStatus"],
  [21, "mfg"],
];
export function deviceCommands(status: H2LoaderStatus): Set<string> {
  return new Set(
    status.commands ?? COMMAND_BITS.filter(([bit]) => status.commandAvailability & (1 << bit)).map(([, name]) => name),
  );
}
/** Commands each job needs; mirrors daemon::job_commands. */
export const jobCommands: Record<SerialJobKind, string[]> = {
  refresh: ["status"],
  stage: ["stagePayload"],
  install: ["stagePayload", "rebootUpgrade"],
  rebootApp: ["rebootApp"],
  rebootLoader: ["rebootLoader"],
  rebootUpgrade: ["rebootUpgrade"],
};

export function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
