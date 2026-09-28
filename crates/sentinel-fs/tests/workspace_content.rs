use sentinel_fs::artifact::{ArtifactPlane, DurabilityLevel, WorkspaceContentRef, WorkspacePatch};
use sentinel_fs::chunker::{chunk_data, MAX_CHUNK_BYTES};
use sentinel_fs::gc::{gc_chunks, gc_trash, release_object};
use sentinel_fs::ingest::{begin_ingest, commit_ingest, BatchIngest};
use sentinel_fs::read_planner::{read_object, read_object_streaming};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Barrier};

fn publish(plane: &ArtifactPlane, data: &[u8]) -> WorkspaceContentRef {
    plane
        .publish_workspace(
            None,
            data.len() as u64,
            &[WorkspacePatch {
                offset: 0,
                data: data.to_vec(),
            }],
        )
        .unwrap()
}

fn assert_bytes(plane: &ArtifactPlane, content: WorkspaceContentRef, expected: &[u8]) {
    assert_eq!(
        plane
            .read_workspace_range(content, 0, expected.len() + 17)
            .unwrap(),
        expected
    );
    let digest: [u8; 32] = Sha256::digest(expected).into();
    assert_eq!(content.sha256, digest);
    assert_eq!(content.size, expected.len() as u64);
    assert_eq!(read_object(plane, content.object_id).unwrap(), expected);
    let streamed: Vec<u8> = read_object_streaming(plane, content.object_id)
        .unwrap()
        .flat_map(|part| part.unwrap())
        .collect();
    assert_eq!(streamed, expected);
}

fn physical_bytes(db: &Path) -> u64 {
    std::fs::read_dir(db.with_extension("segments"))
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum()
}

#[test]
fn named_artifact_reference_prevents_workspace_release_and_gc() {
    let dir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(dir.path().join("plane.redb")).unwrap();
    let content = publish(&plane, b"accepted artifact");
    plane.retain_workspace("active-workspace", content).unwrap();
    plane
        .set_ref("accepted-release", content.object_id)
        .unwrap();
    plane.release_workspace("active-workspace").unwrap();
    release_object(&plane, content.object_id).unwrap();
    gc_chunks(&plane).unwrap();
    gc_trash(&plane, 0).unwrap();
    assert_bytes(&plane, content, b"accepted artifact");
}

#[test]
fn two_owners_share_physical_chunks_but_edits_are_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plane.redb");
    let plane = ArtifactPlane::open(&path).unwrap();
    let data: Vec<u8> = (0..800_000).map(|i| (i % 251) as u8).collect();
    let alice = publish(&plane, &data);
    plane.retain_workspace("alice/inode", alice).unwrap();
    let bytes = physical_bytes(&path);
    let bob = publish(&plane, &data);
    plane.retain_workspace("bob/inode", bob).unwrap();
    assert_ne!(alice.object_id, bob.object_id);
    assert_eq!(alice.sha256, bob.sha256);
    assert_eq!(physical_bytes(&path), bytes);
    assert_eq!(
        plane.get_manifest(alice.object_id).unwrap(),
        plane.get_manifest(bob.object_id).unwrap()
    );

    let edited = plane
        .publish_workspace(
            Some(alice),
            alice.size,
            &[WorkspacePatch {
                offset: 30_000,
                data: b"private edit".to_vec(),
            }],
        )
        .unwrap();
    let mut expected = data.clone();
    expected[30_000..30_012].copy_from_slice(b"private edit");
    assert_bytes(&plane, edited, &expected);
    assert_bytes(&plane, bob, &data);
    // Even the touched chunk is reused through immutable prefix/suffix slices.
    let base_hashes: HashSet<_> = plane
        .get_manifest(alice.object_id)
        .unwrap()
        .unwrap()
        .into_iter()
        .collect();
    let edited_hashes: HashSet<_> = plane
        .get_manifest(edited.object_id)
        .unwrap()
        .unwrap()
        .into_iter()
        .collect();
    assert!(base_hashes.is_subset(&edited_hashes));
    plane.retain_workspace("alice/inode", edited).unwrap();
    // Retaining a candidate must not delete the previous namespace publication.
    assert!(plane.get_object(alice.object_id).unwrap().is_some());
    release_object(&plane, alice.object_id).unwrap();
    assert!(plane.get_object(alice.object_id).unwrap().is_none());
    release_object(&plane, bob.object_id).unwrap();
    assert_bytes(&plane, bob, &data);
}

