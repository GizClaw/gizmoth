use super::*;
use crate::compile::artifacts::{Artifact, Store};
use std::{fs, os::unix::fs::symlink};

#[test]
fn writer_lease_and_pinned_roots_survive_rename_without_following_replacements() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("project");
    fs::create_dir(&path).unwrap();
    let workspace = Workspace::prepare(&path).unwrap();
    let another = Workspace::prepare(&path).unwrap();
    let lease = workspace.lock().unwrap();
    assert!(another.lock().is_err());
    drop(lease);
    drop(another.lock().unwrap());

    fs::rename(path.join("out"), root.join("old-out")).unwrap();
    fs::create_dir(path.join("out")).unwrap();
    assert!(workspace.verify().is_err());
    workspace.out.write("pinned", b"original inode").unwrap();
    assert!(!path.join("out/pinned").exists());
    let replacement = Workspace::prepare(&path).unwrap();
    assert!(workspace.key.conflicts(&replacement.key));
    fs::rename(&path, root.join("moved")).unwrap();
    symlink(root.join("moved"), &path).unwrap();
    assert!(replacement.verify().is_err());
    assert!(Workspace::prepare(&path).is_err());
    fs::remove_dir(root.join("moved/out")).unwrap();
    symlink(root.join("old-out"), root.join("moved/out")).unwrap();
    assert!(Workspace::prepare(&root.join("moved")).is_err());
}

#[test]
fn output_collection_bounds_and_snapshot_lifetime() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("firmware");
    fs::create_dir(&project).unwrap();
    let workspace = Workspace::prepare(&project).unwrap();
    workspace.out.create_dir_all("Debug/Bin").unwrap();
    workspace
        .out
        .write("Debug/Bin/Firmware.BIN", b"abcd")
        .unwrap();
    symlink(&project, project.join("out/elsewhere")).unwrap();
    symlink("Debug/Bin/Firmware.BIN", project.join("out/alias.bin")).unwrap();
    let store = Store::new(root.join("archives"));
    let patterns = [
        "debug/Bin/Firmware.BIN",
        "Debug/bin/*.BIN",
        "Debug/Bin/firmware.bin",
        "Debug/Bin/Firmware.BIN",
        "**/*.BIN",
        "Debug/*/Firmware.*",
        "elsewhere/**/*.bin",
        "missing",
    ]
    .map(str::to_owned);
    for (count, size, cancelled) in [(0, 4, false), (1, 3, false), (1, 4, true)] {
        assert!(workspace
            .collect(&patterns, &mut store.begin().unwrap(), count, size, &|| {
                cancelled
            })
            .is_err());
    }
    let mut staging = store.begin().unwrap();
    let missing = workspace
        .collect(&patterns, &mut staging, 1, 4, &|| false)
        .unwrap();
    assert_eq!(
        missing,
        [
            "debug/Bin/Firmware.BIN",
            "Debug/bin/*.BIN",
            "Debug/Bin/firmware.bin",
            "elsewhere/**/*.bin",
            "missing"
        ]
    );
    let artifact = store
        .commit(
            staging,
            Artifact {
                id: String::new(),
                job_id: "source-job".into(),
                client: "test".into(),
                title: "Direct build".into(),
                workspace: project.display().to_string(),
                command: vec!["cc".into()],
                created_at: 1,
                pinned: false,
                files: vec![],
                bytes: 0,
            },
        )
        .unwrap();
    assert_eq!(artifact.files.len(), 1);
    assert_eq!(artifact.files[0].path, "Debug/Bin/Firmware.BIN");
    assert_eq!(artifact.files[0].size, 4);
    assert_eq!(artifact.bytes, 4);
    assert_eq!(
        artifact.files[0].sha256,
        "88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589"
    );
    let file = store.file(&artifact.id, None, ".BIN").unwrap();
    workspace
        .out
        .write("Debug/Bin/Firmware.BIN", b"next")
        .unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"abcd");
    store.set_pinned(&artifact.id, true).unwrap();
    assert_eq!(store.clear_unpinned().unwrap(), 0);
    fs::rename(project, root.join("retired")).unwrap();
    assert_eq!(
        Store::new(store.root()).get(&artifact.id).unwrap().job_id,
        "source-job"
    );
    store.delete(&artifact.id).unwrap();
    assert_eq!(
        workspace.out.read("Debug/Bin/Firmware.BIN").unwrap(),
        b"next"
    );
}

/// Exercise the worker, host filesystem and managed archive together. The pack
/// seed is read-only. The service, project and artifact store live in the test directory.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "requires staged VM resources and GIZMOTH_LIVE_PACK_SEED"]
fn live_direct_build_preserves_artifact_names() {
    use crate::compile::{jobs::Status, packages, Service};
    use serde_json::json;
    use std::time::{Duration, Instant};

    let seed = PathBuf::from(std::env::var_os("GIZMOTH_LIVE_PACK_SEED").unwrap());
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let storage = data.join("compile");
    fs::create_dir_all(&storage).unwrap();
    for file in [packages::PACK, packages::PACK_INFO] {
        fs::copy(seed.join(file), storage.join(file)).unwrap();
    }
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    let case_sensitive = !temp.path().join("PROJECT").try_exists().unwrap();
    let mut command = String::from("mkdir /out/Release && printf upper >/out/Release/Firmware.BIN");
    if case_sensitive {
        command.push_str(" && printf lower >/out/Release/firmware.bin");
    }
    let service = Service::load(&data);
    service
        .patch_settings(json!({"vmResources": {"cpus":1,"memoryMiB":2048}}))
        .unwrap();
    let request = serde_json::from_value(json!({
        "workspace":project, "workspaceMode":"direct", "command":["sh", "-ec", command],
        "artifacts":["release/Firmware.BIN", "Release/FIRMWARE.bin", "Release/Firmware.BIN", "Release/firmware.bin", "Release/*", "**/Firmware.BIN"], "timeoutSeconds":30
    })).unwrap();
    let (job, _) = service
        .submit("test".into(), "Direct artifact names".into(), request)
        .unwrap();
    service.dispatch(&mut service.state.lock().unwrap());
    let deadline = Instant::now() + Duration::from_secs(90);
    while !job.status().finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    service.shutdown();
    assert_eq!(job.status(), Status::Succeeded, "{}", job.summary());
    let state = job.state.lock().unwrap();
    let record = service
        .artifacts
        .get(state.artifact.as_ref().unwrap())
        .unwrap();
    let mut expected = vec![("Release/Firmware.BIN", b"upper")];
    let mut missing = vec!["release/Firmware.BIN", "Release/FIRMWARE.bin"];
    if case_sensitive {
        expected.push(("Release/firmware.bin", b"lower"));
    } else {
        missing.push("Release/firmware.bin");
    }
    assert_eq!(state.missing, missing);
    assert_eq!(record.job_id, job.id);
    assert_eq!(record.files.len(), expected.len());
    assert_eq!(record.bytes, expected.len() as u64 * 5);
    for (path, contents) in expected {
        let archived = service.artifacts.file(&record.id, Some(path), "").unwrap();
        assert_eq!(fs::read(archived).unwrap(), contents);
        assert_eq!(fs::read(project.join("out").join(path)).unwrap(), contents);
    }
    println!(
        "case_sensitive={case_sensitive} job={} artifact={} files={} bytes={}",
        job.id,
        record.id,
        record.files.len(),
        record.bytes
    );
}
