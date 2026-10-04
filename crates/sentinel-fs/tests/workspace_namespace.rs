//! Namespace/handle contracts, intentionally independent of mounted FUSE.

use std::sync::Arc;

use sentinel_fs::artifact::ArtifactPlane;
use sentinel_fs::cas::CasStore;
use sentinel_fs::layer::{
    LayerManager, DEFAULT_WORKSPACE_BUDGET_BYTES, MAX_WORKSPACE_BUDGET_BYTES,
};
use sentinel_fs::metadata::{
    referenced_blob_hashes, referenced_workspace_content, FileKind, MetadataDurability,
    MetadataStore,
};
use sentinel_fs::SHARED_BASE_LAYER_ID;

fn manager(dir: &std::path::Path, chunked: bool) -> LayerManager {
    let cas = CasStore::open(dir).unwrap();
    let meta = MetadataStore::open_with_durability(
        dir.join("namespace.redb"),
        MetadataDurability::Eventual,
    )
    .unwrap();
    let layer = if chunked {
        LayerManager::with_artifact_plane(
            cas,
            meta,
            Arc::new(ArtifactPlane::open(dir.join("content.redb")).unwrap()),
        )
    } else {
        LayerManager::new(cas, meta)
    };
    layer.init_base_root().unwrap();
    layer
}

fn both(mut test: impl FnMut(&LayerManager)) {
    for chunked in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        test(&manager(dir.path(), chunked));
    }
}

