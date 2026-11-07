import { isTauri } from "@tauri-apps/api/core";
import { version } from "../package.json";
import type { UpdateDriver } from "./updater.ts";

export function updateDriver(): UpdateDriver | null {
  if (isTauri())
    return {
      check: async () => (await import("@tauri-apps/plugin-updater")).check({ timeout: 15000 }),
      restart: async () => (await import("@tauri-apps/plugin-process")).relaunch(),
    };
  if (!import.meta.env.DEV) return null;
  const params = new URLSearchParams(location.search);
  const example = params.get("update");
  if (!params.has("demo") || !["available", "unknown", "error"].includes(example ?? "")) return null;
  const pause = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
  let attempts = 0;
  const next = version.split(".").map(Number);
  next[2]++;
  return {
    check: async () => ({
      version: next.join("."),
      async download(onEvent) {
        attempts++;
        onEvent({ event: "Started", data: { contentLength: example === "unknown" ? undefined : 10_000_000 } });
        for (let index = 0; index < 20; index++) {
          await pause(180);
          if (example === "error" && attempts === 1 && index === 8) throw new Error("演示下载中断，点击重试。");
          onEvent({ event: "Progress", data: { chunkLength: 500_000 } });
        }
        onEvent({ event: "Finished" });
        await pause(500);
      },
      install: async () => pause(700),
      close: async () => {},
    }),
    restart: async () => {}, // Browser fixture never installs or restarts anything.
  };
}
