//! Source-only offline import tests. No interpreters, mount, or runtime required.
use sentinel_fs::SHARED_BASE_LAYER_ID;

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sentinel_fs::artifact::ArtifactPlane;
use sentinel_fs::cas::CasStore;
use sentinel_fs::layer::LayerManager;
use sentinel_fs::metadata::{MetadataDurability, MetadataStore};
use sentinel_fs::workspace_import::{import_native_workspace, WorkspaceImportDisposition};

const MARKER: &str = ".sentinel-workspace-import-v1.json";

fn manager(path: &Path) -> LayerManager {
    fs::create_dir_all(path).unwrap();
    let layer = LayerManager::with_artifact_plane(
        CasStore::open(path).unwrap(),
        MetadataStore::open_with_durability(
            path.join("namespace.redb"),
            MetadataDurability::Eventual,
        )
        .unwrap(),
        Arc::new(ArtifactPlane::open(path.join("content.redb")).unwrap()),
    );
    layer.init_base_root().unwrap();
    layer
}

fn source(path: &Path) -> PathBuf {
    let source = path.join("agents/alice/workspaces");
    fs::create_dir_all(&source).unwrap();
    source
}

fn lookup(layer: &LayerManager, root: u64, path: &str) -> u64 {
    path.split('/').fold(root, |parent, name| {
        layer.lookup_dirent("alice", parent, name).unwrap().unwrap()
    })
}

fn receipt(layer: &LayerManager) -> serde_json::Value {
    let inode = lookup(layer, 1, MARKER);
    serde_json::from_slice(&layer.read_file_range("alice", inode, 0, 4096).unwrap()).unwrap()
}

fn prepared_receipt(layer: &LayerManager) -> serde_json::Value {
    let mut marker = receipt(layer);
    marker["state"] = serde_json::json!("prepared");
    layer
        .write_file(
            "alice",
            1,
            MARKER,
            &serde_json::to_vec(&marker).unwrap(),
            0o600,
        )
        .unwrap();
    layer.sync_directory("alice", 1).unwrap();
    marker
}

