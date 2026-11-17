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
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export function buildCompileAgent({
  crate = join(root, "native/compile-agent"),
  destination = join(root, "src-tauri/resources/compile-agent"),
} = {}) {
  const messages = execFileSync(
    "cargo",
    ["build", "--locked", "--release", "--target", "aarch64-unknown-linux-musl", "--message-format=json-render-diagnostics"],
    {
      cwd: crate,
      encoding: "utf8",
      stdio: ["inherit", "pipe", "inherit"],
      maxBuffer: 32 * 1024 * 1024,
      env: { ...process.env, CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER: "rust-lld" },
    },
  );
  // Cargo resolves both CARGO_TARGET_DIR and build.target-dir configuration.
  // Its artifact message is authoritative, including when the build is fresh.
  let executable;
  for (const line of messages.split(/\r?\n/).filter(Boolean)) {
    const message = JSON.parse(line);
    if (message.reason === "compiler-artifact"
        && message.target?.name === "gizmoth-compile-agent"
        && message.target.kind.includes("bin")
        && message.executable) {
      executable = message.executable;
    }
  }
  if (!executable) throw new Error("Cargo did not report the compile-agent executable");
  mkdirSync(destination, { recursive: true });
  const output = join(destination, "gizmoth-compile-agent");
  copyFileSync(resolve(crate, executable), output);
  chmodSync(output, 0o755);
  console.log(`Staged ${output}`);
  return output;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  buildCompileAgent();
}
