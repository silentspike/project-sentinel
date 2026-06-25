//! In-process QUIC control-stream integration test (Phase 3a0): real quinn server +
//! client over loopback, exercising the RPC round-trip, idempotency dedup over the
//! wire, and mutual cert-pinning (V10). This proves the transport without a VM; the
//! cross-host 2-node ACs run after deploy.

use std::collections::HashSet;
use std::sync::Arc;

use sentinel_cluster_control::{
    ControlClient, ControlEnvelope, ControlRequest, ControlResponse, ControlServer,
    NodeCertificate, StubHandler,
};

fn loopback() -> std::net::SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

#[tokio::test]
async fn rpc_roundtrip_idempotency_and_server_pin() {
    let server_node = NodeCertificate::generate("node-0").unwrap();
    let client_node = NodeCertificate::generate("node-1").unwrap();
    let server_fp = server_node.fingerprint();
    let client_fp = client_node.fingerprint();

    // The server pins the client (V10).
    let mut pins = HashSet::new();
    pins.insert(client_fp);
    let server =
        ControlServer::bind(loopback(), &server_node, pins, Arc::new(StubHandler)).unwrap();
    let addr = server.local_addr();
    let client = ControlClient::new(&client_node).unwrap();

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

    // AC-2 (live): a re-send with the SAME idempotency_key returns the cached reply,
    // not the new request's — even though env2 carries a different request body.
    let env2 = ControlEnvelope::new(
        "k1",
        ControlRequest::PinQuery {
            block_ref: "something-else".into(),
        },
    );
    let reply2 = client.rpc(addr, server_fp, &env2).await.unwrap();
    assert_eq!(
        reply2.request_id, env.request_id,
        "idempotency: re-send returns the ORIGINAL cached reply"
    );
    assert!(
        matches!(reply2.response, ControlResponse::RefQueryResult { .. }),
        "cached reply, not the second request's PinQueryResult"
    );

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
async fn server_rejects_unpinned_client() {
    let server_node = NodeCertificate::generate("node-0").unwrap();
    let server_fp = server_node.fingerprint();
    let stranger = NodeCertificate::generate("stranger").unwrap();

    // The server pins only some OTHER cert, never the stranger.
    let mut pins = HashSet::new();
    pins.insert(NodeCertificate::generate("allowed").unwrap().fingerprint());
    let server =
        ControlServer::bind(loopback(), &server_node, pins, Arc::new(StubHandler)).unwrap();
    let client = ControlClient::new(&stranger).unwrap();

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