#[test]
fn python_node_binary_files_modes_links_and_source_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    fs::create_dir(source.join("src")).unwrap();
    fs::create_dir_all(source.join("node_modules/.bin")).unwrap();
    let python = b"from pathlib import Path\nprint(Path('result.txt').read_text())\n";
    let node =
        b"const fs = require('node:fs');\nconsole.log(fs.readFileSync('result.txt', 'utf8'));\n";
    fs::write(source.join("app.py"), python).unwrap();
    fs::write(source.join("src/main.js"), node).unwrap();
    fs::write(
        source.join("package.json"),
        b"{\"scripts\":{\"start\":\"node src/main.js\"}}\n",
    )
    .unwrap();
    fs::write(
        source.join("package-lock.json"),
        b"{\"lockfileVersion\":3}\n",
    )
    .unwrap();
    fs::write(source.join("result.txt"), b"accepted work\n").unwrap();
    fs::write(
        source.join("node_modules/tool.js"),
        b"#!/usr/bin/env node\nconsole.log('tool');\n",
    )
    .unwrap();
    let binary: Vec<u8> = (0..350_000).map(|i| (i % 251) as u8).collect();
    fs::write(source.join("data.bin"), &binary).unwrap();
    fs::set_permissions(source.join("app.py"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(source.join("src"), fs::Permissions::from_mode(0o750)).unwrap();
    fs::hard_link(source.join("app.py"), source.join("src/alias.py")).unwrap();
    symlink("../app.py", source.join("src/current.py")).unwrap();
    symlink("../tool.js", source.join("node_modules/.bin/tool")).unwrap();
    let before = fs::metadata(source.join("app.py")).unwrap();
    let root_before = fs::metadata(&source).unwrap();
    let layer = manager(&dir.path().join("plane"));
    let result = import_native_workspace(&layer, "alice", &source).unwrap();
    assert_eq!(result.disposition, WorkspaceImportDisposition::Imported);
    assert_eq!(
        layer.lookup_dirent("alice", 1, "workspaces").unwrap(),
        Some(result.workspace_inode)
    );
    let app = lookup(&layer, result.workspace_inode, "app.py");
    assert_eq!(app, lookup(&layer, result.workspace_inode, "src/alias.py"));
    let data = layer.lookup_inode("alice", app).unwrap().unwrap();
    assert_eq!(data.nlinks, 2);
    assert_eq!(data.mode, 0o755);
    assert_eq!(data.mtime, before.mtime() as u64);
    assert_eq!(layer.read_file("alice", app).unwrap(), python);
    assert_eq!(
        layer
            .read_file(
                "alice",
                lookup(&layer, result.workspace_inode, "src/main.js")
            )
            .unwrap(),
        node
    );
    assert_eq!(
        layer
            .read_file("alice", lookup(&layer, result.workspace_inode, "data.bin"))
            .unwrap(),
        binary
    );
    assert_eq!(
        layer
            .lookup_inode(
                "alice",
                lookup(&layer, result.workspace_inode, "src/current.py")
            )
            .unwrap()
            .unwrap()
            .symlink_target,
        "../app.py"
    );
    assert_eq!(
        layer
            .lookup_inode(
                "alice",
                lookup(&layer, result.workspace_inode, "node_modules/.bin/tool")
            )
            .unwrap()
            .unwrap()
            .symlink_target,
        "../tool.js"
    );
    assert_eq!(
        layer
            .lookup_inode("alice", lookup(&layer, result.workspace_inode, "src"))
            .unwrap()
            .unwrap()
            .mode,
        0o750
    );
    assert!(layer
        .lookup_dirent("alice", result.workspace_inode, MARKER)
        .unwrap()
        .is_none());
    assert_eq!(receipt(&layer)["state"], "installed");
    assert_eq!(
        receipt(&layer)["tree_sha256"],
        serde_json::json!(result.imported_sha256)
    );
    assert_eq!(receipt(&layer)["bytes"], result.imported_bytes);
    assert!(!source.join(MARKER).exists());
    assert_eq!(fs::read(source.join("app.py")).unwrap(), python);
    assert_eq!(fs::read(source.join("src/main.js")).unwrap(), node);
    assert_eq!(fs::read(source.join("data.bin")).unwrap(), binary);
    assert_eq!(
        fs::read_link(source.join("src/current.py")).unwrap(),
        Path::new("../app.py")
    );
    let after = fs::metadata(source.join("app.py")).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.nlink(),
            before.mtime(),
            before.ctime()
        ),
        (
            after.dev(),
            after.ino(),
            after.nlink(),
            after.mtime(),
            after.ctime()
        )
    );
    let root_after = fs::metadata(&source).unwrap();
    assert_eq!(
        (
            root_before.dev(),
            root_before.ino(),
            root_before.mtime(),
            root_before.ctime()
        ),
        (
            root_after.dev(),
            root_after.ino(),
            root_after.mtime(),
            root_after.ctime()
        )
    );
}

#[test]
fn streaming_import_exceeds_per_file_dirty_limit_without_buffering_whole_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    let file = fs::File::create(source.join("large.bin")).unwrap();
    file.set_len(9 * 1024 * 1024 + 17).unwrap();
    let layer = manager(&dir.path().join("plane"));
    let result = import_native_workspace(&layer, "alice", &source).unwrap();
    let inode = lookup(&layer, result.workspace_inode, "large.bin");
    assert_eq!(result.imported_bytes, 9 * 1024 * 1024 + 17);
    assert_eq!(
        layer.lookup_inode("alice", inode).unwrap().unwrap().size,
        result.imported_bytes
    );
    assert_eq!(
        layer
            .read_file_range("alice", inode, result.imported_bytes - 3, 99)
            .unwrap(),
        vec![0; 3]
    );
    assert_eq!(
        fs::metadata(source.join("large.bin")).unwrap().len(),
        result.imported_bytes
    );
}

