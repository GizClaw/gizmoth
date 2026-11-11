// In-memory stand-in for the devenv commands, mirroring the published registry
// (GizClaw/gizmoth-devenv). Installation is simulated; nothing is downloaded or written.
import type {
  DevEnvChip,
  DevEnvInstallation,
  DevEnvState,
  DevEnvToolchain,
  RemovalPreview,
  RemovalTarget,
} from "./devenv.ts";

const planned = { code: "planned" } as const;
const plannedReason = "配方待补，暂时无法安装";
const chip = (
  id: string,
  name: string,
  vendor: string,
  sdk: string,
  description: string,
  notes: string,
): DevEnvChip => ({
  id,
  name,
  vendor,
  description,
  homepage: null,
  sdk,
  notes,
  status: "planned",
  blocked: planned,
  reason: plannedReason,
  version: null,
  packages: [],
});
const chips: DevEnvChip[] = [
  chip(
    "esp32s3",
    "ESP32-S3",
    "Espressif",
    "ESP-IDF",
    "Espressif Wi-Fi and Bluetooth LE MCU supported by GizOS.",
    "Publish after ESP-IDF releases declare the esp32s3 target and its xtensa-esp-elf toolchain.",
  ),
  chip(
    "bk7258",
    "BK7258",
    "Beken",
    "Beken BK7258 SDK",
    "Beken wireless SoC supported by GizOS.",
    "Publish after the BK7258 SDK and its Arm GNU toolchain have releases. Apple Silicon hosts need a Rosetta check.",
  ),
  chip(
    "ac707n",
    "AC707N",
    "JieLi",
    "JieLi AC707N SDK",
    "JieLi Bluetooth SoC supported by GizOS.",
    "Publish after the SDK and JieLi toolchain have downloadable releases. The current toolchain runs only on Linux x86_64.",
  ),
];
const toolchain = (
  id: string,
  name: string,
  category: DevEnvToolchain["category"],
  description: string,
  version: string | null = null,
): DevEnvToolchain => ({
  id,
  name,
  description,
  category,
  homepage: "https://github.com/GizClaw/gizmoth-devenv",
  notes: null,
  status: version ? "available" : "planned",
  blocked: version ? null : planned,
  reason: version ? null : plannedReason,
  version,
});
const toolchains: DevEnvToolchain[] = [
  toolchain(
    "bazelisk",
    "Bazel (Bazelisk)",
    "build-tool",
    "Runs the Bazel version selected by each project's .bazelversion file.",
    "1.30.0",
  ),
  toolchain("arm-gnu", "Arm GNU Toolchain", "compiler", "Arm cross-compilers shared by Beken SDKs."),
  toolchain(
    "xtensa-esp-elf",
    "Xtensa ESP Toolchain",
    "compiler",
    "Espressif's GCC toolchain for Xtensa-based ESP32 chips.",
  ),
  toolchain("jieli", "JieLi Toolchain", "compiler", "JieLi compiler and post-build tools."),
];
const installations: DevEnvInstallation[] = [];
let job: DevEnvState["job"] = null;
const BYTES = 7_402_354;
const DURATION = 3200;

function advance() {
  if (!job || job.state !== "running") return;
  const progress = Math.min(1, (Date.now() - job.startedAt) / DURATION);
  job.received = Math.round(BYTES * Math.min(1, progress / 0.8));
  job.phase = progress < 0.8 ? "downloading" : progress < 0.9 ? "installing" : progress < 1 ? "checking" : "done";
  if (progress < 1) return;
  job.state = "succeeded";
  job.finishedAt = Date.now();
  job.log.push("已校验 SHA-256 555d2ff912cc", "已写入 bin/bazelisk", "已写入 bin/bazel", "完成。");
  installations.push({
    installationId: crypto.randomUUID(),
    kind: "toolchain",
    id: job.target,
    name: "Bazel (Bazelisk)",
    version: "1.30.0",
    category: "build-tool",
    origin: "managed",
    requestedByUser: true,
    path: `/Users/example/Library/Application Support/com.gizclaw.gizmoth/devenv/installs/toolchains/${job.target}/1.30.0/macos-arm64`,
    chips: [],
    healthy: true,
  });
}

function state(): DevEnvState {
  advance();
  const installed = new Set(installations.map((item) => `${item.id}@${item.version}`));
  return structuredClone({
    host: "macos-arm64",
    registry: {
      repository: "https://github.com/GizClaw/gizmoth-devenv",
      commit: "4e8027152adf7d9e6da9dcff9760b81d6e53bbc3",
      fetchedAt: Date.now() - 4 * 60_000,
    },
    error: null,
    chips,
    toolchains: toolchains.map((item) => ({
      ...item,
      status:
        job?.state === "running" && job.target === item.id
          ? "installing"
          : item.version && installed.has(`${item.id}@${item.version}`)
            ? "installed"
            : item.status,
    })),
    installations,
    job,
  });
}

function preview(target: RemovalTarget): RemovalPreview {
  const item = installations.find((entry) => entry.installationId === target.id);
  if (target.kind !== "installation" || !item) throw new Error("未知安装");
  return item.origin === "local"
    ? { action: "unregister", delete: [], keep: [item], chips: [], dependants: [], confirm: [] }
    : { action: "delete", delete: [item], keep: [], chips: [], dependants: [], confirm: [item.installationId] };
}

export function demoDevEnv(command: string, args: Record<string, unknown>): unknown {
  switch (command) {
    case "devenv_state":
    case "devenv_refresh":
      return state();
    case "devenv_install": {
      const target = args.target as { kind: string; id: string };
      if (job?.state === "running") throw new Error("另一个开发环境任务正在进行");
      if (target.kind !== "toolchain" || target.id !== "bazelisk") throw new Error(plannedReason);
      job = {
        id: crypto.randomUUID(),
        kind: "toolchain",
        target: target.id,
        name: "Bazel (Bazelisk)",
        phase: "resolving",
        state: "running",
        received: 0,
        total: BYTES,
        log: [
          "安装 Bazel (Bazelisk) 1.30.0",
          "下载 https://github.com/bazelbuild/bazelisk/releases/download/v1.30.0/bazelisk-darwin-arm64",
        ],
        error: null,
        startedAt: Date.now(),
        finishedAt: null,
      };
      return state();
    }
    case "devenv_cancel":
      if (job?.state === "running" && job.id === args.jobId) {
        job.state = "cancelled";
        job.finishedAt = Date.now();
        job.log.push("已取消。已完成的安装会保留并在下次复用。");
      }
      return state();
    case "devenv_remove_preview":
      return preview(args.target as RemovalTarget);
    case "devenv_remove": {
      const plan = preview(args.target as RemovalTarget);
      const index = installations.findIndex((entry) => entry.installationId === (args.target as RemovalTarget).id);
      if (plan.action === "blocked" || index < 0) throw new Error("仍有芯片或其他工具链在使用，无法删除");
      installations.splice(index, 1);
      return state();
    }
    case "devenv_add_local": {
      const id = String(args.toolchain);
      const entry = toolchains.find((item) => item.id === id);
      if (!entry) throw new Error("目录中没有这个工具链");
      installations.push({
        installationId: crypto.randomUUID(),
        kind: "toolchain",
        id,
        name: entry.name,
        version: String(args.version),
        category: entry.category,
        origin: "local",
        requestedByUser: true,
        path: `/opt/${id}`,
        chips: [],
        healthy: true,
      });
      return state();
    }
    default:
      throw new Error(`Unknown devenv command: ${command}`);
  }
}
