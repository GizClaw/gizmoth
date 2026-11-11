// Explicit browser-demo fixture. No downloads, processes, folder opening or
// filesystem changes happen here; real browsing uses devenv-browser.ts instead.
import type {
  DevEnvChip,
  DevEnvEnvironment,
  DevEnvInstallation,
  DevEnvPackage,
  DevEnvState,
  DevEnvToolchain,
  DevEnvVersion,
  RemovalPreview,
  RemovalTarget,
} from "./devenv.ts";

const planned = { code: "planned" } as const;
const plannedReason = "配方待补，暂时无法安装";
const chip = (id: string, name: string, vendor: string, sdk: string): DevEnvChip => ({
  id,
  name,
  vendor,
  sdk,
  description: `${vendor} ${name}`,
  homepage: null,
  notes: null,
  status: "planned",
  blocked: planned,
  reason: plannedReason,
  version: null,
  packages: [],
  installationId: null,
  path: null,
  reinstallable: false,
});
const chips = [
  chip("esp32s3", "ESP32-S3", "Espressif", "ESP-IDF"),
  chip("bk7258", "BK7258", "Beken", "Beken BK7258 SDK"),
  chip("ac707n", "AC707N", "JieLi", "JieLi AC707N SDK"),
];
const available = (version: string): DevEnvVersion => ({ version, status: "available", blocked: null, reason: null });
const tool = (
  id: string,
  name: string,
  category: DevEnvToolchain["category"],
  description: string,
  versions: string[] = [],
): DevEnvToolchain => ({
  id,
  name,
  category,
  description,
  homepage: "https://github.com/GizClaw/gizmoth-devenv",
  notes: null,
  version: versions[0] ?? null,
  versions: versions.map(available),
  status: versions.length ? "available" : "planned",
  blocked: versions.length ? null : planned,
  reason: versions.length ? null : plannedReason,
});
const toolchains = [
  tool(
    "bazelisk",
    "Bazel (Bazelisk)",
    "build-tool",
    "Runs the Bazel version selected by the project's .bazelversion.",
    ["1.30.0", "1.29.0"],
  ),
  tool("python", "Python", "runtime", "Managed Python runtimes. Different versions can be installed side by side.", [
    "3.12.16",
    "3.11.18",
  ]),
  tool("arm-gnu", "Arm GNU Toolchain", "compiler", "Arm cross-compilers shared by Beken SDKs."),
  tool("xtensa-esp-elf", "Xtensa ESP Toolchain", "compiler", "Espressif's GCC toolchain for Xtensa-based ESP32 chips."),
  tool("jieli", "JieLi Toolchain", "compiler", "JieLi compiler and post-build tools."),
];
type Package = { id: string; version: string };
const gizosDependencies: Package[] = [
  { id: "bazelisk", version: "1.30.0" },
  { id: "python", version: "3.12.16" },
];
const installations: DevEnvInstallation[] = [];
const installedEnvironments = new Map<string, string[]>();
let job: DevEnvState["job"] = null;
let pending: { packages: Package[]; requested: boolean; environment?: string; replacementId?: string } | null = null;
const DURATION = 3200;

function ensureIdle() {
  if (job?.state === "running") throw new Error("另一个开发环境任务正在进行");
}
function getTool(id: string) {
  const result = toolchains.find((item) => item.id === id);
  if (!result) throw new Error("目录中没有这个工具链");
  return result;
}
function find({ id, version }: Package) {
  return installations.find((item) => item.id === id && item.version === version);
}
function bytes(pkg: Package) {
  if (pkg.id === "python") return pkg.version === "3.12.16" ? 25_237_581 : 26_982_722;
  return pkg.version === "1.30.0" ? 7_402_354 : 6_659_202;
}
function users(installationId: string) {
  return [...installedEnvironments]
    .filter(([, roots]) => roots.includes(installationId))
    .map(([id]) => (id === "gizos" ? "GizOS" : id));
}
function schedule(
  kind: "toolchain" | "environment",
  target: string,
  packages: Package[],
  replacement?: DevEnvInstallation,
) {
  ensureIdle();
  for (const pkg of packages) {
    if (!getTool(pkg.id).versions.some((release) => release.version === pkg.version)) throw new Error(plannedReason);
  }
  pending = {
    packages,
    requested: kind === "toolchain",
    environment: kind === "environment" ? target : undefined,
    replacementId: replacement?.installationId,
  };
  const missing = packages.filter((pkg) => replacement || !find(pkg));
  job = {
    id: crypto.randomUUID(),
    kind,
    target,
    name: kind === "environment" ? "GizOS" : getTool(target).name,
    version: kind === "toolchain" ? packages[0].version : null,
    phase: "resolving",
    state: "running",
    received: 0,
    total: missing.reduce((total, pkg) => total + bytes(pkg), 0),
    log: packages.map((pkg) => `${replacement ? "重新安装" : "安装"} ${getTool(pkg.id).name} ${pkg.version}`),
    error: null,
    startedAt: Date.now(),
    finishedAt: null,
  };
}

