#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The resident serial daemon and the global stdio MCP server are modes of this
    // executable; dispatch them before any window or Tauri state exists.
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = gizmoth_lib::dispatch(&args) {
        std::process::exit(code);
    }
    gizmoth_lib::run()
}