#[test]
fn hardlink_aliases_over_64_mib_by_name_charge_one_inode_and_matching_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    let size = 40 * 1024 * 1024;
    fs::File::create(source.join("first.bin"))
        .unwrap()
        .set_len(size)
        .unwrap();
    fs::set_permissions(source.join("first.bin"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::hard_link(source.join("first.bin"), source.join("second.bin")).unwrap();
    let before = fs::metadata(source.join("first.bin")).unwrap();
    let layer = manager(&dir.path().join("plane"));
    let result = import_native_workspace(&layer, "alice", &source).unwrap();
    assert_eq!(result.imported_bytes, size);
    assert_eq!(result.imported_nodes, 3);
    let first = lookup(&layer, result.workspace_inode, "first.bin");
    assert_eq!(first, lookup(&layer, result.workspace_inode, "second.bin"));
    let data = layer.lookup_inode("alice", first).unwrap().unwrap();
    assert_eq!(
        (data.size, data.nlinks, data.mode, data.mtime),
        (size, 2, 0o640, before.mtime() as u64)
    );
    assert_eq!(
        layer.read_file_range("alice", first, size - 3, 99).unwrap(),
        vec![0; 3]
    );
    let marker = receipt(&layer);
    assert_eq!(marker["bytes"], size);
    assert_eq!(
        marker["tree_sha256"],
        serde_json::json!(result.imported_sha256)
    );
    let marker_inode = lookup(&layer, 1, MARKER);
    let marker_size = layer
        .lookup_inode("alice", marker_inode)
        .unwrap()
        .unwrap()
        .size;
    assert_eq!(
        layer.workspace_budget("alice").unwrap().used_bytes,
        size + marker_size
    );
    let retry = import_native_workspace(&layer, "alice", &source).unwrap();
    assert_eq!(retry.imported_bytes, size);
    assert_eq!(retry.imported_sha256, result.imported_sha256);
    assert_eq!(
        retry.disposition,
        WorkspaceImportDisposition::AlreadyImported
    );
    // Exercise namespace digest and byte-count verification during crash recovery.
    prepared_receipt(&layer);
    let recovered = import_native_workspace(&layer, "alice", &source).unwrap();
    assert_eq!(recovered.imported_bytes, size);
    assert_eq!(recovered.imported_sha256, result.imported_sha256);
    assert_eq!(recovered.disposition, WorkspaceImportDisposition::Recovered);
    let after = fs::metadata(source.join("first.bin")).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.nlink(),
            before.mtime(),
            before.ctime()
        ),
        (
            after.dev(),
            after.ino(),
            after.len(),
            after.nlink(),
            after.mtime(),
            after.ctime()
        )
    );
}

#[test]
fn exact_64_mib_source_fails_marker_reservation_before_creating_a_stage() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    let size = 64 * 1024 * 1024;
    fs::File::create(source.join("full.bin"))
        .unwrap()
        .set_len(size)
        .unwrap();
    let before = fs::metadata(source.join("full.bin")).unwrap();
    let layer = manager(&dir.path().join("plane"));
    let error = import_native_workspace(&layer, "alice", &source).unwrap_err();
    assert!(error
        .to_string()
        .contains("8192-byte trusted marker reservation"));
    assert!(layer.readdir("alice", 1).unwrap().is_empty());
    assert_eq!(layer.workspace_budget("alice").unwrap().used_bytes, 0);
    let after = fs::metadata(source.join("full.bin")).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.ctime()
        ),
        (after.dev(), after.ino(), size, after.mtime(), after.ctime())
    );
}

