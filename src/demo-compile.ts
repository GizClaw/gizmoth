// Explicit browser-demo fixture for the compile service. Nothing here starts a
// VM, opens folders or touches files; builds follow a time-based script.
import type {
  CompileArtifact,
  CompileCache,
  CompileJob,
  CompileJobStatus,
  CompileSettings,
  CompileSlot,
  CompileSlotState,
  CompileSnapshot,
  CompilePackage,
  CompilePackages,
  CompileStorage,
  CompileSync,
  LogChunk,
  StorageItem,
} from "./compile.ts";

const KB = 1024;
const MB = 1024 ** 2;
const GB = 1024 ** 3;
const HOST = { cpus: 12, memoryMiB: 24 * 1024 };
const ROOT = "/Users/demo/Library/Application Support/com.gizclaw.gizmoth/compile";
const DIM = "\x1b[2m";
const RESET = "\x1b[0m";
const note = (text: string) => `${DIM}[gizmoth] ${text}${RESET}\n`;
const active: readonly CompileJobStatus[] = ["queued", "booting", "syncing", "running", "collecting"];
const isActive = (status: CompileJobStatus) => active.includes(status);
const uuid = () => globalThis.crypto.randomUUID();

let start = Date.now();
let version = 1;
let order = 1;

/** The bundled image's apt packages, as in native/linux-image/packages.txt. */
const IMAGE_PACKAGES = [
  "build-essential",
  "ccache",
  "cmake",
  "ninja-build",
  "pkg-config",
  "git",
  "python3",
  "python3-venv",
  "python3-pip",
  "curl",
  "ca-certificates",
  "zip",
  "unzip",
  "xz-utils",
  "file",
  "erofs-utils",
];
const settings: CompileSettings = {
  slots: [],
  caches: {
    bazel: { maxGib: 50 },
    ccache: { maxGib: 20, localGib: 5 },
    custom: [{ name: "idf-components", env: "IDF_COMPONENT_CACHE_URL", maxGib: 10 }],
  },
  network: true,
  idleMinutes: 10,
  bazelrc: ["build --jobs=8"],
  env: [],
  excludes: ["node_modules", "*.o"],
  artifacts: { keepLast: 100, maxGib: 20 },
};

// ---------------------------------------------------------------- storage

/** A simulated move to another volume takes this long. */
const MOVE_MS = 4000;
const storage: {
  root: string;
  error: string | null;
  moving: { from: string; to: string; total: number; startedAt: number } | null;
} = { root: ROOT, error: null, moving: null };

// ---------------------------------------------------------------- packages

/** Registry packages: all installed, while a newly published toolchain
 * downloads and the packer VM adds it to the pack. */
const PACKAGES: Omit<CompilePackage, "path" | "state">[] = [
  {
    kind: "toolchain",
    id: "arm-gnu",
    name: "Arm GNU Toolchain",
    version: "14.3.rel1",
    default: true,
    download: 148 * MB,
  },
  { kind: "toolchain", id: "bazelisk", name: "Bazelisk", version: "1.30.0", default: true, download: 7.2 * MB },
  { kind: "toolchain", id: "bazelisk", name: "Bazelisk", version: "1.29.0", default: false, download: 6.9 * MB },
  { kind: "toolchain", id: "python", name: "Python", version: "3.12.16", default: true, download: 29.4 * MB },
  { kind: "toolchain", id: "python", name: "Python", version: "3.11.18", default: false, download: 28.9 * MB },
];
const PACKAGE_ROOT = "/opt/gizmoth/packages";
const DOWNLOAD_MS = 20_000;
const PACK_MS = 4_000;
const REFRESH_MS = 1_500;
const packages = { active: true, refreshUntil: 0 };
const packagesBusy = (now: number) => now - start < DOWNLOAD_MS + PACK_MS;
const packedBytes = (now: number) => (now - start < DOWNLOAD_MS + PACK_MS ? 110 * MB : 538 * MB);

function packagesView(now: number): CompilePackages {
  const elapsed = now - start;
  const items = PACKAGES.map((item): CompilePackage => {
    const path = `${PACKAGE_ROOT}/toolchains/${item.id}/${item.version}`;
    if (item.id !== "arm-gnu" || elapsed >= DOWNLOAD_MS + PACK_MS) return { ...item, path, state: "installed" };
    if (elapsed >= DOWNLOAD_MS) return { ...item, path, state: "packing" };
    const done = Math.round((item.download * Math.max(0, elapsed)) / DOWNLOAD_MS);
    return { ...item, path, state: "downloading", done, total: item.download };
  });
  const pack: CompilePackages["pack"] =
    elapsed < DOWNLOAD_MS
      ? { state: "stale", bytes: packedBytes(now) }
      : elapsed < DOWNLOAD_MS + PACK_MS
        ? { state: "packing", phase: "Installing toolchains/arm-gnu/14.3.rel1" }
        : { state: "ready", bytes: packedBytes(now), builtAt: start + DOWNLOAD_MS + PACK_MS };
  return {
    active: packages.active,
    busy: packagesBusy(now),
    registry: {
      repository: "https://github.com/GizClaw/gizmoth-devenv",
      commit: "e23bd7a89278dba606321b454889d8126b33f4d6",
      fetchedAt: packages.refreshUntil ? Math.min(now, packages.refreshUntil) : start - 3 * 3600_000,
      refreshing: now < packages.refreshUntil,
      error: null,
    },
    platform: "linux-arm64",
    guestPath: PACKAGE_ROOT,
    file: `${storage.root}/packages.erofs`,
    items,
    pack,
  };
}

