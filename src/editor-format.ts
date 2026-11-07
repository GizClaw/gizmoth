import { invoke, isTauri } from "@tauri-apps/api/core";
import type { EditorLanguage } from "./editor-language.ts";

/** Parse and format text only; never execute source or load project-owned plugins. */
export async function formatEditorCode(code: string, language: EditorLanguage): Promise<string> {
  if (new TextEncoder().encode(code).length > 2 * 1024 * 1024) throw new Error("格式化文件不能超过 2 MiB。");
  if (language === "text") throw new Error("请先选择代码语言。");
  if (language === "lua") {
    if (isTauri()) return invoke<string>("format_lua_code", { code });
    return (await import("./editor-lua-browser")).formatBrowserLua(code);
  }
  const { format } = await import("prettier/standalone");
  const options = { tabWidth: 2, useTabs: false, printWidth: 100, endOfLine: "lf" as const };
  switch (language) {
    case "javascript":
    case "json": {
      const [babel, estree] = await Promise.all([import("prettier/plugins/babel"), import("prettier/plugins/estree")]);
      return format(code, { ...options, parser: language === "json" ? "json" : "babel", plugins: [babel, estree] });
    }
    case "typescript": {
      const [typescript, estree] = await Promise.all([
        import("prettier/plugins/typescript"),
        import("prettier/plugins/estree"),
      ]);
      return format(code, { ...options, parser: "typescript", plugins: [typescript, estree] });
    }
    case "html":
      return format(code, {
        ...options,
        parser: "html",
        plugins: [await import("prettier/plugins/html")],
        embeddedLanguageFormatting: "off",
      });
    case "css":
      return format(code, { ...options, parser: "css", plugins: [await import("prettier/plugins/postcss")] });
    case "markdown":
      return format(code, {
        ...options,
        parser: "markdown",
        plugins: [await import("prettier/plugins/markdown")],
        embeddedLanguageFormatting: "off",
      });
    case "yaml":
      return format(code, { ...options, parser: "yaml", plugins: [await import("prettier/plugins/yaml")] });
  }
}
