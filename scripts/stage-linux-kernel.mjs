// Stages only the pinned OpenVMM ARM64 test kernel. macOS/Linux need curl and
// tar; no root privileges, Docker, distro filesystem or host userland is used.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  closeSync,
  createReadStream,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  readSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const pinned = JSON.parse(readFileSync(join(root, "scripts/linux-kernel.json"), "utf8"));
const digest = async (path) => {
  const hash = createHash("sha256");
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  return hash.digest("hex");
};

export async function stageKernel({
  output = join(root, "src-tauri/resources/linux-kernel"),
  cache = process.env.GIZMOTH_KERNEL_CACHE ??
    join(process.env.XDG_CACHE_HOME ?? join(homedir(), ".cache"), "gizmoth", "linux-kernel"),
  source = pinned,
} = {}) {
  if (!/^[a-f0-9]{64}$/.test(source.sha256)) throw new Error("A pinned kernel SHA-256 is required");
  mkdirSync(cache, { recursive: true });
  mkdirSync(output, { recursive: true });
  const archive = join(cache, source.sha256);
  if ((await digest(archive).catch(() => "")) !== source.sha256) {
    const download = mkdtempSync(join(cache, ".download-"));
    const partial = join(download, "archive");
    try {
      execFileSync(
        "curl",
        [
          "--fail",
          "--location",
          "--retry",
          "3",
          "--connect-timeout",
          "30",
          "--max-time",
          "300",
          "--output",
          partial,
          source.url,
        ],
        { stdio: "inherit" },
      );
      if ((await digest(partial)) !== source.sha256)
        throw new Error("The Linux kernel archive does not match its pinned SHA-256");
      renameSync(partial, archive);
    } finally {
      rmSync(download, { recursive: true, force: true });
    }
  }
  // Extract only one verified member to stdout. Never unpack an archive tree on
  // the host, where a symlink or path traversal could overwrite unrelated files.
  const members = execFileSync("tar", ["-tzf", archive], { encoding: "utf8" }).trim().split(/\r?\n/);
  const member = members.find(
    (name) => /(^|\/)Image$/.test(name) && !name.startsWith("/") && !name.split("/").includes(".."),
  );
  if (!member) throw new Error("The pinned kernel archive has no ARM64 Image");
  const image = join(output, "Image");
  const staging = mkdtempSync(join(output, ".kernel-"));
  const staged = join(staging, "Image");
  try {
    const fd = openSync(staged, "w", 0o644);
    try {
      execFileSync("tar", ["-xOzf", archive, "--", member], { stdio: ["ignore", fd, "pipe"] });
    } finally {
      closeSync(fd);
    }
    const header = Buffer.alloc(64);
    const headerFd = openSync(staged, "r");
    try {
      if (readSync(headerFd, header, 0, header.length, 0) !== header.length
          || header.readUInt32LE(56) !== 0x644d5241) {
        throw new Error("The extracted kernel is not an ARM64 Linux Image");
      }
    } finally {
      closeSync(headerFd);
    }
    const sha256 = await digest(staged);
    renameSync(staged, image);
    let commit = null;
    try {
      commit = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    } catch {
      // A source archive has no Git commit.
    }
    const info = {
      kernel: source.name,
      builtAt: Date.now(),
      commit,
      source: { url: source.url, sha256: source.sha256 },
      sha256,
    };
    writeFileSync(join(output, "kernel.json"), `${JSON.stringify(info, null, 2)}\n`);
    console.log(`Staged ${source.name} in ${output}`);
    return info;
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  await stageKernel({ output: resolve(process.argv[2] ?? join(root, "src-tauri/resources/linux-kernel")) });
}
