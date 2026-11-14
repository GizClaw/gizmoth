// Builds native/compile-agent (PID 1 of GizMoth's Linux compile VMs) as a
// static aarch64 musl executable and stages it as a bundle resource.
//
//   npm run compile-agent:build
//
// Requires the Rust target: rustup target add aarch64-unknown-linux-musl
// Linking uses rust-lld, so no cross C toolchain is needed.
import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const crate = join(root, "native/compile-agent");
const target = "aarch64-unknown-linux-musl";
execFileSync("cargo", ["build", "--locked", "--release", "--target", target], {
  cwd: crate,
  stdio: "inherit",
  env: { ...process.env, CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER: "rust-lld" },
});
const destination = join(root, "src-tauri/resources/compile-agent");
mkdirSync(destination, { recursive: true });
const output = join(destination, "gizmoth-compile-agent");
copyFileSync(join(crate, "target", target, "release/gizmoth-compile-agent"), output);
chmodSync(output, 0o755);
console.log(`Staged ${output}`);
