// Mounts the real App in the browser demo with an isolated, in-memory artifact store.
// Pin and delete calls only change this page's fixture; nothing reaches a GizMoth service.
// The language is chosen before any App module reads its first-paint preference.
const params = new URLSearchParams(location.search);
localStorage.setItem("gizmoth.language", params.get("lang") === "en" ? "en" : "zh-CN");
const { demo } = await import("/src/bridge.ts");
const { compile } = await import("/src/compile.ts");
if (!demo) throw new Error("Open this fixture with ?demo=1.");
history.replaceState(null, "", "/?demo=1");

const now = Date.now();
const MINUTE = 60_000;
const file = (dir, path, size, index) => ({
  path,
  size,
  sha256: String(index).repeat(64).slice(0, 64),
  absolutePath: `${dir}/${path}`,
});
const artifact = (id, title, age, paths, pinned = false) => {
  const dir = `/fixture/artifacts/${id}/files`;
  const files = paths.map(([path, size], index) => file(dir, path, size, index));
  return {
    id,
    jobId: `job-${id}`,
    client: index % 2 ? "codex" : "claude-code",
    title,
    workspace: "/fixture/workspace",
    command: ["bazel", "build", "//app:firmware"],
    createdAt: now - age,
    pinned,
    files,
    bytes: files.reduce((total, item) => total + item.size, 0),
    dir,
  };
};
let index = 0;
const firmware = [
  ["bazel-bin/app/firmware.update.tar.zlib", 2_310_144],
  ["bazel-bin/app/firmware.elf", 9_874_212],
  ["bazel-bin/app/firmware.map", 1_204_880],
];
const store = [
  [
    "a-long",
    "Build AMOLED firmware with the new display driver, extended diagnostics, verbose boot logging and a deliberately long title that must truncate",
    40_000,
    firmware,
    true,
  ],
  [
    "b-docs",
    "Protocol docs",
    2 * 60 * MINUTE,
    [
      ["out/protocol.pdf", 812_000],
      ["out/manifest.json", 1_200],
    ],
  ],
  ["c-bk", "Build BK7258 firmware", 26 * 60 * MINUTE, [firmware[0]]],
  ["d-logs", "Export crash logs", 3 * 24 * 60 * MINUTE, [["logs/crash.txt", 54_000]], true],
  ["e-amoled", "Build AMOLED firmware 0.9.3", 5 * 24 * 60 * MINUTE, firmware.slice(0, 2)],
  [
    "f-tests",
    "Run protocol unit tests and collect coverage reports",
    9 * 24 * 60 * MINUTE,
    [["coverage/index.html", 98_000]],
  ],
  ["g-bk", "BK", 20 * 24 * 60 * MINUTE, [firmware[0], ["bazel-bin/app/bk7258.bin", 1_800_000]]],
  ["h-map", "Linker map only", 40 * 24 * 60 * MINUTE, [["bazel-bin/app/firmware.map", 1_204_880]]],
].map(([id, title, age, paths, pinned]) => {
  index++;
  return artifact(id, title, age, paths, pinned);
});
// `many=1`: 130 artifacts. A demo build's own artifact ID sits at index 55, past
// the first 50-row page, so its log's "View artifact" exercises paging.
let target = null;
if (params.get("many") === "1") {
  const { jobs } = await compile.snapshot();
  const job = jobs.find((item) => item.artifact && item.title === "Build AMOLED firmware 0.9.3");
  if (!job) throw new Error("The demo build with an artifact is missing.");
  store.length = 0;
  for (let position = 0; position < 130; position++) {
    index = position;
    const id = position === 55 ? job.artifact : `n-${String(position).padStart(3, "0")}`;
    const title = position === 55 ? job.title : `Nightly build ${String(position).padStart(3, "0")}`;
    const paths = position % 3 ? [firmware[0]] : [["logs/build.txt", 4_000 + position]];
    store.push(artifact(id, title, (position + 1) * 60 * MINUTE, paths));
  }
  target = { id: job.artifact, jobTitle: job.title, index: 55 };
}
const calls = [];
window.__artifactFixture = { calls, target, ids: () => store.map((item) => item.id) };
const copy = () => structuredClone(store);
compile.artifacts = async () => copy();
compile.pinArtifact = async (id, pinned) => {
  calls.push(["pin", id, pinned]);
  const item = store.find((candidate) => candidate.id === id);
  if (!item) throw new Error(`Artifact ${id} not found`);
  item.pinned = pinned;
  return structuredClone(item);
};
compile.deleteArtifact = async (id) => {
  calls.push(["delete", id]);
  const position = store.findIndex((candidate) => candidate.id === id);
  if (position < 0) throw new Error(`Artifact ${id} not found`);
  store.splice(position, 1);
  return { deleted: id };
};
compile.reveal = async (path) => {
  calls.push(["reveal", path]);
};

await import("/src/preferences-init.ts");
await import("/src/main.tsx");