fn check_errno<T>(result: anyhow::Result<T>, expected: i32) {
    let error = match result {
        Ok(_) => panic!("expected errno {expected}"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .and_then(|e| e.raw_os_error()),
        Some(expected),
        "{error:?}"
    );
}

#[test]
fn base_and_agent_inodes_cannot_collide_in_either_allocation_order() {
    both(|layer| {
        let base = layer.populate_base_file(1, "base", b"base", 0o644).unwrap();
        let own = layer.write_file("alice", 1, "own", b"own", 0o644).unwrap();
        let later_base = layer
            .populate_base_file(1, "later", b"later", 0o644)
            .unwrap();
        let other = layer
            .write_file("bob", 1, "other", b"other", 0o644)
            .unwrap();
        assert_ne!(base, own);
        assert_ne!(later_base, own);
        assert_ne!(other, later_base);
        assert_eq!(layer.read_file("alice", base).unwrap(), b"base");
        check_errno(layer.read_file("bob", own), 2);
    });
}

#[test]
fn replacement_preserves_inode_and_private_cow() {
    both(|layer| {
        let inode = layer
            .populate_base_file(1, "shared", b"original", 0o644)
            .unwrap();
        let handle = layer
            .open_file("alice", inode, false, false, false)
            .unwrap();
        assert_eq!(
            layer
                .write_file("alice", 1, "shared", b"private", 0o644)
                .unwrap(),
            inode
        );
        assert_eq!(
            layer.read_handle("alice", handle, 0, 100).unwrap(),
            b"private"
        );
        assert_eq!(layer.read_file("bob", inode).unwrap(), b"original");
        assert_eq!(
            layer.read_file(SHARED_BASE_LAYER_ID, inode).unwrap(),
            b"original"
        );
        layer.release_handle("alice", handle).unwrap();
    });
}

#[test]
fn same_inode_opens_share_write_order_append_and_dirty_getattr() {
    both(|layer| {
        let inode = layer.write_file("alice", 1, "file", b"abc", 0o644).unwrap();
        let first = layer.open_file("alice", inode, true, false, false).unwrap();
        let second = layer.open_file("alice", inode, true, true, false).unwrap();
        layer.write_handle("alice", first, 1, b"X").unwrap();
        layer.write_handle("alice", second, 0, b"YZ").unwrap();
        assert_eq!(layer.lookup_inode("alice", inode).unwrap().unwrap().size, 5);
        assert_eq!(layer.getattr_handle("alice", first).unwrap().size, 5);
        assert_eq!(layer.read_handle("alice", first, 0, 20).unwrap(), b"aXcYZ");
        assert_eq!(layer.read_file_range("alice", inode, 1, 3).unwrap(), b"XcY");
        layer.sync_handle("alice", second).unwrap();
        layer.release_handle("alice", first).unwrap();
        layer.release_handle("alice", second).unwrap();
        assert_eq!(layer.read_file("alice", inode).unwrap(), b"aXcYZ");
    });
}

#[test]
fn truncate_zero_holes_and_open_truncate_are_shared() {
    both(|layer| {
        let inode = layer
            .write_file("alice", 1, "file", b"abcdef", 0o644)
            .unwrap();
        let first = layer.open_file("alice", inode, true, false, false).unwrap();
        layer.write_handle("alice", first, 4, b"XY").unwrap();
        layer.truncate_file("alice", inode, 2).unwrap();
        layer.truncate_file("alice", inode, 6).unwrap();
        assert_eq!(
            layer.read_handle("alice", first, 0, 20).unwrap(),
            b"ab\0\0\0\0"
        );
        layer.write_handle("alice", first, 8, b"Z").unwrap();
        assert_eq!(
            layer.read_handle("alice", first, 0, 20).unwrap(),
            b"ab\0\0\0\0\0\0Z"
        );
        let second = layer.open_file("alice", inode, true, false, true).unwrap();
        assert!(layer.read_handle("alice", first, 0, 20).unwrap().is_empty());
        layer.write_handle("alice", second, 2, b"Q").unwrap();
        layer.sync_handle("alice", first).unwrap();
        assert_eq!(layer.read_handle("alice", second, 0, 20).unwrap(), b"\0\0Q");
        layer.release_handle("alice", first).unwrap();
        layer.release_handle("alice", second).unwrap();
    });
}

#[test]
fn open_unlink_preserves_bytes_without_resurrecting_name() {
    both(|layer| {
        let inode = layer
            .populate_base_file(1, "shared", b"base", 0o644)
            .unwrap();
        let handle = layer.open_file("alice", inode, true, false, false).unwrap();
        layer.write_handle("alice", handle, 4, b"!").unwrap();
        layer.unlink("alice", 1, "shared", inode).unwrap();
        assert!(layer.lookup_inode("alice", inode).unwrap().is_none());
        assert_eq!(layer.getattr_handle("alice", handle).unwrap().nlinks, 0);
        assert_eq!(layer.read_handle("alice", handle, 0, 20).unwrap(), b"base!");
        layer.write_handle("alice", handle, 0, b"B").unwrap();
        layer.sync_handle("alice", handle).unwrap();
        assert!(layer.lookup_dirent("alice", 1, "shared").unwrap().is_none());
        assert_eq!(layer.read_file("bob", inode).unwrap(), b"base");
        layer.release_handle("alice", handle).unwrap();
        check_errno(layer.open_file("alice", inode, false, false, false), 2);
    });
}

#[test]
fn rename_overwrite_keeps_both_open_inode_identities() {
    both(|layer| {
        let source = layer
            .write_file("alice", 1, "source", b"source", 0o644)
            .unwrap();
        let target = layer
            .populate_base_file(1, "target", b"target", 0o644)
            .unwrap();
        let source_handle = layer
            .open_file("alice", source, true, false, false)
            .unwrap();
        let target_handle = layer
            .open_file("alice", target, true, false, false)
            .unwrap();
        layer.write_handle("alice", source_handle, 6, b"!").unwrap();
        layer.rename("alice", 1, "source", 1, "target", 0).unwrap();
        assert!(layer.lookup_dirent("alice", 1, "source").unwrap().is_none());
        assert_eq!(
            layer.lookup_dirent("alice", 1, "target").unwrap(),
            Some(source)
        );
        assert_eq!(
            layer.read_handle("alice", source_handle, 0, 20).unwrap(),
            b"source!"
        );
        assert_eq!(
            layer.read_handle("alice", target_handle, 0, 20).unwrap(),
            b"target"
        );
        assert_eq!(
            layer.getattr_handle("alice", target_handle).unwrap().nlinks,
            0
        );
        layer.write_handle("alice", target_handle, 0, b"T").unwrap();
        layer.sync_handle("alice", target_handle).unwrap();
        layer.sync_handle("alice", source_handle).unwrap();
        assert_eq!(layer.read_file("alice", source).unwrap(), b"source!");
        assert_eq!(layer.read_file("bob", target).unwrap(), b"target");
        layer.release_handle("alice", source_handle).unwrap();
        layer.release_handle("alice", target_handle).unwrap();
    });
}

#[test]
fn directory_rename_parent_link_counts_and_nonempty_rmdir() {
    both(|layer| {
        let left = layer.mkdir("alice", 1, "left", 0o755).unwrap();
        let right = layer.mkdir("alice", 1, "right", 0o755).unwrap();
        let child = layer.mkdir("alice", left, "child", 0o755).unwrap();
        assert_eq!(layer.parent_inode("alice", child).unwrap(), left);
        let file = layer
            .write_file("alice", child, "file", b"data", 0o644)
            .unwrap();
        check_errno(layer.rmdir("alice", left, "child"), 39);
        check_errno(layer.unlink("alice", left, "child", child), 21);
        check_errno(layer.rename("alice", 1, "left", child, "cycle", 0), 22);
        layer
            .rename("alice", left, "child", right, "moved", 0)
            .unwrap();
        assert_eq!(layer.parent_inode("alice", child).unwrap(), right);
        assert_eq!(
            layer.lookup_inode("alice", left).unwrap().unwrap().nlinks,
            2
        );
        assert_eq!(
            layer.lookup_inode("alice", right).unwrap().unwrap().nlinks,
            3
        );
        layer.unlink("alice", child, "file", file).unwrap();
        layer.rmdir("alice", right, "moved").unwrap();
        assert_eq!(
            layer.lookup_inode("alice", right).unwrap().unwrap().nlinks,
            2
        );
        assert_eq!(layer.parent_inode("alice", 1).unwrap(), 1);
    });
}

#[test]
fn hardlinks_and_symlinks_keep_scope_and_counts() {
    both(|layer| {
        let inode = layer
            .populate_base_file(1, "shared", b"base", 0o644)
            .unwrap();
        assert_eq!(layer.link("alice", inode, 1, "alias").unwrap(), inode);
        assert_eq!(
            layer.lookup_inode("alice", inode).unwrap().unwrap().nlinks,
            2
        );
        let handle = layer.open_file("alice", inode, true, false, false).unwrap();
        layer.write_handle("alice", handle, 0, b"B").unwrap();
        layer.unlink("alice", 1, "shared", inode).unwrap();
        assert_eq!(layer.getattr_handle("alice", handle).unwrap().nlinks, 1);
        layer.sync_handle("alice", handle).unwrap();
        assert_eq!(
            layer
                .read_file(
                    "alice",
                    layer.lookup_dirent("alice", 1, "alias").unwrap().unwrap()
                )
                .unwrap(),
            b"Base"
        );
        assert!(layer.lookup_dirent("bob", 1, "alias").unwrap().is_none());
        assert_eq!(layer.read_file("bob", inode).unwrap(), b"base");
        let link = layer.symlink("alice", 1, "symbolic", "alias").unwrap();
        let data = layer.lookup_inode("alice", link).unwrap().unwrap();
        assert_eq!(data.kind, FileKind::Symlink);
        assert_eq!(data.symlink_target, "alias");
        assert_eq!(data.size, 5);
        check_errno(layer.link("alice", 1, 1, "bad"), 1);
        layer.release_handle("alice", handle).unwrap();
    });
}

#[test]
fn cross_agent_handle_misuse_and_released_handles_return_ebadf() {
    both(|layer| {
        let inode = layer
            .write_file("alice", 1, "secret", b"secret", 0o600)
            .unwrap();
        let handle = layer.open_file("alice", inode, true, false, false).unwrap();
        check_errno(layer.read_handle("bob", handle, 0, 10), 9);
        check_errno(layer.write_handle("bob", handle, 0, b"x"), 9);
        check_errno(layer.sync_handle("bob", handle), 9);
        check_errno(layer.release_handle("bob", handle), 9);
        check_errno(layer.getattr_handle("bob", handle), 9);
        layer.release_handle("alice", handle).unwrap();
        check_errno(layer.read_handle("alice", handle, 0, 10), 9);
    });
}

#[test]
fn atomic_create_race_returns_one_stable_inode_and_exclusive_eexist() {
    for chunked in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let layer = Arc::new(manager(dir.path(), chunked));
        let jobs: Vec<_> = (0..12)
            .map(|_| {
                let layer = Arc::clone(&layer);
                std::thread::spawn(move || {
                    layer
                        .create_file("alice", 1, "raced", 0o644, false)
                        .unwrap()
                })
            })
            .collect();
        let inodes: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
        assert!(inodes.iter().all(|inode| *inode == inodes[0]));
        assert_eq!(layer.readdir("alice", 1).unwrap().len(), 1);
        check_errno(layer.create_file("alice", 1, "raced", 0o644, true), 17);
        layer
            .write_file("alice", 1, "raced", b"preserved", 0o644)
            .unwrap();
        assert_eq!(
            layer
                .create_file("alice", 1, "raced", 0o644, false)
                .unwrap(),
            inodes[0]
        );
        assert_eq!(layer.read_file("alice", inodes[0]).unwrap(), b"preserved");
    }
}

