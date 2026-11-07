export const editorLanguages = [
  { id: "lua", name: "Lua" },
  { id: "json", name: "JSON" },
  { id: "javascript", name: "JavaScript" },
  { id: "typescript", name: "TypeScript" },
  { id: "html", name: "HTML" },
  { id: "css", name: "CSS" },
  { id: "markdown", name: "Markdown" },
  { id: "yaml", name: "YAML" },
  { id: "text", name: "纯文本" },
] as const;
export type EditorLanguage = (typeof editorLanguages)[number]["id"];
export function detectEditorLanguage(path: string): EditorLanguage {
  const extension = path.split(".").at(-1)?.toLowerCase();
  switch (extension) {
    case "lua":
      return "lua";
    case "json":
      return "json";
    case "js":
    case "jsx":
    case "mjs":
    case "cjs":
      return "javascript";
    case "ts":
    case "tsx":
    case "mts":
    case "cts":
      return "typescript";
    case "html":
    case "htm":
      return "html";
    case "css":
      return "css";
    case "md":
    case "markdown":
      return "markdown";
    case "yml":
    case "yaml":
      return "yaml";
    default:
      return "text";
  }
}
export const languageName = (language: EditorLanguage) => editorLanguages.find((item) => item.id === language)!.name;
