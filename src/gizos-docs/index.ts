import { connectGizosDocs } from "./client.ts";
import { bundledGizosDocs } from "./data.ts";

export { bundledGizosDocs } from "./data.ts";

/** Built in and enabled for every Lua project; no URL, API key, filesystem grant or Web permission. */
export const builtinGizosMcp = {
  id: "gizos-docs",
  enabled: true,
  transport: "in-memory",
  source: bundledGizosDocs.source,
} as const;

let connection: ReturnType<typeof connectGizosDocs> | undefined;

export async function gizosDocumentationTools() {
  connection ??= connectGizosDocs(bundledGizosDocs).catch((error) => {
    connection = undefined;
    throw error;
  });
  return (await connection).tools;
}