#[test]
fn existing_stage_and_inherited_data_reduce_available_import_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    fs::write(source.join("file"), b"accepted native work").unwrap();
    let layer = manager(&dir.path().join("plane"));
    layer
        .write_file(SHARED_BASE_LAYER_ID, 1, "inherited", b"shared bytes", 0o644)
        .unwrap();
    let stage = layer
        .mkdir("alice", 1, ".sentinel-workspace-stage-abandoned", 0o700)
        .unwrap();
    layer
        .write_file("alice", stage, "partial", b"private evidence", 0o600)
        .unwrap();
    // Hardlink the existing evidence: reservation must not charge its alias twice.
    let partial = lookup(&layer, stage, "partial");
    layer.link("alice", partial, stage, "alias").unwrap();
    let used = layer.workspace_budget("alice").unwrap().used_bytes;
    assert_eq!(
        used,
        b"shared bytes".len() as u64 + b"private evidence".len() as u64
    );
    let source_size = b"accepted native work".len() as u64;
    layer
        .set_workspace_budget("alice", used + source_size + 8191)
        .unwrap();
    let entries = layer.readdir("alice", 1).unwrap();
    assert!(import_native_workspace(&layer, "alice", &source)
        .unwrap_err()
        .to_string()
        .contains("marker reservation"));
    assert_eq!(layer.readdir("alice", 1).unwrap(), entries);
    assert_eq!(
        layer.read_file("alice", partial).unwrap(),
        b"private evidence"
    );
    assert_eq!(
        fs::read(source.join("file")).unwrap(),
        b"accepted native work"
    );
    // The exact conservative reservation fits without increasing this limit.
    layer
        .set_workspace_budget("alice", used + source_size + 8192)
        .unwrap();
    let result = import_native_workspace(&layer, "alice", &source).unwrap();
    assert_eq!(result.imported_bytes, source_size);
    let budget = layer.workspace_budget("alice").unwrap();
    assert_eq!(budget.limit_bytes, used + source_size + 8192);
    assert!(budget.used_bytes <= budget.limit_bytes);
    assert_eq!(
        layer.read_file("alice", partial).unwrap(),
        b"private evidence"
    );
}

#[test]
fn installed_retry_and_reopen_keep_new_namespace_work_not_stale_native_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    fs::write(source.join("main.py"), b"old native work").unwrap();
    let database = dir.path().join("plane");
    let layer = manager(&database);
    let first = import_native_workspace(&layer, "alice", &source).unwrap();
    layer
        .write_file(
            "alice",
            first.workspace_inode,
            "main.py",
            b"new namespace work",
            0o644,
        )
        .unwrap();
    layer
        .write_file(
            "alice",
            first.workspace_inode,
            "new.js",
            b"new Node task",
            0o644,
        )
        .unwrap();
    layer
        .sync_directory("alice", first.workspace_inode)
        .unwrap();
    drop(layer);
    let reopened = manager(&database);
    let retry = import_native_workspace(&reopened, "alice", &source).unwrap();
    assert_eq!(
        retry.disposition,
        WorkspaceImportDisposition::AlreadyImported
    );
    assert_eq!(retry.workspace_inode, first.workspace_inode);
    assert_eq!(
        reopened
            .read_file("alice", lookup(&reopened, retry.workspace_inode, "main.py"))
            .unwrap(),
        b"new namespace work"
    );
    assert_eq!(
        reopened
            .read_file("alice", lookup(&reopened, retry.workspace_inode, "new.js"))
            .unwrap(),
        b"new Node task"
    );
    assert_eq!(
        fs::read(source.join("main.py")).unwrap(),
        b"old native work"
    );
    assert!(!source.join("new.js").exists());
}

