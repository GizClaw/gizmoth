import snapshot from "./snapshot.json" with { type: "json" };
import type { DocumentationSnapshot } from "./catalog.ts";

/** Generated from docs/gizos/*.md; shared by the reader and the default MCP. */
export const bundledGizosDocs = snapshot as DocumentationSnapshot;
