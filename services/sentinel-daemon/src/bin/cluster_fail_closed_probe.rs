use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use sentinel_cluster_control::{
    BlockProvider, BlockPullClient, BlockPullServer, ChefAuthorizingHandler, ControlClient,
    ControlEnvelope, ControlRequest, ControlResponse, ControlServer, FailClosedHandler,
    NodeCertificate, PeerRegistry,
};
use sentinel_common::{BlockRef, Heartbeat, NodeId};
use sentinel_daemon::cluster_membership::{MembershipRuntime, QuicMembershipHandler};
use uuid::Uuid;

struct OneBlock {
    block_ref: BlockRef,
    encoded: Vec<u8>,
}

impl BlockProvider for OneBlock {
    fn encoded_blob(&self, block_ref: &BlockRef) -> Option<Vec<u8>> {
        (block_ref == &self.block_ref).then(|| self.encoded.clone())
    }
}

fn heartbeat(cluster_id: Uuid, node_id: NodeId, incarnation: u64) -> ControlEnvelope {
    ControlEnvelope::new(
        format!("probe-heartbeat-{incarnation}"),
        ControlRequest::MembershipHeartbeat {
            cluster_id,
            heartbeat: Heartbeat {
                node_id,
                alias: "fail-closed-probe".into(),
                boot_id: Uuid::nil(),
                incarnation,
                endpoints: Vec::new(),
            },
        },
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cluster_id = Uuid::new_v4();
    let server_node_id = NodeId::new();
    let client_node_id = NodeId::new();
    let server_identity = NodeCertificate::generate("probe-server")?;
    let client_identity = NodeCertificate::generate("probe-client")?;
    let server_fingerprint = server_identity.fingerprint();
    let client_fingerprint = client_identity.fingerprint();
    let peers = PeerRegistry::new([(client_fingerprint, client_node_id)])?;
    let client_peers = PeerRegistry::new([(server_fingerprint, server_node_id)])?;
    let membership = Arc::new(MembershipRuntime::new(Default::default()));
    let handler = QuicMembershipHandler::new(
        cluster_id,
        server_node_id,
        Arc::clone(&membership),
        ChefAuthorizingHandler::new(
            Some(client_node_id),
            FailClosedHandler::new("cluster metastore unavailable"),
        ),
    );
    let control_server = ControlServer::bind(
        "127.0.0.1:0".parse()?,
        &server_identity,
        peers.clone(),
        Arc::new(handler),
    )?;
    let block_ref = BlockRef::blob_sha256([42; 32], 5);
    let encoded = vec![0x00, b'h', b'e', b'l', b'l', b'o'];
    let pull_server = BlockPullServer::bind(
        "127.0.0.1:0".parse()?,
        &server_identity,
        peers.clone(),
        Arc::new(OneBlock {
            block_ref: block_ref.clone(),
            encoded: encoded.clone(),
        }),
    )?;
    let control_client = ControlClient::new(&client_identity, client_peers.clone())?;
    let pull_client = BlockPullClient::new(&client_identity, client_peers.clone())?;
    let control_session = control_client
        .connect(control_server.local_addr(), server_fingerprint)
        .await?;
    let pull_session = pull_client
        .connect(pull_server.local_addr(), server_fingerprint)
        .await?;

    let membership_reply = control_session
        .rpc(&heartbeat(cluster_id, client_node_id, 1))
        .await?;
    ensure!(
        matches!(
            membership_reply.response,
            ControlResponse::MembershipAccepted { .. }
        ),
        "membership did not remain available"
    );
    println!("MEMBERSHIP_AVAILABLE response=MembershipAccepted");

    let owner_reply = control_session
        .rpc(&ControlEnvelope::new(
            "probe-owner-commit",
            ControlRequest::OwnerCommit {
                scope: "world".into(),
                owner_node: client_node_id.to_string(),
                epoch: 2,
            },
        ))
        .await?;
    ensure!(
        owner_reply.response
            == (ControlResponse::Rejected {
                reason: "cluster metastore unavailable".into(),
            }),
        "owner mutation was not rejected: {:?}",
        owner_reply.response
    );
    println!("METASTORE_FAIL_CLOSED response=Rejected reason=cluster_metastore_unavailable");

    ensure!(
        pull_session.pull(&block_ref).await? == Some(encoded.clone()),
        "initial block pull failed"
    );
    let closed = peers.revoke(client_node_id);
    ensure!(
        closed == 2,
        "expected two live sessions to close, got {closed}"
    );
    println!("LIVE_SESSIONS_REVOKED count={closed}");

    ensure!(
        tokio::time::timeout(
            Duration::from_secs(2),
            control_session.rpc(&heartbeat(cluster_id, client_node_id, 2)),
        )
        .await
        .context("revoked control session did not terminate")?
        .is_err(),
        "revoked control session opened another stream"
    );
    ensure!(
        tokio::time::timeout(Duration::from_secs(2), pull_session.pull(&block_ref))
            .await
            .context("revoked pull session did not terminate")?
            .is_err(),
        "revoked pull session opened another stream"
    );
    ensure!(
        tokio::time::timeout(
            Duration::from_secs(2),
            control_client.rpc(
                control_server.local_addr(),
                server_fingerprint,
                &heartbeat(cluster_id, client_node_id, 3),
            ),
        )
        .await
        .context("revoked reconnect did not terminate")?
        .is_err(),
        "revoked certificate reconnected"
    );
    println!("POST_REVOKE_DENIED control=true block_pull=true reconnect=true");

    peers.authorize(client_fingerprint, client_node_id)?;
    let recovered = control_client
        .rpc(
            control_server.local_addr(),
            server_fingerprint,
            &heartbeat(cluster_id, client_node_id, 4),
        )
        .await?;
    ensure!(
        matches!(
            recovered.response,
            ControlResponse::MembershipAccepted { .. }
        ),
        "control did not recover after explicit re-authorization"
    );
    ensure!(
        pull_client
            .pull(pull_server.local_addr(), server_fingerprint, &block_ref)
            .await?
            == Some(encoded.clone()),
        "block pull did not recover after explicit re-authorization"
    );
    println!("EXPLICIT_REAUTH_RECOVERED control=true block_pull=true");

    let outbound_control_session = control_client
        .connect(control_server.local_addr(), server_fingerprint)
        .await?;
    let outbound_pull_session = pull_client
        .connect(pull_server.local_addr(), server_fingerprint)
        .await?;
    let outbound_closed = client_peers.revoke(server_node_id);
    ensure!(
        outbound_closed == 2,
        "expected two outbound sessions to close, got {outbound_closed}"
    );
    println!("OUTBOUND_SESSIONS_REVOKED count={outbound_closed}");

    ensure!(
        tokio::time::timeout(
            Duration::from_secs(2),
            outbound_control_session.rpc(&heartbeat(cluster_id, client_node_id, 5)),
        )
        .await
        .context("revoked outbound control session did not terminate")?
        .is_err(),
        "revoked outbound control session opened another stream"
    );
    ensure!(
        tokio::time::timeout(
            Duration::from_secs(2),
            outbound_pull_session.pull(&block_ref),
        )
        .await
        .context("revoked outbound pull session did not terminate")?
        .is_err(),
        "revoked outbound pull session opened another stream"
    );
    ensure!(
        control_client
            .rpc(
                control_server.local_addr(),
                server_fingerprint,
                &heartbeat(cluster_id, client_node_id, 6),
            )
            .await
            .is_err(),
        "locally revoked server reconnected"
    );
    println!("OUTBOUND_POST_REVOKE_DENIED control=true block_pull=true reconnect=true");

    client_peers.authorize(server_fingerprint, server_node_id)?;
    let outbound_recovered = control_client
        .rpc(
            control_server.local_addr(),
            server_fingerprint,
            &heartbeat(cluster_id, client_node_id, 7),
        )
        .await?;
    ensure!(
        matches!(
            outbound_recovered.response,
            ControlResponse::MembershipAccepted { .. }
        ),
        "outbound control did not recover after explicit re-authorization"
    );
    ensure!(
        pull_client
            .pull(pull_server.local_addr(), server_fingerprint, &block_ref)
            .await?
            == Some(encoded),
        "outbound block pull did not recover after explicit re-authorization"
    );
    println!("OUTBOUND_REAUTH_RECOVERED control=true block_pull=true");

    control_server.close();
    pull_server.close();
    Ok(())
}
