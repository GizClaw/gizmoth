import type { PreviewButton } from "./preview-input.ts";

export interface SimulatorSnapshot {
  profileId: string;
  requestId: string | null;
  phase: string;
  firmware: string | null;
  app: { appId: string | null; status: string; error?: string };
  error: string | null;
  notice: string | null;
  events: string[];
  screen?: { data: string; mimeType: "image/png"; width: number; height: number; brightness: number; hash: string };
}

export interface SimulatorCommand {
  action: "start" | "inspect" | "press";
  requestId: string;
  button?: PreviewButton;
  durationMs?: number;
  waitMs?: number;
  capture?: boolean;
}
export type SimulatorController = (command: SimulatorCommand, signal: AbortSignal) => Promise<SimulatorSnapshot>;
export type ControlRequest = {
  type: "gizmoth-simulator-control";
  profileId: string;
  token: string;
  commandId: string;
  command: SimulatorCommand;
};
export type ControlReply = {
  type: "gizmoth-simulator-result";
  profileId: string;
  token: string;
  commandId: string;
  result?: SimulatorSnapshot;
  error?: string;
};
export type ControlCancel = Omit<ControlRequest, "type" | "command"> & { type: "gizmoth-simulator-cancel" };

const buttons = new Set(["left", "right", "ok", "back", "power", "record", "volume_down", "volume_up"]);
function bounded(value: unknown, max: number, field: string) {
  if (value !== undefined && (typeof value !== "number" || !Number.isInteger(value) || value < 0 || value > max))
    throw new Error(`${field} must be an integer between 0 and ${max}.`);
}

/** Validate at the receiving window too; the event channel never accepts arbitrary firmware RPC. */
export function validateSimulatorCommand(command: SimulatorCommand) {
  if (!command || !["start", "inspect", "press"].includes(command.action) || typeof command.requestId !== "string")
    throw new Error("Invalid simulator command.");
  if (command.capture !== undefined && typeof command.capture !== "boolean") throw new Error("Invalid capture flag.");
  bounded(command.waitMs, 10000, "waitMs");
  bounded(command.durationMs, 3000, "durationMs");
  if (command.action === "press" && !buttons.has(command.button ?? "")) throw new Error("Unsupported device button.");
}

export function simulatorDelay(ms: number, signal: AbortSignal): Promise<void> {
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

export function createSimulatorController(host: {
  snapshot: () => SimulatorSnapshot;
  start: (signal: AbortSignal) => Promise<void>;
  press: (button: PreviewButton, source: string) => void;
  release: (source: string) => void;
  capture: () => NonNullable<SimulatorSnapshot["screen"]>;
}): SimulatorController {
  let pressing = false;
  return async (command, signal) => {
    validateSimulatorCommand(command);
    const check = () => {
      signal.throwIfAborted();
      if (host.snapshot().requestId !== command.requestId)
        throw new Error("Simulator target changed. Run this project in the simulator again before testing.");
    };
    check();
    if (command.action === "start") await host.start(signal);
    if (command.action === "press") {
      if (pressing) throw new Error("A simulator button command is already running.");
      pressing = true;
      const source = `agent:${crypto.randomUUID()}`;
      try {
        host.press(command.button!, source);
        await simulatorDelay(command.durationMs ?? 100, signal);
      } finally {
        host.release(source);
        pressing = false;
      }
    }
    if (command.waitMs) await simulatorDelay(command.waitMs, signal);
    check();
    const result = host.snapshot();
    if (command.capture) result.screen = host.capture();
    return result;
  };
}
