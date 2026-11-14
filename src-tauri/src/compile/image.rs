//! The Linux system image of compile VMs: Ubuntu with a fixed package list,
//! built by CI (`native/linux-image/build.sh`) and bundled with the app as
//! `linux-image/{system.erofs,Image,image.json}`. Nothing is built at runtime;
//! development checkouts stage it with `npm run linux-image:build`.
//! `GIZMOTH_LINUX_IMAGE` names another folder with the same files.
use super::{
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
    "The Linux system image is not bundled with this build; run `npm run linux-image:build`";

/// The bundled image, found once at startup.
pub struct ImageState {
    pub system: Option<PathBuf>,
    pub kernel: Option<PathBuf>,
    pub info: Option<Value>,
}

fn folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(path) = std::env::var_os("GIZMOTH_LINUX_IMAGE") {
        folders.push(PathBuf::from(path));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        // Bundled resources, then resources copied next to a dev executable.
        folders.push(dir.join("../Resources/linux-image"));
        folders.push(dir.join("linux-image"));
    }
    folders.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/linux-image"));
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
        let found = folders
            .iter()
            .find(|dir| dir.join("system.erofs").is_file() && dir.join("Image").is_file());
        match found {
            Some(dir) => ImageState {
                system: Some(dir.join("system.erofs")),
                kernel: Some(dir.join("Image")),
                info: fs::read(dir.join("image.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok()),
            },
            None => ImageState {
                system: None,
                kernel: None,
                info: None,
            },
        }
    }

    pub fn ready(&self) -> bool {
        self.system.is_some()
    }

    /// Kernel `Image` and system disk for VMs.
    pub fn files(&self) -> Result<(PathBuf, PathBuf), String> {
        match (&self.kernel, &self.system) {
            (Some(kernel), Some(system)) => Ok((kernel.clone(), system.clone())),
            _ => Err(MISSING.into()),
        }
    }

    pub fn view(&self) -> Value {
        let info = self.info.clone().unwrap_or(Value::Null);
        let text = |key: &str| info[key].as_str().map(str::to_owned);
        json!({
            "status": if self.ready() { "ready" } else { "missing" },
            "error": if self.ready() { Value::Null } else { MISSING.into() },
            "bytes": self.system.as_ref().and_then(|p| fs::metadata(p).ok()).map(|m| m.len()),
            "builtAt": info["builtAt"].as_u64(),
            "commit": text("commit"),
            "base": text("base"),
            "kernel": text("kernel"),
            "bazelisk": text("bazelisk"),
            "packages": info["packages"].as_array().cloned().unwrap_or_default(),
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
    fn the_first_folder_with_an_image_and_kernel_wins() {
        let dir = tempfile::tempdir().unwrap();
        let (empty, partial, full) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        for folder in [&empty, &partial, &full] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(partial.join("system.erofs"), b"x").unwrap();
        fs::write(full.join("system.erofs"), b"xyz").unwrap();
        fs::write(full.join("Image"), b"k").unwrap();
        fs::write(
            full.join("image.json"),
            br#"{"builtAt":5,"base":"Ubuntu","packages":["git"]}"#,
        )
        .unwrap();
        let image = ImageState::from_folders(&[empty.clone(), partial.clone(), full.clone()]);
        assert_eq!(
            image.files().unwrap(),
            (full.join("Image"), full.join("system.erofs"))
        );
        let view = image.view();
        assert_eq!(view["status"], "ready");
        assert_eq!(view["bytes"], 3);
        assert_eq!(view["packages"], json!(["git"]));
        let missing = ImageState::from_folders(&[empty, partial]);
        assert_eq!(missing.files().unwrap_err(), MISSING);
        assert_eq!(missing.view()["status"], "missing");
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