// ---------------------------------------------------------------- slots

interface DemoSlot {
  id: string;
  name: string;
  cpus: number;
  memoryMiB: number;
  diskGiB: number;
  state: CompileSlotState;
  error: string | null;
  job: string | null;
  startedAt: number | null;
  lastUsed: number;
  allocated: number;
  used: number | null;
  stopAt: number | null;
}
const slot = (id: string, name: string, allocated: number): DemoSlot => ({
  id,
  name,
  cpus: 4,
  memoryMiB: 6144,
  diskGiB: 64,
  state: "ready",
  error: null,
  job: null,
  startedAt: start - 26 * 60_000,
  lastUsed: start - 4 * 60_000,
  allocated,
  used: allocated,
  stopAt: null,
});
const slots: DemoSlot[] = [slot("s1a2b3c4d", "Slot 1", 12.4 * GB), slot("s5e6f7a8b", "Slot 2", 7.1 * GB)];

// ---------------------------------------------------------------- caches

const usage: Record<string, { bytes: number; files: number; hits: number; misses: number; uploads: number }> = {
  bazel: { bytes: 18.4 * GB, files: 48_213, hits: 92_418, misses: 6_102, uploads: 6_087 },
  ccache: { bytes: 6.2 * GB, files: 31_904, hits: 14_380, misses: 2_210, uploads: 2_198 },
  "idf-components": { bytes: 1.3 * GB, files: 812, hits: 640, misses: 41, uploads: 41 },
};
const usageOf = (name: string) => (usage[name] ??= { bytes: 0, files: 0, hits: 0, misses: 0, uploads: 0 });

// ---------------------------------------------------------------- image

/** Built by CI and bundled with the app; the demo never builds one. */
const image = {
  builtAt: 0,
  commit: "4c3da2f52d38c7a40622915d4544b177b3177610",
};

// ---------------------------------------------------------------- jobs

interface Plan {
  boot: number;
  sync: number;
  run: number;
  collect: number;
  exit: number;
  lines: string[];
  files: { path: string; size: number }[];
  synced: CompileSync;
}
interface Entry {
  job: CompileJob;
  order: number;
  plan: Plan | null;
  dispatchedAt: number | null;
  cancelAt: number | null;
  log: string;
  events: { at: number; text: string }[] | null;
  emitted: number;
}
const entries: Entry[] = [];

const bazelLines = (target: string, output: string) => [
  "Starting local Bazel server and connecting to it...\n",
  "INFO: Invocation ID: 7d1c5e8a-93f2-4c51-b4a0-3e6f0f8b9c21\n",
  `INFO: Analyzed target ${target} (112 packages loaded, 2384 targets configured).\n`,
  "INFO: Found 1 target...\n",
  "[12 / 418] Compiling src/display/amoled.c; 0s remote-cache, linux-sandbox\n",
  "[96 / 418] Compiling third_party/lvgl/src/core/lv_obj.c; 1s remote-cache\n",
  "[211 / 418] Compiling src/net/wifi.c; 2s linux-sandbox\n",
  "[355 / 418] Linking app/firmware.elf; 1s linux-sandbox\n",
  "[417 / 418] Packaging app/firmware.update.tar.zlib; 0s linux-sandbox\n",
  `Target ${target} up-to-date:\n  ${output}\n`,
  "INFO: Elapsed time: 31.402s, Critical Path: 9.87s\n",
  "INFO: 418 processes: 361 remote cache hit, 52 linux-sandbox, 5 internal.\n",
  "INFO: Build completed successfully, 418 total actions\n",
];
const makeLines = [
  "mkdir -p build\n",
  "cc -O2 -Wall -c src/protocol.c -o build/protocol.o\n",
  "cc -O2 -Wall -c src/test_protocol.c -o build/test_protocol.o\n",
  "cc build/protocol.o build/test_protocol.o -o build/test_protocol\n",
  "./build/test_protocol\n",
  "[ RUN      ] frame_round_trip\n[       OK ] frame_round_trip (0 ms)\n",
  "[ RUN      ] rejects_oversize\n[       OK ] rejects_oversize (0 ms)\n",
  "[==========] 24 tests ran. 24 passed.\n",
];
const failingLines = [
  "Starting local Bazel server and connecting to it...\n",
  "INFO: Analyzed target //app:bk7258 (64 packages loaded, 1210 targets configured).\n",
  "[48 / 302] Compiling sdk/bk7258/driver/uart.c; 1s linux-sandbox\n",
  "ERROR: /work/src/1f3a/app/BUILD.bazel:14:10: Compiling app/main.c failed: (Exit 1): gcc failed\n",
  "app/main.c:42:5: error: implicit declaration of function 'bk_wifi_sta_start'\n",
  "   42 |     bk_wifi_sta_start(&config);\n      |     ^~~~~~~~~~~~~~~~~\n",
  "Target //app:bk7258 failed to build\n",
  "INFO: Elapsed time: 18.211s, Critical Path: 4.02s\n",
  "ERROR: Build did NOT complete successfully\n",
];