#[test]
fn installed_retry_after_tmpfs_loss_or_recreation_keeps_newer_namespace_work() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    fs::write(source.join("main.py"), b"original native work").unwrap();
    let database = dir.path().join("plane");
    let layer = manager(&database);
    let first = import_native_workspace(&layer, "alice", &source).unwrap();
    layer
        .write_file(
            "alice",
            first.workspace_inode,
            "main.py",
            b"new durable work",
            0o755,
        )
        .unwrap();
    let modified = lookup(&layer, first.workspace_inode, "main.py");
    layer
        .set_file_attributes("alice", modified, Some(0o755), None, None, None, None)
        .unwrap();
    layer
        .write_file(
            "alice",
            first.workspace_inode,
            "new.js",
            b"new Node work",
            0o644,
        )
        .unwrap();
    layer
        .sync_directory("alice", first.workspace_inode)
        .unwrap();
    drop(layer);
    // Simulate reboot losing the entire native /ram subtree.
    fs::remove_dir_all(source.parent().unwrap().parent().unwrap()).unwrap();
    let reopened = manager(&database);
    let missing = import_native_workspace(&reopened, "alice", &source).unwrap();
    assert_eq!(
        missing.disposition,
        WorkspaceImportDisposition::AlreadyImported
    );
    assert_eq!(missing.workspace_inode, first.workspace_inode);
    assert_eq!(missing.imported_sha256, first.imported_sha256);
    assert!(!source.exists());
    // Hold a recreated directory open to prove a different inode without relying
    // on allocator reuse after deletion. Installed recognition must ignore both.
    let replacement = dir.path().join("replacement");
    fs::create_dir(&replacement).unwrap();
    let held = fs::File::open(&replacement).unwrap();
    let original_ino = receipt(&reopened)["source_ino"].as_u64().unwrap();
    if held.metadata().unwrap().ino() == original_ino {
        fs::create_dir(dir.path().join("different-replacement")).unwrap();
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::rename(dir.path().join("different-replacement"), &source).unwrap();
    } else {
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::rename(&replacement, &source).unwrap();
    }
    assert_ne!(fs::metadata(&source).unwrap().ino(), original_ino);
    let empty = import_native_workspace(&reopened, "alice", &source).unwrap();
    assert_eq!(
        empty.disposition,
        WorkspaceImportDisposition::AlreadyImported
    );
    assert!(fs::read_dir(&source).unwrap().next().is_none());
    fs::write(source.join("main.py"), b"unrelated recreated tmpfs work").unwrap();
    let recreated = import_native_workspace(&reopened, "alice", &source).unwrap();
    assert_eq!(
        recreated.disposition,
        WorkspaceImportDisposition::AlreadyImported
    );
    assert_eq!(recreated.workspace_inode, first.workspace_inode);
    assert_eq!(
        reopened
            .read_file("alice", lookup(&reopened, first.workspace_inode, "main.py"))
            .unwrap(),
        b"new durable work"
    );
    assert_eq!(
        reopened
            .read_file("alice", lookup(&reopened, first.workspace_inode, "new.js"))
            .unwrap(),
        b"new Node work"
    );
    assert_eq!(
        reopened
            .lookup_inode("alice", lookup(&reopened, first.workspace_inode, "main.py"))
            .unwrap()
            .unwrap()
            .mode,
        0o755
    );
    assert_eq!(
        fs::read(source.join("main.py")).unwrap(),
        b"unrelated recreated tmpfs work"
    );
    assert_eq!(
        receipt(&reopened)["tree_sha256"],
        serde_json::json!(first.imported_sha256)
    );
}

#[test]
fn prepared_reboot_recovers_durable_stage_or_destination_without_original_source() {
    for staged in [false, true] {
        for recreated in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let source = source(dir.path());
            fs::write(source.join("main.js"), b"durable checkpoint").unwrap();
            let plane = dir.path().join("plane");
            let layer = manager(&plane);
            let first = import_native_workspace(&layer, "alice", &source).unwrap();
            let marker = prepared_receipt(&layer);
            if staged {
                layer
                    .rename(
                        "alice",
                        1,
                        "workspaces",
                        1,
                        marker["stage_name"].as_str().unwrap(),
                        1,
                    )
                    .unwrap();
                layer.sync_directory("alice", 1).unwrap();
            }
            drop(layer);
            // Keep the old inode allocated so recreation cannot reuse it.
            let old_source = source.with_file_name("prior-source");
            fs::rename(&source, &old_source).unwrap();
            if recreated {
                fs::create_dir(&source).unwrap();
                fs::write(source.join("main.js"), b"new native work").unwrap();
            }
            let layer = manager(&plane);
            let recovered = import_native_workspace(&layer, "alice", &source).unwrap();
            assert_eq!(recovered.disposition, WorkspaceImportDisposition::Recovered);
            assert_eq!(recovered.workspace_inode, first.workspace_inode);
            assert_eq!(
                layer
                    .read_file("alice", lookup(&layer, first.workspace_inode, "main.js"))
                    .unwrap(),
                b"durable checkpoint"
            );
            assert_eq!(receipt(&layer)["state"], "installed");
            assert_eq!(
                fs::read(old_source.join("main.js")).unwrap(),
                b"durable checkpoint"
            );
            if recreated {
                assert_eq!(
                    fs::read(source.join("main.js")).unwrap(),
                    b"new native work"
                );
            }
        }
    }
}

