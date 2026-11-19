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
    workspace.out.create_dir_all("debug/bin").unwrap();
    workspace.out.write("debug/bin/fw.bin", b"abcd").unwrap();
    symlink(&project, project.join("out/elsewhere")).unwrap();
    symlink("debug/bin/fw.bin", project.join("out/alias.bin")).unwrap();
    let store = Store::new(root.join("archives"));
    let patterns = [
        "**/*.bin",
        "debug/bin/fw.bin",
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
    assert_eq!(missing, ["elsewhere/**/*.bin", "missing"]);
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
    assert_eq!(artifact.files[0].size, 4);
    assert_eq!(artifact.bytes, 4);
    assert_eq!(
        artifact.files[0].sha256,
        "88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589"
    );
    let file = store.file(&artifact.id, None, ".bin").unwrap();
    workspace.out.write("debug/bin/fw.bin", b"next").unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"abcd");
    store.set_pinned(&artifact.id, true).unwrap();
    assert_eq!(store.clear_unpinned().unwrap(), 0);
    fs::rename(project, root.join("retired")).unwrap();
    assert_eq!(
        Store::new(store.root()).get(&artifact.id).unwrap().job_id,
        "source-job"
    );
    store.delete(&artifact.id).unwrap();
    assert_eq!(workspace.out.read("debug/bin/fw.bin").unwrap(), b"next");
}