function advance() {
  if (!job || job.state !== "running" || !pending) return;
  const progress = Math.min(1, (Date.now() - job.startedAt) / DURATION);
  job.received = Math.round(job.total * Math.min(1, progress / 0.7));
  job.phase =
    progress < 0.7
      ? "downloading"
      : progress < 0.85 && pending.packages.some((pkg) => pkg.id === "python")
        ? "extracting"
        : progress < 0.9
          ? "installing"
          : progress < 1
            ? "checking"
            : "done";
  if (progress < 1) return;
  const roots: string[] = [];
  for (const pkg of pending.packages) {
    const previous = pending.replacementId
      ? installations.find((item) => item.installationId === pending!.replacementId)
      : null;
    const existing = !previous ? find(pkg) : null;
    if (existing) {
      roots.push(existing.installationId);
      continue;
    }
    const item = getTool(pkg.id);
    const installationId = crypto.randomUUID();
    const record: DevEnvInstallation = {
      installationId,
      kind: "toolchain",
      id: pkg.id,
      name: item.name,
      version: pkg.version,
      category: item.category,
      origin: "managed",
      requestedByUser: previous?.requestedByUser ?? pending.requested,
      path: `/Users/example/Library/Application Support/com.gizclaw.gizmoth/devenv/installs/toolchains/${pkg.id}/${pkg.version}/macos-arm64/${installationId}`,
      chips: [],
      environments: [],
      healthy: true,
      reinstallable: true,
    };
    if (previous) {
      installations.splice(installations.indexOf(previous), 1, record);
      for (const [id, refs] of installedEnvironments)
        installedEnvironments.set(
          id,
          refs.map((ref) => (ref === previous.installationId ? installationId : ref)),
        );
    } else installations.push(record);
    roots.push(installationId);
  }
  if (pending.environment) installedEnvironments.set(pending.environment, roots);
  pending = null;
  job.state = "succeeded";
  job.finishedAt = Date.now();
  job.log.push("完成。");
}

function state(): DevEnvState {
  advance();
  const packages: DevEnvPackage[] = gizosDependencies.map((pkg) => ({
    kind: "toolchain",
    ...pkg,
    name: getTool(pkg.id).name,
    installed: !!find(pkg),
  }));
  const environment: DevEnvEnvironment = {
    id: "gizos",
    name: "GizOS",
    description: "Base host tools for GizOS development: Bazel through Bazelisk and a managed Python runtime.",
    homepage: "https://github.com/GizClaw/gizos",
    notes: null,
    status: installedEnvironments.has("gizos")
      ? "installed"
      : job?.state === "running" && job.kind === "environment"
        ? "installing"
        : "available",
    blocked: null,
    reason: null,
    packages,
  };
  return structuredClone({
    host: "macos-arm64",
    registry: {
      repository: "https://github.com/GizClaw/gizmoth-devenv",
      commit: "e23bd7a89278dba606321b454889d8126b33f4d6",
      fetchedAt: Date.now() - 240_000,
    },
    error: null,
    chips,
    environments: [environment],
    toolchains: toolchains.map((item) => {
      const versions = item.versions.map((release) => ({
        ...release,
        status: find({ id: item.id, version: release.version })
          ? ("installed" as const)
          : job?.state === "running" && job.target === item.id && job.version === release.version
            ? ("installing" as const)
            : release.status,
      }));
      const root = versions.find((release) => release.version === item.version);
      return { ...item, versions, status: root?.status ?? item.status };
    }),
    installations: installations.map((record) => ({ ...record, environments: users(record.installationId) })),
    job,
  });
}