#[test]
fn concurrent_append_is_serialized_across_distinct_handles() {
    let dir = tempfile::tempdir().unwrap();
    let layer = Arc::new(manager(dir.path(), true));
    let inode = layer
        .create_file("alice", 1, "append", 0o644, true)
        .unwrap();
    let jobs: Vec<_> = (0u8..16)
        .map(|byte| {
            let layer = Arc::clone(&layer);
            std::thread::spawn(move || {
                let handle = layer.open_file("alice", inode, true, true, false).unwrap();
                layer
                    .write_handle("alice", handle, u64::MAX, &[byte; 16])
                    .unwrap();
                layer.release_handle("alice", handle).unwrap();
            })
        })
        .collect();
    for job in jobs {
        job.join().unwrap();
    }
    let bytes = layer.read_file("alice", inode).unwrap();
    assert_eq!(bytes.len(), 256);
    let mut markers: Vec<_> = bytes
        .as_chunks::<16>()
        .0
        .iter()
        .map(|chunk| {
            assert!(chunk.iter().all(|byte| *byte == chunk[0]));
            chunk[0]
        })
        .collect();
    markers.sort_unstable();
    assert_eq!(markers, (0u8..16).collect::<Vec<_>>());
}

#[test]
fn fsync_and_directory_barrier_reopen_versioned_bindings_in_eventual_mode() {
    for chunked in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let inode;
        let expected_hash = CasStore::hash(b"durable!");
        {
            let layer = manager(dir.path(), chunked);
            inode = layer
                .write_file("alice", 1, "before", b"durable", 0o644)
                .unwrap();
            let handle = layer.open_file("alice", inode, true, true, false).unwrap();
            layer.write_handle("alice", handle, 0, b"!").unwrap();
            layer.rename("alice", 1, "before", 1, "after", 0).unwrap();
            layer.sync_handle("alice", handle).unwrap();
            layer.sync_directory("alice", 1).unwrap();
            layer.release_handle("alice", handle).unwrap();
            let dump = layer.meta().dump_all_tables().unwrap();
            let row = dump
                .inodes
                .iter()
                .find(|(agent, number, _)| agent == "alice" && *number == inode)
                .unwrap();
            assert!(row.2.starts_with(b"SFI2"));
            if chunked {
                assert!(referenced_blob_hashes(&dump).is_empty());
                assert_eq!(referenced_workspace_content(&dump)[0].sha256, expected_hash);
                assert!(layer.workspace_retention_cleanup_pending());
                assert_eq!(layer.cas().stats().unwrap().blob_count, 0);
            }
        }
        let reopened = manager(dir.path(), chunked);
        assert_eq!(
            reopened.lookup_dirent("alice", 1, "after").unwrap(),
            Some(inode)
        );
        assert!(reopened
            .lookup_dirent("alice", 1, "before")
            .unwrap()
            .is_none());
        assert_eq!(reopened.read_file("alice", inode).unwrap(), b"durable!");
        assert_eq!(
            reopened.lookup_inode("alice", inode).unwrap().unwrap().hash,
            expected_hash
        );
    }
}

#[test]
fn invalid_names_parent_kinds_permissions_and_rename_flags() {
    both(|layer| {
        for name in ["", ".", "..", "a/b", "nul\0name"] {
            check_errno(layer.mkdir("alice", 1, name, 0o755), 22);
        }
        check_errno(layer.mkdir("alice", 1, &"a".repeat(256), 0o755), 36);
        let file = layer
            .write_file("alice", 1, "file", b"data", 0o644)
            .unwrap();
        check_errno(layer.mkdir("alice", file, "bad", 0o755), 20);
        check_errno(layer.mkdir("alice", u64::MAX, "bad", 0o755), 2);
        let locked = layer.mkdir("alice", 1, "locked", 0o555).unwrap();
        check_errno(layer.create_file("alice", locked, "bad", 0o644, false), 13);
        check_errno(layer.rename("alice", 1, "file", 1, "file2", 2), 22);
        layer
            .write_file("alice", 1, "target", b"target", 0o644)
            .unwrap();
        check_errno(layer.rename("alice", 1, "file", 1, "target", 1), 17);
        layer
            .set_file_attributes(
                "alice",
                file,
                Some(0o444),
                Some(42),
                Some(43),
                Some(123),
                Some(456),
            )
            .unwrap();
        let data = layer.lookup_inode("alice", file).unwrap().unwrap();
        assert_eq!(
            (data.uid, data.gid, data.atime, data.mtime),
            (42, 43, 123, 456)
        );
        check_errno(layer.open_file("alice", file, true, false, false), 13);
    });
}

#[test]
fn bounded_dirty_writes_fail_without_changing_visible_state() {
    both(|layer| {
        let inode = layer
            .create_file("alice", 1, "bounded", 0o644, true)
            .unwrap();
        let handle = layer.open_file("alice", inode, true, false, false).unwrap();
        let bytes = vec![7; 8 * 1024 * 1024];
        layer.write_handle("alice", handle, 0, &bytes).unwrap();
        check_errno(
            layer.write_handle("alice", handle, bytes.len() as u64, b"x"),
            28,
        );
        check_errno(layer.write_handle("alice", handle, u64::MAX, b"x"), 27);
        assert_eq!(
            layer.getattr_handle("alice", handle).unwrap().size,
            bytes.len() as u64
        );
        assert_eq!(
            layer
                .read_handle("alice", handle, bytes.len() as u64 - 1, 100)
                .unwrap(),
            vec![7]
        );
        layer.sync_handle("alice", handle).unwrap();
        layer
            .write_handle("alice", handle, bytes.len() as u64, b"x")
            .unwrap();
        layer.release_handle("alice", handle).unwrap();
    });
}

#[test]
fn owned_create_assigns_once_and_nonexclusive_preserves_existing_owner() {
    both(|layer| {
        let inode = layer
            .create_file_owned("alice", 1, "owned", 0o640, true, 1000, 1001)
            .unwrap();
        let data = layer.lookup_inode("alice", inode).unwrap().unwrap();
        assert_eq!((data.mode, data.uid, data.gid), (0o640, 1000, 1001));
        assert_eq!(
            layer
                .create_file_owned("alice", 1, "owned", 0o600, false, 99, 98)
                .unwrap(),
            inode
        );
        let data = layer.lookup_inode("alice", inode).unwrap().unwrap();
        assert_eq!((data.mode, data.uid, data.gid), (0o640, 1000, 1001));
    });
}

