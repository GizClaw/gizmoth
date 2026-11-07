import { invoke, isTauri } from "@tauri-apps/api/core";
import { emitTo, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { bridge, demo, errorMessage, type ProjectSummary } from "./bridge";
import type { DeviceModel } from "./device-model";
import type { Project } from "./project";

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
    const respond = async (request: Request, send: (reply: Reply) => void) => {
      if (
        request?.type !== "gizmoth-simulator-read" ||
        request.profileId !== id ||
        request.token !== current.token ||
        typeof request.requestId !== "string"
      )
        return;
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
      current.nativeReady = listen<Request>(`simulator-${current.token}`, (event) => {
        if (current.nativeLabel)
          void respond(event.payload, (reply) => {
            void emitTo(current.nativeLabel!, `simulator-${current.token}`, reply);
          });
      });
    } else {
      const receive = (event: MessageEvent<Request>) => {
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
}
export function closeSimulator(id: string) {
  const session = sessions.get(id);
  session?.popup?.close();
  void session?.nativeReady?.then((unlisten) => unlisten());
  sessions.delete(id);
}
export function simulatorConnection(profileId: string, token: string, onRetarget: () => void) {
  const pending = new Map<
    string,
    { resolve: (reply: Reply) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }
  >();
  let disposed = false;
  const receive = (reply: Reply | Retarget) => {
    if (!reply || reply.profileId !== profileId || reply.token !== token) return;
    if (reply.type === "gizmoth-simulator-retarget") {
      onRetarget();
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
  const ready = isTauri() ? listen<Reply | Retarget>(`simulator-${token}`, (event) => receive(event.payload)) : null;
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
    dispose: () => {
      disposed = true;
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