function newJob(
  partial: Partial<CompileJob> & Pick<CompileJob, "client" | "title" | "workspace" | "command">,
): CompileJob {
  return {
    id: uuid(),
    cwd: "",
    artifacts: [],
    status: "queued",
    slot: null,
    position: null,
    createdAt: Date.now(),
    startedAt: null,
    finishedAt: null,
    exit: null,
    error: null,
    artifact: null,
    synced: null,
    missing: [],
    logBytes: 0,
    ...partial,
  };
}
function add(job: CompileJob, plan: Plan | null, log = "") {
  const entry: Entry = {
    job,
    order: order++,
    plan,
    dispatchedAt: null,
    cancelAt: null,
    log: log || note(`Queued by ${job.client}: ${job.title}`),
    events: null,
    emitted: 0,
  };
  entries.push(entry);
  return entry;
}

// ---------------------------------------------------------------- artifacts

const artifacts: CompileArtifact[] = [];
const artifactDir = (id: string) => `${ROOT}/artifacts/${id}/files`;
function storeArtifact(job: CompileJob, files: { path: string; size: number }[], createdAt: number, pinned = false) {
  const id = uuid();
  const dir = artifactDir(id);
  const artifact: CompileArtifact = {
    id,
    jobId: job.id,
    client: job.client,
    title: job.title,
    workspace: job.workspace,
    command: job.command,
    createdAt,
    pinned,
    files: files.map((file, index) => ({
      ...file,
      sha256: (id.replace(/-/g, "") + index.toString(16).repeat(32)).slice(0, 64),
      absolutePath: `${dir}/${file.path}`,
    })),
    bytes: files.reduce((total, file) => total + file.size, 0),
    dir,
  };
  artifacts.unshift(artifact);
  artifacts.sort((a, b) => b.createdAt - a.createdAt);
  return artifact;
}
function retain() {
  const { keepLast, maxGib } = settings.artifacts;
  let total = artifacts.reduce((sum, artifact) => sum + artifact.bytes, 0);
  let count = artifacts.filter((artifact) => !artifact.pinned).length;
  for (const artifact of [...artifacts].reverse()) {
    if (artifact.pinned) continue;
    if (count <= keepLast && (maxGib === 0 || total <= maxGib * GB)) break;
    artifacts.splice(artifacts.indexOf(artifact), 1);
    count--;
    total -= artifact.bytes;
  }
}

// ---------------------------------------------------------------- fixture

const firmwareFiles = [
  { path: "bazel-bin/app/firmware.update.tar.zlib", size: 2_310_144 },
  { path: "bazel-bin/app/firmware.elf", size: 9_874_212 },
  { path: "bazel-bin/app/firmware.map", size: 1_204_880 },
];
const AMOLED = "/Users/demo/Projects/amoled-firmware";
const PROTOCOL = "/Users/demo/Projects/gizos-protocol";
const BK = "/Users/demo/Projects/bk7258-app";
const bazelCommand = (target: string) => ["bazel", "build", target];

