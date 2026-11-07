import init, { Config, IndentType, LuaVersion, OutputVerification, formatCode } from "@johnnymorganz/stylua/web";
import wasmUrl from "@johnnymorganz/stylua/stylua_lib_bg.wasm?url";
let ready: Promise<unknown> | undefined;
export async function formatBrowserLua(code: string): Promise<string> {
  ready ??= init({ module_or_path: wasmUrl }).catch((error) => {
    ready = undefined;
    throw error;
  });
  await ready;
  const config = Config.new();
  config.syntax = LuaVersion.Lua54;
  config.indent_type = IndentType.Spaces;
  config.indent_width = 2;
  config.column_width = 100;
  // formatCode consumes Config; do not free its transferred pointer afterwards.
  return formatCode(code, config, undefined, OutputVerification.Full);
}
