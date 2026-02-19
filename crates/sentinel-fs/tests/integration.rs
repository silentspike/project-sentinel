//! Integration tests for sentinel-fs: CAS + Metadata + Layer Manager.
//!
//! These tests verify cross-component behavior that unit tests can't cover.

use sentinel_fs::cas::CasStore;
use sentinel_fs::layer::LayerManager;
use sentinel_fs::metadata::{FileKind, MetadataStore};

fn setup() -> (LayerManager, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cas = CasStore::open(dir.path()).unwrap();
    let meta = MetadataStore::open(dir.path().join("meta.redb")).unwrap();
    let lm = LayerManager::new(cas, meta);
    lm.init_base_root().unwrap();
    (lm, dir)
}

// === AC-1: Deduplication Rate ===

#[test]
fn ac1_dedup_15_identical_trees() {
    let (lm, _dir) = setup();

    // Simulate 15 agents each with identical file trees
    let files = [
        ("package.json", br#"{"name":"app","version":"1.0.0"}"#.as_slice()),
        ("index.js", b"console.log('hello world');"),
        ("README.md", b"# My App\n\nA sample application."),
        ("config.toml", b"[server]\nport = 8080\nhost = '0.0.0.0'"),
        ("Makefile", b"all:\n\techo build\n\ntest:\n\techo test"),
    ];

    // Populate base layer with these files
    for (name, content) in &files {
        lm.populate_base_file(1, name, content, 0o644).unwrap();
    }

    // Each agent writes the same files (simulating npm install / copy)
    for agent_num in 1..=15 {
        let agent_id = format!("AGENT-{agent_num:02}");
        for (name, content) in &files {
            lm.write_file(&agent_id, 1, name, content, 0o644).unwrap();
        }
    }

    let stats = lm.cas().stats().unwrap();

    // 5 unique files × (base + 15 agents) = 80 writes, but only 5 unique blobs
    assert_eq!(
        stats.blob_count, 5,
        "Should have exactly 5 unique blobs, got {}",
        stats.blob_count
    );

    // Total raw size = 5 files × 16 agents = 80 copies
    let total_raw: u64 = files.iter().map(|(_, c)| c.len() as u64).sum::<u64>() * 16;
    let dedup_ratio = 1.0 - (stats.total_bytes_on_disk as f64 / total_raw as f64);
    assert!(
        dedup_ratio > 0.87,
        "Dedup ratio should be >87%, got {:.1}%",
        dedup_ratio * 100.0
    );
}

// === AC-3: Agent Isolation ===

#[test]
fn ac3_agent_01_writes_agent_02_cannot_see() {
    let (lm, _dir) = setup();

    // Agent-01 writes a private file
    let inode = lm
        .write_file("AGENT-01", 1, "secret.txt", b"top secret data", 0o600)
        .unwrap();

    // Agent-01 can read it
    let content = lm.read_file("AGENT-01", inode).unwrap();
    assert_eq!(content, b"top secret data");

    // Agent-02 cannot see the dirent
    assert!(
        lm.lookup_dirent("AGENT-02", 1, "secret.txt")
            .unwrap()
            .is_none(),
        "AGENT-02 should not see AGENT-01's file"
    );

    // Agent-02 cannot read the inode (doesn't exist in their layer)
    assert!(
        lm.lookup_inode("AGENT-02", inode).unwrap().is_none(),
        "AGENT-02 should not see AGENT-01's inode"
    );
}

#[test]
fn ac3_agent_isolation_readdir() {
    let (lm, _dir) = setup();

    lm.write_file("AGENT-01", 1, "a01.txt", b"a", 0o644).unwrap();
    lm.write_file("AGENT-02", 1, "a02.txt", b"b", 0o644).unwrap();

    let a01_entries = lm.readdir("AGENT-01", 1).unwrap();
    let a01_names: Vec<&str> = a01_entries.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(a01_names.contains(&"a01.txt"));
    assert!(!a01_names.contains(&"a02.txt"), "AGENT-01 must not see AGENT-02's file");

    let a02_entries = lm.readdir("AGENT-02", 1).unwrap();
    let a02_names: Vec<&str> = a02_entries.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(a02_names.contains(&"a02.txt"));
    assert!(!a02_names.contains(&"a01.txt"), "AGENT-02 must not see AGENT-01's file");
}

// === AC-4: Crash Recovery (redb ACID) ===

#[test]
fn ac4_crash_recovery_redb_acid() {
    let dir = tempfile::tempdir().unwrap();

    // Phase 1: Write data
    {
        let cas = CasStore::open(dir.path()).unwrap();
        let meta = MetadataStore::open(dir.path().join("meta.redb")).unwrap();
        let lm = LayerManager::new(cas, meta);
        lm.init_base_root().unwrap();

        lm.write_file("AGENT-01", 1, "persist.txt", b"persistent data", 0o644)
            .unwrap();
        lm.populate_base_file(1, "base.txt", b"base data", 0o644)
            .unwrap();
    }
    // LayerManager + MetadataStore dropped — simulates crash/restart

    // Phase 2: Reopen and verify data survived
    {
        let cas = CasStore::open(dir.path()).unwrap();
        let meta = MetadataStore::open(dir.path().join("meta.redb")).unwrap();
        let lm = LayerManager::new(cas, meta);

        let dirent = lm.lookup_dirent("AGENT-01", 1, "persist.txt").unwrap();
        assert!(dirent.is_some(), "Agent file should survive restart");

        let inode = dirent.unwrap();
        let content = lm.read_file("AGENT-01", inode).unwrap();
        assert_eq!(content, b"persistent data");

        let base_dirent = lm
            .lookup_dirent("AGENT-01", 1, "base.txt")
            .unwrap();
        assert!(base_dirent.is_some(), "Base file should survive restart");
    }
}

// === Copy-on-Write semantics ===

#[test]
fn cow_agent_override_does_not_modify_base() {
    let (lm, _dir) = setup();

    let base_inode = lm
        .populate_base_file(1, "config.txt", b"default config", 0o644)
        .unwrap();

    // Agent overrides the file
    let _agent_inode = lm
        .write_file("AGENT-01", 1, "config.txt", b"custom config", 0o644)
        .unwrap();

    // Agent sees their version
    let agent_dirent = lm.lookup_dirent("AGENT-01", 1, "config.txt").unwrap().unwrap();
    let agent_content = lm.read_file("AGENT-01", agent_dirent).unwrap();
    assert_eq!(agent_content, b"custom config");

    // Base layer still has original
    let base_content = lm.read_file("__BASE__", base_inode).unwrap();
    assert_eq!(base_content, b"default config");

    // Other agent sees base version
    let other_dirent = lm
        .lookup_dirent("AGENT-02", 1, "config.txt")
        .unwrap()
        .unwrap();
    let other_content = lm.read_file("AGENT-02", other_dirent).unwrap();
    assert_eq!(other_content, b"default config");
}

// === Whiteout + Readdir merge ===

#[test]
fn whiteout_and_readdir_complex() {
    let (lm, _dir) = setup();

    // Base: 4 files
    lm.populate_base_file(1, "keep1.txt", b"k1", 0o644).unwrap();
    lm.populate_base_file(1, "keep2.txt", b"k2", 0o644).unwrap();
    let del1_inode = lm.populate_base_file(1, "delete1.txt", b"d1", 0o644).unwrap();
    let del2_inode = lm.populate_base_file(1, "delete2.txt", b"d2", 0o644).unwrap();

    // Agent: delete 2, add 1
    lm.unlink("AGENT-01", 1, "delete1.txt", del1_inode).unwrap();
    lm.unlink("AGENT-01", 1, "delete2.txt", del2_inode).unwrap();
    lm.write_file("AGENT-01", 1, "new.txt", b"new", 0o644).unwrap();

    let entries = lm.readdir("AGENT-01", 1).unwrap();
    let names: Vec<&str> = entries.iter().map(|(n, _, _)| n.as_str()).collect();

    assert_eq!(names.len(), 3, "should have keep1, keep2, new");
    assert!(names.contains(&"keep1.txt"));
    assert!(names.contains(&"keep2.txt"));
    assert!(names.contains(&"new.txt"));
    assert!(!names.contains(&"delete1.txt"));
    assert!(!names.contains(&"delete2.txt"));
}

// === Nested directories ===

#[test]
fn nested_directory_structure() {
    let (lm, _dir) = setup();

    // Create: /root/src/main.rs
    let src_inode = lm.mkdir("AGENT-01", 1, "src", 0o755).unwrap();
    let main_inode = lm
        .write_file("AGENT-01", src_inode, "main.rs", b"fn main() {}", 0o644)
        .unwrap();

    // Verify lookup chain
    let found_src = lm.lookup_dirent("AGENT-01", 1, "src").unwrap().unwrap();
    assert_eq!(found_src, src_inode);

    let found_main = lm.lookup_dirent("AGENT-01", src_inode, "main.rs").unwrap().unwrap();
    assert_eq!(found_main, main_inode);

    let content = lm.read_file("AGENT-01", main_inode).unwrap();
    assert_eq!(content, b"fn main() {}");

    // Readdir of src/
    let entries = lm.readdir("AGENT-01", src_inode).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "main.rs");
    assert_eq!(entries[0].2, FileKind::Regular);
}

