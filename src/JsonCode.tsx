import { useMemo, type ReactNode } from "react";
import { jsonLanguage } from "@codemirror/lang-json";
import { highlightTree, tagHighlighter, tags } from "@lezer/highlight";
import "./JsonCode.css";

const highlighter = tagHighlighter([
  { tag: tags.propertyName, class: "json-property" },
  { tag: tags.string, class: "json-string" },
  { tag: tags.number, class: "json-number" },
  { tag: [tags.bool, tags.null], class: "json-literal" },
]);

/** Read-only syntax highlighting; React escapes every token and preserves the original text. */
export function JsonCode({ text }: { text: string }) {
  const content = useMemo(() => {
    const nodes: ReactNode[] = [];
    let offset = 0;
    highlightTree(jsonLanguage.parser.parse(text), highlighter, (from, to, className) => {
      if (from > offset) nodes.push(text.slice(offset, from));
      nodes.push(<span key={from} className={className}>{text.slice(from, to)}</span>);
      offset = to;
    });
    if (offset < text.length) nodes.push(text.slice(offset));
    return nodes;
  }, [text]);
  return <code className="json-code">{content}</code>;
}
