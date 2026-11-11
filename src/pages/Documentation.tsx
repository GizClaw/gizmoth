import {
  Children,
  createElement,
  isValidElement,
  useDeferredValue,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { BookOpen, Check, Copy, FileText, MagnifyingGlass } from "@phosphor-icons/react";
import { IconButton, PageHeader, SearchInput, Select } from "../ui";
import { bundledGizosDocs as docs } from "../gizos-docs/data";
import {
  documentationSections,
  localizeTopic,
  searchTopics,
  type DocumentationLanguage,
  type DocumentationTopic,
} from "../gizos-docs/catalog";
import { useI18n } from "../i18n";
import { copyText as copy } from "../clipboard";
import "./Documentation.css";

const luaLabels: Record<string, string> = {
  overview: "Lua Runtime 概览",
  "input-example": "绘图与按键示例",
  display: "绘图与显示",
  "runtime-bindings": "按键、事件与计时",
  "game-example": "Flappy Bird 完整示例",
  "game-requirements": "游戏参数与设备能力",
  packaging: "Lua 应用打包",
  storage: "文件与 KV 存储",
  audio: "音频播放与录制",
  link: "BLE 设备通信",
  loading: "模块加载与错误处理",
  math: "向量与几何",
  installation: "Lua 应用安装",
  embedding: "Lua Runtime 集成与能力",
  "display-async": "异步画面提交",
  "display-api": "Display 接口契约",
  "display-bindings": "Display 完整绑定",
  "host-api": "Lua Host 接口契约",
  "events-api": "Lua 事件投递契约",
  "package-rule": "Lua 应用打包规则",
  license: "GizOS 许可",
  "game-provenance": "示例来源",
  "game-license": "示例许可",
};
const defaults: Record<string, string> = {
  start: "gizos-overview",
  lua: "lua-device-api",
  native: "native-apps",
  "runtime-pal": "runtime-pal",
  libraries: "libraries",
  boards: "boards-platforms",
  build: "build-toolchains",
  firmware: "firmware-development",
  testing: "validation-testing",
};
const preferred = [...Object.values(defaults), "overview", "display", "game-example", "game-requirements", "packaging"];
const ordered = [
  ...preferred
    .map((id) => docs.topics.find((topic) => topic.id === id))
    .filter((topic): topic is DocumentationTopic => !!topic),
  ...docs.topics.filter((topic) => !preferred.includes(topic.id)),
];
function plainText(children: ReactNode): string {
  return Children.toArray(children)
    .map((child) => (isValidElement<{ children?: ReactNode }>(child) ? plainText(child.props.children) : String(child)))
    .join("");
}
function headingId(children: ReactNode) {
  return plainText(children)
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .trim()
    .replace(/\s+/g, "-");
}
function CodeBlock({ children, language }: { children?: ReactNode; language: DocumentationLanguage }) {
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState("");
  const code = plainText(children).replace(/\n$/, "");
  const first = Children.toArray(children)[0];
  const className = isValidElement<{ className?: string }>(first) ? first.props.className : "";
  const syntax = className?.replace("language-", "") ?? "code";
  const zh = language === "zh-CN";
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <div className="docs-code-block">
      <div className="docs-code-head">
        <span>{syntax}</span>
        <IconButton
          controlSize="sm"
          aria-label={copied ? (zh ? "代码已复制" : "Code copied") : zh ? "复制代码" : "Copy code"}
          title={copied ? (zh ? "已复制" : "Copied") : zh ? "复制代码" : "Copy code"}
          onClick={() =>
            void copy(code)
              .then(() => {
                setCopied(true);
                setError("");
              })
              .catch((e: Error) => setError(e.message))
          }
        >
          {copied ? <Check size={14} /> : <Copy size={14} />}
        </IconButton>
      </div>
      <pre tabIndex={0} role="region" aria-label={`${syntax} ${zh ? "代码" : "code"}`}>
        {children}
      </pre>
      {error && (
        <p role="alert" className="error-text">
          {error}
        </p>
      )}
    </div>
  );
}

