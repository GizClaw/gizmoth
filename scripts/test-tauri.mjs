import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const args = process.argv.slice(2);
const skipBuild = args.includes("--no-build");
const testArgs = args.filter((arg) => arg !== "--no-build");

async function run(args) {
  const child = spawn(process.execPath, args, {
    cwd: root,
    stdio: "inherit",
    env: { ...process.env, CARGO_TARGET_DIR: join(root, "src-tauri", "target", "native-e2e") },
  });
  const terminate = () => child.kill();
  process.once("SIGINT", terminate);
  process.once("SIGTERM", terminate);
  try {
    await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("exit", (code, signal) => {
        if (code === 0) resolve();
        else reject(new Error(`Native integration command exited with ${signal ?? code}`));
      });
    });
  } finally {
    process.removeListener("SIGINT", terminate);
    process.removeListener("SIGTERM", terminate);
  }
}

try {
  if (!skipBuild) {
    await run([
      join(root, "node_modules", "@tauri-apps", "cli", "tauri.js"),
      "build",
      "--debug",
      "--no-bundle",
      "--features",
      "native-e2e",
      "--config",
      join(root, "src-tauri", "tauri.e2e.conf.json"),
      "--",
      "--locked",
    ]);
  }
  await run(["--test", "--test-concurrency=1", ...testArgs, join(root, "test", "integration", "agent.test.mjs")]);
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
