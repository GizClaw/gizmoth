use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

/// Links native/serial-core (GizOS Host Core, built by
/// scripts/build-serial-core.mjs) when its archive exists for this target.
/// Release builds set GIZMOTH_REQUIRE_SERIAL_CORE=1 so it cannot be omitted.
fn serial_core() {
    println!("cargo::rustc-check-cfg=cfg(gizmoth_serial_core)");
    println!("cargo:rerun-if-env-changed=GIZMOTH_REQUIRE_SERIAL_CORE");
    let target = std::env::var("TARGET").unwrap();
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let directory = manifest.join("../native/serial-core/dist").join(&target);
    let archive = if target.contains("windows") {
        "gizmoth_serial_core.lib"
    } else {
        "libgizmoth_serial_core.a"
    };
    println!(
        "cargo:rerun-if-changed={}",
        directory.join(archive).display()
    );
    if !directory.join(archive).is_file() {
        if std::env::var("GIZMOTH_REQUIRE_SERIAL_CORE").as_deref() == Ok("1") {
            panic!(
                "missing {}; run npm run serial-core:build",
                directory.join(archive).display()
            );
        }
        println!("cargo:warning=H2Loader serial core not built for {target}; H2Loader ports will be unavailable");
        return;
    }
    println!("cargo:rustc-cfg=gizmoth_serial_core");
    println!("cargo:rustc-link-search=native={}", directory.display());
    println!("cargo:rustc-link-lib=static=gizmoth_serial_core");
    if target.contains("apple") {
        for framework in [
            "CoreBluetooth",
            "CoreFoundation",
            "Foundation",
            "IOKit",
            "Security",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:rustc-link-lib=objc");
        println!("cargo:rustc-link-lib=c++");
    } else if target.contains("windows") {
        for library in [
            "bcrypt",
            "iphlpapi",
            "synchronization",
            "ws2_32",
            "setupapi",
            "advapi32",
            "cfgmgr32",
        ] {
            println!("cargo:rustc-link-lib={library}");
        }
    } else {
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=stdc++");
    }
}

// A deterministic implementation identity, separate from the compatible API
// version. FNV-1a needs no build dependency and catches same-version dev rebuilds.
struct Fingerprint(u64);

impl Fingerprint {
    fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    fn file(&mut self, root: &Path, path: &Path) {
        println!("cargo:rerun-if-changed={}", path.display());
        let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
        self.bytes(&(relative.len() as u64).to_le_bytes());
        self.bytes(relative.as_bytes());
        let mut file = File::open(path)
            .unwrap_or_else(|error| panic!("cannot fingerprint {}: {error}", path.display()));
        self.bytes(&file.metadata().unwrap().len().to_le_bytes());
        let mut buffer = [0; 16 * 1024];
        loop {
            let count = file.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            self.bytes(&buffer[..count]);
        }
    }

    fn directory(&mut self, root: &Path, path: &Path) {
        println!("cargo:rerun-if-changed={}", path.display());
        let mut children: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let name = child.file_name();
            let name = name.to_string_lossy();
            if name == "dist" || name == ".git" || name.starts_with("bazel-") {
                continue;
            }
            let kind = child.file_type().unwrap();
            if kind.is_dir() {
                self.directory(root, &child.path());
            } else if kind.is_file() {
                self.file(root, &child.path());
            }
        }
    }
}

fn serial_build_identity() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.parent().unwrap();
    let mut fingerprint = Fingerprint(0xcbf29ce484222325);
    for key in [
        "CARGO_PKG_VERSION",
        "TARGET",
        "PROFILE",
        "OPT_LEVEL",
        "DEBUG",
    ] {
        fingerprint.bytes(key.as_bytes());
        fingerprint.bytes(std::env::var(key).unwrap_or_default().as_bytes());
    }
    for name in ["Cargo.toml", "Cargo.lock", "build.rs"] {
        fingerprint.file(root, &manifest.join(name));
    }
    fingerprint.directory(root, &manifest.join("src/serial"));
    fingerprint.directory(root, &root.join("native/serial-core"));
    fingerprint.file(root, &root.join("scripts/build-serial-core.mjs"));
    let target = std::env::var("TARGET").unwrap();
    let archive =
        root.join("native/serial-core/dist")
            .join(&target)
            .join(if target.contains("windows") {
                "gizmoth_serial_core.lib"
            } else {
                "libgizmoth_serial_core.a"
            });
    // Also fingerprints a staged local GizOS override, whose implementation
    // may have changed without changing this adapter or its pinned dependency.
    println!("cargo:rerun-if-changed={}", archive.display());
    if archive.is_file() {
        fingerprint.file(root, &archive);
    } else {
        fingerprint.bytes(b"serial-core-unavailable");
    }
    println!(
        "cargo:rustc-env=GIZMOTH_SERIAL_BUILD={}-{:016x}",
        std::env::var("CARGO_PKG_VERSION").unwrap(),
        fingerprint.0
    );
}

fn main() {
    serial_build_identity();
    serial_core();
    tauri_build::build()
}
