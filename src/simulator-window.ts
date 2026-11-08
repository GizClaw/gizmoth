import { invoke, isTauri } from "@tauri-apps/api/core";
import { emitTo, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { bridge, demo, errorMessage, type ProjectSummary } from "./bridge";
import type { DeviceModel } from "./device-model";
import type { Project } from "./project";
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
const projectTargets = new Map<string, string>();
export const simulatorTarget = (projectId: string) => projectTargets.get(projectId);
export const clearSimulatorTarget = (projectId: string) => projectTargets.delete(projectId);
const controls = new Map<string, (reply: ControlReply) => void>();
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
  let session = sessions.get(id);
  if (!session || (!isTauri() && session.popup?.closed)) {
    void session?.nativeReady?.then((unlisten) => unlisten());
    session = { options, actionId: null, token: crypto.randomUUID(), popup: null };
    sessions.set(id, session);
    const current = session;
    const respond = async (request: Request | ControlReply, send: (reply: Reply) => void) => {
      if (!request || request.profileId !== id || request.token !== current.token) return;
      if (request.type === "gizmoth-simulator-result") {
        controls.get(request.commandId)?.(request);
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
  session.actionId = options.project ? crypto.randomUUID() : null;
  const url = new URL(location.href);
  url.search = "";
  url.hash = "";
  url.searchParams.set("simulator", id);
  url.searchParams.set("channel", session.token);
  if (demo) url.searchParams.set("demo", "1");
  const retarget: Retarget = { type: "gizmoth-simulator-retarget", profileId: id, token: session.token };
  if (isTauri()) {
    await session.nativeReady;
    session.nativeLabel = await invoke<string>("open_simulator", { profileId: id, token: session.token });
    await emitTo(session.nativeLabel, `simulator-${session.token}`, retarget);
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
  if (options.project) projectTargets.set(options.project.id, id);
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
  command: Omit<SimulatorCommand, "requestId">,
  signal: AbortSignal,
  timeoutMs = 20000,
): Promise<SimulatorSnapshot> {
  signal.throwIfAborted();
  const session = sessions.get(profileId);
  if (!session || session.options.project?.id !== projectId || !session.actionId)
    throw new Error("Run this project with run_in_simulator before controlling this simulator.");
  if (session.popup?.closed) throw new Error("Simulator window is closed. Run it again before testing.");
  await session.nativeReady;
  signal.throwIfAborted();
  const commandId = crypto.randomUUID();
  const payload: ControlRequest = {
    type: "gizmoth-simulator-control",
    profileId,
    token: session.token,
    commandId,
    command: { ...command, requestId: session.actionId },
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
    const abort = () => {
      cleanup();
      cancel();
      reject(signal.reason);
    };
    const timer = setTimeout(() => {
      cleanup();
      cancel();
      reject(new Error("Simulator did not respond. Its command outcome is unknown; inspect before retrying."));
    }, timeoutMs);
    controls.set(commandId, (reply) => {
      cleanup();
      if (reply.error) reject(new Error(reply.error));
      else if (!reply.result) reject(new Error("Simulator returned an empty result."));
      else if (session.actionId !== payload.command.requestId)
        reject(new Error("Simulator target changed while the command was running."));
      else resolve(reply.result);
    });
    signal.addEventListener("abort", abort, { once: true });
    void send(payload).catch((error) => {
      cleanup();
      reject(error);
    });
  });
}

/** Only read requests are retried while the new window installs its control listener. */
export async function waitForSimulator(profileId: string, projectId: string, signal: AbortSignal) {
  const deadline = Date.now() + 30000;
  while (true) {
    signal.throwIfAborted();
    try {
      return await simulatorRequest(profileId, projectId, { action: "inspect" }, signal, 1500);
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
