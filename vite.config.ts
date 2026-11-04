import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const stub = fileURLToPath(new URL("./src/stubs/unsupported-provider.ts", import.meta.url));

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: [{ find: /^@mistralai\/mistralai$/, replacement: stub }],
  },
  server: { port: 1420, strictPort: true },
  clearScreen: false,
  build: { target: "es2022", chunkSizeWarningLimit: 1500 },
});
