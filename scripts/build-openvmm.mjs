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
import { chmodSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
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
if (head !== OPENVMM_COMMIT && !process.env.OPENVMM_SOURCE) {
  throw new Error(`OpenVMM checkout is at ${head}, expected ${OPENVMM_COMMIT}`);
}
const protoc = process.env.PROTOC ?? output("sh", ["-c", "command -v protoc"]);
run("cargo", ["build", "--locked", "--release", "-p", "openvmm"], { cwd: source, env: { ...process.env, PROTOC: protoc } });
const destination = join(root, "src-tauri/binaries");
mkdirSync(destination, { recursive: true });
const sidecar = join(destination, "openvmm-aarch64-apple-darwin");
copyFileSync(join(source, "target/release/openvmm"), sidecar);
chmodSync(sidecar, 0o755);
// OpenVMM's release profile keeps debug info; the sidecar does not need it.
run("strip", ["-S", sidecar]);
run("codesign", ["--force", "--sign", "-", "--entitlements", join(root, "src-tauri/openvmm.entitlements"), sidecar]);
console.log(`Staged ${sidecar} (OpenVMM ${head.slice(0, 12)})`);
