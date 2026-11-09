import { invoke, isTauri } from "@tauri-apps/api/core";
import { emitTo, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { bridge, demo, errorMessage, type ProjectSummary } from "./bridge";
import type { DeviceModel } from "./device-model";
import type { Project } from "./project";
import { createSimulatorTargets } from "./simulator-target";
import {
  simulatorDelay,
  type SimulatorCommand,
  type SimulatorController,
  type SimulatorSnapshot,
  type ControlRequest,
  type ControlReply,
  type ControlCancel,
} from "./simulator-control";

export interface SimulatorOptions {
  profileId: string;
  project?: ProjectSummary;
  deviceModel?: DeviceModel;
  readProject?: () => Promise<Project>;
  requestedByAgent?: boolean;
  confirmedByUser?: boolean;
  requestId?: string;
  selection?: symbol;
}
export interface SimulatorAppRequest {
  id: string;
  name: string;
  snapshot: Project;
}
type Request = { type: "gizmoth-simulator-read"; profileId: string; token: string; requestId: string };
type Reply = {
  type: "gizmoth-simulator-reply";
  profileId: string;
  token: string;
  requestId: string;
  app?: SimulatorAppRequest;
  error?: string;
};
type Retarget = { type: "gizmoth-simulator-retarget"; profileId: string; token: string };
const sessions = new Map<
  string,
  {
    options: SimulatorOptions;
    actionId: string | null;
    token: string;
    popup: Window | null;
    nativeLabel?: string;
    nativeReady?: Promise<UnlistenFn>;
  }
>();
const projectTargets = createSimulatorTargets();
export const simulatorTarget = projectTargets.current;
export const beginSimulatorTarget = projectTargets.begin;
export const clearSimulatorTarget = projectTargets.clear;
export function simulatorRequestId(profileId: string, projectId: string) {
  const session = sessions.get(profileId);
  return session?.options.project?.id === projectId ? (session.actionId ?? undefined) : undefined;
}

async function notifySimulator(profileId: string) {
  const session = sessions.get(profileId);
  if (!session) return;
  const payload: Retarget = { type: "gizmoth-simulator-retarget", profileId, token: session.token };
  if (session.nativeLabel) await emitTo(session.nativeLabel, `simulator-${session.token}`, payload);
  else session.popup?.postMessage(payload, location.origin);
}

/** Cancel only the assistant request owned by this tool invocation, preserving any successor. */
export async function cancelAgentSimulatorRun(profileId: string, projectId: string, requestId: string) {
  const session = sessions.get(profileId);
  if (session?.options.project?.id !== projectId || !session.options.requestedByAgent || session.actionId !== requestId)
    return;
  session.actionId = null;
  await notifySimulator(profileId);
}

/** Invalidate before notifying: late project reads and native opens must see the canceled request. */
export async function cancelAgentSimulatorRuns(projectId: string, unconfirmedOnly = false) {
  // Assistant input is owned by its control RPC even when the app was started in the UI.
  for (const control of controls.values()) {
    if (control.projectId !== projectId || control.action === "inspect") continue;
    if (unconfirmedOnly && control.confirmedByUser) continue;
    control.cancel(new Error("Running apps is disabled or requires renewed approval for this project."));
  }
  const notifications: Promise<void>[] = [];
  for (const [profileId, session] of sessions) {
    if (session.options.project?.id !== projectId || !session.options.requestedByAgent) continue;
    if (unconfirmedOnly && session.options.confirmedByUser) continue;
    session.actionId = null;
    notifications.push(notifySimulator(profileId));
  }
  await Promise.all(notifications);
}
const controls = new Map<
  string,
  {
    projectId: string;
    action: SimulatorCommand["action"];
    confirmedByUser: boolean;
    receive: (reply: ControlReply) => void;
    cancel: (reason: Error) => void;
  }
>();
const readers = new Map<string, () => Promise<Project>>();
export function registerSimulatorProjectReader(id: string, reader: () => Promise<Project>): () => void {
  readers.set(id, reader);
  return () => {
    if (readers.get(id) === reader) readers.delete(id);
  };
}
async function savedProject(id: string): Promise<Project> {
  const files = await bridge.files(id);
  return {
    version: 1,
    files: await Promise.all(files.map(async ({ path }) => ({ path, content: await bridge.read(id, path) }))),
  };
}
export async function openSimulator(options: SimulatorOptions): Promise<void> {
  const id = options.profileId;
  const selection = options.project ? (options.selection ?? projectTargets.begin(options.project.id)) : undefined;
  if (options.project && selection && !projectTargets.isCurrent(options.project.id, selection))
    throw new Error("Simulator run was canceled or superseded by a newer target selection.");
  let session = sessions.get(id);
  if (!session || (!isTauri() && session.popup?.closed)) {
    void session?.nativeReady?.then((unlisten) => unlisten());
    session = { options, actionId: null, token: crypto.randomUUID(), popup: null };
    sessions.set(id, session);
    const current = session;
    const respond = async (request: Request | ControlReply, send: (reply: Reply) => void) => {
      if (!request || request.profileId !== id || request.token !== current.token) return;
      if (request.type === "gizmoth-simulator-result") {
        controls.get(request.commandId)?.receive(request);
        return;
      }
      if (request.type !== "gizmoth-simulator-read" || typeof request.requestId !== "string") return;
      const reply: Reply = {
        type: "gizmoth-simulator-reply",
        profileId: id,
        token: current.token,
        requestId: request.requestId,
      };
      try {
        if (current.options.project && current.actionId) {
          const source = current.options,
            projectId = source.project!.id,
            actionId = current.actionId;
          const snapshot = await (readers.get(projectId) ?? source.readProject ?? (() => savedProject(projectId)))();
          if (current.actionId === actionId) reply.app = { id: actionId, name: source.project!.name, snapshot };
        }
      } catch (error) {
        reply.error = errorMessage(error);
      }
      send(reply);
    };
    if (isTauri()) {
      // Native simulator assets use a separate, isolated loopback origin in packaged
      // builds. Targeted Tauri events retain the per-window token across origins.
      current.nativeReady = listen<Request | ControlReply>(`simulator-${current.token}`, (event) => {
        if (current.nativeLabel)
          void respond(event.payload, (reply) => {
            void emitTo(current.nativeLabel!, `simulator-${current.token}`, reply);
          });
      });
    } else {
      const receive = (event: MessageEvent<Request | ControlReply>) => {
        if (event.origin === location.origin && event.source === current.popup)
          void respond(event.data, (reply) => current.popup?.postMessage(reply, location.origin));
      };
      window.addEventListener("message", receive);
      const cleanup = window.setInterval(() => {
        if (current.popup?.closed || sessions.get(id) !== current) {
          window.removeEventListener("message", receive);
          window.clearInterval(cleanup);
        }
      }, 1000);
    }
  }
  session.options = options;
  const requestId = (session.actionId = options.project ? (options.requestId ?? crypto.randomUUID()) : null);
  const requireCurrentRequest = () => {
    if (sessions.get(id) !== session || session.options !== options || session.actionId !== requestId)
      throw new Error("Simulator run was canceled or superseded.");
  };
  const url = new URL(location.href);
  url.search = "";
  url.hash = "";
  url.searchParams.set("simulator", id);
  url.searchParams.set("channel", session.token);
  if (demo) url.searchParams.set("demo", "1");
  const retarget: Retarget = { type: "gizmoth-simulator-retarget", profileId: id, token: session.token };
  if (isTauri()) {
    await session.nativeReady;
    requireCurrentRequest();
    session.nativeLabel = await invoke<string>("open_simulator", { profileId: id, token: session.token });
    await notifySimulator(id);
    requireCurrentRequest();
  } else {
    // Open before any awaited work to retain the click's popup permission.
    if (!session.popup || session.popup.closed)
      session.popup = window.open(
        url.href,
        `gizmoth-simulator-${session.token}`,
        "popup=yes,width=560,height=860,resizable=yes,scrollbars=yes",
      );
    if (!session.popup) {
      sessions.delete(id);
      throw new Error("模拟器窗口被浏览器拦截，请允许此站点打开弹窗后重试。");
    }
    session.popup.focus();
    session.popup.postMessage(retarget, location.origin);
  }
  if (options.project && selection) projectTargets.commit(options.project.id, selection, id);
}
export function closeSimulator(id: string) {
  const session = sessions.get(id);
  session?.popup?.close();
  void session?.nativeReady?.then((unlisten) => unlisten());
  sessions.delete(id);
}
/** Bound to the project snapshot most recently sent to this window. Never falls back to a paired device. */
export async function simulatorRequest(
  profileId: string,
  projectId: string,
  command: Omit<SimulatorCommand, "requestId"> & { requestId?: string },
  signal: AbortSignal,
  timeoutMs = 20000,
): Promise<SimulatorSnapshot> {
  signal.throwIfAborted();
  const session = sessions.get(profileId);
  if (!session || session.options.project?.id !== projectId || !session.actionId)
    throw new Error("Run this project with run_in_simulator before controlling this simulator.");
  if (session.popup?.closed) throw new Error("Simulator window is closed. Run it again before testing.");
  const requestId = command.requestId ?? session.actionId;
  const requireCurrentRequest = () => {
    if (sessions.get(profileId) !== session || session.actionId !== requestId)
      throw new Error("Simulator run was canceled or superseded.");
  };
  requireCurrentRequest();
  const commandId = crypto.randomUUID();
  const payload: ControlRequest = {
    type: "gizmoth-simulator-control",
    profileId,
    token: session.token,
    commandId,
    command: { ...command, requestId },
  };
  const send = (message: ControlRequest | ControlCancel) =>
    session.nativeLabel
      ? emitTo(session.nativeLabel, `simulator-${session.token}`, message)
      : Promise.resolve(session.popup?.postMessage(message, location.origin));
  return new Promise((resolve, reject) => {
    const cleanup = () => {
      clearTimeout(timer);
      controls.delete(commandId);
      signal.removeEventListener("abort", abort);
    };
    const cancel = () => void send({ ...payload, type: "gizmoth-simulator-cancel" }).catch(() => {});
    const fail = (reason: unknown) => {
      cleanup();
      cancel();
      reject(reason);
    };
    const abort = () => fail(signal.reason);
    const timer = setTimeout(
      () => fail(new Error("Simulator did not respond. Its command outcome is unknown; inspect before retrying.")),
      timeoutMs,
    );
    controls.set(commandId, {
      projectId,
      action: command.action,
      confirmedByUser: !!session.options.confirmedByUser,
      cancel: fail,
      receive: (reply) => {
        cleanup();
        if (reply.error) reject(new Error(reply.error));
        else if (!reply.result) reject(new Error("Simulator returned an empty result."));
        else if (sessions.get(profileId) !== session || session.actionId !== requestId)
          reject(new Error("Simulator run was canceled or superseded."));
        else resolve(reply.result);
      },
    });
    signal.addEventListener("abort", abort, { once: true });
    // Register before waiting for the channel so permission revocation can cancel queued controls too.
    void Promise.resolve(session.nativeReady)
      .then(() => {
        if (!controls.has(commandId)) return;
        signal.throwIfAborted();
        requireCurrentRequest();
        return send(payload);
      })
      .catch((error) => {
        cleanup();
        reject(error);
      });
  });
}

/** Only read requests are retried while the new window installs its control listener. */
export async function waitForSimulator(profileId: string, projectId: string, signal: AbortSignal, requestId: string) {
  const deadline = Date.now() + 30000;
  while (true) {
    signal.throwIfAborted();
    try {
      return await simulatorRequest(profileId, projectId, { action: "inspect", requestId }, signal, 1500);
    } catch (error) {
      signal.throwIfAborted();
      if (Date.now() >= deadline || !/did not respond|not ready|target changed/i.test(errorMessage(error))) throw error;
      await simulatorDelay(200, signal);
    }
  }
}

export function simulatorConnection(profileId: string, token: string, onRetarget: () => void) {
  const pending = new Map<
    string,
    { resolve: (reply: Reply) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }
  >();
  let disposed = false;
  let controller: SimulatorController | null = null;
  const active = new Map<string, AbortController>();
  const sendResult = (reply: ControlReply) =>
    isTauri()
      ? emitTo("main", `simulator-${token}`, reply)
      : Promise.resolve(window.opener?.postMessage(reply, location.origin));
  const receive = (reply: Reply | Retarget | ControlRequest | ControlCancel) => {
    if (!reply || reply.profileId !== profileId || reply.token !== token) return;
    if (reply.type === "gizmoth-simulator-retarget") {
      for (const request of active.values()) request.abort();
      onRetarget();
      return;
    }
    if (reply.type === "gizmoth-simulator-cancel") {
      active.get(reply.commandId)?.abort();
      return;
    }
    if (reply.type === "gizmoth-simulator-control") {
      if (active.has(reply.commandId) || active.size >= 8) return;
      const request = new AbortController();
      active.set(reply.commandId, request);
      const result: ControlReply = {
        type: "gizmoth-simulator-result",
        profileId,
        token,
        commandId: reply.commandId,
      };
      void Promise.resolve()
        .then(() => {
          if (!controller) throw new Error("Simulator controller is not ready.");
          return controller(reply.command, request.signal);
        })
        .then(
          (snapshot) => {
            result.result = snapshot;
          },
          (error) => {
            result.error = errorMessage(error);
          },
        )
        .finally(() => {
          active.delete(reply.commandId);
          if (!disposed) void sendResult(result).catch(() => {});
        });
      return;
    }
    if (reply.type !== "gizmoth-simulator-reply") return;
    const item = pending.get(reply.requestId);
    if (!item) return;
    clearTimeout(item.timer);
    pending.delete(reply.requestId);
    if (reply.error) item.reject(new Error(reply.error));
    else item.resolve(reply);
  };
  const message = (event: MessageEvent) => {
    if (event.origin === location.origin && event.source === window.opener) receive(event.data);
  };
  const ready = isTauri()
    ? listen<Reply | Retarget | ControlRequest | ControlCancel>(`simulator-${token}`, (event) => receive(event.payload))
    : null;
  if (!ready) window.addEventListener("message", message);
  const request = async () => {
    if (ready) await ready;
    if (disposed) throw new Error("模拟器已关闭。");
    return new Promise<Reply>((resolve, reject) => {
      const requestId = crypto.randomUUID();
      const timer = setTimeout(() => {
        pending.delete(requestId);
        reject(new Error("无法连接项目窗口，请重新打开模拟器。"));
      }, 5000);
      pending.set(requestId, { resolve, reject, timer });
      const payload: Request = { type: "gizmoth-simulator-read", profileId, token, requestId };
      if (ready)
        void emitTo("main", `simulator-${token}`, payload).catch((error) => {
          clearTimeout(timer);
          pending.delete(requestId);
          reject(error);
        });
      else window.opener?.postMessage(payload, location.origin);
    });
  };
  return {
    incomingApp: async (): Promise<SimulatorAppRequest | null> => (await request()).app ?? null,
    setController: (next: SimulatorController | null) => {
      controller = next;
    },
    dispose: () => {
      disposed = true;
      for (const request of active.values()) request.abort();
      active.clear();
      window.removeEventListener("message", message);
      void ready?.then((unlisten) => unlisten());
      for (const item of pending.values()) {
        clearTimeout(item.timer);
        item.reject(new Error("模拟器已关闭。"));
      }
      pending.clear();
    },
  };
}
