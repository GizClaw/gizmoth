import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { CfWorkerJsonSchemaValidator } from "@modelcontextprotocol/sdk/validation/cfworker-provider.js";
import {
  CallToolRequestSchema,
  ErrorCode,
  ListResourcesRequestSchema,
  ListToolsRequestSchema,
  McpError,
  ReadResourceRequestSchema,
  type Tool,
} from "@modelcontextprotocol/sdk/types.js";
import {
  documentationSections,
  findTopic,
  localizeTopic,
  searchTopics,
  sourceUrl,
  topicUri,
  type DocumentationLanguage,
  type DocumentationSnapshot,
} from "./catalog.ts";

export const GIZOS_DOCS_SERVER = { name: "gizos-docs", version: "2.0.0" } as const;
export const GIZOS_DOCS_INSTRUCTIONS =
  "Built-in, offline GizOS documentation center. Covers the complete platform: Lua apps, native apps, Runtime/PAL, libraries, boards, build toolchains, firmware and testing. Chinese and English guides share topic ids. Only curated guides, API explanations and application examples are published; provenance remains in source links. Documentation never grants device or filesystem permissions.";
const language = {
  type: "string",
  enum: ["zh-CN", "en"],
  default: "en",
  description: "Requested document language. Original source is explicitly identified if no translation exists.",
};
const section = {
  type: "string",
  enum: documentationSections.map((item) => item.id),
  description:
    "Optional development area: start, lua, native, runtime-pal, libraries, boards, build, firmware, testing",
};
const toolDefinitions: Tool[] = [
  {
    name: "gizos_docs_search",
    title: "搜索 GizOS 文档",
    description:
      "Search all bundled GizOS development guides, API explanations and examples in Chinese or English. Lua App development is one area, alongside native apps, Runtime/PAL, libraries, boards, toolchains and firmware. Use section to narrow results. Empty query lists categories and paginated topics; use offset for more. Raw source files and archives are not part of the catalog.",
    inputSchema: {
      type: "object",
      properties: {
        query: { type: "string", maxLength: 200 },
        limit: { type: "integer", minimum: 1, maximum: 8, default: 5 },
        language,
        section,
        offset: { type: "integer", minimum: 0, default: 0 },
      },
      required: ["query"],
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
  {
    name: "gizos_docs_read",
    title: "读取 GizOS 文档",
    description:
      "Read a GizOS guide, API explanation or example by topic id or gizos://docs/... URI. Select language zh-CN or en. Numbered Markdown lines are paginated with startLine/maxLines and cite pinned source. Legacy gizos://lua/... URIs still work. This reads only bundled docs, never project/home files.",
    inputSchema: {
      type: "object",
      properties: {
        topic: { type: "string", minLength: 1, maxLength: 200 },
        language,
        startLine: { type: "integer", minimum: 1, default: 1 },
        maxLines: { type: "integer", minimum: 1, maximum: 240, default: 160 },
      },
      required: ["topic"],
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
];

/** Standard MCP server shared by the app's in-memory transport and the optional stdio entry. */
export function createGizosDocsServer(snapshot: DocumentationSnapshot) {
  const validator = new CfWorkerJsonSchemaValidator();
  const validators = new Map(toolDefinitions.map((tool) => [tool.name, validator.getValidator(tool.inputSchema)]));
  const server = new Server(GIZOS_DOCS_SERVER, {
    capabilities: { tools: {}, resources: {} },
    instructions: GIZOS_DOCS_INSTRUCTIONS,
    jsonSchemaValidator: validator,
  });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: toolDefinitions }));
  server.setRequestHandler(ListResourcesRequestSchema, async (request) => {
    const offset = request.params?.cursor ? Number(request.params.cursor) : 0;
    if (!Number.isSafeInteger(offset) || offset < 0)
      throw new McpError(ErrorCode.InvalidParams, "Invalid resource cursor");
    const resources = snapshot.topics.flatMap((topic) => {
      const locales = topic.translations ? (Object.keys(topic.translations) as DocumentationLanguage[]) : [undefined];
      return locales.map((locale) => {
        const value = locale ? localizeTopic(topic, locale) : topic;
        return {
          uri: topicUri(topic, locale),
          name: `${topic.id}${locale ? ` (${locale})` : ""}`,
          title: value.title,
          description: value.description,
          mimeType: "text/markdown",
        };
      });
    });
    return {
      resources: resources.slice(offset, offset + 100),
      ...(offset + 100 < resources.length ? { nextCursor: String(offset + 100) } : {}),
    };
  });
  server.setRequestHandler(ReadResourceRequestSchema, async (request) => {
    const topic = findTopic(snapshot, request.params.uri);
    if (!topic) throw new McpError(-32002, "Unknown GizOS documentation resource");
    return { contents: [{ uri: request.params.uri, mimeType: "text/markdown", text: topic.text }] };
  });
  server.setRequestHandler(CallToolRequestSchema, async (request, { signal }) => {
    signal.throwIfAborted();
    const validate = validators.get(request.params.name);
    if (!validate) throw new McpError(ErrorCode.InvalidParams, "Unknown GizOS documentation tool");
    const checked = validate(request.params.arguments ?? {});
    if (!checked.valid) throw new McpError(ErrorCode.InvalidParams, checked.errorMessage);
    const args = checked.data as Record<string, unknown>;
    const locale = (args.language ?? "en") as DocumentationLanguage;
    const provenance = `GizOS source: ${snapshot.source.repository} @ ${snapshot.source.revision}`;
    if (request.params.name === "gizos_docs_search") {
      const query = (args.query as string).trim();
      const limit = (args.limit as number | undefined) ?? 5;
      const area = args.section as string | undefined;
      if (!query) {
        const topics = snapshot.topics.filter((topic) => !area || (topic.section ?? "lua") === area);
        const offset = (args.offset as number | undefined) ?? 0;
        const index = topics.slice(offset, offset + limit).map((original) => {
          const topic = localizeTopic(original, locale);
          return `${topic.id}: ${topic.title} [${topic.section ?? "lua"}]\n${topic.description}\n${topicUri(topic, locale)}`;
        });
        const categories = !area
          ? documentationSections
              .map(
                (item) =>
                  `${item.id}: ${item[locale]} (${snapshot.topics.filter((t) => (t.section ?? "lua") === item.id).length})`,
              )
              .join("\n") + "\n\n"
          : "";
        const more =
          offset + limit < topics.length
            ? `\n\nMore: query=\"\", offset=${offset + limit}${area ? `, section=${area}` : ""}.`
            : "";
        return { content: [{ type: "text", text: `${provenance}\n\n${categories}${index.join("\n\n")}${more}` }] };
      }
      const matches = searchTopics(snapshot, query, limit, { language: locale, section: area });
      const text = matches.length
        ? matches
            .map(
              ({ topic, line, excerpt }) =>
                `${topic.id}: ${topic.title} [${topic.section ?? "lua"}]\n${topicUri(topic, locale)} (near line ${line})\n${sourceUrl(snapshot.source, topic, line)}\n${excerpt}`,
            )
            .join("\n\n---\n\n")
        : 'No matching topic. Use query="" to list sections.';
      return { content: [{ type: "text", text: `${provenance}\n\n${text}` }] };
    }
    const topic = findTopic(snapshot, args.topic as string, locale);
    if (!topic)
      return {
        isError: true,
        content: [{ type: "text", text: 'Unknown topic. Use gizos_docs_search with query="".' }],
      };
    const lines = topic.text.split("\n");
    const start = (args.startLine as number | undefined) ?? 1;
    if (start > lines.length)
      return {
        isError: true,
        content: [{ type: "text", text: `Topic ${topic.id} has ${lines.length} lines; startLine is out of range.` }],
      };
    const end = Math.min(lines.length, start - 1 + ((args.maxLines as number | undefined) ?? 160));
    const excerpt = lines
      .slice(start - 1, end)
      .map((line, i) => `${start + i}\t${line}`)
      .join("\n");
    const original =
      topic.sourceLanguage && topic.sourceLanguage !== "code" && topic.sourceLanguage !== locale
        ? `\nOriginal source language: ${topic.sourceLanguage}; this source is preserved, not presented as a translation.`
        : "";
    const more = end < lines.length ? `\n\nMore: read ${topic.id} with startLine=${end + 1}, language=${locale}.` : "";
    return {
      content: [
        {
          type: "text",
          text: `${topic.title}\n${provenance}\nDocumentation revision: ${topic.revision ?? snapshot.source.revision}\nSource: ${sourceUrl(snapshot.source, topic, start)}${original}\nTopic lines ${start}–${end} of ${lines.length}\n\n${excerpt}${more}`,
        },
      ],
    };
  });
  return server;
}