#[test]
fn invalidation_callback_can_reenter_after_mutations_without_manager_lock() {
    let dir = tempfile::tempdir().unwrap();
    let layer = Arc::new(manager(dir.path(), true));
    let weak = Arc::downgrade(&layer);
    let (send, receive) = std::sync::mpsc::sync_channel(64);
    layer
        .set_invalidation_hook(Some(Arc::new(move |agent, inode| {
            if let Some(layer) = weak.upgrade() {
                // Read-only reentry proves callbacks run after the namespace lock is dropped.
                let _ = layer.lookup_inode(agent, inode).unwrap();
            }
            send.try_send((agent.to_string(), inode)).unwrap();
        })))
        .unwrap();
    let inode = layer
        .create_file("alice", 1, "notified", 0o644, true)
        .unwrap();
    let initial: Vec<_> = receive.try_iter().collect();
    assert!(initial.contains(&("alice".to_string(), inode)));
    assert!(initial.contains(&("alice".to_string(), 1)));
    let handle = layer.open_file("alice", inode, true, false, false).unwrap();
    layer.write_handle("alice", handle, 0, b"new").unwrap();
    assert_eq!(
        receive.try_iter().collect::<Vec<_>>(),
        vec![("alice".to_string(), inode)]
    );
    let plane = layer.artifact_plane().unwrap();
    assert!(Arc::ptr_eq(&plane, &layer.artifact_plane().unwrap()));
    layer.release_handle("alice", handle).unwrap();
    receive.try_iter().for_each(drop);
    layer.set_invalidation_hook(None).unwrap();
    layer.rename("alice", 1, "notified", 1, "quiet", 0).unwrap();
    assert!(receive.try_recv().is_err());
}

#[test]
fn ambiguous_historical_base_private_collision_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let layer = manager(dir.path(), false);
    let base = layer.populate_base_file(1, "base", b"base", 0o644).unwrap();
    // Reproduce an old per-layer allocation that used the same integer identity.
    let hash = layer.cas().store(b"private").unwrap().0;
    let data = sentinel_fs::metadata::InodeData::regular(hash, 7, 0o644);
    layer
        .meta()
        .create_file("alice", 1, "private", base, &data)
        .unwrap();
    check_errno(layer.read_file("alice", base), 116);
    check_errno(layer.open_file("alice", base, true, false, false), 116);
    assert_eq!(layer.read_file("bob", base).unwrap(), b"base");
}

#[test]
fn snapshot_cut_flushes_dirty_and_restore_rejects_live_handles() {
    both(|layer| {
        let inode = layer
            .write_file("alice", 1, "file", b"before", 0o644)
            .unwrap();
        let handle = layer.open_file("alice", inode, true, false, false).unwrap();
        layer.write_handle("alice", handle, 0, b"after!").unwrap();
        let snapshot = layer.snapshot_metadata().unwrap();
        check_errno(layer.restore_metadata(&snapshot), 16);
        layer.release_handle("alice", handle).unwrap();
        layer.rename("alice", 1, "file", 1, "moved", 0).unwrap();
        layer
            .write_file("alice", 1, "extra", b"extra", 0o644)
            .unwrap();
        layer.restore_metadata(&snapshot).unwrap();
        assert_eq!(
            layer.lookup_dirent("alice", 1, "file").unwrap(),
            Some(inode)
        );
        assert!(layer.lookup_dirent("alice", 1, "moved").unwrap().is_none());
        assert!(layer.lookup_dirent("alice", 1, "extra").unwrap().is_none());
        assert_eq!(layer.read_file("alice", inode).unwrap(), b"after!");
        let new_handle = layer
            .open_file("alice", inode, false, false, false)
            .unwrap();
        assert_ne!(new_handle, handle);
        check_errno(layer.read_handle("alice", handle, 0, 100), 9);
        layer.release_handle("alice", new_handle).unwrap();
    });
}

#[test]
fn directory_handles_block_restore_until_exact_release() {
    both(|layer| {
        let snapshot = layer.snapshot_metadata().unwrap();
        let directory = layer.mkdir("alice", 1, "directory", 0o755).unwrap();
        let handle = layer.open_directory("alice", directory).unwrap();
        assert_eq!(
            layer.getattr_handle("alice", handle).unwrap().kind,
            FileKind::Directory
        );
        check_errno(layer.getattr_handle("bob", handle), 9);
        check_errno(layer.validate_workspace_restore_budget(&snapshot), 16);
        check_errno(layer.restore_metadata(&snapshot), 16);
        assert_eq!(
            layer.lookup_dirent("alice", 1, "directory").unwrap(),
            Some(directory)
        );
        check_errno(layer.readdir_handle("bob", directory, handle), 9);
        check_errno(layer.readdir_handle("alice", 1, handle), 9);
        check_errno(layer.sync_directory_handle("alice", 1, handle), 9);
        check_errno(layer.release_directory("bob", directory, handle), 9);
        check_errno(layer.release_directory("alice", 1, handle), 9);
        check_errno(layer.restore_metadata(&snapshot), 16);
        assert!(layer
            .readdir_handle("alice", directory, handle)
            .unwrap()
            .is_empty());
        layer
            .sync_directory_handle("alice", directory, handle)
            .unwrap();
        // Release must not require a surviving pathname/link either.
        layer.rmdir("alice", 1, "directory").unwrap();
        check_errno(layer.restore_metadata(&snapshot), 16);
        layer.release_directory("alice", directory, handle).unwrap();
        check_errno(layer.release_directory("alice", directory, handle), 9);
        layer.restore_metadata(&snapshot).unwrap();
        let next = layer.open_directory("alice", 1).unwrap();
        assert_ne!(next, handle);
        check_errno(layer.readdir_handle("alice", 1, handle), 9);
        layer.release_directory("alice", 1, next).unwrap();
    });
}

#[test]
fn restore_preserves_inode_high_water_across_agents_and_reopen() {
    for chunked in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let snapshot;
        let removed;
        let later;
        {
            let layer = manager(dir.path(), chunked);
            snapshot = layer.snapshot_metadata().unwrap();
            removed = layer.mkdir("alice", 1, "removed", 0o755).unwrap();
            layer.restore_metadata(&snapshot).unwrap();
            later = layer.mkdir("bob", 1, "later", 0o755).unwrap();
            assert!(later > removed);
            assert!(layer.lookup_inode("alice", removed).unwrap().is_none());
            layer.restore_metadata(&snapshot).unwrap();
        }
        let layer = manager(dir.path(), chunked);
        let next = layer.mkdir("alice", 1, "next", 0o755).unwrap();
        assert!(next > later);
        assert!(layer.lookup_inode("alice", removed).unwrap().is_none());
        assert!(layer.lookup_inode("bob", later).unwrap().is_none());
        // The retained floor also applies to older allocation entrypoints.
        let legacy = layer.meta().next_inode("legacy").unwrap();
        assert!(legacy > later);
        let allocated = layer
            .meta()
            .create_file_allocating_inode(
                "legacy-directory",
                1,
                "allocated",
                &sentinel_fs::metadata::InodeData::directory(0o755),
                true,
            )
            .unwrap();
        assert!(allocated > later);
    }
}

