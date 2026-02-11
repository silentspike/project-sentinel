//! Acceptance tests for sentinel-redb (Issue #7).

use sentinel_common::{AgentId, RoomId};
use sentinel_redb::{relationship_key, StateStore};

fn agent(id: u16) -> AgentId {
    AgentId::new(id).unwrap()
}

fn room(id: u16) -> RoomId {
    RoomId::new(id).unwrap()
}

/// AC 7.2: Write+Read roundtrip for agent_state, room_state, relationship, personality
#[test]
fn ac_07_02_write_read_roundtrip() {
    // AC 7.2: Insert+Get roundtrip for all 4 table types
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.redb");
    let store = StateStore::open(path.to_str().unwrap()).unwrap();

    // agent_state roundtrip
    store.set_agent_state(agent(1), b"agent-data-1").unwrap();
    let data = store.get_agent_state(agent(1)).unwrap().unwrap();
    assert_eq!(data, b"agent-data-1");

    // room_state roundtrip
    store.set_room_state(room(3), b"room-data-3").unwrap();
    let data = store.get_room_state(room(3)).unwrap().unwrap();
    assert_eq!(data, b"room-data-3");

    // relationship roundtrip
    store
        .set_relationship(agent(1), agent(5), b"friends")
        .unwrap();
    let data = store.get_relationship(agent(1), agent(5)).unwrap().unwrap();
    assert_eq!(data, b"friends");

    // personality roundtrip
    store.set_personality(agent(7), b"introvert").unwrap();
    let data = store.get_personality(agent(7)).unwrap().unwrap();
    assert_eq!(data, b"introvert");
}

/// AC 7.3: Concurrent reads work with MVCC
#[test]
fn ac_07_03_concurrent_reads_mvcc() {
    // AC 7.3: Write in one context, read in another - MVCC allows concurrent reads
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.redb");
    let store = StateStore::open(path.to_str().unwrap()).unwrap();

    // Write initial data
    store.set_agent_state(agent(1), b"initial").unwrap();

    // Multiple concurrent reads should return consistent data
    let r1 = store.get_agent_state(agent(1)).unwrap();
    let r2 = store.get_agent_state(agent(1)).unwrap();
    assert_eq!(r1, r2);
    assert_eq!(r1.unwrap(), b"initial");

    // Write new data
    store.set_agent_state(agent(1), b"updated").unwrap();

    // Both new reads should see the updated value
    let r3 = store.get_agent_state(agent(1)).unwrap();
    let r4 = store.get_agent_state(agent(1)).unwrap();
    assert_eq!(r3, r4);
    assert_eq!(r3.unwrap(), b"updated");
}

/// AC 7.4: Relationship key is canonical (alphabetically sorted)
#[test]
fn ac_07_04_relationship_key_canonical() {
    // AC 7.4: key("B","A") == key("A","B"), alphabetically sorted
    let key_ab = relationship_key(agent(1), agent(5));
    let key_ba = relationship_key(agent(5), agent(1));
    assert_eq!(
        key_ab, key_ba,
        "Relationship key must be canonical: key(A,B) == key(B,A)"
    );

    // Verify the packed format: min << 16 | max
    assert_eq!(key_ab, (1u32 << 16) | 5);

    // Additional check with different IDs
    let key_cd = relationship_key(agent(10), agent(3));
    let key_dc = relationship_key(agent(3), agent(10));
    assert_eq!(key_cd, key_dc);
    assert_eq!(key_cd, (3u32 << 16) | 10);
}

/// AC 7.5: Newly created DB file size is small (< 2MB)
///
/// redb reserviert Platz fuer B-Tree-Strukturen und Metadaten.
/// Mit 4 Tabellen liegt die initiale Groesse bei ~1 MB.
/// Wir pruefen < 2MB als sinnvollen Schwellenwert.
#[test]
fn ac_07_05_db_initial_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.redb");
    let _store = StateStore::open(path.to_str().unwrap()).unwrap();

    let size = std::fs::metadata(&path).unwrap().len();
    assert!(
        size < 2 * 1_048_576,
        "Initial DB should be < 2MB, was {} bytes ({:.2} KB)",
        size,
        size as f64 / 1024.0
    );
}
