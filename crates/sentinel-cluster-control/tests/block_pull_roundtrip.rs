//! In-process QUIC block-pull integration test (#498 4b): a real quinn server + client
//! over loopback, exercising pull-by-hash, a miss, and mutual cert-pinning (V10). Proves
//! the byte-path transport without a VM; the cross-host 2-VM ACs run after deploy.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sentinel_cluster_control::{
    BlockProvider, BlockPullClient, BlockPullServer, NodeCertificate, PeerRegistry,
};
use sentinel_common::{BlockRef, NodeId};

fn loopback() -> std::net::SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// A provider holding one block, by its on-disk encoded bytes.
struct OneBlock {
    want: BlockRef,
    encoded: Vec<u8>,
}
impl BlockProvider for OneBlock {
    fn encoded_blob(&self, block_ref: &BlockRef) -> Option<Vec<u8>> {
        (block_ref == &self.want).then(|| self.encoded.clone())
    }
}

#[tokio::test]
async fn pull_by_hash_roundtrip_miss_and_server_pin() {
    let server_node = NodeCertificate::generate("holder").unwrap();
    let client_node = NodeCertificate::generate("puller").unwrap();
    let server_fp = server_node.fingerprint();
    let client_fp = client_node.fingerprint();

    // The server holds exactly one block (encoded form: 0x00 raw-prefix + content).
    let held = BlockRef::blob_sha256([42; 32], 5);
    let encoded = vec![0x00u8, b'h', b'e', b'l', b'l', b'o'];
    let provider = Arc::new(OneBlock {
        want: held.clone(),
        encoded: encoded.clone(),
    });

    // The server pins the client (V10).
    let peers = PeerRegistry::new([(client_fp, NodeId::new())]).unwrap();
    let server = BlockPullServer::bind(loopback(), &server_node, peers, provider).unwrap();
    let addr = server.local_addr();
    let client = BlockPullClient::new(&client_node).unwrap();

    // AC-2 (transport): pull the held block by hash -> the encoded bytes come back.
    let got = client.pull(addr, server_fp, &held).await.unwrap();
    assert_eq!(got, Some(encoded), "pull-by-hash returns the encoded blob");

    // A miss: a ref the server does not hold -> typed None (no path leak).
    let missing = BlockRef::blob_sha256([7; 32], 5);
    assert_eq!(
        client.pull(addr, server_fp, &missing).await.unwrap(),
        None,
        "an unheld ref is a clean miss"
    );

    // V10: the client rejects a server whose fingerprint does not match the pin.
    let wrong = NodeCertificate::generate("evil").unwrap().fingerprint();
    assert!(
        client.pull(addr, wrong, &held).await.is_err(),
        "wrong server pin must be rejected"
    );

    server.close();
}

#[tokio::test]
async fn server_rejects_an_unpinned_client() {
    let server_node = NodeCertificate::generate("holder").unwrap();
    let server_fp = server_node.fingerprint();
    let stranger = NodeCertificate::generate("stranger").unwrap();

    let held = BlockRef::blob_sha256([1; 32], 3);
    let provider = Arc::new(OneBlock {
        want: held.clone(),
        encoded: vec![0x00u8, b'a', b'b', b'c'],
    });

    // The server pins only some OTHER cert, never the stranger.
    let peers = PeerRegistry::new([(
        NodeCertificate::generate("allowed").unwrap().fingerprint(),
        NodeId::new(),
    )])
    .unwrap();
    let server = BlockPullServer::bind(loopback(), &server_node, peers, provider).unwrap();
    let client = BlockPullClient::new(&stranger).unwrap();

    // The handshake succeeds, but the server closes the connection on the unpinned
    // fingerprint, so the pull fails.
    assert!(
        client
            .pull(server.local_addr(), server_fp, &held)
            .await
            .is_err(),
        "server must reject an unpinned block-pull client"
    );

    server.close();
}

struct CountingBlock {
    want: BlockRef,
    encoded: Vec<u8>,
    calls: AtomicUsize,
}

impl BlockProvider for CountingBlock {
    fn encoded_blob(&self, block_ref: &BlockRef) -> Option<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (block_ref == &self.want).then(|| self.encoded.clone())
    }
}

#[tokio::test]
async fn revocation_closes_an_established_pull_session_and_blocks_reconnect() {
    let server_node = NodeCertificate::generate("holder").unwrap();
    let client_node = NodeCertificate::generate("puller").unwrap();
    let server_fp = server_node.fingerprint();
    let client_fp = client_node.fingerprint();
    let client_node_id = NodeId::new();
    let held = BlockRef::blob_sha256([9; 32], 3);
    let provider = Arc::new(CountingBlock {
        want: held.clone(),
        encoded: vec![0x00, b'a', b'b', b'c'],
        calls: AtomicUsize::new(0),
    });
    let peers = PeerRegistry::new([(client_fp, client_node_id)]).unwrap();
    let server = BlockPullServer::bind(
        loopback(),
        &server_node,
        peers.clone(),
        Arc::clone(&provider),
    )
    .unwrap();
    let client = BlockPullClient::new(&client_node).unwrap();
    let session = client
        .connect(server.local_addr(), server_fp)
        .await
        .unwrap();

    assert_eq!(
        session.pull(&held).await.unwrap(),
        Some(provider.encoded.clone())
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        peers.revoke(client_node_id),
        1,
        "the established pull connection must be registered for active close"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), session.pull(&held))
            .await
            .expect("revoked pull session must terminate promptly")
            .is_err(),
        "a revoked established session cannot pull another block"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "revocation must stop the request before provider access"
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.pull(server.local_addr(), server_fp, &held),
        )
        .await
        .expect("pull reconnect rejection must be prompt")
        .is_err(),
        "the revoked certificate cannot reconnect to block-pull"
    );

    peers.authorize(client_fp, client_node_id).unwrap();
    assert_eq!(
        client
            .pull(server.local_addr(), server_fp, &held)
            .await
            .unwrap(),
        Some(provider.encoded.clone())
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    server.close();
}