#[test]
fn patches_are_ordered_and_truncation_extension_and_holes_are_exact() {
    let dir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(dir.path().join("plane.redb")).unwrap();
    let base = publish(&plane, b"abcdefghij");
    let changed = plane
        .publish_workspace(
            Some(base),
            16,
            &[
                WorkspacePatch {
                    offset: 2,
                    data: b"123456".to_vec(),
                },
                WorkspacePatch {
                    offset: 4,
                    data: b"XY".to_vec(),
                },
                WorkspacePatch {
                    offset: 14,
                    data: b"END".to_vec(),
                },
                WorkspacePatch {
                    offset: 99,
                    data: b"ignored".to_vec(),
                },
            ],
        )
        .unwrap();
    assert_bytes(&plane, changed, b"ab12XY56ij\0\0\0\0EN");
    let short = plane.publish_workspace(Some(changed), 5, &[]).unwrap();
    assert_bytes(&plane, short, b"ab12X");
    let extended = plane.publish_workspace(Some(short), 9, &[]).unwrap();
    assert_bytes(&plane, extended, b"ab12X\0\0\0\0");
    let empty = plane.publish_workspace(Some(extended), 0, &[]).unwrap();
    assert_bytes(&plane, empty, b"");
    let hole = plane
        .publish_workspace(
            None,
            2_000_000,
            &[WorkspacePatch {
                offset: 1_999_998,
                data: b"ok".to_vec(),
            }],
        )
        .unwrap();
    let before = plane.cache_stats();
    assert_eq!(
        plane.read_workspace_range(hole, 100, 31).unwrap(),
        vec![0; 31]
    );
    let after = plane.cache_stats();
    assert_eq!(before.misses + before.hits, after.misses + after.hits);
    assert_eq!(
        plane.read_workspace_range(hole, 1_999_993, 100).unwrap(),
        b"\0\0\0\0\0ok"
    );
    assert_eq!(
        plane.get_manifest(hole.object_id).unwrap().unwrap().len(),
        1
    );
}

#[test]
fn range_read_fetches_only_intersecting_chunks_and_validates_even_empty_reads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plane.redb");
    let content;
    let data: Vec<u8> = (0..1_000_000).map(|i| (i % 251) as u8).collect();
    {
        let plane = ArtifactPlane::open(&path).unwrap();
        content = publish(&plane, &data);
    }
    let plane = ArtifactPlane::open(&path).unwrap();
    assert_eq!(
        plane.read_workspace_range(content, 0, 7).unwrap(),
        &data[..7]
    );
    assert_eq!(plane.cache_stats().misses, 1);
    let boundary = chunk_data(&data).next().unwrap().data.len();
    assert_eq!(
        plane
            .read_workspace_range(content, (boundary - 2) as u64, 5)
            .unwrap(),
        &data[boundary - 2..boundary + 3]
    );
    assert_eq!(
        plane
            .read_workspace_range(content, content.size - 3, usize::MAX)
            .unwrap(),
        &data[data.len() - 3..]
    );
    assert!(plane
        .read_workspace_range(content, u64::MAX, usize::MAX)
        .unwrap()
        .is_empty());
    assert!(plane
        .read_workspace_range(content, 1, 0)
        .unwrap()
        .is_empty());
    let mut forged = content;
    forged.sha256[0] ^= 1;
    assert!(plane.read_workspace_range(forged, 0, 0).is_err());
    assert!(plane.publish_workspace(Some(forged), 0, &[]).is_err());
    assert!(plane.retain_workspace("forged", forged).is_err());
    forged = content;
    forged.size += 1;
    assert!(plane.read_workspace_range(forged, u64::MAX, 0).is_err());
    assert!(plane
        .publish_workspace(
            None,
            1,
            &[WorkspacePatch {
                offset: u64::MAX,
                data: vec![1]
            }]
        )
        .is_err());
}

