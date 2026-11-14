//! Pinned downloads for compile VMs (guest kernel, Ubuntu base, Bazelisk), the
//! bundled OpenVMM and agent executables, and per-boot initramfs images.
use super::guest::protocol::BootConfig;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Asset {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

/// OpenVMM's published arm64 test kernel (Linux 6.18) with virtio-blk/net,
/// vsock, ext4 and overlayfs built in. The `Image` member is extracted.
pub const KERNEL: Asset = Asset {
    name: "Linux 6.18 (openvmm-deps 0.3.0-185)",
    url: "https://github.com/microsoft/openvmm-deps/releases/download/0.3.0-185/openvmm-test-linux-6.18.aarch64.0.3.0-185.tar.gz",
    sha256: "095b713b02013f826dd67c76d915f991fa461070fec94bb4e488d90acbb26ab1",
    bytes: 19_013_610,
};

pub const UBUNTU_BASE: Asset = Asset {
    name: "Ubuntu 26.04.1 base (arm64)",
    url: "https://cdimage.ubuntu.com/ubuntu-base/releases/26.04/release/ubuntu-base-26.04.1-base-arm64.tar.gz",
    sha256: "5a1906794ced63a71a8119c3f211ef5f0bbe0a243001b4bbd41fdf80c5b219fd",
    bytes: 35_092_106,
};

pub const BAZELISK: Asset = Asset {
    name: "Bazelisk 1.30.0 (linux-arm64)",
    url: "https://github.com/bazelbuild/bazelisk/releases/download/v1.30.0/bazelisk-linux-arm64",
    sha256: "b2bcc040c213b44586b121bda6d983c35df121b917c3a7a66fb16f2fa0df1a4f",
    bytes: 7_209_120,
};

pub const AGENT_FILE: &str = "gizmoth-compile-agent";

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Returns the verified local copy, downloading it once. `progress` receives
/// (downloaded, total) bytes.
pub fn fetch(
    dir: &Path,
    asset: &Asset,
    progress: &dyn Fn(u64, u64),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    fetch_url(
        dir,
        asset.name,
        asset.url,
        asset.sha256,
        asset.bytes,
        progress,
        cancel,
    )
}

/// Content-addressed download into `dir/<sha256>`, shared by pinned assets and
/// registry packages.
pub fn fetch_url(
    dir: &Path,
    name: &str,
    url: &str,
    sha256: &str,
    bytes: u64,
    progress: &dyn Fn(u64, u64),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{name}: invalid SHA-256"));
    }
    let path = dir.join(sha256);
    if path.is_file() {
        if sha256_file(&path).map_err(|e| e.to_string())? == sha256 {
            progress(bytes, bytes);
            return Ok(path);
        }
        let _ = fs::remove_file(&path);
    }
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let partial = dir.join(format!("{sha256}.partial"));
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(30 * 60))
        .user_agent(concat!("GizMoth/", env!("CARGO_PKG_VERSION")))
        // Release downloads redirect to storage hosts; only HTTPS hops are followed.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() == "https" && attempt.previous().len() < 10 {
                attempt.follow()
            } else {
                attempt.error("only HTTPS redirects are followed")
            }
        }))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("{name}: {e}"))?;
    let total = response.content_length().unwrap_or(bytes);
    let mut file = File::create(&partial).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let (mut done, mut buffer) = (0u64, vec![0u8; 256 * 1024]);
    loop {
        if cancel() {
            let _ = fs::remove_file(&partial);
            return Err("cancelled".into());
        }
        let n = response
            .read(&mut buffer)
            .map_err(|e| format!("{name}: {e}"))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if bytes > 0 && done > bytes {
            let _ = fs::remove_file(&partial);
            return Err(format!("{name}: larger than the {bytes} bytes expected"));
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        progress(done, total);
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let digest = hex(&hasher.finalize());
    if digest != sha256 {
        let _ = fs::remove_file(&partial);
        return Err(format!("{name}: checksum mismatch ({digest})"));
    }
    fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Extracts the arm64 `Image` from the kernel archive next to it.
pub fn kernel_image(archive: &Path) -> Result<PathBuf, String> {
    let target = archive.with_extension("Image");
    if target.is_file() {
        return Ok(target);
    }
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(io::BufReader::new(file)));
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        if path.file_name().is_some_and(|name| name == "Image") {
            let partial = target.with_extension("Image.partial");
            let mut out = File::create(&partial).map_err(|e| e.to_string())?;
            io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
            out.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&partial, &target).map_err(|e| e.to_string())?;
            return Ok(target);
        }
    }
    Err("the kernel archive has no Image".into())
}

fn bundle_candidates(name: &str, env: &str, resource: Option<&str>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os(env) {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            match resource {
                // Bundled resources: Contents/Resources/<dir>/<name>; dev builds copy
                // resources next to the executable.
                Some(resource) => {
                    candidates.push(dir.join("../Resources").join(resource).join(name));
                    candidates.push(dir.join(resource).join(name));
                }
                // Sidecars live beside the executable.
                None => candidates.push(dir.join(name)),
            }
        }
    }
    // Development checkouts: artifacts produced by scripts/build-*.mjs.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    match resource {
        Some(resource) => candidates.push(manifest.join("resources").join(resource).join(name)),
        None => candidates.push(
            manifest
                .join("binaries")
                .join(format!("{name}-aarch64-apple-darwin")),
        ),
    }
    candidates
}

