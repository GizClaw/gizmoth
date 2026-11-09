import type { AgentTool } from "@earendil-works/pi-agent-core";
import { obj, str, opt, int, bool, oneOf } from "@earendil-works/pi-schema";
import type { AgentHost } from "./agent";
import { bridge, defaultPermissions } from "./bridge";
import { selectSimulatorProfile, simulatorProfiles } from "./simulator-profiles";
import {
  cancelAgentSimulatorRun,
  openSimulator,
  simulatorRequest,
  simulatorRequestId,
  simulatorTarget,
  waitForSimulator,
} from "./simulator-window";
import type { SimulatorSnapshot } from "./simulator-control";

const text = (value: string) => ({ type: "text" as const, text: value });
function result(snapshot: SimulatorSnapshot, includeImage = false) {
  const { screen, ...status } = snapshot;
  return {
    content: [
      text(JSON.stringify({ ...status, ...(screen ? { screen: { ...screen, data: undefined } } : {}) }, null, 2)),
      ...(screen && includeImage ? [{ type: "image" as const, data: screen.data, mimeType: screen.mimeType }] : []),
    ],
    // Images belong in content only, so history budgeting can remove old captures.
    details: { ...status, ...(screen ? { screenHash: screen.hash } : {}) },
  };
}
const profileParameter = opt(
  str({ description: "Simulator id from list_simulators; defaults to this project's last simulator." }),
);

export function createSimulatorTools(host: AgentHost): AgentTool<any>[] {
  const approved = new Map<string, string>();
  const permission = () => {
    const run = (host.permissions?.() ?? defaultPermissions).run;
    if (run === "deny") throw new Error("Running apps is disabled for this project (项目信息 › 助手权限).");
    return run;
  };
  const target = (profileId?: string) => {
    const id = profileId || simulatorTarget(host.projectId);
    if (!id) throw new Error("No simulator target. Call run_in_simulator first.");
    return id;
  };
  return [
    {
      name: "list_simulators",
      label: "列出模拟器",
      description:
        "List local simulators, their model and saved firmware, and this project's last simulator target. These are separate from paired physical devices.",
      parameters: obj({}),
      execute: async () => ({
        content: [
          text(
            JSON.stringify({
              selected: simulatorTarget(host.projectId) ?? null,
              simulators: await simulatorProfiles.list(),
            }),
          ),
        ],
        details: {},
      }),
    },
    {
      name: "run_in_simulator",
      label: "在模拟器运行",
      description:
        "Validate and install the current project in a local simulator and boot its compatible firmware. With no id, reuse this project's last simulator, the first existing simulator, or create an H106 Tiga. Obeys the project's run permission. Returns startup/install/run-request status, not proof of correct Lua execution. Inspect the screen and send buttons to test, including first-boot setup. Never uses the physical device service.",
      parameters: obj({ profileId: profileParameter }),
      executionMode: "sequential",
      execute: async (_id, { profileId }, signal = new AbortController().signal) => {
        signal.throwIfAborted();
        permission();
        const all = await simulatorProfiles.list();
        const remembered = simulatorTarget(host.projectId);
        let profile = selectSimulatorProfile(all, profileId, remembered);
        // Ignore a deleted ID for selection, but retain local routing until openSimulator
        // commits its replacement. Declined/failed recovery must never select physical hardware.
        let confirmed = false;
        const confirmRun = async () => {
          const ok = await host.confirm(
            host.translate?.("在模拟器运行") ?? "在模拟器运行",
            host.translate?.("允许助手在「{name}」安装当前项目、启动固件并进行按键测试？同名应用会被替换。", {
              name: profile?.name ?? "H106 Tiga",
            }) ??
              `允许助手在「${profile?.name ?? "H106 Tiga"}」安装当前项目、启动固件并进行按键测试？同名应用会被替换。`,
          );
          signal.throwIfAborted();
          if (!ok) throw new Error("The user declined simulator testing.");
          confirmed = true;
        };
        while (permission() === "ask" && !confirmed) await confirmRun();
        await bridge.check(host.projectId);
        const project = (await bridge.projects()).find((p) => p.id === host.projectId);
        if (!project) throw new Error("Project no longer exists.");
        signal.throwIfAborted();
        while (permission() === "ask" && !confirmed) await confirmRun();
        profile ??= await simulatorProfiles.create("H106 Tiga", "h106-tiga");
        // Preparation and profile creation await native work; authorization may change meanwhile.
        signal.throwIfAborted();
        while (permission() === "ask" && !confirmed) await confirmRun();
        const requestId = crypto.randomUUID();
        const cancel = () => void cancelAgentSimulatorRun(profile.id, host.projectId, requestId).catch(() => {});
        // The turn's signal must still cancel queued startup after this tool returns its status.
        signal.addEventListener("abort", cancel, { once: true });
        signal.throwIfAborted();
        await openSimulator({
          profileId: profile.id,
          project,
          requestedByAgent: true,
          confirmedByUser: confirmed,
          requestId,
        });
        // Readiness retries, startup and consent must stay on this invocation's request.
        await waitForSimulator(profile.id, host.projectId, signal, requestId);
        permission();
        const snapshot = await simulatorRequest(
          profile.id,
          host.projectId,
          { action: "start", waitMs: 1000, requestId },
          signal,
        );
        if (confirmed) approved.set(profile.id, requestId);
        return result(snapshot);
      },
    },
    {
      name: "inspect_simulator",
      label: "读取模拟器状态",
      description:
        "Read actual player phase, app install/run-request progress, errors and recent host events. Optionally wait up to 10 seconds. capture=true includes a 240x240 PNG of the visible framebuffer (requires a vision model); brightness and hash help detect a sleeping or changing display. This is not a Lua runtime log or automatic test verdict.",
      parameters: obj({
        profileId: profileParameter,
        waitMs: opt(int({ minimum: 0, maximum: 10000 })),
        capture: opt(bool()),
      }),
      execute: async (_id, { profileId, waitMs, capture }, signal = new AbortController().signal) => {
        if (capture && host.supportsImages?.() === false)
          throw new Error("The selected model cannot view images. Inspect without capture or choose a vision model.");
        return result(
          await simulatorRequest(target(profileId), host.projectId, { action: "inspect", waitMs, capture }, signal),
          capture,
        );
      },
    },
    {
      name: "press_simulator_button",
      label: "模拟器按键测试",
      description:
        "Press then release a real simulator button, optionally hold up to 3000 ms and wait up to 10000 ms afterwards. Requires an approved run_in_simulator test session or run permission allow. Back held about 1500-2000 ms exits an app; Power wakes a dark display. Returns current status; inspect/capture afterwards to check the observed behavior.",
      parameters: obj({
        profileId: profileParameter,
        button: oneOf(["left", "right", "ok", "back", "power", "record", "volume_down", "volume_up"]),
        durationMs: opt(int({ minimum: 0, maximum: 3000 })),
        waitMs: opt(int({ minimum: 0, maximum: 10000 })),
      }),
      executionMode: "sequential",
      execute: async (_id, { profileId, button, durationMs, waitMs }, signal = new AbortController().signal) => {
        const id = target(profileId);
        if (
          permission() === "ask" &&
          (!approved.has(id) || approved.get(id) !== simulatorRequestId(id, host.projectId))
        )
          throw new Error("Call run_in_simulator to approve a simulator test session before sending input.");
        return result(
          await simulatorRequest(id, host.projectId, { action: "press", button, durationMs, waitMs }, signal),
        );
      },
    },
  ] as AgentTool<any>[];
}
