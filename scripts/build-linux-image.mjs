// Builds GizMoth's Linux system image for compile VMs (native/linux-image)
// and stages it as a bundle resource: system.erofs, the guest kernel and
// image.json in src-tauri/resources/linux-image/.
//
//   npm run linux-image:build
//   APT_MIRROR=https://mirrors.ustc.edu.cn/ubuntu npm run linux-image:build
//
// On Linux arm64 the build runs with sudo. Elsewhere (macOS on Apple silicon)
// it runs in a privileged linux/arm64 Docker container, which needs Docker
// Desktop or OrbStack. Verified downloads are kept in ~/.cache/gizmoth.
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = resolve(process.argv[2] ?? join(root, "src-tauri/resources/linux-image"));
const cache = join(process.env.XDG_CACHE_HOME ?? join(homedir(), ".cache"), "gizmoth", "linux-image");
mkdirSync(output, { recursive: true });
mkdirSync(cache, { recursive: true });
const run = (command, args, options = {}) => execFileSync(command, args, { stdio: "inherit", ...options });
let commit = process.env.GIZMOTH_IMAGE_COMMIT ?? "";
try {
  commit ||= execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
} catch {
  // Not a Git checkout: image.json records no commit.
}
const owner = typeof process.getuid === "function" ? `${process.getuid()}:${process.getgid()}` : "";
const environment = {
  GIZMOTH_IMAGE_CACHE: cache,
  GIZMOTH_IMAGE_COMMIT: commit,
  OUTPUT_OWNER: owner,
  ...(process.env.APT_MIRROR ? { APT_MIRROR: process.env.APT_MIRROR } : {}),
};

if (process.platform === "linux" && process.arch === "arm64") {
  const assignments = Object.entries(environment).map(([key, value]) => `${key}=${value}`);
  run("sudo", ["env", ...assignments, join(root, "native/linux-image/build.sh"), output]);
} else {
  // The checkout, output and cache are mounted at the same relative places.
  const inside = (path) => `/src/${relative(root, path)}`;
  if (relative(root, output).startsWith("..")) throw new Error("The output folder must be inside the checkout");
  const variables = Object.entries({ ...environment, GIZMOTH_IMAGE_CACHE: "/cache", OUTPUT_OWNER: "" }).flatMap(
    ([key, value]) => ["-e", `${key}=${value}`],
  );
  run("docker", [
    "run",
    "--rm",
    "--privileged",
    "--platform",
    "linux/arm64",
    "-v",
    `${root}:/src`,
    "-v",
    `${cache}:/cache`,
    ...variables,
    "ubuntu:24.04",
    "bash",
    "-c",
    'apt-get update -qq && apt-get install -y -qq --no-install-recommends curl ca-certificates erofs-utils >/dev/null && exec /src/native/linux-image/build.sh "$0"',
    inside(output),
  ]);
}
console.log(`Staged ${output}`);