function history() {
  const older = newJob({
    client: "claude-code",
    title: "Build AMOLED firmware 0.9.3",
    workspace: AMOLED,
    command: bazelCommand("//app:firmware"),
    artifacts: ["bazel-bin/app/*.update.tar.zlib", "bazel-bin/app/firmware.elf"],
    status: "succeeded",
    slot: slots[0].id,
    createdAt: start - 26 * 3_600_000,
    startedAt: start - 26 * 3_600_000 + 400,
    finishedAt: start - 26 * 3_600_000 + 88_000,
    exit: { code: 0, signal: null, timedOut: false, cancelled: false },
    synced: { entries: 2184, files: 2184, bytes: 48 * MB, removed: 0 },
  });
  const pinned = storeArtifact(older, firmwareFiles.slice(0, 2), older.finishedAt!, true);
  older.artifact = pinned.id;
  add(
    older,
    null,
    note(`Queued by ${older.client}: ${older.title}`) + bazelLines("//app:firmware", firmwareFiles[0].path).join(""),
  );

  const recent = newJob({
    client: "claude-code",
    title: "Build AMOLED firmware with new display driver",
    workspace: AMOLED,
    command: bazelCommand("//app:firmware"),
    artifacts: ["bazel-bin/app/*.update.tar.zlib", "bazel-bin/app/firmware.elf", "bazel-bin/app/firmware.map"],
    status: "succeeded",
    slot: slots[0].id,
    createdAt: start - 2 * 3_600_000,
    startedAt: start - 2 * 3_600_000 + 300,
    finishedAt: start - 2 * 3_600_000 + 41_000,
    exit: { code: 0, signal: null, timedOut: false, cancelled: false },
    synced: { entries: 2190, files: 14, bytes: 182 * KB, removed: 1 },
  });
  recent.artifact = storeArtifact(recent, firmwareFiles, recent.finishedAt!).id;
  add(
    recent,
    null,
    note(`Queued by ${recent.client}: ${recent.title}`) + bazelLines("//app:firmware", firmwareFiles[0].path).join(""),
  );

  const failed = newJob({
    client: "codex",
    title: "Build BK7258 Wi-Fi sample",
    workspace: BK,
    command: bazelCommand("//app:bk7258"),
    status: "failed",
    slot: slots[1].id,
    createdAt: start - 3 * 3_600_000,
    startedAt: start - 3 * 3_600_000 + 500,
    finishedAt: start - 3 * 3_600_000 + 19_200,
    exit: { code: 1, signal: null, timedOut: false, cancelled: false },
    error: "The command exited with code 1",
    synced: { entries: 9120, files: 9120, bytes: 212 * MB, removed: 0 },
  });
  add(
    failed,
    null,
    note(`Queued by ${failed.client}: ${failed.title}`) +
      failingLines.join("") +
      note("error: The command exited with code 1"),
  );

  const cancelled = newJob({
    client: "gemini-cli",
    title: "Full rebuild after toolchain update",
    workspace: AMOLED,
    command: ["bazel", "build", "--nocache_test_results", "//..."],
    status: "cancelled",
    slot: slots[1].id,
    createdAt: start - 27 * 3_600_000,
    startedAt: start - 27 * 3_600_000 + 600,
    finishedAt: start - 27 * 3_600_000 + 95_000,
    exit: { code: null, signal: 2, timedOut: false, cancelled: true },
  });
  add(
    cancelled,
    null,
    note(`Queued by ${cancelled.client}: ${cancelled.title}`) + note("The WebSocket closed; cancelling"),
  );
}

function scheduled(job: CompileJob, plan: Plan) {
  return add(job, plan);
}
// The scripted builds start with the first compile request, not when the demo
// bridge loads, so the page always opens on running work.
let seeded = false;
function seed() {
  if (seeded) return;
  seeded = true;
  start = Date.now();
  for (const item of slots) {
    item.startedAt = start - 26 * 60_000;
    item.lastUsed = start - 4 * 60_000;
  }
  image.builtAt = start - 2 * 86_400_000;
  history();
  const running = scheduled(
    newJob({
      client: "claude-code",
      title: "Build AMOLED firmware for review",
      workspace: AMOLED,
      command: bazelCommand("//app:firmware"),
      artifacts: ["bazel-bin/app/*.update.tar.zlib", "bazel-bin/app/firmware.elf"],
      createdAt: start - 2500,
    }),
    {
      boot: 0,
      sync: 1500,
      run: 75_000,
      collect: 1500,
      exit: 0,
      lines: bazelLines("//app:firmware", firmwareFiles[0].path),
      files: firmwareFiles.slice(0, 2),
      synced: { entries: 2191, files: 3, bytes: 41 * KB, removed: 0 },
    },
  );
  const tests = scheduled(
    newJob({
      client: "codex",
      title: "Run protocol unit tests",
      workspace: PROTOCOL,
      command: ["make", "test"],
      createdAt: start - 2000,
    }),
    {
      boot: 0,
      sync: 1000,
      run: 40_000,
      collect: 0,
      exit: 0,
      lines: makeLines,
      files: [],
      synced: { entries: 318, files: 318, bytes: 2.4 * MB, removed: 0 },
    },
  );
  scheduled(
    newJob({
      client: "claude-code",
      title: "Build BK7258 firmware",
      workspace: BK,
      command: bazelCommand("//app:bk7258"),
      artifacts: ["bazel-bin/app/*.update.tar.zlib"],
      createdAt: start - 1500,
    }),
    {
      boot: 0,
      sync: 1200,
      run: 30_000,
      collect: 0,
      exit: 1,
      lines: failingLines,
      files: [],
      synced: { entries: 9122, files: 4, bytes: 12 * KB, removed: 0 },
    },
  );
  dispatch(running, slots[0], start - 1800);
  dispatch(tests, slots[1], start - 1200);
}

// ---------------------------------------------------------------- simulation