// === Refcount integrity ===

#[test]
fn refcount_integrity_across_operations() {
    let (lm, _dir) = setup();
    let content = b"shared content across operations";
    let hash = CasStore::hash(content);

    // Multiple agents reference same content
    lm.write_file("AGENT-01", 1, "f.txt", content, 0o644)
        .unwrap();
    lm.write_file("AGENT-02", 1, "f.txt", content, 0o644)
        .unwrap();
    lm.write_file("AGENT-03", 1, "f.txt", content, 0o644)
        .unwrap();
    assert_eq!(lm.meta().get_refcount(&hash).unwrap(), 3);

    // Remove one agent's file
    let a1_inode = lm.lookup_dirent("AGENT-01", 1, "f.txt").unwrap().unwrap();
    lm.meta()
        .remove_file("AGENT-01", 1, "f.txt", a1_inode)
        .unwrap();
    assert_eq!(lm.meta().get_refcount(&hash).unwrap(), 2);

    // Content still accessible by other agents
    let a2_inode = lm.lookup_dirent("AGENT-02", 1, "f.txt").unwrap().unwrap();
    let data = lm.read_file("AGENT-02", a2_inode).unwrap();
    assert_eq!(data, content);
}

// === Scale test: many agents ===

#[test]
fn scale_100_agents_with_shared_content() {
    let (lm, _dir) = setup();
    let content = b"identical package-lock content for all 100 agents";

    for i in 1..=100 {
        let agent = format!("AGENT-{i:04}");
        lm.write_file(&agent, 1, "lock.json", content, 0o644)
            .unwrap();
    }

    // Only 1 blob despite 100 writes
    let stats = lm.cas().stats().unwrap();
    assert_eq!(stats.blob_count, 1);

    // Refcount = 100
    let hash = CasStore::hash(content);
    assert_eq!(lm.meta().get_refcount(&hash).unwrap(), 100);
}
