//! Live negative probe for the QUIC certificate-to-NodeId membership boundary.
//!
//! The probe presents the configured node's existing control certificate while
//! claiming a different NodeId. Success means the peer returns the expected typed
//! rejection; accepting the heartbeat is a failing exit status.

use std::path::Path;

use anyhow::{bail, Context};
use sentinel_cluster_control::{
    ControlClient, ControlEnvelope, ControlRequest, ControlResponse, NodeCertificate,
};
use sentinel_common::{Heartbeat, NodeId};
use sentinel_daemon::config::DaemonConfig;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        bail!("usage: membership_spoof_probe <daemon.toml> <peer-alias> <claimed-node-id>");
    }

    let config = DaemonConfig::load(Path::new(&args[1]))?;
    let cluster = config
        .cluster
        .as_ref()
        .context("daemon config has no cluster section")?;
    let claimed_node_id =
        NodeId(Uuid::parse_str(&args[3]).context("claimed-node-id is not a UUID")?);
    if claimed_node_id == cluster.node_id {
        bail!("claimed-node-id must differ from the certificate owner's NodeId");
    }
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
    let client = ControlClient::new(&identity)?;
    let heartbeat = Heartbeat {
        node_id: claimed_node_id,
        alias: "spoof-probe".into(),
        boot_id: Uuid::new_v4(),
        incarnation: 0,
        endpoints: Vec::new(),
    };
    let envelope = ControlEnvelope::new(
        format!("membership-spoof-{}", Uuid::new_v4()),
        ControlRequest::MembershipHeartbeat {
            cluster_id: cluster.cluster_id,
            heartbeat,
        },
    );
    let peer_addr = peer.addr.parse().context("peer address is invalid")?;
    let peer_fingerprint =
        sentinel_cluster_control::CertFingerprint::from_hex(&peer.cert_fingerprint)
            .context("peer certificate fingerprint is invalid")?;
    let reply = client.rpc(peer_addr, peer_fingerprint, &envelope).await?;

    match reply.response {
        ControlResponse::Rejected { reason }
            if reason.contains("does not match authenticated peer") =>
        {
            println!(
                "SPOOF_REJECTED certificate_node={} claimed_node={} reason={}",
                cluster.node_id, claimed_node_id, reason
            );
            Ok(())
        }
        other => bail!("spoofed membership heartbeat was not rejected: {other:?}"),
    }
}