function preview(target: RemovalTarget): RemovalPreview {
  if (target.kind === "environment") {
    const roots = installedEnvironments.get(target.id);
    if (!roots) throw new Error("环境未安装");
    const records = roots.map((id) => installations.find((record) => record.installationId === id)!);
    const remaining = [...installedEnvironments].filter(([id]) => id !== target.id).flatMap(([, refs]) => refs);
    const keep = records.filter((record) => record.requestedByUser || remaining.includes(record.installationId));
    const remove = records.filter((record) => !keep.includes(record));
    return {
      action: "delete",
      delete: remove,
      keep,
      chips: [],
      environments: [],
      dependants: [],
      confirm: remove.map((record) => record.installationId),
    };
  }
  const record = installations.find((item) => item.installationId === target.id);
  if (target.kind !== "installation" || !record) throw new Error("未知安装");
  const environments = users(record.installationId);
  if (environments.length)
    return { action: "blocked", delete: [], keep: [], chips: [], environments, dependants: [], confirm: [] };
  return record.origin === "local"
    ? { action: "unregister", delete: [], keep: [record], chips: [], environments: [], dependants: [], confirm: [] }
    : {
        action: "delete",
        delete: [record],
        keep: [],
        chips: [],
        environments: [],
        dependants: [],
        confirm: [record.installationId],
      };
}

export function demoDevEnv(command: string, args: Record<string, unknown>): unknown {
  switch (command) {
    case "devenv_state":
    case "devenv_refresh":
      return state();
    case "devenv_install": {
      ensureIdle();
      const target = args.target as { kind: string; id: string; version?: string };
      if (target.kind === "environment" && target.id === "gizos") {
        if (installedEnvironments.has("gizos")) throw new Error("环境已安装");
        schedule("environment", target.id, gizosDependencies);
      } else if (target.kind === "toolchain") {
        const item = getTool(target.id);
        const version = target.version ?? item.version;
        if (!version) throw new Error(plannedReason);
        if (find({ id: item.id, version })) throw new Error("工具链已安装");
        schedule("toolchain", item.id, [{ id: item.id, version }]);
      } else throw new Error(plannedReason);
      return state();
    }
    case "devenv_reinstall": {
      ensureIdle();
      const target = args.target as RemovalTarget;
      const record = installations.find((item) => item.installationId === target.id);
      if (target.kind !== "installation" || !record) throw new Error("未知安装");
      if (record.origin !== "managed" || !record.reinstallable) throw new Error("本地工具链不能由 GizMoth 重新安装。");
      schedule("toolchain", record.id, [{ id: record.id, version: record.version }], record);
      return state();
    }
    case "devenv_open_folder":
      if (!installations.some((record) => record.installationId === args.installationId)) throw new Error("未知安装");
      return undefined;
    case "devenv_cancel":
      if (job?.state === "running" && job.id === args.jobId) {
        job.state = "cancelled";
        job.finishedAt = Date.now();
        pending = null;
        job.log.push("已取消。已完成的安装会保留并在下次复用。");
      }
      return state();
    case "devenv_remove_preview":
      return preview(args.target as RemovalTarget);
    case "devenv_remove": {
      ensureIdle();
      const target = args.target as RemovalTarget;
      const plan = preview(target);
      if (plan.action === "blocked") throw new Error("仍有芯片或其他工具链在使用，无法删除");
      if (JSON.stringify([...(args.confirm as string[])].sort()) !== JSON.stringify([...plan.confirm].sort()))
        throw new Error("删除内容已变化，请重新确认");
      if (target.kind === "environment") installedEnvironments.delete(target.id);
      const remove = plan.action === "unregister" ? [target.id] : plan.confirm;
      for (const id of remove) {
        const index = installations.findIndex((record) => record.installationId === id);
        if (index >= 0) installations.splice(index, 1);
      }
      return state();
    }
    case "devenv_add_local": {
      ensureIdle();
      const item = getTool(String(args.toolchain));
      installations.push({
        installationId: crypto.randomUUID(),
        kind: "toolchain",
        id: item.id,
        name: item.name,
        version: String(args.version),
        category: item.category,
        origin: "local",
        requestedByUser: true,
        path: `/opt/${item.id}`,
        chips: [],
        environments: [],
        healthy: true,
        reinstallable: false,
      });
      return state();
    }
    default:
      throw new Error(`Unknown devenv command: ${command}`);
  }
}
