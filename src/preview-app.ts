import type { ModelPlay } from "@gizclaw/models-player/play";

const busyMessage = "设备仍忙或资源不足。请完成首次开机引导，或长按返回退出当前应用，再重试运行。";
const interfaceMessage = "固件命令接口未就绪。请完成首次引导，或长按返回退出当前应用，再重试。";
const interfaceUnavailable = (error: unknown) =>
  error instanceof Error && !("code" in error) && error.message.startsWith("Firmware GizClaw interface is not ready;");

type CommandFailure = { code?: unknown; rpcCode?: unknown };
const failure = (error: unknown): CommandFailure => (error && typeof error === "object" ? error : {});
const rejectedRun = (error: unknown) => {
  const { code, rpcCode } = failure(error);
  // Native tool handlers wrap BUSY/NO_MEMORY as RPC RESOURCE_EXHAUSTED (8).
  return code === -18 || (code === -1000 && rpcCode === 8);
};

/** Preserve both PAL and RPC errors before Error.message loses their meaning. */
export function previewCommandError(error: unknown): string | null {
  const { code, rpcCode } = failure(error);
  const result = code === -1000 ? ({ 4: -6, 5: -8, 8: -18, 12: -3 }[rpcCode as number] ?? code) : code;
  switch (result) {
    case -18:
      return busyMessage;
    case -3:
      return "当前固件不支持此操作，请选择支持 Lua 应用的固件。";
    case -8:
      return "固件找不到请求的应用或资源，请重新安装后重试。";
    case -6:
      return "固件请求超时，结果尚未确认。请先查看设备屏幕，再决定是否重试。";
    default:
      return null;
  }
}

function delay(ms: number, signal: AbortSignal): Promise<void> {
  signal.throwIfAborted();
  return new Promise((resolve, reject) => {
    const abort = () => {
      clearTimeout(timer);
      reject(signal.reason);
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", abort);
      resolve();
    }, ms);
    signal.addEventListener("abort", abort, { once: true });
  });
}

/** Player readiness precedes the firmware's local GizClaw command registration. */
export async function waitForPreviewCommands(
  player: Pick<ModelPlay, "executeCommand">,
  { signal, timeoutMs = 60000, retryMs = 250 }: { signal: AbortSignal; timeoutMs?: number; retryMs?: number },
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (true) {
    signal.throwIfAborted();
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error(interfaceMessage);
    try {
      const tools = await player.executeCommand(
        { method: "tools.list" },
        { signal, timeout_ms: Math.min(5000, remaining) },
      );
      signal.throwIfAborted();
      if (
        !Array.isArray(tools) ||
        !["lua.app.install", "lua.app.run"].every((name) => tools.some((tool) => tool?.name === name))
      )
        throw new Error("当前固件不支持此操作，请选择支持 Lua 应用的固件。");
      return;
    } catch (error) {
      signal.throwIfAborted();
      if (!interfaceUnavailable(error)) throw error;
      await delay(Math.min(retryMs, Math.max(0, deadline - Date.now())), signal);
    }
  }
}

/** BUSY explicitly rejects admission. Never replay a timed-out or ambiguous run. */
export async function runPreviewApp(
  player: Pick<ModelPlay, "executeCommand">,
  appId: string,
  {
    signal,
    onWaiting,
    timeoutMs = 120000,
    retryMs = 500,
  }: { signal: AbortSignal; onWaiting: () => void; timeoutMs?: number; retryMs?: number },
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let waiting = false;
  let unavailable = false;
  while (true) {
    signal.throwIfAborted();
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error(unavailable ? interfaceMessage : busyMessage);
    try {
      await player.executeCommand(
        { method: "tools.invoke", tool: "lua.app.run", args: { app_id: appId } },
        // A submitted write cannot be undone. Cancel waiting at the boundary
        // after its acknowledgement, rather than canceling its firmware handoff.
        { timeout_ms: Math.min(10000, remaining) },
      );
      signal.throwIfAborted();
      return;
    } catch (error) {
      signal.throwIfAborted();
      unavailable = interfaceUnavailable(error);
      // The SDK rejects a missing ABI before submitting the write. A firmware
      // rebind during setup is safe to wait through, just like BUSY admission.
      if (!rejectedRun(error) && !unavailable) throw error;
      if (!waiting) {
        waiting = true;
        onWaiting();
      }
      await delay(Math.min(retryMs, Math.max(0, deadline - Date.now())), signal);
    }
  }
}