#[test]
fn marker_write_crash_after_rename_adopts_only_exact_verified_target() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    fs::write(source.join("main.js"), b"checkpoint").unwrap();
    let layer = manager(&dir.path().join("plane"));
    let first = import_native_workspace(&layer, "alice", &source).unwrap();
    prepared_receipt(&layer);
    let recovered = import_native_workspace(&layer, "alice", &source).unwrap();
    assert_eq!(recovered.disposition, WorkspaceImportDisposition::Recovered);
    assert_eq!(recovered.workspace_inode, first.workspace_inode);
    assert_eq!(receipt(&layer)["state"], "installed");
    prepared_receipt(&layer);
    layer
        .write_file(
            "alice",
            first.workspace_inode,
            "main.js",
            b"new work after crash",
            0o644,
        )
        .unwrap();
    assert!(import_native_workspace(&layer, "alice", &source).is_err());
    assert_eq!(
        layer
            .read_file("alice", lookup(&layer, first.workspace_inode, "main.js"))
            .unwrap(),
        b"new work after crash"
    );
    assert_eq!(fs::read(source.join("main.js")).unwrap(), b"checkpoint");
    assert_eq!(receipt(&layer)["state"], "prepared");
}

#[test]
fn prepared_target_mode_time_and_same_length_digest_changes_refuse_adoption() {
    for changed in ["mode", "mtime", "digest"] {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        fs::write(source.join("main.py"), b"original").unwrap();
        let layer = manager(&dir.path().join("plane"));
        let first = import_native_workspace(&layer, "alice", &source).unwrap();
        prepared_receipt(&layer);
        let inode = lookup(&layer, first.workspace_inode, "main.py");
        let data = layer.lookup_inode("alice", inode).unwrap().unwrap();
        match changed {
            "mode" => layer
                .set_file_attributes(
                    "alice",
                    inode,
                    Some(data.mode ^ 0o100),
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            "mtime" => layer
                .set_file_attributes("alice", inode, None, None, None, None, Some(data.mtime + 1))
                .unwrap(),
            _ => {
                layer
                    .write_file(
                        "alice",
                        first.workspace_inode,
                        "main.py",
                        b"new work",
                        data.mode,
                    )
                    .unwrap();
                layer
                    .set_file_attributes("alice", inode, None, None, None, None, Some(data.mtime))
                    .unwrap();
            }
        }
        layer
            .sync_directory("alice", first.workspace_inode)
            .unwrap();
        let before = layer.lookup_inode("alice", inode).unwrap().unwrap();
        let error = import_native_workspace(&layer, "alice", &source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("target differs from its receipt"),
            "{changed}: {error}"
        );
        let after = layer.lookup_inode("alice", inode).unwrap().unwrap();
        assert_eq!(
            (after.mode, after.mtime, after.size),
            (before.mode, before.mtime, before.size)
        );
        assert_eq!(receipt(&layer)["state"], "prepared");
        assert_eq!(fs::read(source.join("main.py")).unwrap(), b"original");
        if changed == "digest" {
            assert_eq!(layer.read_file("alice", inode).unwrap(), b"new work");
        }
    }
}

#[test]
fn prepared_recovery_reserves_replacement_marker_without_mutating_target() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    fs::write(source.join("file"), b"source").unwrap();
    let layer = manager(&dir.path().join("plane"));
    let first = import_native_workspace(&layer, "alice", &source).unwrap();
    prepared_receipt(&layer);
    let used = layer.workspace_budget("alice").unwrap().used_bytes;
    layer.set_workspace_budget("alice", used + 4095).unwrap();
    let entries = layer.readdir("alice", 1).unwrap();
    assert!(import_native_workspace(&layer, "alice", &source)
        .unwrap_err()
        .to_string()
        .contains("4096-byte trusted marker reservation"));
    assert_eq!(layer.readdir("alice", 1).unwrap(), entries);
    assert_eq!(receipt(&layer)["state"], "prepared");
    assert_eq!(
        layer
            .read_file("alice", lookup(&layer, first.workspace_inode, "file"))
            .unwrap(),
        b"source"
    );
    assert_eq!(fs::read(source.join("file")).unwrap(), b"source");
}

