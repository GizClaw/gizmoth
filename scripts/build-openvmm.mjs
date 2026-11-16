// Builds the pinned OpenVMM release binary for macOS on Apple silicon and
// stages it as GizMoth's `openvmm` sidecar, signed with the Hypervisor
// entitlement that Hypervisor.framework requires.
//
//   npm run openvmm:build
//   OPENVMM_SOURCE=~/src/openvmm npm run openvmm:build   # existing checkout
//
// Needs Git, Rust and protoc (Homebrew: brew install protobuf). Release
// signing replaces the ad-hoc signature with the app's identity.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const OPENVMM_REPOSITORY = "https://github.com/microsoft/openvmm.git";
export const OPENVMM_COMMIT = "7d90c41044077e31ca060df9206532f579f24716";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
if (process.platform !== "darwin" || process.arch !== "arm64") {
  throw new Error("The OpenVMM sidecar is built for macOS on Apple silicon only");
}
const run = (command, args, options = {}) => execFileSync(command, args, { stdio: "inherit", ...options });
const output = (command, args, options = {}) => execFileSync(command, args, { encoding: "utf8", ...options }).trim();

let source = process.env.OPENVMM_SOURCE && resolve(process.env.OPENVMM_SOURCE);
if (!source) {
  source = join(process.env.XDG_CACHE_HOME ?? join(homedir(), ".cache"), "gizmoth", "openvmm");
  if (!existsSync(join(source, ".git"))) {
    mkdirSync(source, { recursive: true });
    run("git", ["init", "-q", source]);
    run("git", ["-C", source, "remote", "add", "origin", OPENVMM_REPOSITORY]);
  }
  run("git", ["-C", source, "fetch", "--depth", "1", "origin", OPENVMM_COMMIT]);
  run("git", ["-C", source, "checkout", "-q", "--detach", OPENVMM_COMMIT]);
}
const head = output("git", ["-C", source, "rev-parse", "HEAD"]);
if (head !== OPENVMM_COMMIT) {
  throw new Error(`OpenVMM checkout is at ${head}, expected ${OPENVMM_COMMIT}`);
}
// Never apply GizMoth's patch to a developer's OPENVMM_SOURCE checkout. Build
// the exact pin plus patch in an isolated, content-addressed worktree instead.
const patch = join(root, "scripts/patches/openvmm-hvsock-recheck.patch");
const patchBytes = readFileSync(patch);
const patchDigest = createHash("sha256").update(patchBytes).digest("hex");
const buildSource = join(root, "src-tauri/target/openvmm", `${OPENVMM_COMMIT.slice(0, 12)}-${patchDigest}`);
if (!existsSync(buildSource)) {
  mkdirSync(dirname(buildSource), { recursive: true });
  run("git", ["-C", source, "worktree", "add", "--detach", buildSource, OPENVMM_COMMIT]);
}
if (output("git", ["-C", buildSource, "rev-parse", "HEAD"]) !== OPENVMM_COMMIT) {
  throw new Error("OpenVMM build worktree has an unexpected revision");
}
const changes = output("git", ["-C", buildSource, "status", "--porcelain", "--untracked-files=all"]);
if (!changes) {
  run("git", ["-C", buildSource, "apply", "--check", patch]);
  run("git", ["-C", buildSource, "apply", patch]);
}
const actualPatch = execFileSync("git", ["-C", buildSource, "diff", "--binary", "HEAD", "--"]);
if (!actualPatch.equals(patchBytes)
    || output("git", ["-C", buildSource, "ls-files", "--others", "--exclude-standard"])) {
  throw new Error("OpenVMM build worktree differs from the pinned GizMoth patch; preserve it and use a clean build worktree");
}
const protoc = process.env.PROTOC ?? output("sh", ["-c", "command -v protoc"]);
const targetDir = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(buildSource, "target");
run("cargo", ["build", "--locked", "--release", "-p", "openvmm"], {
  cwd: buildSource,
  env: { ...process.env, CARGO_TARGET_DIR: targetDir, PROTOC: protoc },
});
const destination = join(root, "src-tauri/binaries");
mkdirSync(destination, { recursive: true });
const sidecar = join(destination, "openvmm-aarch64-apple-darwin");
copyFileSync(join(targetDir, "release/openvmm"), sidecar);
chmodSync(sidecar, 0o755);
// OpenVMM's release profile keeps debug info; the sidecar does not need it.
run("strip", ["-S", sidecar]);
run("codesign", ["--force", "--sign", "-", "--entitlements", join(root, "src-tauri/openvmm.entitlements"), sidecar]);
console.log(`Staged ${sidecar} (OpenVMM ${head.slice(0, 12)}, GizMoth patch ${patchDigest.slice(0, 12)})`);