function dispatch(entry: Entry, target: DemoSlot, at: number) {
  const plan = entry.plan!;
  const cold = target.state !== "ready";
  if (cold) plan.boot = Math.max(plan.boot, 3500);
  target.state = cold ? "starting" : "busy";
  target.job = entry.job.id;
  target.error = null;
  entry.dispatchedAt = at;
  entry.job.slot = target.id;
  entry.job.startedAt = at;
  entry.job.position = null;
  const boot = at + plan.boot;
  const sync = boot + plan.sync;
  const events = [{ at, text: note(`Running on ${target.name}`) }];
  if (cold) {
    events.push({ at, text: note(`Starting VM (${target.cpus} CPU, ${target.memoryMiB} MiB)`) });
    events.push({ at: boot - 1, text: note("VM ready") });
  }
  events.push({
    at: boot,
    text: note(
      `Synchronizing ${entry.job.workspace} (${plan.synced.files} changed, ${plan.synced.removed} removed, ${formatSize(plan.synced.bytes)})`,
    ),
  });
  plan.lines.forEach((text, index) =>
    events.push({ at: sync + Math.round((plan.run * index) / plan.lines.length), text }),
  );
  entry.events = events;
  entry.emitted = 0;
}
function formatSize(bytes: number) {
  if (bytes < MB) return `${(bytes / KB).toFixed(1)} KB`;
  return `${(bytes / MB).toFixed(1)} MB`;
}
function times(entry: Entry) {
  const plan = entry.plan!;
  const boot = entry.dispatchedAt! + plan.boot;
  const sync = boot + plan.sync;
  const run = sync + plan.run;
  const collect = run + (plan.exit === 0 && plan.files.length && entry.job.artifacts.length ? plan.collect : 0);
  const end = Math.min(collect, entry.cancelAt === null ? Infinity : entry.cancelAt + 1200);
  return { boot, sync, run, collect, end };
}
/** Advances one dispatched job to `now`; returns whether it changed. */
function step(entry: Entry, now: number) {
  const { job } = entry;
  const before = `${job.status}:${entry.log.length}`;
  const { boot, sync, run, collect, end } = times(entry);
  const at = Math.min(now, end);
  const events = entry.events ?? [];
  while (entry.emitted < events.length && events[entry.emitted].at <= at) entry.log += events[entry.emitted++].text;
  const target = slots.find((item) => item.id === job.slot);
  if (at < boot) job.status = "booting";
  else if (at < sync) job.status = "syncing";
  else if (at < run) job.status = "running";
  else if (at < collect) job.status = "collecting";
  if (target && target.state === "starting" && at >= boot) {
    target.state = "busy";
    target.startedAt = boot;
  }
  if (at >= sync) job.synced = entry.plan!.synced;
  if (now >= end) finish(entry, end, target);
  return before !== `${job.status}:${entry.log.length}`;
}
function finish(entry: Entry, at: number, target: DemoSlot | undefined) {
  const { job, plan } = entry;
  job.finishedAt = at;
  if (entry.cancelAt !== null && at < times(entry).collect) {
    const started = at > times(entry).sync;
    job.status = "cancelled";
    job.exit = started ? { code: null, signal: 2, timedOut: false, cancelled: true } : null;
    entry.log += started ? "^C\nINFO: Build interrupted.\n" : "";
  } else {
    const code = plan!.exit;
    job.exit = { code, signal: null, timedOut: false, cancelled: false };
    job.status = code === 0 ? "succeeded" : "failed";
    if (code !== 0) {
      job.error = `The command exited with code ${code}`;
      entry.log += note(`error: ${job.error}`);
    } else if (plan!.files.length && job.artifacts.length) {
      const artifact = storeArtifact(job, plan!.files, at);
      job.artifact = artifact.id;
      entry.log += note(`Collected ${plan!.files.length} artifact file(s), ${formatSize(artifact.bytes)}`);
      retain();
    }
    const bazel = usageOf("bazel");
    if (job.command[0] === "bazel") {
      bazel.hits += 361;
      bazel.misses += 57;
      bazel.uploads += 52;
      bazel.files += 52;
      bazel.bytes += 38 * MB;
    } else {
      const ccache = usageOf("ccache");
      ccache.hits += 2;
      ccache.misses += 1;
      ccache.uploads += 1;
      ccache.files += 1;
      ccache.bytes += 0.4 * MB;
    }
  }
  if (target && target.job === job.id) {
    target.job = null;
    target.state = "ready";
    target.lastUsed = at;
    target.allocated += 0.2 * GB;
    target.used = target.allocated;
  }
}
function advance(now = Date.now()) {
  let changed = false;
  for (const item of slots) {
    if (item.state === "stopping" && item.stopAt !== null && now >= item.stopAt) {
      item.state = "stopped";
      item.startedAt = null;
      item.stopAt = null;
      changed = true;
    }
  }
  // Package downloads and the refresh move every step.
  if (now - start < DOWNLOAD_MS + PACK_MS + 1000 || now < packages.refreshUntil + 1000) changed = true;
  if (storage.moving) {
    // Progress is part of the snapshot, so every step is a change.
    changed = true;
    if (now - storage.moving.startedAt >= MOVE_MS) {
      storage.root = storage.moving.to;
      storage.moving = null;
    }
  }
  // Finishing frees a slot at its finish time; queued builds start there.
  for (let rounds = 0; rounds < 32; rounds++) {
    for (const entry of entries) {
      if (entry.dispatchedAt !== null && isActive(entry.job.status) && step(entry, now)) changed = true;
    }
    const queued = entries.filter((entry) => entry.job.status === "queued").sort((a, b) => a.order - b.order);
    let started = false;
    for (const entry of queued) {
      const free =
        slots.filter((item) => item.state === "ready").sort((a, b) => a.lastUsed - b.lastUsed)[0] ??
        slots.find((item) => item.state === "stopped" || item.state === "failed");
      if (!free) break;
      dispatch(entry, free, Math.max(entry.job.createdAt, free.state === "ready" ? free.lastUsed : now, start));
      started = changed = true;
    }
    if (!started) break;
  }
  if (changed) version++;
}

