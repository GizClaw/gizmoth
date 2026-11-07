// Optional stdio entry for other MCP clients. GizMoth itself embeds this server without a subprocess.
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import snapshot from "../src/gizos-docs/snapshot.json" with { type: "json" };
import { createGizosDocsServer } from "../src/gizos-docs/server.ts";

const server = createGizosDocsServer(snapshot);
await server.connect(new StdioServerTransport());
