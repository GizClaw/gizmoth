import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { previewPackages } from "./dev/preview-packages";

const stub = fileURLToPath(new URL("./src/stubs/unsupported-provider.ts", import.meta.url));

export default defineConfig({
  plugins: [react(), previewPackages()],
  resolve: {
    alias: [{ find: /^@mistralai\/mistralai$/, replacement: stub }],
  },
  server: {
    port: 1420,
    strictPort: true,
    headers: {
      "Cross-Origin-Opener-Policy": "same-origin",
      "Cross-Origin-Embedder-Policy": "require-corp",
    },
  },
  clearScreen: false,
  build: { target: "es2022", chunkSizeWarningLimit: 1500 },
});
