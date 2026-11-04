use flate2::{write::ZlibEncoder, Compression};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs};

#[derive(Clone, Serialize, Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub content: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Project {
    pub version: u8,
    pub files: Vec<SourceFile>,
}
#[derive(Deserialize)]
pub struct Config {
    pub app_id: String,
    pub version: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInfo {
    pub app_id: String,
    pub version: String,
    pub files: usize,
    pub bytes: usize,
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id.as_bytes()[0] != b'.'
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c))
}

pub fn validate(project: &Project) -> Result<Config, String> {
    if project.version != 1 || !(2..=65).contains(&project.files.len()) {
        return Err("工作区格式无效，需要 2–65 个文件。".into());
    }
    let mut paths = HashSet::new();
    for file in &project.files {
        let path = &file.path;
        if path.is_empty()
            || path.len() >= 192
            || path
                .chars()
                .any(|c| c.is_control() || c == '\\' || c == ':')
            || path
                .split('/')
                .any(|part| part.is_empty() || part.starts_with('.'))
        {
            return Err("文件路径无效，不能包含隐藏路径、绝对路径或控制字符。".into());
        }
        if !paths.insert(path.to_lowercase()) {
            return Err("文件路径重复（不区分大小写）。".into());
        }
    }
    for path in &paths {
        if paths
            .iter()
            .any(|p| p != path && p.starts_with(&format!("{path}/")))
        {
            return Err("文件与目录路径冲突。".into());
        }
    }
    let config_file = project
        .files
        .iter()
        .find(|f| f.path == "app.json")
        .ok_or("缺少 app.json")?;
    if config_file.content.len() > 32768 {
        return Err("app.json 超过 32 KiB".into());
    }
    let config: Config = serde_json::from_str(&config_file.content)
        .map_err(|_| "app.json 无效，请检查 app_id 和 version。")?;
    if !valid_id(&config.app_id)
        || config.version.len() > 31
        || semver::Version::parse(&config.version).is_err()
    {
        return Err("app_id 或 SemVer 版本号无效。".into());
    }
    for (text, max) in [(&config.display_name, 128), (&config.description, 1024)] {
        if text
            .as_ref()
            .is_some_and(|t| t.len() > max || t.contains('\0'))
        {
            return Err("程序名称或描述超出长度限制，或含 NUL。".into());
        }
    }
    let entry = format!("{}.lua", config.app_id);
    let source = project
        .files
        .iter()
        .find(|f| f.path == entry)
        .ok_or_else(|| format!("缺少入口文件 {entry}"))?;
    if source.content.len() > 256 * 1024
        || source.content.contains('\0')
        || source.content.starts_with("\u{1b}Lua")
    {
        return Err("Lua 入口必须是 256 KiB 以内的文本源码。".into());
    }
    let mut size = 0;
    for file in project.files.iter().filter(|f| f.path != "app.json") {
        if file.path != entry && !file.path.starts_with(&format!("{}/", config.app_id)) {
            return Err(format!("资源文件必须位于 {}/ 目录内。", config.app_id));
        }
        size += file.content.len();
    }
    if size > 2 * 1024 * 1024 {
        return Err("程序内容超过 2 MiB。".into());
    }
    Ok(config)
}

