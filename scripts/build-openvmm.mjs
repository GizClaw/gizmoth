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
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const OPENVMM_REPOSITORY = "https://github.com/microsoft/openvmm.git";
export const OPENVMM_COMMIT = "7d90c41044077e31ca060df9206532f579f24716";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const run = (command, args, options = {}) => execFileSync(command, args, { stdio: "inherit", ...options });
const output = (command, args, options = {}) => execFileSync(command, args, { encoding: "utf8", ...options }).trim();

// Compare content against a tree constructed without changing either checkout
// or its real index. Git's diff presentation settings do not affect this test.
export function verifyPatchedTree(source, patch, commit = OPENVMM_COMMIT) {
  const temporary = mkdtempSync(join(tmpdir(), "gizmoth-openvmm-index-"));
  const options = { env: { ...process.env, GIT_INDEX_FILE: join(temporary, "index") } };
  try {
    run("git", ["-C", source, "read-tree", commit], options);
    run("git", ["-C", source, "apply", "--cached", patch], options);
    const expected = output("git", ["-C", source, "write-tree"], options);
    run("git", ["-C", source, "diff", "--quiet", "--no-ext-diff", "--no-textconv", expected, "--"], options);
    if (output("git", ["-C", source, "ls-files", "--others", "--exclude-standard"], options)) {
      throw new Error("Unexpected untracked files");
    }
  } catch (cause) {
    throw new Error("OpenVMM build worktree differs from the pinned GizMoth patch; preserve it and use a clean build worktree", { cause });
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

export function buildOpenVmm() {
  if (process.platform !== "darwin" || process.arch !== "arm64") {
    throw new Error("The OpenVMM sidecar is built for macOS on Apple silicon only");
  }
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
  verifyPatchedTree(buildSource, patch);
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
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  buildOpenVmm();
}