// ---------------------------------------------------------------- views

const summary = (entry: Entry): CompileJob => ({ ...structuredClone(entry.job), logBytes: entry.log.length });
function slotView(item: DemoSlot): CompileSlot {
  const entry = entries.find((candidate) => candidate.job.id === item.job);
  return {
    id: item.id,
    name: item.name,
    cpus: item.cpus,
    memoryMiB: item.memoryMiB,
    diskGiB: item.diskGiB,
    state: item.state,
    error: item.error,
    job: entry
      ? {
          id: entry.job.id,
          client: entry.job.client,
          title: entry.job.title,
          status: entry.job.status,
          startedAt: entry.job.startedAt,
        }
      : null,
    startedAt: item.startedAt,
    lastUsed: item.lastUsed || null,
    disk: {
      allocated: item.allocated,
      size: item.diskGiB * GB,
      used: item.used,
      free: item.used === null ? null : item.diskGiB * GB - item.used,
    },
  };
}
function cacheRows(): CompileCache[] {
  const row = (name: string, kind: CompileCache["kind"], maxGib: number, env: string | null) => ({
    name,
    kind,
    maxBytes: maxGib * GB,
    env,
    ...usageOf(name),
  });
  return [
    row("bazel", "bazel", settings.caches.bazel.maxGib, null),
    row("ccache", "ccache", settings.caches.ccache.maxGib, null),
    ...settings.caches.custom.map((cache) => row(cache.name, "custom", cache.maxGib, cache.env)),
  ];
}
function storageView(now: number): CompileStorage {
  const item = (id: StorageItem["id"], dir: string, bytes: number): StorageItem => ({
    id,
    path: `${storage.root}/${dir}`,
    bytes,
    exists: bytes > 0,
  });
  const items = [
    item(
      "slots",
      "slots",
      slots.reduce((total, slot) => total + slot.allocated, 0),
    ),
    item(
      "caches",
      "cache",
      cacheRows().reduce((total, cache) => total + cache.bytes, 0),
    ),
    item(
      "artifacts",
      "artifacts",
      artifacts.reduce((total, artifact) => total + artifact.bytes, 0),
    ),
    item("packages", "packages.erofs", packedBytes(now)),
    item("assets", "assets", 0.4 * GB),
  ];
  const moving = storage.moving
    ? {
        ...storage.moving,
        phase: "moving" as const,
        done: Math.min(
          storage.moving.total,
          Math.round((storage.moving.total * (now - storage.moving.startedAt)) / MOVE_MS),
        ),
      }
    : null;
  return {
    root: storage.root,
    default: ROOT,
    isDefault: storage.root === ROOT,
    available: true,
    free: 412 * GB,
    error: storage.error,
    moving,
    items,
  };
}
function snapshot(): CompileSnapshot {
  const now = Date.now();
  const queued = entries.filter((entry) => entry.job.status === "queued").sort((a, b) => a.order - b.order);
  queued.forEach((entry, index) => (entry.job.position = index + 1));
  return {
    protocol: "gizmoth.compile.v1",
    version,
    available: { ok: true },
    host: { ...HOST },
    settings: structuredClone({
      ...settings,
      slots: slots.map((item) => ({
        id: item.id,
        name: item.name,
        cpus: item.cpus,
        memoryMib: item.memoryMiB,
        diskGib: item.diskGiB,
      })),
    }),
    image: {
      status: "ready",
      error: null,
      bytes: 297 * MB,
      builtAt: image.builtAt,
      commit: image.commit,
      base: "Ubuntu 26.04.1 base (arm64)",
      kernel: "Linux 6.18 (openvmm-deps 0.3.0-185)",
      bazelisk: "Bazelisk 1.30.0 (linux-arm64)",
      packages: [...IMAGE_PACKAGES],
    },
    slots: slots.map(slotView),
    queue: queued.map(summary),
    jobs: [...entries]
      .sort((a, b) => b.order - a.order)
      .slice(0, 60)
      .map(summary),
    caches: cacheRows(),
    artifacts: {
      count: artifacts.length,
      bytes: artifacts.reduce((total, artifact) => total + artifact.bytes, 0),
      dir: `${storage.root}/artifacts`,
    },
    storage: storageView(now),
    packages: packagesView(now),
  };
}
function readLog(text: string, after: number | null, limit: number): LogChunk {
  const end = text.length;
  const from = after === null ? Math.max(0, end - limit) : Math.min(Math.max(after, 0), end);
  const stop = Math.min(end, from + limit);
  return { text: text.slice(from, stop), next: stop, truncated: false };
}

// ---------------------------------------------------------------- requests

