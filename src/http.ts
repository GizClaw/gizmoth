import { fetch as tauriFetch } from "@tauri-apps/plugin-http";
import { isTauri } from "@tauri-apps/api/core";

// pi-ai calls `process.env` when a provider has no explicit key. The webview
// has no `process`, so give it an empty environment instead of crashing.
const g = globalThis as { process?: { env: Record<string, string>; versions: object } };
g.process ??= { env: {}, versions: {} };

/**
 * Model providers do not send CORS headers for a desktop webview, so external
 * HTTP(S) requests made by the embedded agent go through Tauri's Rust HTTP
 * client. IPC and the app's own assets keep using the webview fetch.
 */
export function installProviderFetch() {
  if (!isTauri()) return;
  const webviewFetch = window.fetch.bind(window);
  window.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    const external =
      /^https?:\/\//i.test(url) &&
      !url.startsWith(location.origin) &&
      !/^https?:\/\/(ipc|asset|tauri)\.localhost/i.test(url) &&
      // Device models are public and CORS-enabled; the webview fetch caches them.
      !url.startsWith("https://static-volc.gizclaw.com/");
    return external ? tauriFetch(input, init) : webviewFetch(input, init);
  };
}
