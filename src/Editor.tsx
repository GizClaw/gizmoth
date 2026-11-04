import CodeMirror from "@uiw/react-codemirror";
import { StreamLanguage } from "@codemirror/language";
import { lua } from "@codemirror/legacy-modes/mode/lua";
import { json } from "@codemirror/lang-json";
import { EditorView } from "@codemirror/view";
import { useMemo } from "react";

const theme = EditorView.theme({
  "&": { height: "100%", fontSize: "13px", backgroundColor: "#fff" },
  ".cm-scroller": {
    overflow: "auto",
    fontFamily: "'SFMono-Regular', Consolas, 'Liberation Mono', monospace",
    lineHeight: "1.85",
  },
  ".cm-content": { padding: "22px 0" },
  ".cm-line": { padding: "0 22px" },
  ".cm-gutters": { backgroundColor: "#fff", color: "#8e8e93", border: "none" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "#f6f6f8" },
  "&.cm-focused": { outline: "none" },
});

export function Editor({
  path,
  content,
  onChange,
}: {
  path: string;
  content: string;
  onChange: (value: string) => void;
}) {
  const extensions = useMemo(
    () => [
      theme,
      ...(path.endsWith(".lua") ? [StreamLanguage.define(lua)] : path.endsWith(".json") ? [json()] : []),
      EditorView.contentAttributes.of({
        "aria-label": `编辑 ${path}`,
        spellcheck: "false",
      }),
    ],
    [path],
  );
  return (
    <CodeMirror
      className="source-editor"
      value={content}
      height="100%"
      extensions={extensions}
      onChange={onChange}
      basicSetup={{
        foldGutter: true,
        autocompletion: true,
        highlightActiveLine: true,
      }}
    />
  );
}