class RequestError extends Error {}
const fail = (message: string): never => {
  throw new RequestError(message);
};
function findSlot(id: string) {
  return slots.find((item) => item.id === id) ?? fail("Slot not found");
}
function validateSlot(name: string, cpus: number, memoryMiB: number, diskGiB: number) {
  if (!name.trim() || [...name].length > 40) fail("Slot names must have 1–40 characters");
  if (!Number.isInteger(cpus) || cpus < 1 || cpus > HOST.cpus) fail(`A slot can use 1–${HOST.cpus} CPUs`);
  if (memoryMiB < 1024 || memoryMiB > HOST.memoryMiB - 1024)
    fail("A slot needs at least 1 GiB and must leave 1 GiB for macOS");
  if (diskGiB < 8 || diskGiB > 4096) fail("Work disks can be 8–4096 GiB");
}
function idleSlot(id: string) {
  const item = findSlot(id);
  if (item.state === "busy" || item.state === "starting") fail("The slot is running a build; cancel it first");
  return item;
}
function patchSettings(body: Record<string, unknown>) {
  if (!body || typeof body !== "object" || Array.isArray(body)) fail("Expected an object");
  const next = structuredClone(settings) as unknown as Record<string, unknown>;
  for (const [key, value] of Object.entries(body)) {
    if (key === "slots") fail("Use the slot endpoints to change slots");
    if (!(key in next)) fail(`Unknown setting ${key}`);
    next[key] = value;
  }
  const merged = next as unknown as CompileSettings;
  const names = ["bazel", "ccache"];
  for (const cache of merged.caches.custom) {
    if (!/^[a-z0-9-]{1,32}$/.test(cache.name) || names.includes(cache.name))
      fail(`Invalid or duplicate cache name ${cache.name}`);
    if (!/^[A-Za-z_][A-Za-z0-9_]{0,127}$/.test(cache.env)) fail(`Invalid environment variable ${cache.env}`);
    names.push(cache.name);
  }
  if (!(merged.idleMinutes >= 1 && merged.idleMinutes <= 1440))
    fail("Idle shutdown must be between 1 minute and 24 hours");
  if (merged.bazelrc.some((line) => /[\n\r\0]/.test(line))) fail("Invalid bazelrc lines");
  if (!merged.env.every((variable) => /^[A-Za-z_][A-Za-z0-9_]*$/.test(variable.name)))
    fail("Invalid environment variables");
  if (!(merged.artifacts.keepLast >= 1 && merged.artifacts.keepLast <= 10_000))
    fail("Keep between 1 and 10000 artifacts");
  Object.assign(settings, merged, { slots: [] });
  retain();
}

