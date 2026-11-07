import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { CallToolResultSchema } from "@modelcontextprotocol/sdk/types.js";
import { CfWorkerJsonSchemaValidator } from "@modelcontextprotocol/sdk/validation/cfworker-provider.js";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { Type } from "typebox";
import type { DocumentationSnapshot } from "./catalog.ts";
import { createGizosDocsServer } from "./server.ts";

/** Negotiate MCP, discover schemas, and adapt the advertised tools to pi-agent. No HTTP or subprocess. */
export async function connectGizosDocs(snapshot: DocumentationSnapshot) {
  const server = createGizosDocsServer(snapshot);
  const client = new Client(
    { name: "gizmoth", version: "0.1.0" },
    { capabilities: {}, jsonSchemaValidator: new CfWorkerJsonSchemaValidator() },
  );
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  try {
    await server.connect(serverTransport);
    await client.connect(clientTransport);
    const { tools: advertised } = await client.listTools();
    const tools: AgentTool[] = advertised.map((tool) => ({
      name: tool.name,
      label: tool.title ?? tool.name,
      description: tool.description ?? "Read bundled GizOS documentation.",
      parameters: Type.Unsafe<Record<string, unknown>>(tool.inputSchema),
      execute: async (_toolCallId, args, signal) => {
        signal?.throwIfAborted();
        const arguments_ = args as Record<string, unknown>;
        const result = CallToolResultSchema.parse(
          await client.callTool({ name: tool.name, arguments: arguments_ }, CallToolResultSchema, {
            signal,
            timeout: 10_000,
          }),
        );
        const content = result.content.flatMap((block) =>
          block.type === "text" ? [{ type: "text" as const, text: block.text }] : [],
        );
        if (result.isError)
          throw new Error(content.map((block) => block.text).join("\n") || "GizOS documentation request failed");
        return {
          content,
          details: { server: "gizos-docs", revision: snapshot.source.revision, ...arguments_ },
        };
      },
    }));
    return {
      client,
      tools,
      async close() {
        await client.close();
        await server.close();
      },
    };
  } catch (error) {
    await client.close().catch(() => {});
    await server.close().catch(() => {});
    throw error;
  }
}
