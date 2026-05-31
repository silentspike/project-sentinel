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
    let (plane, full_manifest) = seeded_plane();
    // Client already has 90% of the blocks.
    let keep = full_manifest.len() * 9 / 10;
    let client_has: Vec<BlockHash> = full_manifest.iter().copied().take(keep).collect();
    let expected_missing = full_manifest.len() - keep;

    let full_bytes: u64 = {
        let g = plane.lock().unwrap();
        full_manifest
            .iter()
            .map(|h| g.block(h).map(|b| b.len() as u64).unwrap_or(0))
            .sum()
    };

    let (client, server) = tokio::io::duplex(16 * 1024 * 1024);
    let (mut client_rx, mut client_tx) = tokio::io::split(client);
    let (server_rx, server_tx) = tokio::io::split(server);
    let pl = plane.clone();
    let srv = tokio::spawn(async move { serve_protocol(server_rx, server_tx, &pl).await });

    let hello = serde_json::to_vec(&HelloManifest { have: client_has.clone() }).unwrap();
    write_frame(&mut client_tx, &hello).await.unwrap();
    let delta_bytes = read_frame(&mut client_rx).await.unwrap();
    let delta: Delta = serde_json::from_slice(&delta_bytes).unwrap();

    assert_eq!(delta.missing.len(), expected_missing, "only missing ~10% of blocks");
    let known: HashSet<BlockHash> = client_has.into_iter().collect();
    assert!(
        delta.missing.iter().all(|h| !known.contains(h)),
        "delta never re-sends a block the client already has"
    );
    // Transfer is far smaller than the full state.
    assert!(
        delta.transfer_bytes() * 3 < full_bytes,
        "delta {} must be much smaller than full state {}",
        delta.transfer_bytes(),
        full_bytes
    );
    srv.await.unwrap().unwrap();
}
