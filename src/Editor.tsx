import CodeMirror from "@uiw/react-codemirror";
import { StreamLanguage, HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags } from "@lezer/highlight";
import { lua } from "@codemirror/legacy-modes/mode/lua";
import { yaml } from "@codemirror/legacy-modes/mode/yaml";
import { json } from "@codemirror/lang-json";
import { javascript } from "@codemirror/lang-javascript";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import { markdown } from "@codemirror/lang-markdown";
import { EditorView } from "@codemirror/view";
import { openSearchPanel } from "@codemirror/search";
import { useEffect, useMemo, useRef } from "react";
import type { EditorLanguage } from "./editor-language";
import { useTheme } from "./use-theme";
import { useI18n } from "./i18n";

const theme = EditorView.theme({
  "&": { height: "100%", fontSize: "13px", backgroundColor: "var(--card)", color: "var(--text)" },
  ".cm-scroller": {
    overflow: "auto",
    fontFamily: "'SFMono-Regular', Consolas, 'Liberation Mono', monospace",
    lineHeight: "1.85",
  },
  ".cm-content": { padding: "10px 0" },
  ".cm-line": { padding: "0 16px" },
  ".cm-gutters": { backgroundColor: "var(--card)", color: "var(--editor-gutter)", border: "none" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "var(--editor-active-line)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--text)" },
  "&.cm-focused": { outline: "none" },
});
export interface EditorPosition {
  anchor: number;
  head: number;
  scrollTop: number;
  scrollLeft: number;
}
export function Editor({
  path,
  content,
  onChange,
  language,
  disabled = false,
  searchRequest = 0,
  positions,
}: {
  path: string;
  content: string;
  onChange: (value: string) => void;
  language: EditorLanguage;
  disabled?: boolean;
  searchRequest?: number;
  positions?: Map<string, EditorPosition>;
}) {
  const { theme: appearance } = useTheme();
  const { t, language: appLanguage } = useI18n();
  const view = useRef<EditorView | null>(null);
  const extensions = useMemo(() => {
    const syntax =
      language === "lua"
        ? StreamLanguage.define(lua)
        : language === "json"
          ? json()
          : language === "javascript" || language === "typescript"
            ? javascript({ typescript: language === "typescript", jsx: /\.[jt]sx$/i.test(path) })
            : language === "html"
              ? html()
              : language === "css"
                ? css()
                : language === "markdown"
                  ? markdown()
                  : language === "yaml"
                    ? StreamLanguage.define(yaml)
                    : [];
    return [
      theme,
      syntax,
      syntaxHighlighting(HighlightStyle.define([{ tag: tags.comment, class: "editor-comment" }])),
      EditorView.contentAttributes.of({ "aria-label": t("编辑 {path}", { path }), spellcheck: "false" }),
    ];
  }, [path, language, appLanguage]);
  const lastSearch = useRef(searchRequest);
  useEffect(() => {
    if (searchRequest !== lastSearch.current && view.current) openSearchPanel(view.current);
    lastSearch.current = searchRequest;
  }, [searchRequest]);
  const remember = () => {
    const current = view.current;
    if (current && positions)
      positions.set(path, {
        anchor: current.state.selection.main.anchor,
        head: current.state.selection.main.head,
        scrollTop: current.scrollDOM.scrollTop,
        scrollLeft: current.scrollDOM.scrollLeft,
      });
  };
  useEffect(() => remember, [path, positions]);
  return (
    <CodeMirror
      className="source-editor"
      value={content}
      theme={appearance}
      height="100%"
      extensions={extensions}
      readOnly={disabled}
      editable={!disabled}
      onChange={onChange}
      onCreateEditor={(created) => {
        view.current = created;
        const position = positions?.get(path);
        if (position) {
          const bound = (n: number) => Math.min(n, created.state.doc.length);
          created.dispatch({ selection: { anchor: bound(position.anchor), head: bound(position.head) } });
          created.scrollDOM.scrollTop = position.scrollTop;
          created.scrollDOM.scrollLeft = position.scrollLeft;
        }
      }}
      onUpdate={remember}
      basicSetup={{ foldGutter: true, autocompletion: true, highlightActiveLine: true }}
    />
  );
}