fn locate(name: &str, env: &str, resource: Option<&str>) -> Option<PathBuf> {
    bundle_candidates(name, env, resource)
        .into_iter()
        .find(|path| path.is_file())
}

pub fn openvmm() -> Result<PathBuf, String> {
    locate("openvmm", "GIZMOTH_OPENVMM", None)
        .ok_or_else(|| "OpenVMM is not bundled with this build; run `npm run openvmm:build`".into())
}

pub fn agent() -> Result<PathBuf, String> {
    locate(AGENT_FILE, "GIZMOTH_COMPILE_AGENT", Some("compile-agent")).ok_or_else(|| {
        "The compile agent is not bundled with this build; run `npm run compile-agent:build`".into()
    })
}

/// Writes a newc cpio archive: `/init`, `/dev/console`, `/config.json` and
/// optional extra files. The kernel needs `/dev/console` for PID 1's output.
pub fn write_initramfs(
    path: &Path,
    agent: &Path,
    config: &BootConfig,
    extra: &[(&str, &Path)],
) -> Result<(), String> {
    let mut out = io::BufWriter::new(File::create(path).map_err(|e| e.to_string())?);
    let mut ino = 1u32;
    let mut entry = |out: &mut io::BufWriter<File>,
                     name: &str,
                     mode: u32,
                     rdev: (u32, u32),
                     data: &mut dyn Read,
                     size: u64|
     -> io::Result<()> {
        let namesize = name.len() + 1;
        let header = format!(
            "070701{ino:08x}{mode:08x}{:08x}{:08x}{:08x}{:08x}{size:08x}{:08x}{:08x}{:08x}{:08x}{namesize:08x}{:08x}",
            0, 0, 1, 0, 0, 0, rdev.0, rdev.1, 0
        );
        ino += 1;
        out.write_all(header.as_bytes())?;
        out.write_all(name.as_bytes())?;
        out.write_all(&[0])?;
        out.write_all(&vec![0u8; (4 - (110 + namesize) % 4) % 4])?;
        let copied = io::copy(data, out)?;
        if copied != size {
            return Err(io::Error::other(format!("{name} changed while packing")));
        }
        out.write_all(&vec![0u8; ((4 - size % 4) % 4) as usize])
    };
    let config = serde_json::to_vec(config).map_err(|e| e.to_string())?;
    let result = (|| -> io::Result<()> {
        entry(&mut out, "dev", 0o040755, (0, 0), &mut io::empty(), 0)?;
        entry(
            &mut out,
            "dev/console",
            0o020600,
            (5, 1),
            &mut io::empty(),
            0,
        )?;
        let size = fs::metadata(agent)?.len();
        entry(
            &mut out,
            "init",
            0o100755,
            (0, 0),
            &mut File::open(agent)?,
            size,
        )?;
        entry(
            &mut out,
            "config.json",
            0o100644,
            (0, 0),
            &mut config.as_slice(),
            config.len() as u64,
        )?;
        for (name, source) in extra {
            let size = fs::metadata(source)?.len();
            entry(
                &mut out,
                name,
                0o100755,
                (0, 0),
                &mut File::open(source)?,
                size,
            )?;
        }
        entry(&mut out, "TRAILER!!!", 0, (0, 0), &mut io::empty(), 0)?;
        out.flush()
    })();
    result.map_err(|e| format!("initramfs: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initramfs_is_a_valid_newc_archive() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        fs::write(&agent, b"\x7fELF-agent").unwrap();
        let config: BootConfig =
            serde_json::from_value(serde_json::json!({"mode":"worker","slot":"s1"})).unwrap();
        let path = dir.path().join("initramfs.cpio");
        write_initramfs(&path, &agent, &config, &[]).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len() % 4, 0);
        let mut offset = 0;
        let mut names = Vec::new();
        loop {
            assert_eq!(&bytes[offset..offset + 6], b"070701");
            let field = |i: usize| {
                u32::from_str_radix(
                    std::str::from_utf8(&bytes[offset + 6 + i * 8..offset + 14 + i * 8]).unwrap(),
                    16,
                )
                .unwrap() as usize
            };
            let (size, namesize) = (field(6), field(11));
            let name = std::str::from_utf8(&bytes[offset + 110..offset + 110 + namesize - 1])
                .unwrap()
                .to_string();
            offset += (110 + namesize + 3) & !3;
            if name == "init" {
                assert_eq!(&bytes[offset..offset + size], b"\x7fELF-agent");
            }
            offset += (size + 3) & !3;
            names.push(name.clone());
            if name == "TRAILER!!!" {
                break;
            }
        }
        assert_eq!(
            names,
            ["dev", "dev/console", "init", "config.json", "TRAILER!!!"]
        );
        assert_eq!(offset, bytes.len());
    }
}