#[test]
fn descriptor_setattr_and_truncate_preserve_open_unlinked_files() {
    both(|layer| {
        let inode = layer
            .write_file("alice", 1, "file", b"abcdef", 0o644)
            .unwrap();
        let handle = layer.open_file("alice", inode, true, false, false).unwrap();
        layer.unlink("alice", 1, "file", inode).unwrap();
        layer.truncate_handle("alice", inode, handle, 2).unwrap();
        layer
            .set_handle_attributes(
                "alice",
                inode,
                handle,
                Some(4),
                Some(0o444),
                Some(123),
                Some(456),
                Some(10),
                Some(20),
            )
            .unwrap();
        let attributes = layer.getattr_handle("alice", handle).unwrap();
        assert_eq!(attributes.nlinks, 0);
        assert_eq!(attributes.size, 4);
        assert_eq!(attributes.mode, 0o444);
        assert_eq!((attributes.uid, attributes.gid), (123, 456));
        assert_eq!((attributes.atime, attributes.mtime), (10, 20));
        assert_eq!(
            layer.read_handle("alice", handle, 0, 10).unwrap(),
            b"ab\0\0"
        );
        assert!(layer.lookup_inode("alice", inode).unwrap().is_none());
        assert!(layer.lookup_dirent("alice", 1, "file").unwrap().is_none());
        layer.set_workspace_budget("alice", 4).unwrap();
        check_errno(layer.truncate_handle("alice", inode, handle, 5), 122);
        assert_eq!(layer.getattr_handle("alice", handle).unwrap().size, 4);
        layer.sync_handle("alice", handle).unwrap();
        layer.release_handle("alice", handle).unwrap();
        assert_eq!(layer.workspace_budget("alice").unwrap().used_bytes, 0);
    });
}