pub fn package(project: &Project) -> Result<(Config, Vec<u8>), String> {
    let config = validate(project)?;
    let mut payload: Vec<_> = project
        .files
        .iter()
        .filter(|f| f.path != "app.json")
        .collect();
    payload.sort_by(|a, b| a.path.cmp(&b.path));
    let files: Vec<_> = payload.iter().map(|f| json!({ "path": f.path, "size": f.content.len(), "sha256": format!("{:x}", Sha256::digest(f.content.as_bytes())) })).collect();
    let mut manifest = json!({ "format": 1, "type": "lua-app", "app_id": config.app_id, "version": config.version, "entry": format!("{}.lua", config.app_id), "data_dir": if payload.len() > 1 { Some(&config.app_id) } else { None }, "compact": false, "files": files });
    if let Some(name) = &config.display_name {
        manifest["display_name"] = json!(name);
    }
    if let Some(description) = &config.description {
        manifest["description"] = json!(description);
    }
    let manifest_bytes = serde_json::to_vec(&manifest).map_err(|_| "无法生成 manifest")?;
    if manifest_bytes.len() > 32768 {
        return Err("manifest 超过 32 KiB".into());
    }
    let encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    let mut archive = tar::Builder::new(encoder);
    let mut append = |path: &str, data: &[u8]| -> Result<(), String> {
        let mut header = tar::Header::new_ustar();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header
            .set_path(path)
            .map_err(|_| "文件路径无法编码为 USTAR，请缩短文件名。")?;
        header.set_cksum();
        archive
            .append(&header, data)
            .map_err(|_| "无法写入程序包".into())
    };
    append("manifest.json", &manifest_bytes)?;
    for file in payload {
        append(&file.path, file.content.as_bytes())?;
    }
    let encoder = archive.into_inner().map_err(|_| "无法完成 USTAR 文件")?;
    let bytes = encoder.finish().map_err(|_| "无法压缩程序包")?;
    Ok((config, bytes))
}

pub fn inspect(project: &Project) -> Result<PackageInfo, String> {
    let (config, bytes) = package(project)?;
    Ok(PackageInfo {
        app_id: config.app_id,
        version: config.version,
        files: project.files.len() - 1,
        bytes: bytes.len(),
    })
}

pub fn export(project: Project) -> Result<Option<String>, String> {
    let (config, bytes) = package(&project)?;
    let filename = format!("{}-{}.lua-app.tar.zlib", config.app_id, config.version);
    let Some(path) = rfd::FileDialog::new().set_file_name(filename).save_file() else {
        return Ok(None);
    };
    fs::write(&path, bytes).map_err(|_| "无法写入程序包，请检查文件权限。")?;
    Ok(Some(path.to_string_lossy().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    fn project() -> Project {
        Project {
            version: 1,
            files: vec![
                SourceFile {
                    path: "app.json".into(),
                    content: r#"{"app_id":"hello","version":"1.0.0"}"#.into(),
                },
                SourceFile {
                    path: "hello.lua".into(),
                    content: "return 1\n".into(),
                },
            ],
        }
    }
    #[test]
    fn package_matches_firmware_format_and_hashes() {
        let p = project();
        let (_, bytes) = package(&p).unwrap();
        assert_eq!(bytes, package(&p).unwrap().1);
        let decoder = flate2::read::ZlibDecoder::new(bytes.as_slice());
        let mut archive = tar::Archive::new(decoder);
        let mut entries = archive.entries().unwrap();
        let mut manifest = entries.next().unwrap().unwrap();
        assert_eq!(manifest.path().unwrap().to_str().unwrap(), "manifest.json");
        assert!(manifest.header().as_ustar().is_some());
        let mut text = String::new();
        manifest.read_to_string(&mut text).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["files"][0]["sha256"],
            "0805bfdc02e872ed322a4a4e440ae985f3a4335f4dd59f3d373dfaf2a68a4a3c"
        );
        assert_eq!(value["files"][0]["size"], 9);
        assert!(value["data_dir"].is_null());
        let mut source = entries.next().unwrap().unwrap();
        assert_eq!(source.path().unwrap().to_str().unwrap(), "hello.lua");
        let mut text = String::new();
        source.read_to_string(&mut text).unwrap();
        assert_eq!(text, "return 1\n");
        assert!(entries.next().is_none());
    }
    #[test]
    fn rejects_traversal_case_collisions_and_invalid_entry() {
        for path in [
            "../evil",
            "hello/.hidden",
            "hello/x/../y",
            "/hello",
            "hello\\evil",
            "hello.lua",
            "HELLO.LUA",
        ] {
            let mut p = project();
            p.files.push(SourceFile {
                path: path.into(),
                content: String::new(),
            });
            assert!(package(&p).is_err(), "{path}");
        }
        let mut p = project();
        p.files[1].content = "\0".into();
        assert!(package(&p).is_err());
    }
}
