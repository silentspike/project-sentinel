//! In-process QUIC control-stream integration test (Phase 3a0): real quinn server +
//! client over loopback, exercising the RPC round-trip, idempotency dedup over the
//! wire, and mutual cert-pinning (V10). This proves the transport without a VM; the
//! cross-host 2-node ACs run after deploy.

use std::sync::Arc;

use sentinel_cluster_control::{
    AuthenticatedPeer, ControlClient, ControlEnvelope, ControlHandler, ControlRequest,
    ControlResponse, ControlServer, NodeCertificate, PeerRegistry, StubHandler,
};

fn loopback() -> std::net::SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn client_for(
    node: &NodeCertificate,
    server_fp: sentinel_cluster_control::CertFingerprint,
) -> ControlClient {
    ControlClient::new(
        node,
        PeerRegistry::new([(server_fp, sentinel_common::NodeId::new())]).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn rpc_roundtrip_idempotency_and_server_pin() {
    let server_node = NodeCertificate::generate("node-0").unwrap();
    let client_node = NodeCertificate::generate("node-1").unwrap();
    let server_fp = server_node.fingerprint();
    let client_fp = client_node.fingerprint();

    // The server pins the client (V10).
    let peers = PeerRegistry::new([(client_fp, sentinel_common::NodeId::new())]).unwrap();
    let server =
        ControlServer::bind(loopback(), &server_node, peers, Arc::new(StubHandler)).unwrap();
    let addr = server.local_addr();
    let client = client_for(&client_node, server_fp);

    // AC-5: RefQuery round-trip returns a correct typed response.
    let env = ControlEnvelope::new(
        "k1",
        ControlRequest::RefQuery {
            block_ref: "cas-blob:v1:sha256:ab".into(),
        },
    );
    let reply = client.rpc(addr, server_fp, &env).await.unwrap();
    assert_eq!(reply.request_id, env.request_id);
    assert!(matches!(
        reply.response,
        ControlResponse::RefQueryResult {
            referenced: false,
            ..
        }
    ));

    // Reusing the same peer/method/key tuple for a different payload is rejected.
    let env2 = ControlEnvelope::new(
        "k1",
        ControlRequest::RefQuery {
            block_ref: "something-else".into(),
        },
    );
    let reply2 = client.rpc(addr, server_fp, &env2).await.unwrap();
    assert_eq!(reply2.request_id, env2.request_id);
    assert!(
        matches!(reply2.response, ControlResponse::IdempotencyConflict { .. }),
        "a reused key cannot alias a different payload"
    );

    // The method is part of the scope, so another method may use the same operator key.
    let env_method = ControlEnvelope::new(
        "k1",
        ControlRequest::PinQuery {
            block_ref: "something-else".into(),
        },
    );
    assert!(matches!(
        client
            .rpc(addr, server_fp, &env_method)
            .await
            .unwrap()
            .response,
        ControlResponse::PinQueryResult { .. }
    ));

    // AC-4: the client rejects a server whose fingerprint does not match the pin.
    let wrong = NodeCertificate::generate("evil").unwrap().fingerprint();
    let env3 = ControlEnvelope::new(
        "k2",
        ControlRequest::PinQuery {
            block_ref: "x".into(),
        },
    );
    assert!(
        client.rpc(addr, wrong, &env3).await.is_err(),
        "wrong server pin must be rejected"
    );

    server.close();
}

#[tokio::test]
async fn membership_heartbeats_bypass_response_cache() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sentinel_common::{Heartbeat, NodeId};
    use uuid::Uuid;

    struct CountingMembershipHandler {
        calls: AtomicUsize,
    }

    impl ControlHandler for CountingMembershipHandler {
        fn handle(&self, _peer: AuthenticatedPeer, request: &ControlRequest) -> ControlResponse {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match request {
                ControlRequest::MembershipHeartbeat { heartbeat, .. } => {
                    ControlResponse::MembershipAccepted {
                        node_id: heartbeat.node_id,
                        incarnation: heartbeat.incarnation,
                    }
                }
                _ => ControlResponse::Rejected {
                    reason: "unexpected request".into(),
                },
            }
        }
    }

    let server_node = NodeCertificate::generate("node-0").unwrap();
    let client_node = NodeCertificate::generate("node-1").unwrap();
    let server_fp = server_node.fingerprint();
    let node_id = NodeId::new();
    let peers = PeerRegistry::new([(client_node.fingerprint(), node_id)]).unwrap();
    let handler = Arc::new(CountingMembershipHandler {
        calls: AtomicUsize::new(0),
    });
    let server =
        ControlServer::bind(loopback(), &server_node, peers, Arc::clone(&handler)).unwrap();
    let client = client_for(&client_node, server_fp);
    let request = ControlRequest::MembershipHeartbeat {
        cluster_id: Uuid::new_v4(),
        heartbeat: Heartbeat {
            node_id,
            alias: "node-1".into(),
            boot_id: Uuid::new_v4(),
            incarnation: 1,
            endpoints: vec![],
        },
    };

    for _ in 0..2 {
        let envelope = ControlEnvelope::new("same-heartbeat-key", request.clone());
        let reply = client
            .rpc(server.local_addr(), server_fp, &envelope)
            .await
            .unwrap();
        assert_eq!(
            reply.response,
            ControlResponse::MembershipAccepted {
                node_id,
                incarnation: 1,
            }
        );
    }
    assert_eq!(
        handler.calls.load(Ordering::SeqCst),
        2,
        "every heartbeat arrival must refresh liveness instead of hitting dedup"
    );

    server.close();
}

#[tokio::test]
async fn holder_gossip_over_the_wire_merges_into_the_shared_block_map() {
    use std::sync::Mutex;

    use sentinel_cluster_control::BlockMapGossipHandler;
    use sentinel_common::{BlockMap, BlockRef, HolderAction, HolderAdvertisement, NodeId};
    use uuid::Uuid;

    let server_node = NodeCertificate::generate("node-0").unwrap();
    let client_node = NodeCertificate::generate("node-1").unwrap();
    let server_fp = server_node.fingerprint();
    let client_fp = client_node.fingerprint();

    let holder = NodeId::new();
    let peers = PeerRegistry::new([(client_fp, holder)]).unwrap();

    // node-0's server merges inbound holder gossip into this shared block map (#498).
    let block_map = Arc::new(Mutex::new(BlockMap::new()));
    let handler = Arc::new(BlockMapGossipHandler::new(
        Arc::clone(&block_map),
        StubHandler,
    ));
    let server = ControlServer::bind(loopback(), &server_node, peers, handler).unwrap();
    let addr = server.local_addr();
    let client = client_for(&client_node, server_fp);

    // node-1 advertises that it holds two blocks; the gossip crosses a real QUIC stream.
    let boot = Uuid::new_v4();
    let adv = |n: u8| HolderAdvertisement {
        block_ref: BlockRef::blob_sha256([n; 32], 1024),
        node_id: holder,
        node_boot_id: boot,
        node_incarnation: 1,
        node_cas_generation: 1,
        action: HolderAction::Add,
        expires_after: u64::MAX,
    };
    let env = ControlEnvelope::new(
        "gossip-1",
        ControlRequest::AdvertiseHolders {
            advertisements: vec![adv(1), adv(2)],
        },
    );
    let reply = client.rpc(addr, server_fp, &env).await.unwrap();
    assert_eq!(
        reply.response,
        ControlResponse::HoldersApplied { applied: 2 },
        "both advertisements applied on the receiver"
    );

    // AC-1 (live wire): node-0's block map now knows node-1 holds both blocks.
    let map = block_map.lock().unwrap();
    assert_eq!(map.block_count(), 2);
    assert_eq!(
        map.holders(&BlockRef::blob_sha256([1; 32], 1024)),
        vec![holder],
        "the block map resolves node-1 as the holder after gossip"
    );

    server.close();
}

#[tokio::test]
async fn server_rejects_unpinned_client() {
    let server_node = NodeCertificate::generate("node-0").unwrap();
    let server_fp = server_node.fingerprint();
    let stranger = NodeCertificate::generate("stranger").unwrap();

    // The server pins only some OTHER cert, never the stranger.
    let peers = PeerRegistry::new([(
        NodeCertificate::generate("allowed").unwrap().fingerprint(),
        sentinel_common::NodeId::new(),
    )])
    .unwrap();
    let server =
        ControlServer::bind(loopback(), &server_node, peers, Arc::new(StubHandler)).unwrap();
    let client = client_for(&stranger, server_fp);

    let env = ControlEnvelope::new(
        "k",
        ControlRequest::RefQuery {
            block_ref: "x".into(),
        },
    );
    // The TLS handshake succeeds (any cert), but the server closes the connection on
    // the unpinned fingerprint, so the RPC fails.
    assert!(
        client
            .rpc(server.local_addr(), server_fp, &env)
            .await
            .is_err(),
        "server must reject an unpinned client"
    );

    server.close();
}

#[tokio::test]
async fn revocation_closes_an_established_control_session_and_blocks_reconnect() {
    let server_node = NodeCertificate::generate("node-0").unwrap();
    let client_node = NodeCertificate::generate("node-1").unwrap();
    let server_fp = server_node.fingerprint();
    let client_fp = client_node.fingerprint();
    let client_node_id = sentinel_common::NodeId::new();
    let peers = PeerRegistry::new([(client_fp, client_node_id)]).unwrap();
    let server = ControlServer::bind(
        loopback(),
        &server_node,
        peers.clone(),
        Arc::new(StubHandler),
    )
    .unwrap();
    let client = client_for(&client_node, server_fp);
    let session = client
        .connect(server.local_addr(), server_fp)
        .await
        .unwrap();
    let request = || {
        ControlEnvelope::new(
            uuid::Uuid::new_v4().to_string(),
            ControlRequest::RefQuery {
                block_ref: "cas-blob:v1:sha256:revocation".into(),
            },
        )
    };

    assert!(matches!(
        session.rpc(&request()).await.unwrap().response,
        ControlResponse::RefQueryResult { .. }
    ));
    assert_eq!(
        peers.revoke(client_node_id),
        1,
        "the established control connection must be registered for active close"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), session.rpc(&request()))
            .await
            .expect("revoked session must terminate promptly")
            .is_err(),
        "a revoked established session cannot open another RPC stream"
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.rpc(server.local_addr(), server_fp, &request()),
        )
        .await
        .expect("reconnect rejection must be prompt")
        .is_err(),
        "the revoked certificate cannot reconnect"
    );

    peers.authorize(client_fp, client_node_id).unwrap();
    assert!(matches!(
        client
            .rpc(server.local_addr(), server_fp, &request())
            .await
            .unwrap()
            .response,
        ControlResponse::RefQueryResult { .. }
    ));
    server.close();
}