#[test]
fn prepared_stage_retry_requires_unchanged_source_and_keeps_partial_stages() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    fs::write(source.join("file"), b"source").unwrap();
    let layer = manager(&dir.path().join("plane"));
    layer.ensure_agent_root("alice").unwrap();
    let abandoned = layer
        .mkdir("alice", 1, ".sentinel-workspace-stage-abandoned", 0o700)
        .unwrap();
    layer
        .write_file("alice", abandoned, "partial", b"keep evidence", 0o600)
        .unwrap();
    let first = import_native_workspace(&layer, "alice", &source).unwrap();
    let marker = prepared_receipt(&layer);
    let stage = marker["stage_name"].as_str().unwrap();
    layer.rename("alice", 1, "workspaces", 1, stage, 1).unwrap();
    layer.sync_directory("alice", 1).unwrap();
    let recovered = import_native_workspace(&layer, "alice", &source).unwrap();
    assert_eq!(recovered.disposition, WorkspaceImportDisposition::Recovered);
    assert_eq!(recovered.workspace_inode, first.workspace_inode);
    assert_eq!(
        layer
            .read_file("alice", lookup(&layer, abandoned, "partial"))
            .unwrap(),
        b"keep evidence"
    );
    let marker = prepared_receipt(&layer);
    let stage = marker["stage_name"].as_str().unwrap();
    layer.rename("alice", 1, "workspaces", 1, stage, 1).unwrap();
    fs::write(source.join("file"), b"new native accepted work").unwrap();
    assert!(import_native_workspace(&layer, "alice", &source).is_err());
    assert!(layer
        .lookup_dirent("alice", 1, "workspaces")
        .unwrap()
        .is_none());
    assert_eq!(
        layer.lookup_dirent("alice", 1, stage).unwrap(),
        Some(first.workspace_inode)
    );
    assert_eq!(
        fs::read(source.join("file")).unwrap(),
        b"new native accepted work"
    );
}

#[test]
fn existing_unmarked_nonempty_or_empty_destination_is_never_overwritten() {
    for nonempty in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        fs::write(source.join("file"), b"native").unwrap();
        let layer = manager(&dir.path().join("plane"));
        let workspace = layer.mkdir("alice", 1, "workspaces", 0o755).unwrap();
        if nonempty {
            layer
                .write_file("alice", workspace, "file", b"accepted namespace", 0o644)
                .unwrap();
        }
        assert!(import_native_workspace(&layer, "alice", &source).is_err());
        assert_eq!(
            layer.lookup_dirent("alice", 1, "workspaces").unwrap(),
            Some(workspace)
        );
        if nonempty {
            assert_eq!(
                layer
                    .read_file("alice", lookup(&layer, workspace, "file"))
                    .unwrap(),
                b"accepted namespace"
            );
        }
        assert!(layer.lookup_dirent("alice", 1, MARKER).unwrap().is_none());
        assert_eq!(fs::read(source.join("file")).unwrap(), b"native");
    }
}

#[test]
fn external_hardlink_absolute_parent_and_intermediate_symlink_escapes_are_rejected() {
    for case in 0..4 {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        let outside = dir.path().join("outside");
        fs::write(&outside, b"foreign authority").unwrap();
        match case {
            0 => fs::hard_link(&outside, source.join("alias")).unwrap(),
            1 => symlink(&outside, source.join("escape")).unwrap(),
            2 => symlink("../../../../outside", source.join("escape")).unwrap(),
            _ => {
                fs::create_dir(source.join("d")).unwrap();
                symlink("..", source.join("d/up")).unwrap();
                symlink("d/up/../outside", source.join("escape")).unwrap();
            }
        }
        let layer = manager(&dir.path().join("plane"));
        assert!(import_native_workspace(&layer, "alice", &source).is_err());
        assert!(layer
            .lookup_dirent("alice", 1, "workspaces")
            .unwrap()
            .is_none());
        assert!(layer.lookup_dirent("alice", 1, MARKER).unwrap().is_none());
        assert!(!layer
            .readdir("alice", 1)
            .unwrap()
            .iter()
            .any(|(name, _, _)| name.starts_with(".sentinel-workspace-stage-")));
        assert_eq!(fs::read(&outside).unwrap(), b"foreign authority");
    }
}