#[test]
fn descriptor_truncate_after_rename_replacement_does_not_touch_destination() {
    both(|layer| {
        let old = layer
            .write_file("alice", 1, "destination", b"old", 0o644)
            .unwrap();
        let handle = layer.open_file("alice", old, true, false, false).unwrap();
        let replacement = layer
            .write_file("alice", 1, "source", b"new", 0o644)
            .unwrap();
        layer
            .rename("alice", 1, "source", 1, "destination", 0)
            .unwrap();
        layer
            .set_handle_attributes(
                "alice",
                old,
                handle,
                Some(0),
                Some(0o600),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(layer.getattr_handle("alice", handle).unwrap().nlinks, 0);
        assert!(layer
            .read_handle("alice", handle, 0, 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            layer.lookup_dirent("alice", 1, "destination").unwrap(),
            Some(replacement)
        );
        assert_eq!(layer.read_file("alice", replacement).unwrap(), b"new");
        layer.release_handle("alice", handle).unwrap();
    });
}

#[test]
fn descriptor_truncate_keeps_open_time_write_capability_after_chmod() {
    both(|layer| {
        let inode = layer
            .write_file("alice", 1, "file", b"abcdef", 0o644)
            .unwrap();
        let handle = layer.open_file("alice", inode, true, false, false).unwrap();
        layer
            .set_file_attributes("alice", inode, Some(0o444), None, None, None, None)
            .unwrap();
        check_errno(layer.truncate_file("alice", inode, 0), 13);
        layer.truncate_handle("alice", inode, handle, 2).unwrap();
        layer
            .set_handle_attributes(
                "alice",
                inode,
                handle,
                Some(1),
                None,
                None,
                None,
                None,
                Some(20),
            )
            .unwrap();
        assert_eq!(layer.read_handle("alice", handle, 0, 10).unwrap(), b"a");
        assert_eq!(layer.getattr_handle("alice", handle).unwrap().mode, 0o444);
        layer.release_handle("alice", handle).unwrap();
        assert_eq!(layer.read_file("alice", inode).unwrap(), b"a");
    });
}

#[test]
fn descriptor_mutations_reject_wrong_agent_inode_access_and_handle_kind() {
    both(|layer| {
        let inode = layer
            .write_file("alice", 1, "file", b"kept", 0o644)
            .unwrap();
        let other = layer
            .write_file("alice", 1, "other", b"other", 0o644)
            .unwrap();
        let writable = layer.open_file("alice", inode, true, false, false).unwrap();
        let readonly = layer
            .open_file("alice", inode, false, false, false)
            .unwrap();
        let directory = layer.open_directory("alice", 1).unwrap();
        for (agent, target, handle) in [
            ("bob", inode, writable),
            ("alice", other, writable),
            ("alice", inode, readonly),
            ("alice", 1, directory),
        ] {
            check_errno(layer.truncate_handle(agent, target, handle, 0), 9);
            check_errno(
                layer.set_handle_attributes(
                    agent,
                    target,
                    handle,
                    Some(0),
                    Some(0),
                    None,
                    None,
                    None,
                    None,
                ),
                9,
            );
        }
        check_errno(layer.readdir_handle("alice", inode, writable), 9);
        check_errno(layer.release_directory("alice", inode, writable), 9);
        check_errno(layer.release_handle("alice", directory), 9);
        assert_eq!(layer.read_file("alice", inode).unwrap(), b"kept");
        assert_eq!(layer.read_file("alice", other).unwrap(), b"other");
        assert_eq!(layer.getattr_handle("alice", writable).unwrap().mode, 0o644);
        layer.release_directory("alice", 1, directory).unwrap();
        layer.release_handle("alice", readonly).unwrap();
        layer.release_handle("alice", writable).unwrap();
        check_errno(layer.truncate_handle("alice", inode, writable, 0), 9);
        check_errno(
            layer.set_handle_attributes(
                "alice",
                inode,
                writable,
                None,
                Some(0),
                None,
                None,
                None,
                None,
            ),
            9,
        );
    });
}

#[test]
fn restore_notifies_previous_and_restored_keys_outside_locks() {
    let dir = tempfile::tempdir().unwrap();
    let layer = Arc::new(manager(dir.path(), true));
    let kept = layer
        .write_file("alice", 1, "kept", b"kept", 0o644)
        .unwrap();
    let snapshot = layer.meta().dump_all_tables().unwrap();
    let removed = layer
        .write_file("alice", 1, "removed", b"removed", 0o644)
        .unwrap();
    let weak = Arc::downgrade(&layer);
    let (send, receive) = std::sync::mpsc::sync_channel(64);
    layer
        .set_invalidation_hook(Some(Arc::new(move |agent, inode| {
            if let Some(layer) = weak.upgrade() {
                let _ = layer.lookup_inode(agent, inode).unwrap();
            }
            send.try_send((agent.to_string(), inode)).unwrap();
        })))
        .unwrap();
    layer.restore_metadata(&snapshot).unwrap();
    let keys: Vec<_> = receive.try_iter().collect();
    assert!(keys.contains(&("alice".to_string(), kept)));
    assert!(keys.contains(&("alice".to_string(), removed)));
    assert!(keys.contains(&(SHARED_BASE_LAYER_ID.to_string(), 1)));
    layer.set_invalidation_hook(None).unwrap();
}

#[test]
fn configured_binding_failure_never_falls_back_to_same_sha_legacy_blob() {
    let dir = tempfile::tempdir().unwrap();
    let inode;
    {
        let layer = manager(dir.path(), true);
        inode = layer
            .write_file("alice", 1, "file", b"same SHA", 0o644)
            .unwrap();
        layer.cas().store(b"same SHA").unwrap();
        layer.sync_directory("alice", 1).unwrap();
    }
    let wrong_plane = Arc::new(ArtifactPlane::open(dir.path().join("wrong-content.redb")).unwrap());
    let layer = LayerManager::with_artifact_plane(
        CasStore::open(dir.path()).unwrap(),
        MetadataStore::open(dir.path().join("namespace.redb")).unwrap(),
        wrong_plane,
    );
    assert!(layer.read_file("alice", inode).is_err());
    assert!(layer.cas().contains(&CasStore::hash(b"same SHA")));
}

#[test]
fn legacy_inode_read_and_write_migrate_only_on_publication() {
    let dir = tempfile::tempdir().unwrap();
    let inode;
    {
        let layer = manager(dir.path(), false);
        inode = layer
            .write_file("alice", 1, "legacy", b"legacy", 0o644)
            .unwrap();
        let handle = layer
            .open_file("alice", inode, false, false, false)
            .unwrap();
        layer.sync_handle("alice", handle).unwrap();
        layer.release_handle("alice", handle).unwrap();
    }
    let layer = manager(dir.path(), true);
    assert_eq!(layer.read_file("alice", inode).unwrap(), b"legacy");
    let handle = layer.open_file("alice", inode, true, true, false).unwrap();
    layer.write_handle("alice", handle, 0, b"!").unwrap();
    assert_eq!(
        layer.read_handle("alice", handle, 0, 20).unwrap(),
        b"legacy!"
    );
    layer.sync_handle("alice", handle).unwrap();
    let dump = layer.snapshot_metadata().unwrap();
    assert_eq!(referenced_workspace_content(&dump).len(), 1);
    assert!(referenced_blob_hashes(&dump).is_empty());
    assert_eq!(layer.read_file("alice", inode).unwrap(), b"legacy!");
    layer.release_handle("alice", handle).unwrap();
}

#[test]
fn aggregate_budget_shared_handles_and_append_use_dirty_inode_size_once() {
    both(|layer| {
        let inode = layer
            .write_file("alice", 1, "first", b"abcd", 0o644)
            .unwrap();
        layer
            .write_file("alice", 1, "second", b"xyz", 0o644)
            .unwrap();
        assert_eq!(
            layer.set_workspace_budget("alice", 10).unwrap().used_bytes,
            7
        );
        let first = layer.open_file("alice", inode, true, true, false).unwrap();
        let second = layer.open_file("alice", inode, true, true, false).unwrap();
        layer.write_handle("alice", first, 0, b"ef").unwrap();
        layer.write_handle("alice", second, 0, b"g").unwrap();
        let accounting = layer.workspace_budget("alice").unwrap();
        assert_eq!(
            (
                accounting.used_bytes,
                accounting.remaining_bytes,
                accounting.regular_inode_count
            ),
            (10, 0, 2)
        );
        check_errno(layer.write_handle("alice", first, 0, b"h"), 122);
        assert_eq!(
            layer.read_handle("alice", second, 0, 20).unwrap(),
            b"abcdefg"
        );
        assert_eq!(layer.getattr_handle("alice", first).unwrap().size, 7);
        assert_eq!(layer.workspace_budget("alice").unwrap(), accounting);
        layer.release_handle("alice", first).unwrap();
        layer.release_handle("alice", second).unwrap();
        assert_eq!(layer.workspace_budget("alice").unwrap().used_bytes, 10);
    });
}

#[test]
fn hardlink_budget_releases_only_after_last_unlinked_handle() {
    both(|layer| {
        let inode = layer
            .write_file("alice", 1, "file", b"four", 0o644)
            .unwrap();
        layer.set_workspace_budget("alice", 4).unwrap();
        layer.link("alice", inode, 1, "alias").unwrap();
        assert_eq!(
            layer.workspace_budget("alice").unwrap().regular_inode_count,
            1
        );
        let first = layer.open_file("alice", inode, true, false, false).unwrap();
        let second = layer.open_file("alice", inode, true, false, false).unwrap();
        layer.unlink("alice", 1, "file", inode).unwrap();
        assert_eq!(layer.workspace_budget("alice").unwrap().used_bytes, 4);
        layer.unlink("alice", 1, "alias", inode).unwrap();
        let accounting = layer.workspace_budget("alice").unwrap();
        assert_eq!(
            (
                accounting.used_bytes,
                accounting.unlinked_live_bytes,
                accounting.unlinked_live_inode_count
            ),
            (4, 4, 1)
        );
        check_errno(layer.write_file("alice", 1, "new", b"x", 0o644), 122);
        check_errno(layer.write_handle("alice", first, 4, b"x"), 122);
        layer.release_handle("alice", first).unwrap();
        assert_eq!(layer.workspace_budget("alice").unwrap().used_bytes, 4);
        assert_eq!(layer.read_handle("alice", second, 0, 10).unwrap(), b"four");
        layer.release_handle("alice", second).unwrap();
        let accounting = layer.workspace_budget("alice").unwrap();
        assert_eq!(
            (
                accounting.used_bytes,
                accounting.regular_inode_count,
                accounting.unlinked_live_bytes
            ),
            (0, 0, 0)
        );
        layer.write_file("alice", 1, "new", b"four", 0o644).unwrap();
        assert_eq!(layer.workspace_budget("alice").unwrap().used_bytes, 4);
    });
}

#[test]
fn truncate_budget_growth_failure_preserves_binding_and_holes() {
    both(|layer| {
        let inode = layer.write_file("alice", 1, "file", b"abc", 0o644).unwrap();
        layer.write_file("alice", 1, "other", b"xy", 0o644).unwrap();
        layer.set_workspace_budget("alice", 8).unwrap();
        let handle = layer.open_file("alice", inode, true, false, false).unwrap();
        layer.truncate_file("alice", inode, 6).unwrap();
        let before = layer.meta().dump_all_tables().unwrap();
        let accounting = layer.workspace_budget("alice").unwrap();
        check_errno(layer.truncate_file("alice", inode, 7), 122);
        assert_eq!(
            layer.meta().dump_all_tables().unwrap().inodes,
            before.inodes
        );
        assert_eq!(layer.workspace_budget("alice").unwrap(), accounting);
        assert_eq!(
            layer.read_handle("alice", handle, 0, 10).unwrap(),
            b"abc\0\0\0"
        );
        layer.truncate_file("alice", inode, 2).unwrap();
        assert_eq!(layer.workspace_budget("alice").unwrap().remaining_bytes, 4);
        layer.release_handle("alice", handle).unwrap();
    });
}

#[test]
fn direct_write_budget_failure_has_no_root_allocation_content_or_inode_effect() {
    both(|layer| {
        layer.set_workspace_budget("bob", 2).unwrap();
        let before = layer.meta().dump_all_tables().unwrap();
        let blobs = layer.cas().stats().unwrap().blob_count;
        let objects = layer
            .artifact_plane()
            .map(|plane| plane.chunk_count().unwrap());
        check_errno(layer.write_file("bob", 1, "too-big", b"abc", 0o644), 122);
        assert_eq!(
            layer.meta().dump_all_tables().unwrap().inodes,
            before.inodes
        );
        assert!(layer.meta().get_inode("bob", 1).unwrap().is_none());
        assert_eq!(layer.cas().stats().unwrap().blob_count, blobs);
        assert_eq!(
            layer
                .artifact_plane()
                .map(|plane| plane.chunk_count().unwrap()),
            objects
        );
        let inode = layer.write_file("bob", 1, "file", b"ab", 0o644).unwrap();
        let handle = layer.open_file("bob", inode, true, false, false).unwrap();
        check_errno(layer.write_file("bob", 1, "file", b"xyz", 0o644), 122);
        assert_eq!(layer.read_handle("bob", handle, 0, 10).unwrap(), b"ab");
        assert_eq!(layer.getattr_handle("bob", handle).unwrap().size, 2);
        layer.release_handle("bob", handle).unwrap();
    });
}

#[test]
fn trusted_budget_configuration_rejects_lower_usage_and_keeps_aggregate_independent() {
    both(|layer| {
        layer
            .write_file("alice", 1, "file", b"12345", 0o644)
            .unwrap();
        let default = layer.workspace_budget("alice").unwrap();
        assert_eq!(DEFAULT_WORKSPACE_BUDGET_BYTES, 64 * 1024 * 1024);
        assert_eq!(default.limit_bytes, DEFAULT_WORKSPACE_BUDGET_BYTES);
        assert!(!default.explicitly_configured);
        check_errno(layer.set_workspace_budget("alice", 4), 22);
        assert_eq!(layer.workspace_budget("alice").unwrap(), default);
        let configured = layer
            .set_workspace_budget("alice", DEFAULT_WORKSPACE_BUDGET_BYTES)
            .unwrap();
        assert!(configured.explicitly_configured);
        assert_eq!(configured.agent_id, "alice");
        assert_eq!(
            configured.remaining_bytes,
            DEFAULT_WORKSPACE_BUDGET_BYTES - 5
        );
        check_errno(
            layer.set_workspace_budget("alice", MAX_WORKSPACE_BUDGET_BYTES + 1),
            22,
        );
        assert_eq!(layer.workspace_budget("alice").unwrap(), configured);
        assert_eq!(
            layer
                .set_workspace_budget("bob", 0)
                .unwrap()
                .remaining_bytes,
            0
        );
        check_errno(layer.write_file("bob", 1, "blocked", b"x", 0o644), 122);
    });
}

#[test]
fn concurrent_native_growth_cannot_exceed_one_aggregate_budget() {
    let dir = tempfile::tempdir().unwrap();
    let layer = Arc::new(manager(dir.path(), true));
    let first = layer.create_file("alice", 1, "first", 0o644, true).unwrap();
    let second = layer
        .create_file("alice", 1, "second", 0o644, true)
        .unwrap();
    layer.set_workspace_budget("alice", 10).unwrap();
    let jobs: Vec<_> = [first, second]
        .into_iter()
        .map(|inode| {
            let layer = Arc::clone(&layer);
            std::thread::spawn(move || {
                let handle = layer.open_file("alice", inode, true, true, false).unwrap();
                let result = layer.write_handle("alice", handle, 0, b"123456");
                layer.release_handle("alice", handle).unwrap();
                result
            })
        })
        .collect();
    let results: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    for result in results {
        if result.is_err() {
            check_errno(result, 122);
        }
    }
    let accounting = layer.workspace_budget("alice").unwrap();
    assert_eq!((accounting.used_bytes, accounting.remaining_bytes), (6, 4));
}

#[test]
fn restore_preserves_trusted_budget_and_rejects_growth_before_adoption() {
    both(|layer| {
        layer
            .write_file("alice", 1, "kept", b"four", 0o644)
            .unwrap();
        let small = layer.snapshot_metadata().unwrap();
        let removed = layer
            .write_file("alice", 1, "removed", b"12345", 0o644)
            .unwrap();
        let large = layer.snapshot_metadata().unwrap();
        layer.unlink("alice", 1, "removed", removed).unwrap();
        let configured = layer.set_workspace_budget("alice", 6).unwrap();
        let before = layer.meta().dump_all_tables().unwrap();
        check_errno(layer.validate_workspace_restore_budget(&large), 122);
        check_errno(layer.restore_metadata(&large), 122);
        assert_eq!(
            layer.meta().dump_all_tables().unwrap().inodes,
            before.inodes
        );
        assert_eq!(layer.workspace_budget("alice").unwrap(), configured);
        layer.restore_metadata(&small).unwrap();
        let after = layer.workspace_budget("alice").unwrap();
        assert_eq!(
            (
                after.limit_bytes,
                after.used_bytes,
                after.explicitly_configured
            ),
            (6, 4, true)
        );
    });
}

#[test]
fn inherited_cow_and_trusted_base_growth_respect_namespace_budget() {
    both(|layer| {
        let inode = layer
            .populate_base_file(1, "shared", b"four", 0o644)
            .unwrap();
        layer.set_workspace_budget("alice", 5).unwrap();
        let before = layer.meta().dump_all_tables().unwrap();
        check_errno(layer.populate_base_file(1, "blocked", b"xy", 0o644), 122);
        assert_eq!(
            layer.meta().dump_all_tables().unwrap().inodes,
            before.inodes
        );
        layer
            .write_file("alice", 1, "shared", b"12345", 0o644)
            .unwrap();
        assert_eq!(layer.workspace_budget("alice").unwrap().used_bytes, 5);
        assert_eq!(layer.workspace_budget("bob").unwrap().used_bytes, 4);
        layer.link("alice", inode, 1, "alias").unwrap();
        assert_eq!(
            layer.workspace_budget("alice").unwrap().regular_inode_count,
            1
        );
        layer
            .populate_base_file(1, "shared", b"123456", 0o644)
            .unwrap();
        assert_eq!(layer.workspace_budget("alice").unwrap().used_bytes, 5);
        assert_eq!(layer.workspace_budget("bob").unwrap().used_bytes, 6);
    });
}

#[test]
fn existing_over_default_data_is_preserved_and_can_shrink_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let inode;
    {
        let layer = manager(dir.path(), true);
        inode = layer
            .create_file("alice", 1, "sparse", 0o644, true)
            .unwrap();
        layer
            .set_workspace_budget("alice", DEFAULT_WORKSPACE_BUDGET_BYTES + 1)
            .unwrap();
        layer
            .truncate_file("alice", inode, DEFAULT_WORKSPACE_BUDGET_BYTES + 1)
            .unwrap();
        layer.sync_directory("alice", 1).unwrap();
    }
    let layer = manager(dir.path(), true);
    let before = layer.workspace_budget("alice").unwrap();
    assert!(before.over_limit);
    assert_eq!(before.remaining_bytes, 0);
    check_errno(
        layer.set_workspace_budget("alice", DEFAULT_WORKSPACE_BUDGET_BYTES),
        22,
    );
    check_errno(
        layer.truncate_file("alice", inode, DEFAULT_WORKSPACE_BUDGET_BYTES + 2),
        122,
    );
    assert_eq!(layer.workspace_budget("alice").unwrap(), before);
    assert_eq!(
        layer
            .read_file_range("alice", inode, DEFAULT_WORKSPACE_BUDGET_BYTES, 1)
            .unwrap(),
        vec![0]
    );
    layer
        .truncate_file("alice", inode, DEFAULT_WORKSPACE_BUDGET_BYTES)
        .unwrap();
    assert!(!layer.workspace_budget("alice").unwrap().over_limit);
}

#[test]
fn chunk_stats_report_physical_storage_without_fabricated_dedup_ratio() {
    let dir = tempfile::tempdir().unwrap();
    let layer = manager(dir.path(), true);
    let content = vec![b'A'; 1024 * 1024];
    layer
        .write_file("alice", 1, "compressed", &content, 0o644)
        .unwrap();
    let plane = layer.artifact_plane().unwrap();
    let (chunks, bytes) = plane.physical_storage_stats().unwrap();
    let stats = layer.storage_stats().unwrap();
    assert_eq!(
        stats.accounting_basis,
        "chunk_segments_including_retention_and_legacy_cas"
    );
    assert!(!stats.dedup_metrics_available);
    assert_eq!(
        (stats.cas_blob_count, stats.cas_bytes_on_disk),
        (chunks, bytes)
    );
    assert_eq!(
        (stats.regular_file_count, stats.logical_regular_file_bytes),
        (1, content.len() as u64)
    );
    assert_eq!(stats.unreadable_inode_rows, 0); // Allocation counters are not inode decode failures.
    assert_eq!(stats.dedup_savings_bytes, 0);
    assert_eq!(stats.dedup_ratio_percent, 0.0);
}

#[test]
fn mixed_stats_count_legacy_blobs_and_retained_segments_but_only_live_inode_rows() {
    let dir = tempfile::tempdir().unwrap();
    {
        let layer = manager(dir.path(), false);
        layer
            .write_file("alice", 1, "legacy", b"legacy", 0o644)
            .unwrap();
        let stats = layer.storage_stats().unwrap();
        assert_eq!(stats.accounting_basis, "legacy_cas_compressed_storage");
        assert!(stats.dedup_metrics_available);
        let physical = layer.cas().stats().unwrap();
        let expected_savings = 6u64.saturating_sub(physical.total_bytes_on_disk);
        assert_eq!(stats.dedup_savings_bytes, expected_savings);
        assert_eq!(
            stats.dedup_ratio_percent,
            expected_savings as f64 * 100.0 / 6.0
        );
    }
    let layer = manager(dir.path(), true);
    let inode = layer
        .write_file("alice", 1, "chunked", b"chunk", 0o644)
        .unwrap();
    layer.link("alice", inode, 1, "alias").unwrap();
    let stats = layer.storage_stats().unwrap();
    assert_eq!(
        (stats.regular_file_count, stats.logical_regular_file_bytes),
        (2, 11)
    );
    let plane = layer.artifact_plane().unwrap();
    let (chunks, segments) = plane.physical_storage_stats().unwrap();
    let legacy = layer.cas().stats().unwrap();
    assert_eq!(stats.cas_blob_count, chunks + legacy.blob_count);
    assert_eq!(
        stats.cas_bytes_on_disk,
        segments + legacy.total_bytes_on_disk
    );
    layer.unlink("alice", 1, "chunked", inode).unwrap();
    assert_eq!(layer.storage_stats().unwrap().regular_file_count, 2);
    layer.unlink("alice", 1, "alias", inode).unwrap();
    let retired = layer.storage_stats().unwrap();
    let (retained_chunks, retained_segments) = plane.physical_storage_stats().unwrap();
    assert_eq!(
        (
            retired.regular_file_count,
            retired.logical_regular_file_bytes
        ),
        (1, 6)
    );
    assert_eq!(retired.cas_blob_count, retained_chunks + legacy.blob_count);
    assert_eq!(
        retired.cas_bytes_on_disk,
        retained_segments + legacy.total_bytes_on_disk
    );
    assert!(!retired.dedup_metrics_available);
    assert_eq!(
        (retired.dedup_savings_bytes, retired.dedup_ratio_percent),
        (0, 0.0)
    );
}