#[tokio::test]
async fn local_revocation_closes_outbound_control_session_and_blocks_reconnect() {
    let server_node = NodeCertificate::generate("node-0").unwrap();
    let client_node = NodeCertificate::generate("node-1").unwrap();
    let server_fp = server_node.fingerprint();
    let client_fp = client_node.fingerprint();
    let server_node_id = sentinel_common::NodeId::new();
    let client_node_id = sentinel_common::NodeId::new();
    let server_peers = PeerRegistry::new([(client_fp, client_node_id)]).unwrap();
    let client_peers = PeerRegistry::new([(server_fp, server_node_id)]).unwrap();
    let server = ControlServer::bind(
        loopback(),
        &server_node,
        server_peers,
        Arc::new(StubHandler),
    )
    .unwrap();
    let client = ControlClient::new(&client_node, client_peers.clone()).unwrap();
    let session = client
        .connect(server.local_addr(), server_fp)
        .await
        .unwrap();
    let request = || {
        ControlEnvelope::new(
            uuid::Uuid::new_v4().to_string(),
            ControlRequest::RefQuery {
                block_ref: "cas-blob:v1:sha256:outbound-revocation".into(),
            },
        )
    };

    assert!(session.rpc(&request()).await.is_ok());
    assert_eq!(
        client_peers.revoke(server_node_id),
        1,
        "the outbound control session must be registered on the initiating node"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), session.rpc(&request()))
            .await
            .expect("outbound revocation must terminate promptly")
            .is_err()
    );
    assert!(
        client
            .rpc(server.local_addr(), server_fp, &request())
            .await
            .is_err(),
        "the initiating node must reject reconnect while the peer is revoked"
    );

    client_peers.authorize(server_fp, server_node_id).unwrap();
    assert!(client
        .rpc(server.local_addr(), server_fp, &request())
        .await
        .is_ok());
    server.close();
}

