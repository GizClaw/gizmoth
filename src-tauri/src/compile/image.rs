//! Minimal compile boot resources: an ARM64 Linux kernel and the separately
//! staged static agent. All userland comes from downloaded devenv formulae.
//! `npm run linux-kernel:stage` stages `linux-kernel/{Image,kernel.json}`;
//! `GIZMOTH_LINUX_KERNEL` names another folder with the same files.
use super::{
    assets,
    guest::{
        self,
        protocol::{self, GuestMessage, HostMessage},
    },
    vm,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub const MISSING: &str =
    "The Linux kernel is not bundled with this build; run `npm run linux-kernel:stage`";

/// The bundled kernel, found once at startup. `image` remains the snapshot key
/// for compatibility; no system disk is bundled or required.
pub struct ImageState {
    pub kernel: Option<PathBuf>,
    pub info: Option<Value>,
}

fn folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(path) = std::env::var_os("GIZMOTH_LINUX_KERNEL") {
        folders.push(PathBuf::from(path));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        // Bundled resources, then resources copied next to a dev executable.
        folders.push(dir.join("../Resources/linux-kernel"));
        folders.push(dir.join("linux-kernel"));
    }
    folders.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/linux-kernel"));
    folders
}

/// Removes the image and its downloads that earlier versions built in the
/// working directory, now that the image ships with the app. The kernel and
/// Ubuntu base archives are named by their SHA-256.
pub fn remove_legacy(root: &Path) {
    let image = root.join("image");
    if image.join("image.json").is_file() || image.join("system.img").is_file() {
        let _ = fs::remove_dir_all(&image);
    }
    for sha256 in [
        "095b713b02013f826dd67c76d915f991fa461070fec94bb4e488d90acbb26ab1",
        "5a1906794ced63a71a8119c3f211ef5f0bbe0a243001b4bbd41fdf80c5b219fd",
    ] {
        let _ = fs::remove_file(root.join("assets").join(sha256));
        let _ = fs::remove_file(root.join("assets").join(format!("{sha256}.Image")));
    }
}

impl ImageState {
    pub fn load() -> ImageState {
        Self::from_folders(&folders())
    }