#[test]
fn symlinked_source_ancestor_cannot_transfer_another_tree() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    fs::write(source.join("file"), b"protected").unwrap();
    symlink(source.parent().unwrap(), dir.path().join("agent-link")).unwrap();
    let layer = manager(&dir.path().join("plane"));
    assert!(
        import_native_workspace(&layer, "alice", &dir.path().join("agent-link/workspaces"))
            .is_err()
    );
    assert_eq!(fs::read(source.join("file")).unwrap(), b"protected");
}

#[test]
fn replaced_source_is_ignored_but_replaced_installed_namespace_refuses_replay() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    fs::write(source.join("file"), b"original").unwrap();
    let layer = manager(&dir.path().join("plane"));
    let first = import_native_workspace(&layer, "alice", &source).unwrap();
    let saved_source = source.with_file_name("old-workspaces");
    fs::rename(&source, &saved_source).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"different source").unwrap();
    let retry = import_native_workspace(&layer, "alice", &source).unwrap();
    assert_eq!(
        retry.disposition,
        WorkspaceImportDisposition::AlreadyImported
    );
    assert_eq!(
        layer
            .read_file("alice", lookup(&layer, first.workspace_inode, "file"))
            .unwrap(),
        b"original"
    );
    fs::remove_dir_all(&source).unwrap();
    fs::rename(saved_source, &source).unwrap();
    layer
        .rename("alice", 1, "workspaces", 1, "saved-namespace", 1)
        .unwrap();
    let replacement = layer.mkdir("alice", 1, "workspaces", 0o755).unwrap();
    layer
        .write_file("alice", replacement, "file", b"new namespace", 0o644)
        .unwrap();
    assert!(import_native_workspace(&layer, "alice", &source).is_err());
    assert_eq!(
        layer
            .read_file("alice", lookup(&layer, replacement, "file"))
            .unwrap(),
        b"new namespace"
    );
}

#[test]
fn depth_and_aggregate_byte_limits_fail_without_hiding_source_work() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    let mut nested = source.clone();
    for _ in 0..65 {
        nested = nested.join("d");
        fs::create_dir(&nested).unwrap();
    }
    fs::write(nested.join("file"), b"deep accepted work").unwrap();
    let layer = manager(&dir.path().join("plane"));
    assert!(import_native_workspace(&layer, "alice", &source).is_err());
    assert_eq!(
        fs::read(nested.join("file")).unwrap(),
        b"deep accepted work"
    );
    let other = crate::source(&dir.path().join("second"));
    fs::File::create(other.join("too-large"))
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    assert!(import_native_workspace(&layer, "alice", &other).is_err());
    assert!(layer
        .lookup_dirent("alice", 1, "workspaces")
        .unwrap()
        .is_none());
    assert_eq!(
        fs::metadata(other.join("too-large")).unwrap().len(),
        64 * 1024 * 1024 + 1
    );
}

#[test]
fn unsupported_names_and_dangling_links_fail_explicitly_without_importing_a_subset() {
    use std::os::unix::ffi::OsStringExt;
    for invalid_name in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        fs::write(source.join("accepted.py"), b"print('preserve')\n").unwrap();
        if invalid_name {
            fs::write(
                source.join(std::ffi::OsString::from_vec(vec![0xff])),
                b"accepted binary name",
            )
            .unwrap();
        } else {
            symlink("missing-relative-target", source.join("dangling")).unwrap();
        }
        let layer = manager(&dir.path().join("plane"));
        assert!(import_native_workspace(&layer, "alice", &source).is_err());
        assert!(layer
            .lookup_dirent("alice", 1, "workspaces")
            .unwrap()
            .is_none());
        assert_eq!(
            fs::read(source.join("accepted.py")).unwrap(),
            b"print('preserve')\n"
        );
    }
}