#[tokio::test]
async fn remotely_closed_outbound_session_is_removed_from_the_local_registry() {
    let server_node = NodeCertificate::generate("node-0").unwrap();
    let client_node = NodeCertificate::generate("node-1").unwrap();
    let server_fp = server_node.fingerprint();
    let client_fp = client_node.fingerprint();
    let server_node_id = sentinel_common::NodeId::new();
    let client_node_id = sentinel_common::NodeId::new();
    let server_peers = PeerRegistry::new([(client_fp, client_node_id)]).unwrap();
    let client_peers = PeerRegistry::new([(server_fp, server_node_id)]).unwrap();
    let server = ControlServer::bind(
        loopback(),
        &server_node,
        server_peers.clone(),
        Arc::new(StubHandler),
    )
    .unwrap();
    let client = ControlClient::new(&client_node, client_peers.clone()).unwrap();
    let session = client
        .connect(server.local_addr(), server_fp)
        .await
        .unwrap();
    let request = ControlEnvelope::new(
        uuid::Uuid::new_v4().to_string(),
        ControlRequest::RefQuery {
            block_ref: "cas-blob:v1:sha256:remote-close-cleanup".into(),
        },
    );

    assert!(session.rpc(&request).await.is_ok());
    assert_eq!(server_peers.revoke(client_node_id), 1);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), session.rpc(&request))
            .await
            .expect("remote revocation must terminate promptly")
            .is_err()
    );
    tokio::task::yield_now().await;
    assert_eq!(
        client_peers.revoke(server_node_id),
        0,
        "a remotely closed session must not remain registered as live"
    );
    server.close();
}