    fn from_folders(folders: &[PathBuf]) -> ImageState {
        let found = folders.iter().find(|dir| dir.join("Image").is_file());
        match found {
            Some(dir) => ImageState {
                kernel: Some(dir.join("Image")),
                info: fs::read(dir.join("kernel.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok()),
            },
            None => ImageState {
                kernel: None,
                info: None,
            },
        }
    }

    pub fn ready(&self) -> bool {
        self.kernel.is_some()
    }

    /// The ARM64 kernel `Image`. Userland is supplied by devenv downloads.
    pub fn files(&self) -> Result<PathBuf, String> {
        self.kernel.clone().ok_or_else(|| MISSING.into())
    }

    pub fn view(&self) -> Value {
        let info = self.info.clone().unwrap_or(Value::Null);
        let text = |key: &str| info[key].as_str().map(str::to_owned);
        let agent_bytes = assets::agent()
            .ok()
            .and_then(|path| fs::metadata(path).ok())
            .map_or(0, |metadata| metadata.len());
        let bytes = self
            .kernel
            .as_ref()
            .and_then(|path| fs::metadata(path).ok())
            .map(|metadata| metadata.len().saturating_add(agent_bytes));
        json!({
            "status": if self.ready() { "ready" } else { "missing" },
            "error": if self.ready() { Value::Null } else { MISSING.into() },
            "bytes": bytes,
            "builtAt": info["builtAt"].as_u64(),
            "commit": text("commit"),
            "base": "Linux kernel + static agent",
            "kernel": text("kernel").or_else(|| text("name")),
            "bazelisk": Value::Null,
            "packages": [],
        })
    }
}

/// Holds one agent connection for a whole one-shot run (the package packer):
/// streams output and phases, and forwards cancellation.
pub(crate) fn session(
    machine: &mut vm::Vm,
    cancel: &Arc<AtomicBool>,
    log: &dyn Fn(&[u8]),
    phase: &dyn Fn(&str),
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut stream = loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        if !machine.alive() {
            return Err(machine.failure("The VM stopped during startup"));
        }
        if let Ok(mut stream) = guest::connect(
            &machine.socket,
            protocol::CONTROL_PORT,
            Duration::from_secs(10),
        ) {
            if protocol::write_json(&mut stream, &HostMessage::Hello).is_ok() {
                if let Ok(Some((protocol::JSON, payload))) = protocol::read_frame(&mut stream) {
                    if matches!(
                        protocol::parse::<GuestMessage>(&payload),
                        Ok(GuestMessage::Hello { .. })
                    ) {
                        break stream;
                    }
                }
            }
        }
        if Instant::now() > deadline {
            return Err(machine.failure("The VM did not respond"));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
    let finished = Arc::new(AtomicBool::new(false));
    {
        let (cancel, finished) = (cancel.clone(), finished.clone());
        let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            while !finished.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    let _ = protocol::write_json(&mut writer, &HostMessage::Cancel);
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    }
    let result = loop {
        match protocol::read_frame(&mut stream) {
            Ok(Some((protocol::STDOUT | protocol::STDERR, bytes))) => log(&bytes),
            Ok(Some((protocol::JSON, payload))) => {
                match protocol::parse::<GuestMessage>(&payload) {
                    Ok(GuestMessage::Phase { phase: name }) => phase(&name),
                    Ok(GuestMessage::Done { ok: true, .. }) => break Ok(()),
                    Ok(GuestMessage::Done { ok: false, message }) => {
                        break Err(message.unwrap_or_else(|| "the run failed".into()))
                    }
                    Ok(_) => {}
                    Err(error) => break Err(error.to_string()),
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break Err(machine.failure("The VM disconnected")),
        }
    };
    finished.store(true, Ordering::SeqCst);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_folder_with_a_kernel_wins_without_a_system_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (empty, legacy, full) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        for folder in [&empty, &legacy, &full] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(legacy.join("system.erofs"), b"x").unwrap();
        fs::write(full.join("Image"), b"kernel").unwrap();
        fs::write(
            full.join("kernel.json"),
            br#"{"builtAt":5,"kernel":"Linux test"}"#,
        )
        .unwrap();
        let image = ImageState::from_folders(&[empty.clone(), legacy.clone(), full.clone()]);
        assert_eq!(image.files().unwrap(), full.join("Image"));
        let view = image.view();
        assert_eq!(view["status"], "ready");
        assert!(view["bytes"].as_u64().unwrap() >= 6);
        assert_eq!(view["base"], "Linux kernel + static agent");
        assert_eq!(view["kernel"], "Linux test");
        assert_eq!(view["packages"], json!([]));
        assert!(view["bazelisk"].is_null());
        let missing = ImageState::from_folders(&[empty, legacy]);
        assert_eq!(missing.files().unwrap_err(), MISSING);
        assert_eq!(missing.view()["status"], "missing");
        fs::remove_file(full.join("kernel.json")).unwrap();
        assert!(ImageState::from_folders(&[full]).ready());
    }

    #[test]
    fn the_legacy_image_and_its_downloads_are_removed_but_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("image")).unwrap();
        fs::write(root.join("image/system.img"), b"x").unwrap();
        fs::write(root.join("image/image.json"), b"{}").unwrap();
        fs::create_dir_all(root.join("assets")).unwrap();
        let kernel = "095b713b02013f826dd67c76d915f991fa461070fec94bb4e488d90acbb26ab1";
        fs::write(root.join("assets").join(kernel), b"k").unwrap();
        fs::write(root.join("assets").join(format!("{kernel}.Image")), b"k").unwrap();
        // Package downloads (Bazelisk is one) stay.
        let package = "b2bcc040c213b44586b121bda6d983c35df121b917c3a7a66fb16f2fa0df1a4f";
        fs::write(root.join("assets").join(package), b"p").unwrap();
        remove_legacy(root);
        assert!(!root.join("image").exists());
        assert!(!root.join("assets").join(kernel).exists());
        assert!(root.join("assets").join(package).is_file());
        // A folder named image that GizMoth did not make stays.
        fs::create_dir_all(root.join("image")).unwrap();
        fs::write(root.join("image/notes.txt"), b"mine").unwrap();
        remove_legacy(root);
        assert!(root.join("image/notes.txt").is_file());
    }
}
