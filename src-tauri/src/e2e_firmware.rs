//! Optional native-test firmware delivery. Never compiled into a normal app.
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};
use tauri::{AppHandle, Manager};

const TAR_ROUTE: &str = "/__gizmoth-native-e2e/h106-probe.web.tar";
const GZIP_ROUTE: &str = "/__gizmoth-native-e2e/h106-probe.web.tar.gz";

fn archive(app: &AppHandle) -> Result<Option<Vec<u8>>, String> {
    let Some(value) = std::env::var_os("GIZMOTH_E2E_FIRMWARE_ARCHIVE") else {
        return Ok(None);
    };
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    if !root.join(".gizmoth-native-e2e").is_file() {
        return Err("Firmware probes require harness-owned application data.".into());
    }
    let expected = root
        .join("firmware-probe.archive")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let supplied = PathBuf::from(value)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if supplied != expected {
        return Err("Firmware probes must be staged by the native harness.".into());
    }
    let size = fs::metadata(&expected).map_err(|e| e.to_string())?.len();
    if size == 0 || size > 128 * 1024 * 1024 {
        return Err("Invalid firmware probe size.".into());
    }
    fs::read(expected).map(Some).map_err(|e| e.to_string())
}

pub fn asset(app: &AppHandle, path: &str) -> Option<Vec<u8>> {
    if path != TAR_ROUTE && path != GZIP_ROUTE {
        return None;
    }
    let bytes = archive(app).ok().flatten()?;
    let route = if bytes.starts_with(&[0x1f, 0x8b]) {
        GZIP_ROUTE
    } else {
        TAR_ROUTE
    };
    (path == route).then_some(bytes)
}

pub fn initialization_script(app: &AppHandle) -> Result<Option<String>, String> {
    let Some(bytes) = archive(app)? else {
        return Ok(None);
    };
    let descriptor = serde_json::json!({
        "path": if bytes.starts_with(&[0x1f, 0x8b]) { GZIP_ROUTE } else { TAR_ROUTE },
        "size": bytes.len(),
        "sha256": format!("{:x}", Sha256::digest(&bytes)),
    });
    // The ordinary provider still validates model, board, ABI, size and SHA-256.
    // Only this owned test archive replaces the selected Tiga release descriptor.
    Ok(Some(format!(
        r#"
(() => {{
  const probe = {descriptor};
  const original = window.fetch;
  window.fetch = async function(input, options) {{
    const response = await original.call(this, input, options);
    const url = typeof input === 'string' ? input : input?.url;
    if (url !== 'https://static-volc.gizclaw.com/models/play-trial/manifest.json' || !response.ok) return response;
    const catalog = await response.json();
    for (const model of catalog.models ?? []) {{
      if (model.id !== 'h106-tiga') continue;
      for (const firmware of model.firmwares ?? []) {{
        if (firmware.id !== 'h106-tiga-trial') continue;
        const version = '0.1.1-probe.1';
        firmware.default_version = version;
        firmware.versions = {{ [version]: {{
          abi: firmware.abi, board: firmware.board, version,
          archive: {{ url: new URL(probe.path, location.href).href, size: probe.size, sha256: probe.sha256, mime: 'application/octet-stream' }}
        }} }};
      }}
    }}
    return new Response(JSON.stringify(catalog), {{ status: 200, headers: {{ 'Content-Type': 'application/json', 'Cache-Control': 'no-store' }} }});
  }};
}})();
"#
    )))
}
