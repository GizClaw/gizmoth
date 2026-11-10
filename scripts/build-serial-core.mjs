// Builds native/serial-core (GizMoth's adapter over the pinned GizOS Host Core)
// into one static archive for the host target and stages it for Cargo.
//
//   npm run serial-core:build                 # host target, pinned GizOS commit
//   npm run serial-core:build -- x86_64-apple-darwin     # explicit Rust target
//   GIZOS_SOURCE=~/src/gizos npm run serial-core:build   # local GizOS checkout
import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const module = join(root, "native/serial-core");
const targets = {
  "aarch64-apple-darwin": { config: "macos_arm64", archive: "libgizmoth_serial_core.a" },
  "x86_64-apple-darwin": { config: "macos_x86_64", archive: "libgizmoth_serial_core.a" },
  "x86_64-pc-windows-msvc": { config: "windows_x86_64", archive: "gizmoth_serial_core.lib" },
  "x86_64-unknown-linux-gnu": { config: "linux_x86_64", archive: "libgizmoth_serial_core.a" },
};
const hosts = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "win32-x64": "x86_64-pc-windows-msvc",
  "linux-x64": "x86_64-unknown-linux-gnu",
};
const triple = process.argv[2] ?? hosts[`${process.platform}-${process.arch}`];
const target = targets[triple] && { triple, ...targets[triple] };
if (!target) throw new Error(`Unsupported serial-core target: ${triple ?? `${process.platform}-${process.arch}`}`);
const bazel = process.env.BAZEL_BIN ?? "bazel";
const options = [`--config=${target.config}`];
if (process.env.GIZOS_SOURCE) options.push(`--override_module=gizos=${resolve(process.env.GIZOS_SOURCE)}`);
execFileSync(bazel, ["build", ...options, "//:gizmoth_serial_core"], { cwd: module, stdio: "inherit" });
// .bazelrc disables convenience symlinks; ask Bazel for the exact output.
const output = execFileSync(bazel, ["cquery", ...options, "--output=files", "//:gizmoth_serial_core"], {
  cwd: module,
  encoding: "utf8",
})
  .trim()
  .split("\n")
  .find((line) => line.endsWith(target.archive));
if (!output) throw new Error(`Bazel did not report ${target.archive}`);
const execroot = execFileSync(bazel, ["info", ...options, "execution_root"], { cwd: module, encoding: "utf8" }).trim();
const destination = join(module, "dist", target.triple);
mkdirSync(destination, { recursive: true });
copyFileSync(join(execroot, output), join(destination, target.archive));
// Bazel outputs are read-only; Cargo only reads the staged copy.
chmodSync(join(destination, target.archive), 0o644);
console.log(`Staged ${target.archive} for ${target.triple}`);
