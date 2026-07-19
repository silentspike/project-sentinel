//! Live negative probe for control-plane authorization and idempotency boundaries.
//!
//! The probe uses the configured node identity and sends non-destructive requests to
//! one pinned peer. Success means all three security boundaries reject as expected.

use std::path::Path;

use anyhow::{bail, Context};
use sentinel_cluster_control::{
    ControlClient, ControlEnvelope, ControlRequest, ControlResponse, NodeCertificate, PeerRegistry,
};
use sentinel_common::{BlockRef, HolderAction, HolderAdvertisement, NodeId};
use sentinel_daemon::config::DaemonConfig;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        bail!("usage: control_security_probe <daemon.toml> <peer-alias>");
    }

    let config = DaemonConfig::load(Path::new(&args[1]))?;
    let cluster = config
        .cluster
        .as_ref()
        .context("daemon config has no cluster section")?;
    let peer = cluster
        .control_peers
        .iter()
        .find(|peer| peer.alias == args[2])
        .with_context(|| format!("unknown control peer {}", args[2]))?;

    let cert_path = config.data_dir.join("control-node-cert.der");
    let key_path = config.data_dir.join("control-node-key.der");
    if !cert_path.is_file() || !key_path.is_file() {
        bail!("existing control identity is required; probe will not generate one");
    }
    let local_alias = cluster
        .alias
        .clone()
        .unwrap_or_else(|| cluster.node_id.to_string());
    let identity = NodeCertificate::load_or_generate(&cert_path, &key_path, &local_alias)?;
    let peer_addr = peer.addr.parse().context("peer address is invalid")?;
    let peer_fingerprint =
        sentinel_cluster_control::CertFingerprint::from_hex(&peer.cert_fingerprint)
            .context("peer certificate fingerprint is invalid")?;
    let client = ControlClient::new(
        &identity,
        PeerRegistry::new([(peer_fingerprint, peer.node_id)])?,
    )?;

    let conflict_key = format!("security-idempotency-{}", Uuid::new_v4());
    let first = ControlEnvelope::new(
        &conflict_key,
        ControlRequest::RefQuery {
            block_ref: "security-probe:block-a".into(),
        },
    );
    let first_reply = client.rpc(peer_addr, peer_fingerprint, &first).await?;
    if !matches!(first_reply.response, ControlResponse::RefQueryResult { .. }) {
        bail!(
            "initial read-only request failed: {:?}",
            first_reply.response
        );
    }
    let conflicting = ControlEnvelope::new(
        &conflict_key,
        ControlRequest::RefQuery {
            block_ref: "security-probe:block-b".into(),
        },
    );
    let conflict_reply = client
        .rpc(peer_addr, peer_fingerprint, &conflicting)
        .await?;
    match conflict_reply.response {
        ControlResponse::IdempotencyConflict {
            method,
            idempotency_key,
        } if method == "ref_query" && idempotency_key == conflict_key => {
            println!("IDEMPOTENCY_CONFLICT_REJECTED method={method}");
        }
        other => bail!("digest-conflicting request was not rejected: {other:?}"),
    }

    let owner = ControlEnvelope::new(
        format!("security-owner-{}", Uuid::new_v4()),
        ControlRequest::OwnerCommit {
            scope: format!("security-probe:foreign-chef:{}", Uuid::new_v4()),
            owner_node: cluster.node_id.to_string(),
            epoch: 1,
        },
    );
    let owner_reply = client.rpc(peer_addr, peer_fingerprint, &owner).await?;
    match owner_reply.response {
        ControlResponse::Rejected { reason }
            if reason.contains("requires the configured chef node") =>
        {
            println!("NON_CHEF_OWNER_MUTATION_REJECTED reason={reason}");
        }
        other => bail!("non-chef owner mutation was not rejected: {other:?}"),
    }

    let claimed_node = NodeId::new();
    let gossip = ControlEnvelope::new(
        format!("security-holder-{}", Uuid::new_v4()),
        ControlRequest::AdvertiseHolders {
            advertisements: vec![HolderAdvertisement {
                block_ref: BlockRef::blob_sha256([0x44; 32], 1),
                node_id: claimed_node,
                node_boot_id: Uuid::new_v4(),
                node_incarnation: 1,
                node_cas_generation: 1,
                action: HolderAction::Add,
                expires_after: u64::MAX,
            }],
        },
    );
    let gossip_reply = client.rpc(peer_addr, peer_fingerprint, &gossip).await?;
    match gossip_reply.response {
        ControlResponse::Rejected { reason }
            if reason.contains("must match authenticated peer") =>
        {
            println!(
                "FOREIGN_HOLDER_NODE_REJECTED certificate_node={} claimed_node={} reason={}",
                cluster.node_id, claimed_node, reason
            );
        }
        other => bail!("foreign holder advertisement was not rejected: {other:?}"),
    }

    Ok(())
}