export default function DocumentationPage() {
  const { language } = useI18n();
  useEffect(() => {
    localStorage.removeItem("gizmoth.docs.language");
  }, []);
  const [selected, setSelected] = useState(() => {
    const id = localStorage.getItem("gizmoth.docs.center.topic");
    return docs.topics.some((topic) => topic.id === id) ? id! : "gizos-overview";
  });
  const initialSection = docs.topics.find((topic) => topic.id === selected)?.section ?? "start";
  const [section, setSection] = useState(initialSection);
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState("");
  const article = useRef<HTMLElement>(null);
  const pendingAnchor = useRef<string | null>(null);
  const zh = language === "zh-CN";
  const original = docs.topics.find((topic) => topic.id === selected)!;
  const topic = localizeTopic(original, language);
  const title = (item: DocumentationTopic) =>
    zh ? (luaLabels[item.id] ?? localizeTopic(item, language).title) : localizeTopic(item, language).title;
  const matches = deferredQuery.trim()
    ? new Set(searchTopics(docs, deferredQuery, docs.topics.length, { language }).map(({ topic }) => topic.id))
    : undefined;
  const visible = ordered.filter((item) =>
    matches
      ? matches.has(item.id) || title(item).toLowerCase().includes(deferredQuery.toLowerCase())
      : (item.section ?? "lua") === section,
  );
  const primary = visible;
  const sectionTopics = ordered.filter((item) => (item.section ?? "lua") === section);
  useEffect(() => {
    localStorage.setItem("gizmoth.docs.center.topic", selected);
    if (pendingAnchor.current) {
      article.current?.querySelector(`#${CSS.escape(pendingAnchor.current)}`)?.scrollIntoView({ block: "start" });
      pendingAnchor.current = null;
    } else article.current?.scrollTo({ top: 0 });
    setCopied(false);
    setError("");
  }, [selected, language]);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);
  function choose(id: string) {
    const item = docs.topics.find((topic) => topic.id === id);
    if (!item) return;
    setSelected(id);
    setSection(item.section ?? "lua");
  }
  function chooseSection(id: string) {
    setQuery("");
    setSection(id);
    choose(defaults[id] ?? ordered.find((topic) => topic.section === id)!.id);
  }
  function internalLink(href: string) {
    const withoutLocale = href.replace(/^gizos:\/\/(?:lua\/|docs\/(?:(?:en|zh-CN)\/)?)/, "");
    const name = withoutLocale
      .split("#")[0]
      .replace(/^(?:\.\.\/|\.\/)*(?:en\/|zh-CN\/)?/, "")
      .replace(/\.md$/, "");
    return docs.topics.some((item) => item.id === name) ? name : undefined;
  }
  const topicButton = (item: DocumentationTopic) => (
    <button
      key={item.id}
      type="button"
      className={`docs-topic${selected === item.id ? " is-selected" : ""}`}
      aria-current={selected === item.id ? "page" : undefined}
      title={`${localizeTopic(item, language).description}\n${item.path}`}
      onClick={() => choose(item.id)}
    >
      {title(item)}
    </button>
  );
  const heading = (level: number, children: ReactNode) =>
    createElement(`h${level}`, { id: headingId(children) }, children);
  return (
    <>
      <PageHeader crumbs={[{ label: zh ? "文档" : "Documentation" }, { label: "GizOS" }]} />
      <div className="documentation-layout" data-language={language} lang={language}>
        <aside
          className="docs-navigation"
          aria-label={zh ? "文档目录" : "Documentation navigation"}
          data-searching={!!query.trim()}
        >
          <div className="docs-navigation-head">
            <div className="docs-library-title">
              <BookOpen size={17} aria-hidden="true" />
              <span>GizOS</span>
            </div>
            <SearchInput
              aria-label={zh ? "搜索文档" : "Search documentation"}
              placeholder={zh ? "搜索全部 GizOS 文档…" : "Search all GizOS docs…"}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onClear={() => setQuery("")}
            />
            <Select
              controlSize="sm"
              aria-label={zh ? "文档栏目" : "Documentation section"}
              value={section}
              onValueChange={chooseSection}
              options={documentationSections.map((item) => ({ value: item.id, label: item[language] }))}
            />
          </div>
          <nav aria-label={zh ? "GizOS 文档主题" : "GizOS documentation topics"} className="docs-topic-list">
            {primary.map(topicButton)}
            {!visible.length && (
              <p className="docs-no-results" role="status">
                <MagnifyingGlass size={18} aria-hidden="true" />
                {zh ? "没有匹配的文档" : "No matching documents"}
              </p>
            )}
          </nav>
          <p className="docs-offline-note">
            {zh ? `${docs.topics.length} 个主题 · 本机离线文档` : `${docs.topics.length} topics · Available offline`}
          </p>
        </aside>
        <section
          className="docs-reader"
          ref={article}
          aria-label={zh ? "文档正文" : "Documentation content"}
          tabIndex={-1}
        >
          <div className="docs-mobile-select">
            <Select
              aria-label={zh ? "选择文档" : "Select document"}
              value={selected}
              onValueChange={choose}
              options={sectionTopics.map((item) => ({ value: item.id, label: title(item) }))}
            />
          </div>
          <div className="docs-reading-column">
            <div className="docs-article-toolbar">
              <span>
                <FileText size={14} aria-hidden="true" />
                {documentationSections.find((item) => item.id === (topic.section ?? "lua"))?.[language]} · Markdown
              </span>
              <IconButton
                aria-label={
                  copied ? (zh ? "Markdown 已复制" : "Markdown copied") : zh ? "复制 Markdown" : "Copy Markdown"
                }
                title={copied ? (zh ? "已复制" : "Copied") : zh ? "复制 Markdown" : "Copy Markdown"}
                onClick={() =>
                  void copy(topic.text)
                    .then(() => setCopied(true))
                    .catch((e: Error) => setError(e.message))
                }
              >
                {copied ? <Check size={14} /> : <Copy size={14} />}
              </IconButton>
            </div>
            {error && (
              <p role="alert" className="error-text">
                {error}
              </p>
            )}
            <article className="docs-markdown" key={`${topic.id}:${language}`}>
              <Markdown
                remarkPlugins={[remarkGfm]}
                skipHtml
                components={{
                  h1: ({ node, children }) => heading(1, node?.position?.start.line === 1 ? title(original) : children),
                  h2: ({ children }) => heading(2, children),
                  h3: ({ children }) => heading(3, children),
                  h4: ({ children }) => heading(4, children),
                  pre: ({ children }) => <CodeBlock language={language}>{children}</CodeBlock>,
                  table: ({ children }) => (
                    <div
                      className="docs-table-scroll"
                      tabIndex={0}
                      role="region"
                      aria-label={zh ? "文档表格" : "Documentation table"}
                    >
                      <table>{children}</table>
                    </div>
                  ),
                  img: ({ alt }) => (
                    <span className="docs-image-reference">
                      {zh ? "原文图片：" : "Source image: "}
                      {alt || (zh ? "请查看来源链接" : "see upstream source")}
                    </span>
                  ),
                  a: ({ href = "", children }) => (
                    <a
                      href={href}
                      target={href.startsWith("http") ? "_blank" : undefined}
                      rel="noreferrer"
                      onClick={(event) => {
                        const local = internalLink(href);
                        if (local) {
                          event.preventDefault();
                          choose(local);
                        } else if (href.startsWith("#")) {
                          event.preventDefault();
                          const id = decodeURIComponent(href.slice(1));
                          const target = article.current?.querySelector(`#${CSS.escape(id)}`);
                          if (target) target.scrollIntoView({ block: "start" });
                          else {
                            const destination = docs.topics.find((item) =>
                              localizeTopic(item, language)
                                .text.split("\n")
                                .some((line) => /^#{1,6} /.test(line) && headingId(line.replace(/^#+\s+/, "")) === id),
                            );
                            if (destination) {
                              pendingAnchor.current = id;
                              choose(destination.id);
                            }
                          }
                        }
                      }}
                    >
                      {children}
                    </a>
                  ),
                }}
              >
                {topic.text}
              </Markdown>
            </article>
          </div>
        </section>
      </div>
    </>
  );
}
