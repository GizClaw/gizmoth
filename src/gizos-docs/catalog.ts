import sections from "./sections.json" with { type: "json" };

export type DocumentationLanguage = "zh-CN" | "en";
export const documentationSections = sections;

export interface DocumentationSource {
  repository: string;
  revision: string;
}
export interface DocumentationTranslation {
  file: string;
  title: string;
  description: string;
  text: string;
  sha256: string;
}
export interface DocumentationTopic {
  id: string;
  title: string;
  description: string;
  keywords: string[];
  path: string;
  file?: string;
  section?: string;
  kind?: "guide" | "example";
  sourceLanguage?: DocumentationLanguage | "code";
  translations?: Partial<Record<DocumentationLanguage, DocumentationTranslation>>;
  derived?: boolean;
  sourceLine: number;
  sourceStart?: number;
  sourceSha256?: string;
  sha256: string;
  revision?: string;
  text: string;
}
export interface DocumentationSnapshot {
  format: 1;
  source: DocumentationSource;
  topics: DocumentationTopic[];
}

const aliases: Record<string, string> = {
  绘图: "graphics draw display",
  画面: "graphics display screen",
  屏幕: "graphics display screen",
  图形: "graphics draw display",
  按键: "input button key",
  按钮: "input button key",
  计时: "time timer sleep",
  定时: "time timer",
  游戏: "game example",
  循环: "loop lifecycle",
  启动: "lifecycle entry",
  声音: "audio sound",
  音频: "audio sound",
  文件: "file storage",
  打包: "package manifest",
  示例: "example",
  固件: "firmware h2loader",
  烧录: "firmware flash gizloader",
  构建: "build bazel toolchain",
  工具链: "build toolchain sdk",
  板卡: "board platform",
  平台: "platform board",
  原生: "native application",
  测试: "test validation",
  验证: "test validation",
  协议: "protocol network",
  网络: "network http mqtt",
  驱动: "driver peripheral",
  内存: "memory allocator",
  任务: "task runtime",
  架构: "architecture runtime pal",
};

export function localizeTopic(topic: DocumentationTopic, language: DocumentationLanguage): DocumentationTopic {
  const translation = topic.translations?.[language];
  return translation ? { ...topic, ...translation, sourceLanguage: language, derived: true } : topic;
}
export function topicUri(topic: DocumentationTopic, language?: DocumentationLanguage) {
  return language ? `gizos://docs/${language}/${topic.id}` : `gizos://docs/${topic.id}`;
}
export function sourceUrl(source: DocumentationSource, topic: DocumentationTopic, line = 1) {
  const upstreamLine = topic.sourceLine + (topic.derived ? 0 : Math.max(0, line - (topic.sourceStart ?? 1)));
  return `${source.repository}/blob/${topic.revision ?? source.revision}/${topic.path}#L${upstreamLine}`;
}

/** Read only registered topics. Both the original Lua URIs and the full GizOS URIs remain valid. */
export function findTopic(snapshot: DocumentationSnapshot, idOrUri: string, language?: DocumentationLanguage) {
  let id = idOrUri;
  let locale = language;
  if (idOrUri.startsWith("gizos://lua/")) id = idOrUri.slice("gizos://lua/".length);
  else if (idOrUri.startsWith("gizos://docs/")) {
    const parts = idOrUri.slice("gizos://docs/".length).split("/");
    if (parts.length === 2 && (parts[0] === "en" || parts[0] === "zh-CN")) {
      locale = parts[0];
      id = parts[1];
    } else if (parts.length === 1) id = parts[0];
  }
  const topic = snapshot.topics.find((item) => item.id === id);
  return topic && locale ? localizeTopic(topic, locale) : topic;
}

function searchTerms(query: string) {
  const lower = query.toLowerCase();
  const expanded = Object.entries(aliases)
    .filter(([word]) => lower.includes(word))
    .map(([, terms]) => terms)
    .join(" ");
  return [...new Set(`${lower} ${expanded}`.match(/[a-z0-9_.]+|[\p{Script=Han}]+/gu) ?? [])];
}
const indexes = new WeakMap<DocumentationSnapshot, Map<string, { title: string; lines: string[]; lower: string[] }>>();
function searchIndex(
  snapshot: DocumentationSnapshot,
  original: DocumentationTopic,
  topic: DocumentationTopic,
  language?: DocumentationLanguage,
) {
  let index = indexes.get(snapshot);
  if (!index) {
    index = new Map();
    indexes.set(snapshot, index);
  }
  const key = `${topic.id}:${language ?? "source"}`;
  let entry = index.get(key);
  if (!entry) {
    const variants = Object.values(original.translations ?? {})
      .map((value) => `${value.title} ${value.description}`)
      .join(" ");
    const lines = topic.text.split("\n");
    entry = {
      title:
        `${topic.id} ${topic.section ?? ""} ${topic.title} ${topic.description} ${topic.keywords.join(" ")} ${variants}`.toLowerCase(),
      lines,
      lower: lines.map((line) => line.toLowerCase()),
    };
    index.set(key, entry);
  }
  return entry;
}

/** Search the complete platform corpus, optionally confined to a development area or language. */
export function searchTopics(
  snapshot: DocumentationSnapshot,
  query: string,
  limit: number,
  options: { language?: DocumentationLanguage; section?: string } = {},
) {
  const terms = searchTerms(query);
  if (!terms.length) return [];
  return snapshot.topics
    .filter((topic) => !options.section || (topic.section ?? "lua") === options.section)
    .map((original) => {
      const topic = options.language ? localizeTopic(original, options.language) : original;
      const { title, lines, lower } = searchIndex(snapshot, original, topic, options.language);
      let firstLine = 0;
      let bestHits = 0;
      let bodyScore = 0;
      for (let i = 0; i < lines.length; i++) {
        const hits = terms.filter((term) => lower[i].includes(term)).length;
        if (hits > bestHits) {
          firstLine = i;
          bestHits = hits;
        }
        bodyScore += hits;
      }
      let score = terms.reduce((sum, term) => sum + (title.includes(term) ? 20 : 0), 0) + Math.min(bodyScore, 15);
      return {
        topic,
        score,
        line: firstLine + 1,
        excerpt: lines
          .slice(Math.max(0, firstLine - 1), firstLine + 5)
          .join("\n")
          .slice(0, 900),
      };
    })
    .filter(({ score }) => score > 0)
    .sort((a, b) => b.score - a.score || a.topic.id.localeCompare(b.topic.id))
    .slice(0, limit);
}
