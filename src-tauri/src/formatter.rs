use stylua_lib::{format_code, Config, IndentType, LuaVersion, OutputVerification};

/// Formatting parses text and verifies the output AST; it never runs Lua code.
pub fn lua(code: &str) -> Result<String, String> {
    if code.len() > 2 * 1024 * 1024 {
        return Err("格式化文件不能超过 2 MiB。".into());
    }
    let config = Config {
        syntax: LuaVersion::Lua54,
        indent_type: IndentType::Spaces,
        indent_width: 2,
        column_width: 100,
        ..Config::default()
    };
    format_code(code, config, None, OutputVerification::Full)
        .map_err(|error| format!("Lua 格式化失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_lua54_and_preserves_comment_string_and_attributes() {
        let source = "-- keep\nlocal value<const> = {name='hello world', n=2}\nif value.n>0 then print(value.name) end";
        let formatted = lua(source).unwrap();
        assert!(formatted.contains("-- keep"));
        assert!(formatted.contains("<const>"));
        assert!(formatted.contains("hello world"));
        assert!(formatted.contains("\n  print(value.name)\n"));
        assert_eq!(lua(&formatted).unwrap(), formatted);
    }
    #[test]
    fn invalid_or_oversized_source_is_rejected() {
        assert!(lua("local =").is_err());
        assert!(lua(&"x".repeat(2 * 1024 * 1024 + 1)).is_err());
    }
}
