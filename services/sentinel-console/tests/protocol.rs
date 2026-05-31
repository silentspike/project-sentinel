//! #439 protocol integration test: the server's manifest→delta exchange over an in-memory duplex
//! (no QUIC needed — the wire protocol logic is the unit under test; the live QUIC roundtrip is the
//! VM smoke). Verifies the client gets only the blocks it is missing.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use sentinel_console::{read_frame, serve_protocol, write_frame, SharedPlane};
use sentinel_console_plane::{BlockHash, ConsolePlane, Delta, HelloManifest};

fn varied(len: usize, salt: u32) -> Vec<u8> {
    (0..len as u32)
        .map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(salt) >> 13) as u8)
        .collect()
}

fn seeded_plane() -> (SharedPlane, Vec<BlockHash>) {
    let mut plane = ConsolePlane::new();
    for i in 0..10u32 {
        let mut msg = varied(256 * 1024, 0); // shared system block
        msg.extend(varied(4096, i + 1)); // unique tail
        plane.ingest(&msg);
    }
    let manifest = plane.server_manifest();
    (Arc::new(Mutex::new(plane)), manifest)
}

#[tokio::test]
async fn empty_manifest_gets_all_blocks() {
    let (plane, full_manifest) = seeded_plane();
    let (client, server) = tokio::io::duplex(16 * 1024 * 1024);
    let (mut client_rx, mut client_tx) = tokio::io::split(client);
    let (server_rx, server_tx) = tokio::io::split(server);

    let pl = plane.clone();
    let srv = tokio::spawn(async move { serve_protocol(server_rx, server_tx, &pl).await });

    // Client knows nothing → expects every block.
    let hello = serde_json::to_vec(&HelloManifest { have: vec![] }).unwrap();
    write_frame(&mut client_tx, &hello).await.unwrap();
    let delta_bytes = read_frame(&mut client_rx).await.unwrap();
    let delta: Delta = serde_json::from_slice(&delta_bytes).unwrap();

    assert_eq!(
        delta.missing.len(),
        full_manifest.len(),
        "empty manifest must yield all server blocks"
    );
    assert!(delta.transfer_bytes() > 0);
    srv.await.unwrap().unwrap();
}

#[tokio::test]
async fn partial_manifest_gets_only_missing_blocks() {
    // Real DEV-009 append/push scenario: the client is caught up through the first nine
    // entries (it already holds the recurring 256 KiB system block plus their tails); only
    // the newest entry's brand-new unique tail is missing, so the server transfers just that
    // small delta. We build `client_has` from the actual ingest hashes — NOT from a slice of
    // `server_manifest()`, whose order is non-deterministic (HashMap iteration). The previous
    // version took a count-based 90% slice over that random order, so whether a large shared
    // block landed in the "missing 10%" was random → the byte-size assertion was flaky.
    let mut plane = ConsolePlane::new();
    let mut client_has_vec: Vec<BlockHash> = Vec::new();
    for i in 0..9u32 {
        let mut msg = varied(256 * 1024, 0); // shared system block (recurs in every entry)
        msg.extend(varied(4096, i + 1)); // per-entry unique tail
        client_has_vec.extend(plane.ingest(&msg)); // client is already caught up on these
    }
    // The newest entry: same recurring system block (already known) + a brand-new unique tail.
    let mut newest = varied(256 * 1024, 0);
    newest.extend(varied(4096, 999));
    let newest_hashes = plane.ingest(&newest);

    let client_has: HashSet<BlockHash> = client_has_vec.into_iter().collect();
    // Missing = exactly the newest entry's blocks the client does not already have.
    let expected_missing: HashSet<BlockHash> = {
        let mut seen = HashSet::new();
        newest_hashes
            .iter()
            .copied()
            .filter(|h| !client_has.contains(h) && seen.insert(*h))
            .collect()
    };
    assert!(
        !expected_missing.is_empty(),
        "the new entry must contribute at least one previously-unseen block"
    );

    let full_bytes: u64 = plane
        .server_manifest()
        .iter()
        .map(|h| plane.block(h).map(|b| b.len() as u64).unwrap_or(0))
        .sum();

    let plane = Arc::new(Mutex::new(plane));
    let (client, server) = tokio::io::duplex(16 * 1024 * 1024);
    let (mut client_rx, mut client_tx) = tokio::io::split(client);
    let (server_rx, server_tx) = tokio::io::split(server);
    let pl = plane.clone();
    let srv = tokio::spawn(async move { serve_protocol(server_rx, server_tx, &pl).await });

    let hello = serde_json::to_vec(&HelloManifest {
        have: client_has.iter().copied().collect(),
    })
    .unwrap();
    write_frame(&mut client_tx, &hello).await.unwrap();
    let delta_bytes = read_frame(&mut client_rx).await.unwrap();
    let delta: Delta = serde_json::from_slice(&delta_bytes).unwrap();

    let missing: HashSet<BlockHash> = delta.missing.iter().copied().collect();
    assert_eq!(
        missing, expected_missing,
        "delta carries exactly the newest entry's previously-unseen blocks"
    );
    assert!(
        delta.missing.iter().all(|h| !client_has.contains(h)),
        "delta never re-sends a block the client already has"
    );
    // The recurring 256 KiB system block is already known → only the small new tail transfers,
    // so the delta is far smaller than the full state. Deterministic by construction.
    assert!(
        delta.transfer_bytes() * 3 < full_bytes,
        "delta {} must be much smaller than full state {} (only the new tail transfers)",
        delta.transfer_bytes(),
        full_bytes
    );
    srv.await.unwrap().unwrap();
}