#[test]
fn repeated_chunks_dedup_in_objects_batches_and_concurrent_publications() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plane.redb");
    let plane = Arc::new(ArtifactPlane::open(&path).unwrap());
    let data = vec![0xAA; MAX_CHUNK_BYTES * 4];
    let expected_chunks: Vec<_> = chunk_data(&data).map(|c| c.hash).collect();
    let unique: HashSet<_> = expected_chunks.iter().copied().collect();
    assert!(expected_chunks.len() > unique.len());
    let content = publish(&plane, &data);
    let bytes = physical_bytes(&path);
    assert_eq!(plane.chunk_count().unwrap(), unique.len() as u64);
    assert_eq!(
        plane.get_manifest(content.object_id).unwrap().unwrap(),
        expected_chunks
    );

    let barrier = Arc::new(Barrier::new(5));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let plane = Arc::clone(&plane);
            let barrier = Arc::clone(&barrier);
            let data = data.clone();
            std::thread::spawn(move || {
                barrier.wait();
                publish(&plane, &data)
            })
        })
        .collect();
    barrier.wait();
    for thread in threads {
        assert_bytes(&plane, thread.join().unwrap(), &data);
    }
    assert_eq!(physical_bytes(&path), bytes);

    let mut batch = BatchIngest::new(&plane);
    batch.add(&data, "text/plain").unwrap();
    batch.add(&data, "text/plain").unwrap();
    for id in batch.commit().unwrap() {
        assert_eq!(read_object(&plane, id).unwrap(), data);
    }
    assert_eq!(physical_bytes(&path), bytes);
    for hash in &unique {
        let occurrences = expected_chunks.iter().filter(|h| *h == hash).count() as u32;
        assert_eq!(plane.get_chunk_refcount(hash).unwrap(), occurrences * 7);
    }
}

#[test]
fn fresh_batch_and_simultaneous_legacy_ingest_do_not_copy_repeated_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plane.redb");
    let plane = Arc::new(ArtifactPlane::open(&path).unwrap());
    let data = vec![0xAA; MAX_CHUNK_BYTES * 4];
    let mut batch = BatchIngest::new(&plane);
    batch.add(&data, "application/octet-stream").unwrap();
    batch.add(&data, "application/octet-stream").unwrap();
    let ids = batch.commit().unwrap();
    assert_eq!(plane.chunk_count().unwrap(), 1);
    let hash = plane.get_manifest(ids[0]).unwrap().unwrap()[0];
    let compressed = plane.read_chunk_raw(&hash).unwrap();
    assert_eq!(physical_bytes(&path), compressed.len() as u64 + 16);

    let new_data = vec![0xCC; MAX_CHUNK_BYTES * 3];
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let plane = Arc::clone(&plane);
            let barrier = Arc::clone(&barrier);
            let data = new_data.clone();
            std::thread::spawn(move || {
                let mut session = begin_ingest(&plane, "text/plain");
                session.write(&data);
                barrier.wait();
                commit_ingest(session).unwrap()
            })
        })
        .collect();
    barrier.wait();
    let ids: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    for id in &ids {
        assert_eq!(read_object(&plane, *id).unwrap(), new_data);
    }
    let hashes: HashSet<_> = plane
        .get_manifest(ids[0])
        .unwrap()
        .unwrap()
        .into_iter()
        .collect();
    let added: u64 = hashes
        .iter()
        .map(|h| plane.read_chunk_raw(h).unwrap().len() as u64)
        .sum();
    assert_eq!(physical_bytes(&path), compressed.len() as u64 + 16 + added);
}

#[test]
fn simultaneous_cold_workspace_publications_append_each_chunk_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plane.redb");
    let plane = Arc::new(ArtifactPlane::open(&path).unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let data = vec![0xAA; MAX_CHUNK_BYTES * 4];
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let plane = Arc::clone(&plane);
            let barrier = Arc::clone(&barrier);
            let data = data.clone();
            std::thread::spawn(move || {
                barrier.wait();
                publish(&plane, &data)
            })
        })
        .collect();
    barrier.wait();
    let contents: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_ne!(contents[0].object_id, contents[1].object_id);
    assert_eq!(contents[0].sha256, contents[1].sha256);
    let hashes: HashSet<_> = plane
        .get_manifest(contents[0].object_id)
        .unwrap()
        .unwrap()
        .into_iter()
        .collect();
    let bytes: u64 = hashes
        .iter()
        .map(|h| plane.read_chunk_raw(h).unwrap().len() as u64)
        .sum();
    assert_eq!(physical_bytes(&path), bytes + 16);
    for content in contents {
        assert_bytes(&plane, content, &data);
    }
}

#[test]
fn eventual_plane_workspace_roots_and_publications_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plane.redb");
    let content;
    let replacement;
    {
        let plane = ArtifactPlane::open_with_durability(&path, DurabilityLevel::Eventual).unwrap();
        content = publish(&plane, b"durable workspace");
        plane.retain_workspace("first", content).unwrap();
        plane.retain_workspace("second", content).unwrap();
        replacement = publish(&plane, b"replacement");
        plane.retain_workspace("first", replacement).unwrap();
        plane.release_workspace("first").unwrap();
    }
    let plane = ArtifactPlane::open_with_durability(&path, DurabilityLevel::Eventual).unwrap();
    assert!(plane.get_object(replacement.object_id).unwrap().is_none());
    release_object(&plane, content.object_id).unwrap();
    gc_chunks(&plane).unwrap();
    gc_trash(&plane, 0).unwrap();
    assert_bytes(&plane, content, b"durable workspace");
    plane.release_workspace("missing").unwrap();
    plane.release_workspace("second").unwrap();
    assert!(plane.get_object(content.object_id).unwrap().is_none());
}