function route(method: string, parts: string[], query: URLSearchParams, body: unknown): unknown {
  const param = (key: string) => (query.has(key) ? Number(query.get(key)) : null);
  const object = (body ?? {}) as Record<string, unknown>;
  // Collections take their item ID (or cache name) as the second segment.
  const id = ["artifacts", "jobs", "slots", "caches"].includes(parts[0]) && parts.length > 1 ? parts[1] : "";
  const key = `${method} ${parts.map((part, index) => (index === 1 && id ? ":id" : part)).join("/")}`;
  if (storage.moving && method !== "GET" && !["POST storage/cancel", "POST jobs/:id/cancel"].includes(key))
    fail("The working directory is being moved; try again when it finishes");
  switch (key) {
    case "GET ":
      return snapshot();
    case "GET artifacts":
      return artifacts.map((artifact) => structuredClone(artifact));
    case "GET artifacts/:id":
      return structuredClone(artifacts.find((artifact) => artifact.id === id) ?? fail("Artifact not found"));
    case "GET jobs/:id":
      return summary(entries.find((entry) => entry.job.id === id) ?? fail("Job not found"));
    case "GET jobs/:id/log": {
      const entry = entries.find((candidate) => candidate.job.id === id) ?? fail("Job not found");
      return readLog(entry.log, param("after"), Math.min(param("limit") ?? 256 * KB, MB));
    }
    case "POST jobs/:id/cancel": {
      const entry = entries.find((candidate) => candidate.job.id === id) ?? fail("Job not found");
      if (entry.job.status === "queued") {
        entry.job.status = "cancelled";
        entry.job.finishedAt = Date.now();
        entry.log += note("Cancelled in GizMoth");
      } else if (isActive(entry.job.status) && entry.cancelAt === null) {
        entry.cancelAt = Date.now();
        entry.log += note("Cancelled in GizMoth");
      }
      version++;
      return summary(entry);
    }
    case "PATCH settings":
      patchSettings(object);
      version++;
      return snapshot();
    case "POST slots": {
      if (slots.length >= 16) fail("At most 16 slots");
      const template = slots.at(-1) ?? slot("s00000000", "Slot", 0);
      const name = typeof object.name === "string" ? object.name.trim() : `Slot ${slots.length + 1}`;
      const cpus = Number(object.cpus ?? template.cpus);
      const memoryMiB = Number(object.memoryMiB ?? template.memoryMiB);
      const diskGiB = Number(object.diskGiB ?? template.diskGiB);
      validateSlot(name, cpus, memoryMiB, diskGiB);
      slots.push({
        ...slot(`s${uuid().replace(/-/g, "").slice(0, 8)}`, name, 0),
        cpus,
        memoryMiB,
        diskGiB,
        state: "stopped",
        startedAt: null,
        lastUsed: 0,
        used: null,
      });
      version++;
      return snapshot();
    }
    case "PATCH slots/:id": {
      const item = findSlot(id);
      const resources = ["cpus", "memoryMiB", "diskGiB"].some((field) => field in object);
      if (resources && item.state !== "stopped" && item.state !== "failed")
        fail("Stop the slot before changing its resources");
      const name = typeof object.name === "string" ? object.name.trim() : item.name;
      const cpus = Number(object.cpus ?? item.cpus);
      const memoryMiB = Number(object.memoryMiB ?? item.memoryMiB);
      const diskGiB = Number(object.diskGiB ?? item.diskGiB);
      if (diskGiB !== item.diskGiB && item.allocated > 0) fail("Reset the work disk before changing its size");
      validateSlot(name, cpus, memoryMiB, diskGiB);
      Object.assign(item, { name, cpus, memoryMiB, diskGiB });
      version++;
      return snapshot();
    }
    case "DELETE slots/:id": {
      const item = idleSlot(id);
      slots.splice(slots.indexOf(item), 1);
      version++;
      return snapshot();
    }
    case "POST slots/:id/stop": {
      const item = idleSlot(id);
      if (item.state === "ready" || item.state === "stopping") {
        item.state = "stopping";
        item.stopAt = Date.now() + 1500;
      }
      version++;
      return snapshot();
    }
    case "POST slots/:id/reset": {
      const item = idleSlot(id);
      Object.assign(item, { state: "stopped", startedAt: null, stopAt: null, allocated: 0, used: null });
      version++;
      return snapshot();
    }
    case "DELETE caches/:id": {
      if (!/^[a-z0-9-]{1,32}$/.test(id)) fail("invalid cache name");
      Object.assign(usageOf(id), { bytes: 0, files: 0 });
      version++;
      return snapshot();
    }
    case "PATCH artifacts/:id": {
      if (typeof object.pinned !== "boolean") fail("pinned must be a boolean");
      const artifact = artifacts.find((candidate) => candidate.id === id) ?? fail("Artifact not found");
      artifact.pinned = object.pinned as boolean;
      version++;
      return structuredClone(artifact);
    }
    case "POST storage": {
      const target = object.default === true ? ROOT : String(object.path ?? "").replace(/\/+$/, "");
      if (storage.moving) fail("The working directory is already being moved");
      if (!target.startsWith("/")) fail("The working directory must be an absolute path");
      if (entries.some((entry) => isActive(entry.job.status)))
        fail("Wait for builds to finish before moving the working directory");
      if (packagesBusy(Date.now())) fail("Packages are being installed or packed; try again when they finish");
      if (target === storage.root) fail("This is already the working directory");
      if (target.startsWith(`${storage.root}/`) || (storage.root.startsWith(`${target}/`) && target !== ROOT))
        fail("The new working directory cannot be inside the current one, or contain it");
      const total = storageView(Date.now()).items.reduce((sum, item) => sum + item.bytes, 0);
      for (const item of slots) {
        if (item.state === "ready") Object.assign(item, { state: "stopped", startedAt: null });
      }
      storage.error = null;
      storage.moving = { from: storage.root, to: target, total, startedAt: Date.now() };
      version++;
      return snapshot();
    }
    case "POST prepare":
      packages.active = true;
      version++;
      return snapshot();
    case "POST packages/refresh":
      packages.active = true;
      packages.refreshUntil = Date.now() + REFRESH_MS;
      version++;
      return snapshot();
    case "POST storage/cancel":
      if (storage.moving) {
        storage.moving = null;
        storage.error = "cancelled";
        version++;
      }
      return snapshot();
    case "DELETE artifacts/:id": {
      const index = artifacts.findIndex((candidate) => candidate.id === id);
      if (index < 0) fail("Artifact not found");
      artifacts.splice(index, 1);
      version++;
      return { deleted: id };
    }
  }
  return fail("not found");
}

/** Paths revealed in Finder, for tests; the demo never opens folders. */
export const revealed: string[] = [];

export function demoCompile(command: string, args: Record<string, unknown> = {}): unknown {
  seed();
  advance();
  switch (command) {
    case "compile_request": {
      const url = new URL(String(args.path ?? ""), "http://compile.invalid");
      const parts = url.pathname.split("/").filter(Boolean).map(decodeURIComponent);
      if (parts[0] !== "v1" || parts[1] !== "compile") fail("Invalid compile path");
      // Native IPC serializes bodies; never keep a reference to the caller's object.
      const body = args.body === undefined ? null : structuredClone(args.body);
      const result = route(String(args.method ?? "GET"), parts.slice(2), url.searchParams, body);
      return structuredClone(result);
    }
    case "compile_watch": {
      const since = Number(args.since ?? 0);
      return new Promise((resolve) => {
        const begun = Date.now();
        const wait = () => {
          advance();
          if (version > since || Date.now() - begun > 4000) resolve(snapshot());
          else setTimeout(wait, 250);
        };
        wait();
      });
    }
    case "compile_reveal":
      revealed.push(String(args.path ?? ""));
      return null;
    case "compile_choose_directory":
      // The demo never opens a folder picker; it offers an external disk.
      return "/Volumes/Build SSD/GizMoth";
  }
  return fail(`Unknown compile command ${command}`);
}