#[test]
fn roots_protect_objects_and_reacquired_trash_is_not_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(dir.path().join("plane.redb")).unwrap();
    let original = publish(&plane, b"resurrect me");
    let hashes = plane.get_manifest(original.object_id).unwrap().unwrap();
    plane.retain_workspace("one", original).unwrap();
    plane.retain_workspace("two", original).unwrap();
    plane.release_workspace("one").unwrap();
    release_object(&plane, original.object_id).unwrap();
    assert!(plane.get_object(original.object_id).unwrap().is_some());
    plane.release_workspace("two").unwrap();
    gc_chunks(&plane).unwrap();
    for hash in &hashes {
        assert!(plane.set_trash_timestamp(hash, 0).unwrap());
    }
    let resurrected = publish(&plane, b"resurrect me");
    for hash in &hashes {
        assert!(plane.get_trash_timestamp(hash).unwrap().is_none());
    }
    plane.retain_workspace("resurrected", resurrected).unwrap();
    gc_trash(&plane, 0).unwrap();
    assert_bytes(&plane, resurrected, b"resurrect me");
    plane.release_workspace("resurrected").unwrap();
    gc_chunks(&plane).unwrap();
    assert!(gc_trash(&plane, 0).unwrap().freed_from_trash > 0);
    assert!(plane.read_workspace_range(resurrected, 0, 1).is_err());
}

#[test]
fn legacy_manifests_keep_their_chunk_profile_and_cannot_be_forged_as_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(dir.path().join("plane.redb")).unwrap();
    let data: Vec<u8> = (0..700_000).map(|i| (i % 251) as u8).collect();
    let mut session = begin_ingest(&plane, "text/plain");
    session.write(&data);
    let id = commit_ingest(session).unwrap();
    let expected: Vec<_> = chunk_data(&data).map(|c| c.hash).collect();
    assert_eq!(plane.get_manifest(id).unwrap().unwrap(), expected);
    let meta = plane.get_object(id).unwrap().unwrap();
    let legacy = WorkspaceContentRef {
        object_id: id,
        size: meta.size,
        sha256: meta.sha256,
    };
    assert!(plane.read_workspace_range(legacy, 0, 1).is_err());
    assert!(plane
        .publish_workspace(Some(legacy), legacy.size, &[])
        .is_err());
    assert!(plane.retain_workspace("legacy", legacy).is_err());
    assert_eq!(read_object(&plane, id).unwrap(), data);
}

#[test]
fn duplicate_concurrent_releases_do_not_decrement_another_objects_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let plane = Arc::new(ArtifactPlane::open(dir.path().join("plane.redb")).unwrap());
    let released = publish(&plane, b"shared");
    let surviving = publish(&plane, b"shared");
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let plane = Arc::clone(&plane);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                release_object(&plane, released.object_id).unwrap();
            })
        })
        .collect();
    barrier.wait();
    for thread in threads {
        thread.join().unwrap();
    }
    gc_chunks(&plane).unwrap();
    gc_trash(&plane, 0).unwrap();
    let hash = plane.get_manifest(surviving.object_id).unwrap().unwrap()[0];
    assert_eq!(plane.get_chunk_refcount(&hash).unwrap(), 1);
    assert_bytes(&plane, surviving, b"shared");
}

#[test]
fn legacy_ingest_resurrects_shared_chunk_without_leaving_expired_trash() {
    let dir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(dir.path().join("plane.redb")).unwrap();
    let content = publish(&plane, b"legacy resurrection");
    let hash = plane.get_manifest(content.object_id).unwrap().unwrap()[0];
    release_object(&plane, content.object_id).unwrap();
    gc_chunks(&plane).unwrap();
    assert!(plane.set_trash_timestamp(&hash, 0).unwrap());
    let mut session = begin_ingest(&plane, "text/plain");
    session.write(b"legacy resurrection");
    let id = commit_ingest(session).unwrap();
    assert!(plane.get_trash_timestamp(&hash).unwrap().is_none());
    gc_trash(&plane, 0).unwrap();
    assert_eq!(read_object(&plane, id).unwrap(), b"legacy resurrection");
}
