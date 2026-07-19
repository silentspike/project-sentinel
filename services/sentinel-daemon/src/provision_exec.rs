//! ProvisionNode execution (#495, G3) — the seed-side saga driver that absorbs a
//! bare VM shell into a cluster node.
//!
//! The seed drives a recoverable [`ProvisionOp`] saga
//! (`sentinel_common::provision`) over an injected [`ProvisionTransport`] (the
//! SSH/scp seam): validate the target against the `PendingBareNode` allowlist
//! (V14), confirm the out-of-band host-key pin (AC-S1), push the sha256-verified
//! binary, render `daemon.toml` + the systemd unit + the #517 token-gate drop-ins,
//! create the target's private QUIC identity locally, render a reciprocal pinned-peer
//! configuration, start the daemon, and wait for an authenticated membership join.
//! The saga/decision/render logic is transport-agnostic and unit-tested; the real
//! [`SshProvisionTransport`] is a thin wrapper over `ssh`/`scp`.
//!
//! **Track-A bounded scope (documented, not a silent gap):**
//! - The SSH bootstrap establishes host trust. During `IssuingCert`, the verified
//!   target binary generates or loads its self-signed QUIC certificate and private
//!   key on the target. Only the public certificate fingerprint returns to the seed.
//!   The seed durably binds that fingerprint to the assigned [`NodeId`], while the
//!   target config pins the seed. Membership starts only on that reciprocal pinned
//!   graph; provisioning never falls back to unauthenticated LAN discovery.
//! - **No secrets / LLM tokens are copied** (AC-B7/AC-S3): the token-gate drop-ins
//!   keep gateway/judge/health boot-gated on `ConditionPathExists=/etc/sentinel/allow-llm`,
//!   which the seed never creates.
//! - `ProvisionOp` and its assigned NodeId are atomically persisted in the seed data
//!   directory before remote mutation. Retries reuse the same identity across restarts.
//!
//! The full bare-VM bootstrap remains a destructive cross-node acceptance drill.
//! Unit tests cover the complete transport sequence and fail-closed join behavior;
//! live correction evidence exercises target-local identity generation and the
//! authenticated membership boundary without reprovisioning an active node.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sentinel_cluster_control::CertFingerprint;
use sentinel_common::cluster::{ControlPeer, NodeId, PendingBareNode};
use sentinel_common::provision::ProvisionOp;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The repo-versioned systemd unit the seed installs on a provisioned node (G3:
/// reviewed source compiled in — no on-disk tampering window).
pub const SYSTEMD_UNIT: &str = include_str!("../../../deploy/systemd/sentinel-daemon.service");
/// The #517 token-gate drop-in installed for every LLM-touching unit so a fresh
/// node never autostarts gateway/judge on boot (no token bleed / OOM).
pub const TOKEN_GATE_DROPIN: &str =
    include_str!("../../../deploy/templates/token-gate-dropin.service.conf");
/// LLM-touching units that receive the token-gate drop-in on a provisioned node.
pub const TOKEN_GATE_UNITS: &[&str] = &[
    "sentinel-gateway.service",
    "sentinel-judge.service",
    "sentinel-health-monitor.timer",
];

const REMOTE_BIN: &str = "/opt/sentinel/bin/sentinel-daemon";
const REMOTE_CONFIG: &str = "/opt/sentinel/config/daemon.toml";
const REMOTE_UNIT: &str = "/etc/systemd/system/sentinel-daemon.service";
const REMOTE_CONTROL_CERT: &str = "/opt/sentinel/data/control-node-cert.der";
const REMOTE_CONTROL_KEY: &str = "/opt/sentinel/data/control-node-key.der";
const REMOTE_QUARANTINE: &str = "/opt/sentinel/data/provision-quarantine.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionReservation {
    Completed(ProvisionOp),
    Execute(ProvisionOp),
}

/// Seed-local durable journal. The NodeId is reserved and fsynced before any SSH
/// mutation, so a seed restart cannot mint a second identity for the same target.
pub struct ProvisionJournal {
    path: PathBuf,
    ops: Mutex<Vec<ProvisionOp>>,
}

impl ProvisionJournal {
    pub fn open(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        let ops: Vec<ProvisionOp> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| anyhow::anyhow!("parse {}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(anyhow::anyhow!("read {}: {error}", path.display())),
        };
        let mut keys = HashSet::new();
        let mut targets = HashSet::new();
        let mut nodes = HashSet::new();
        for op in &ops {
            if !keys.insert(op.idempotency_key.clone()) {
                anyhow::bail!("duplicate idempotency key in {}", path.display());
            }
            if !targets.insert(op.pending_target_id.clone()) {
                anyhow::bail!("duplicate pending target in {}", path.display());
            }
            let node_id = op.node_id.ok_or_else(|| {
                anyhow::anyhow!("provision op {} has no reserved NodeId", op.op_id)
            })?;
            if !nodes.insert(node_id) {
                anyhow::bail!("duplicate reserved NodeId in {}", path.display());
            }
        }
        Ok(Self {
            path,
            ops: Mutex::new(ops),
        })
    }

    pub fn reserve(
        &self,
        pending_target_id: &str,
        requested_alias: &str,
        idempotency_key: &str,
        now_ms: u64,
    ) -> anyhow::Result<ProvisionReservation> {
        let mut ops = self
            .ops
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = ops.clone();
        let existing = next.iter().position(|op| {
            op.idempotency_key == idempotency_key || op.pending_target_id == pending_target_id
        });
        if let Some(index) = existing {
            let op = &mut next[index];
            if op.idempotency_key != idempotency_key
                || op.pending_target_id != pending_target_id
                || op.requested_alias != requested_alias
            {
                anyhow::bail!(
                    "provision identity conflict: key/target/alias is already bound to op {}",
                    op.op_id
                );
            }
            if op.state == sentinel_common::provision::ProvisionOpState::Completed {
                return Ok(ProvisionReservation::Completed(op.clone()));
            }
            op.state = sentinel_common::provision::ProvisionOpState::VerifyingTarget;
            op.started_at_ms = now_ms;
            op.updated_at_ms = now_ms;
            op.failure_reason = None;
            let reserved = op.clone();
            self.persist_locked(&next)?;
            *ops = next;
            return Ok(ProvisionReservation::Execute(reserved));
        }

        let mut op = ProvisionOp::new(
            Uuid::new_v4(),
            pending_target_id.to_string(),
            requested_alias.to_string(),
            idempotency_key.to_string(),
            now_ms,
        );
        let reserved_nodes: HashSet<_> = next.iter().filter_map(|op| op.node_id).collect();
        let mut node_id = NodeId::new();
        while reserved_nodes.contains(&node_id) {
            node_id = NodeId::new();
        }
        op.assign_node(node_id, now_ms);
        next.push(op.clone());
        self.persist_locked(&next)?;
        *ops = next;
        Ok(ProvisionReservation::Execute(op))
    }

    pub fn lookup(
        &self,
        pending_target_id: &str,
        requested_alias: &str,
        idempotency_key: &str,
    ) -> anyhow::Result<Option<ProvisionOp>> {
        let ops = self
            .ops
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let existing = ops.iter().find(|op| {
            op.idempotency_key == idempotency_key || op.pending_target_id == pending_target_id
        });
        let Some(op) = existing else {
            return Ok(None);
        };
        if op.idempotency_key != idempotency_key
            || op.pending_target_id != pending_target_id
            || op.requested_alias != requested_alias
        {
            anyhow::bail!(
                "provision identity conflict: key/target/alias is already bound to op {}",
                op.op_id
            );
        }
        Ok(Some(op.clone()))
    }

    pub fn update(&self, op: &ProvisionOp) -> anyhow::Result<()> {
        let mut ops = self
            .ops
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = ops.clone();
        let stored = next
            .iter_mut()
            .find(|stored| stored.op_id == op.op_id)
            .ok_or_else(|| anyhow::anyhow!("provision op {} is not reserved", op.op_id))?;
        *stored = op.clone();
        self.persist_locked(&next)?;
        *ops = next;
        Ok(())
    }

    fn persist_locked(&self, ops: &[ProvisionOp]) -> anyhow::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(ops)?;
        let tmp = self.path.with_extension(format!("tmp-{}", Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        let result = (|| -> anyhow::Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &self.path)?;
            if let Some(parent) = self.path.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

/// Health-poll cadence for `AwaitingHealth` (injected so tests don't sleep).
#[derive(Debug, Clone, Copy)]
pub struct ProvisionTiming {
    pub health_poll_interval: Duration,
    pub health_poll_max: u32,
    pub join_poll_interval: Duration,
    pub join_poll_max: u32,
}

impl Default for ProvisionTiming {
    fn default() -> Self {
        // ~60s budget: a fresh daemon is `active` within a few seconds.
        Self {
            health_poll_interval: Duration::from_secs(2),
            health_poll_max: 30,
            join_poll_interval: Duration::from_secs(1),
            join_poll_max: 30,
        }
    }
}

/// Everything the seed needs to render + install a member node, built from the
/// allowlist entry + the seed's own cluster config — **never** from the request.
#[derive(Debug, Clone)]
pub struct ProvisionPlan {
    pub assigned_node_id: NodeId,
    pub alias: String,
    pub cluster_id: Uuid,
    pub target_control_bind: String,
    pub target_control_addr: String,
    pub seed_peer: ControlPeer,
    /// Local path to the sha256-verified binary the seed pushes.
    pub binary_local_path: PathBuf,
    /// Expected sha256 of that binary (lowercase hex); verified before + after push.
    pub binary_sha256: String,
}

impl ProvisionPlan {
    /// Render the member node's `daemon.toml` (`seed = false`, joins the seed's
    /// cluster). Pure — unit-tested. Alias is validated upstream
    /// ([`sanitize_alias`]) so it cannot break the TOML.
    pub fn render_daemon_toml(&self) -> String {
        let mut s = String::new();
        s.push_str("[daemon]\n");
        s.push_str("config_dir = \"/opt/sentinel/config\"\n");
        s.push_str("data_dir = \"/opt/sentinel/data\"\n\n");
        s.push_str("[daemon.cluster]\n");
        s.push_str(&format!("node_id = \"{}\"\n", self.assigned_node_id));
        s.push_str(&format!("cluster_id = \"{}\"\n", self.cluster_id));
        s.push_str("seed = false\n");
        s.push_str(&format!("alias = \"{}\"\n", self.alias));
        s.push_str(&format!("chef_node_id = \"{}\"\n", self.seed_peer.node_id));
        s.push_str(&format!(
            "control_bind = \"{}\"\n",
            self.target_control_bind
        ));
        s.push_str("\n[[daemon.cluster.control_peers]]\n");
        s.push_str(&format!("node_id = \"{}\"\n", self.seed_peer.node_id));
        s.push_str(&format!("alias = \"{}\"\n", self.seed_peer.alias));
        s.push_str(&format!("addr = \"{}\"\n", self.seed_peer.addr));
        s.push_str(&format!(
            "cert_fingerprint = \"{}\"\n",
            self.seed_peer.cert_fingerprint
        ));
        // A provisioned member only installs sentinel-daemon. Do not inherit supervisor
        // defaults for services that are intentionally absent, and never run the Platform
        // LLM Analyzer on a member unless a later explicit configuration opts in.
        s.push_str("\n[daemon.platform_controlplane]\n");
        s.push_str("monitored_services = []\n");
        s.push_str("llm_enabled = false\n");
        s
    }
}

/// Validate an operator-supplied alias to a safe `[A-Za-z0-9_-]{1,63}` token (no
/// TOML/shell injection via the rendered config). Returns `None` if invalid.
pub fn sanitize_alias(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 63 {
        return None;
    }
    if raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Some(raw.to_string())
    } else {
        None
    }
}

/// Lowercase-hex sha256 of a file (the determinism-profile binary check, #494).
pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("open {} for hashing: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// The SSH/scp seam to a bare target. Injected so the saga is unit-testable with a
/// fake; [`SshProvisionTransport`] is the real impl (strict host-key checking
/// against the allowlist-pinned key, AC-S1).
pub trait ProvisionTransport {
    /// scp a local file to `remote_path` on the target.
    fn put_file(&self, local: &Path, remote_path: &str) -> anyhow::Result<()>;
    /// Write small text content to `remote_path` on the target.
    fn put_text(&self, remote_path: &str, contents: &str) -> anyhow::Result<()>;
    /// Run a command on the target (privileged via the bootstrap cred); return stdout.
    fn run(&self, cmd: &str) -> anyhow::Result<String>;
}

/// Seed-side cluster seam used to authorize a target and prove its observed join.
pub trait ProvisionCluster {
    fn authorize_peer(&self, peer: ControlPeer) -> anyhow::Result<()>;
    fn revoke_peer(&self, node_id: NodeId) -> anyhow::Result<()>;
    fn is_alive(&self, node_id: NodeId) -> bool;
}

/// Drive the provision saga to `Completed` (or `Failed`). Any transport error fails
/// the op (rollback/quarantine, AC-B6) and returns `Err`; a completed op is left
/// terminal. Returns the wall-clock duration of the bootstrap on success.
///
/// `clock` supplies `now_ms` (injected for deterministic tests).
pub fn execute_provision_node<T: ProvisionTransport>(
    op: &mut ProvisionOp,
    pending: &PendingBareNode,
    plan: &ProvisionPlan,
    transport: &T,
    cluster: &impl ProvisionCluster,
    timing: ProvisionTiming,
    clock: &dyn Fn() -> u64,
    persist: &dyn Fn(&ProvisionOp) -> anyhow::Result<()>,
) -> anyhow::Result<u64> {
    let started = Instant::now();

    // VerifyingTarget → the allowlist host key must be pinned (AC-S1 precondition).
    if pending.expected_host_key.trim().is_empty() {
        op.fail("no pinned host key for target (AC-S1)", clock());
        persist(op)?;
        anyhow::bail!(
            "provision: target {} has no pinned host key",
            pending.pending_target_id
        );
    }
    match op.node_id {
        Some(node_id) if node_id != plan.assigned_node_id => {
            op.fail("reserved NodeId differs from provision plan", clock());
            persist(op)?;
            anyhow::bail!("provision: reserved NodeId differs from provision plan");
        }
        None => op.assign_node(plan.assigned_node_id, clock()),
        Some(_) => {}
    }
    op.advance(clock()); // → PinningHostKey
    persist(op)?;

    // PinningHostKey → the transport was constructed with the pinned key for strict
    // checking; confirm reachability over that pinned channel.
    if let Err(error) = fenced_step(op, clock, persist, "reachability", || {
        transport.run("true").map(|_| ())
    }) {
        return Err(with_quarantine(error, cluster, transport, op));
    } // → PushingBinary

    // PushingBinary → verify local sha256 == expected, push, verify remote sha256.
    if let Err(error) = fenced_step(op, clock, persist, "push binary", || {
        let local_sha = sha256_file(&plan.binary_local_path)?;
        if local_sha != plan.binary_sha256 {
            anyhow::bail!(
                "local binary sha256 mismatch (expected {}, got {local_sha})",
                plan.binary_sha256
            );
        }
        transport.run("sudo install -d -m 0755 /opt/sentinel/bin")?;
        transport.put_file(&plan.binary_local_path, "/tmp/sentinel-daemon.new")?;
        transport.run(&format!(
            "sudo install -m 0755 /tmp/sentinel-daemon.new {REMOTE_BIN} && rm -f /tmp/sentinel-daemon.new"
        ))?;
        let remote_sha = transport.run(&format!("sha256sum {REMOTE_BIN} | cut -d' ' -f1"))?;
        if remote_sha.trim() != plan.binary_sha256 {
            anyhow::bail!(
                "remote binary sha256 mismatch after push (expected {}, got {})",
                plan.binary_sha256,
                remote_sha.trim()
            );
        }
        Ok(())
    }) {
        return Err(with_quarantine(error, cluster, transport, op));
    } // → IssuingCert

    // IssuingCert: the verified target binary generates its private key locally. Only
    // the public certificate fingerprint returns over the pinned SSH channel.
    let mut target_fingerprint = None;
    if let Err(error) = fenced_step(op, clock, persist, "issue control identity", || {
        let output = transport.run(&format!(
            "sudo {REMOTE_BIN} generate-control-identity --alias {} --cert {REMOTE_CONTROL_CERT} --key {REMOTE_CONTROL_KEY}",
            plan.alias
        ))?;
        let fingerprint = CertFingerprint::from_hex(output.trim())
            .ok_or_else(|| anyhow::anyhow!("target returned malformed control fingerprint"))?;
        let key_mode = transport.run(&format!("stat -c %a {REMOTE_CONTROL_KEY}"))?;
        if key_mode.trim() != "600" {
            anyhow::bail!(
                "target control private key mode is {}, expected 600",
                key_mode.trim()
            );
        }
        target_fingerprint = Some(fingerprint);
        Ok(())
    }) {
        return Err(with_quarantine(error, cluster, transport, op));
    } // → RenderingConfig
    let target_fingerprint = target_fingerprint.expect("identity step produced a fingerprint");

    // RenderingConfig → daemon.toml + systemd unit + token-gate drop-ins.
    // `config/agents` MUST exist — the daemon `read_dir`s it on startup (an absent
    // dir is fatal). Privileged files are staged to a writable `/tmp` path and then
    // `sudo install`ed (the SSH user is unprivileged; a direct scp into a root-owned
    // dir is denied — same staging pattern as the binary push above).
    if let Err(error) = fenced_step(op, clock, persist, "render config", || {
        // Create every Sentinel runtime dir the systemd unit's ReadWritePaths
        // references (`/opt/sentinel/fs` is required for the ProtectSystem=strict
        // mount namespace even when the member runs without a FUSE mount). `/ram/*`
        // tmpfs + `sentinel.target` are base-image requirements.
        transport.run(
            "sudo install -d -m 0755 /opt/sentinel/config /opt/sentinel/config/agents /opt/sentinel/data /opt/sentinel/fs /etc/sentinel",
        )?;
        install_text(transport, &plan.render_daemon_toml(), REMOTE_CONFIG, "0644")?;
        install_text(transport, SYSTEMD_UNIT, REMOTE_UNIT, "0644")?;
        for unit in TOKEN_GATE_UNITS {
            let dir = format!("/etc/systemd/system/{unit}.d");
            transport.run(&format!("sudo install -d -m 0755 {dir}"))?;
            install_text(
                transport,
                TOKEN_GATE_DROPIN,
                &format!("{dir}/token-gate.conf"),
                "0644",
            )?;
        }
        transport.run("sudo systemctl daemon-reload")?;
        cluster.authorize_peer(ControlPeer {
            node_id: plan.assigned_node_id,
            alias: plan.alias.clone(),
            addr: plan.target_control_addr.clone(),
            cert_fingerprint: target_fingerprint.to_hex(),
        })?;
        Ok(())
    }) {
        return Err(with_quarantine(error, cluster, transport, op));
    } // → StartingDaemon

    // StartingDaemon → enable + start the daemon (gateway/judge stay gated).
    if let Err(error) = fenced_step(op, clock, persist, "start daemon", || {
        transport.run("sudo systemctl enable --now sentinel-daemon.service")?;
        Ok(())
    }) {
        return Err(with_quarantine(error, cluster, transport, op));
    } // → AwaitingHealth

    // AwaitingHealth → poll `systemctl is-active` over the same SSH channel (avoids
    // depending on the target's operator-API network bind/auth).
    let mut healthy = false;
    for _ in 0..timing.health_poll_max {
        match transport.run("systemctl is-active sentinel-daemon.service") {
            Ok(out) if out.trim() == "active" => {
                healthy = true;
                break;
            }
            _ => {}
        }
        if timing.health_poll_interval > Duration::ZERO {
            std::thread::sleep(timing.health_poll_interval);
        }
    }
    if !healthy {
        op.fail("target daemon did not become active", clock());
        let mut error = anyhow::anyhow!(
            "provision: target {} did not become active after bootstrap",
            pending.pending_target_id
        );
        if let Err(persist_error) = persist(op) {
            error = anyhow::anyhow!("{error}; persist failed state: {persist_error}");
        }
        return Err(with_quarantine(error, cluster, transport, op));
    }
    op.advance(clock()); // → ObservingJoin
    persist_remote_state(op, clock, persist, cluster, transport, "health")?;

    // ObservingJoin: completion requires the seed's authenticated membership view to
    // contain this exact NodeId as Alive. Process activity alone is insufficient.
    let mut joined = false;
    for _ in 0..timing.join_poll_max {
        if cluster.is_alive(plan.assigned_node_id) {
            joined = true;
            break;
        }
        if timing.join_poll_interval > Duration::ZERO {
            std::thread::sleep(timing.join_poll_interval);
        }
    }
    if !joined {
        op.fail("target did not join authenticated membership", clock());
        let mut error = anyhow::anyhow!(
            "provision: target {} did not join authenticated membership",
            pending.pending_target_id
        );
        if let Err(persist_error) = persist(op) {
            error = anyhow::anyhow!("{error}; persist failed state: {persist_error}");
        }
        return Err(with_quarantine(error, cluster, transport, op));
    }
    // A retry may have inherited a durable marker from an earlier failed attempt.
    // Keep it until the exact assigned NodeId has joined, then require its removal
    // before recording terminal success.
    if let Err(error) = transport.run(&format!("sudo rm -f {REMOTE_QUARANTINE}")) {
        op.fail(format!("clear quarantine marker: {error}"), clock());
        let mut error = anyhow::anyhow!("provision: clear quarantine marker: {error}");
        if let Err(persist_error) = persist(op) {
            error = anyhow::anyhow!("{error}; persist failed state: {persist_error}");
        }
        return Err(with_quarantine(error, cluster, transport, op));
    }
    op.advance(clock()); // → Completed
    persist_remote_state(op, clock, persist, cluster, transport, "completion")?;
    Ok(started.elapsed().as_millis() as u64)
}

fn quarantine_failed_provision(
    cluster: &impl ProvisionCluster,
    transport: &impl ProvisionTransport,
    op: &ProvisionOp,
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    if let Err(error) = transport.run("sudo systemctl disable --now sentinel-daemon.service") {
        failures.push(format!("stop target: {error}"));
    }
    if let Err(error) = transport.run("rm -f /tmp/sentinel-daemon.new /tmp/sentinel-stage-*") {
        failures.push(format!("remove staging files: {error}"));
    }
    let marker = serde_json::json!({
        "op_id": op.op_id,
        "pending_target_id": op.pending_target_id,
        "node_id": op.node_id,
        "state": "quarantined",
        "reason": op.failure_reason,
        "updated_at_ms": op.updated_at_ms,
    });
    match serde_json::to_string_pretty(&marker) {
        Ok(marker) => {
            if let Err(error) = install_text(transport, &marker, REMOTE_QUARANTINE, "0600") {
                failures.push(format!("write target quarantine marker: {error}"));
            }
        }
        Err(error) => failures.push(format!("serialize quarantine marker: {error}")),
    }
    if let Some(node_id) = op.node_id {
        if let Err(error) = cluster.revoke_peer(node_id) {
            failures.push(format!("revoke peer: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(failures.join("; "))
    }
}

fn with_quarantine(
    error: anyhow::Error,
    cluster: &impl ProvisionCluster,
    transport: &impl ProvisionTransport,
    op: &ProvisionOp,
) -> anyhow::Error {
    match quarantine_failed_provision(cluster, transport, op) {
        Ok(()) => error,
        Err(cleanup_error) => {
            anyhow::anyhow!("{error}; quarantine cleanup incomplete: {cleanup_error}")
        }
    }
}

fn persist_remote_state(
    op: &mut ProvisionOp,
    clock: &dyn Fn() -> u64,
    persist: &dyn Fn(&ProvisionOp) -> anyhow::Result<()>,
    cluster: &impl ProvisionCluster,
    transport: &impl ProvisionTransport,
    label: &str,
) -> anyhow::Result<()> {
    if let Err(error) = persist(op) {
        op.fail(format!("persist after {label}: {error}"), clock());
        let secondary = persist(op).err();
        let mut error = anyhow::anyhow!("persist after {label}: {error}");
        if let Some(secondary) = secondary {
            error = anyhow::anyhow!("{error}; persist failed state: {secondary}");
        }
        return Err(with_quarantine(error, cluster, transport, op));
    }
    Ok(())
}

/// Write `content` to a privileged `dest` on the target: stage it to a writable
/// `/tmp` path (the SSH user is unprivileged) and then `sudo install` it into place.
/// A direct scp into a root-owned dir is denied — this mirrors the binary push.
fn install_text<T: ProvisionTransport>(
    transport: &T,
    content: &str,
    dest: &str,
    mode: &str,
) -> anyhow::Result<()> {
    let staged = format!("/tmp/sentinel-stage-{}", Uuid::new_v4());
    transport.put_text(&staged, content)?;
    transport.run(&format!(
        "sudo install -D -m {mode} {staged} {dest} && rm -f {staged}"
    ))?;
    Ok(())
}

/// Run one fallible saga step: on `Ok` advance the op one happy-path state, on `Err`
/// fail the op (so a partial bootstrap leaves a `Failed`/quarantined op, never an
/// alive half-node, AC-B6) and propagate.
fn fenced_step(
    op: &mut ProvisionOp,
    clock: &dyn Fn() -> u64,
    persist: &dyn Fn(&ProvisionOp) -> anyhow::Result<()>,
    label: &str,
    f: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    match f() {
        Ok(()) => {
            op.advance(clock());
            if let Err(error) = persist(op) {
                op.fail(format!("persist after {label}: {error}"), clock());
                let _ = persist(op);
                return Err(error);
            }
            Ok(())
        }
        Err(e) => {
            op.fail(format!("{label}: {e}"), clock());
            persist(op)?;
            Err(e)
        }
    }
}

/// Real transport: `ssh`/`scp` to the target with strict host-key checking against
/// the allowlist-pinned key (AC-S1). The pinned key is written to a per-op
/// `known_hosts` so the seed never does a blind `StrictHostKeyChecking=no`.
pub struct SshProvisionTransport {
    target_ip: String,
    user: String,
    known_hosts: PathBuf,
    /// Optional bootstrap identity (seed's key); `None` = agent/default key.
    identity: Option<PathBuf>,
}

impl SshProvisionTransport {
    /// Build a transport, pinning `expected_host_key` into a per-op `known_hosts`
    /// under `work_dir` (AC-S1 — no TOFU, no blind accept).
    pub fn new(
        pending: &PendingBareNode,
        user: impl Into<String>,
        work_dir: &Path,
        identity: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let known_hosts = work_dir.join(format!("known_hosts-{}", pending.pending_target_id));
        std::fs::write(
            &known_hosts,
            format!(
                "{} {}\n",
                pending.target_ip,
                pending.expected_host_key.trim()
            ),
        )
        .map_err(|e| anyhow::anyhow!("write known_hosts: {e}"))?;
        Ok(Self {
            target_ip: pending.target_ip.clone(),
            user: user.into(),
            known_hosts,
            identity,
        })
    }

    fn ssh_opts(&self) -> Vec<String> {
        let mut opts = vec![
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            "-o".into(),
            format!("UserKnownHostsFile={}", self.known_hosts.display()),
            "-o".into(),
            "ConnectTimeout=10".into(),
            "-o".into(),
            "BatchMode=yes".into(),
        ];
        if let Some(id) = &self.identity {
            opts.push("-i".into());
            opts.push(id.display().to_string());
        }
        opts
    }
}

impl ProvisionTransport for SshProvisionTransport {
    fn put_file(&self, local: &Path, remote_path: &str) -> anyhow::Result<()> {
        let mut cmd = std::process::Command::new("scp");
        cmd.args(self.ssh_opts());
        cmd.arg(local);
        cmd.arg(format!("{}@{}:{remote_path}", self.user, self.target_ip));
        let out = cmd
            .output()
            .map_err(|e| anyhow::anyhow!("scp spawn: {e}"))?;
        if !out.status.success() {
            anyhow::bail!(
                "scp {} failed: {}",
                remote_path,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }

    fn put_text(&self, remote_path: &str, contents: &str) -> anyhow::Result<()> {
        // Stage to a local temp file, then scp (avoids remote-shell quoting issues).
        let tmp = std::env::temp_dir().join(format!("sentinel-provision-{}", Uuid::new_v4()));
        std::fs::write(&tmp, contents).map_err(|e| anyhow::anyhow!("stage temp: {e}"))?;
        let res = self.put_file(&tmp, remote_path);
        let _ = std::fs::remove_file(&tmp);
        res
    }

    fn run(&self, cmd: &str) -> anyhow::Result<String> {
        let mut c = std::process::Command::new("ssh");
        c.args(self.ssh_opts());
        c.arg(format!("{}@{}", self.user, self.target_ip));
        c.arg(cmd);
        let out = c.output().map_err(|e| anyhow::anyhow!("ssh spawn: {e}"))?;
        if !out.status.success() {
            anyhow::bail!(
                "ssh `{cmd}` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::provision::ProvisionOpState;
    use std::cell::RefCell;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn pending(id: &str, host_key: &str) -> PendingBareNode {
        PendingBareNode {
            pending_target_id: id.into(),
            target_ip: "10.0.0.242".into(),
            expected_host_key: host_key.into(),
            expected_image_id: None,
            expected_hostname: None,
            expected_machine_id: None,
            expires_at: 9_999_999_999,
        }
    }

    /// A fake transport that records every call and returns scripted results.
    struct FakeTransport {
        calls: RefCell<Vec<String>>,
        /// Command substring → forced error (to exercise failure paths).
        fail_on: Option<String>,
        active: RefCell<String>,
    }

    impl FakeTransport {
        fn healthy() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_on: None,
                active: RefCell::new("active".into()),
            }
        }
        fn failing(substr: &str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_on: Some(substr.into()),
                active: RefCell::new("active".into()),
            }
        }
        fn never_active() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_on: None,
                active: RefCell::new("activating".into()),
            }
        }
    }

    impl ProvisionTransport for FakeTransport {
        fn put_file(&self, _local: &Path, remote_path: &str) -> anyhow::Result<()> {
            let call = format!("put_file {remote_path}");
            self.calls.borrow_mut().push(call.clone());
            if self
                .fail_on
                .as_deref()
                .is_some_and(|needle| call.contains(needle))
            {
                anyhow::bail!("forced failure on `{call}`");
            }
            Ok(())
        }
        fn put_text(&self, remote_path: &str, _contents: &str) -> anyhow::Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("put_text {remote_path}"));
            Ok(())
        }
        fn run(&self, cmd: &str) -> anyhow::Result<String> {
            self.calls.borrow_mut().push(format!("run {cmd}"));
            if let Some(f) = &self.fail_on {
                if cmd.contains(f.as_str()) {
                    anyhow::bail!("forced failure on `{cmd}`");
                }
            }
            if cmd.contains("is-active") {
                return Ok(self.active.borrow().clone());
            }
            if cmd.contains("generate-control-identity") {
                return Ok(TEST_FINGERPRINT.to_string());
            }
            if cmd.contains("stat -c %a") {
                return Ok("600".into());
            }
            if cmd.contains("sha256sum") {
                // Echo the expected sha so the post-push check passes.
                return Ok(EXPECTED_SHA.to_string());
            }
            Ok(String::new())
        }
    }

    const EXPECTED_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const TEST_FINGERPRINT: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn journal_reuses_reserved_node_id_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provision-ops.json");
        let first = ProvisionJournal::open(&path).unwrap();
        let op = match first.reserve("bare-1", "node-1", "idem-1", 10).unwrap() {
            ProvisionReservation::Execute(op) => op,
            ProvisionReservation::Completed(_) => panic!("new op cannot be completed"),
        };
        let node_id = op.node_id.unwrap();
        drop(first);

        let reopened = ProvisionJournal::open(&path).unwrap();
        let retry = match reopened.reserve("bare-1", "node-1", "idem-1", 20).unwrap() {
            ProvisionReservation::Execute(op) => op,
            ProvisionReservation::Completed(_) => panic!("incomplete op must be retried"),
        };
        assert_eq!(retry.node_id, Some(node_id));
        assert_eq!(retry.op_id, op.op_id);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn journal_rejects_key_or_target_rebinding() {
        let dir = tempfile::tempdir().unwrap();
        let journal = ProvisionJournal::open(dir.path().join("provision-ops.json")).unwrap();
        journal.reserve("bare-1", "node-1", "idem-1", 10).unwrap();
        assert!(journal.reserve("bare-2", "node-2", "idem-1", 20).is_err());
        assert!(journal.reserve("bare-1", "node-1", "idem-2", 20).is_err());
    }

    #[test]
    fn completed_journal_entry_stays_noop_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provision-ops.json");
        let journal = ProvisionJournal::open(&path).unwrap();
        let mut op = match journal.reserve("bare-1", "node-1", "idem-1", 10).unwrap() {
            ProvisionReservation::Execute(op) => op,
            ProvisionReservation::Completed(_) => panic!("new op cannot be completed"),
        };
        while op.advance(11) {}
        journal.update(&op).unwrap();
        drop(journal);

        let reopened = ProvisionJournal::open(path).unwrap();
        let reservation = reopened.reserve("bare-1", "node-1", "idem-1", 20).unwrap();
        assert!(matches!(reservation, ProvisionReservation::Completed(_)));
    }

    struct FakeCluster {
        alive_node: Option<NodeId>,
        authorized: RefCell<Vec<ControlPeer>>,
        revoked: RefCell<Vec<NodeId>>,
        checked: RefCell<Vec<NodeId>>,
    }

    impl FakeCluster {
        fn joined(node_id: NodeId) -> Self {
            Self {
                alive_node: Some(node_id),
                authorized: RefCell::new(Vec::new()),
                revoked: RefCell::new(Vec::new()),
                checked: RefCell::new(Vec::new()),
            }
        }

        fn absent() -> Self {
            Self {
                alive_node: None,
                authorized: RefCell::new(Vec::new()),
                revoked: RefCell::new(Vec::new()),
                checked: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProvisionCluster for FakeCluster {
        fn authorize_peer(&self, peer: ControlPeer) -> anyhow::Result<()> {
            self.authorized.borrow_mut().push(peer);
            Ok(())
        }

        fn revoke_peer(&self, node_id: NodeId) -> anyhow::Result<()> {
            self.revoked.borrow_mut().push(node_id);
            Ok(())
        }

        fn is_alive(&self, node_id: NodeId) -> bool {
            self.checked.borrow_mut().push(node_id);
            self.alive_node == Some(node_id)
        }
    }

    fn fast_timing() -> ProvisionTiming {
        ProvisionTiming {
            health_poll_interval: Duration::ZERO,
            health_poll_max: 3,
            join_poll_interval: Duration::ZERO,
            join_poll_max: 3,
        }
    }

    /// Write a temp file whose sha256 is `EXPECTED_SHA` (an empty file).
    fn empty_binary() -> tempfile::NamedTempFile {
        tempfile::NamedTempFile::new().unwrap()
    }

    fn plan_for(bin: &Path) -> ProvisionPlan {
        ProvisionPlan {
            assigned_node_id: NodeId::new(),
            alias: "test-node-1".into(),
            cluster_id: Uuid::new_v4(),
            target_control_bind: "0.0.0.0:8085".into(),
            target_control_addr: "10.0.0.242:8085".into(),
            seed_peer: ControlPeer {
                node_id: NodeId::new(),
                alias: "test-node-0".into(),
                addr: "10.0.0.241:8085".into(),
                cert_fingerprint:
                    "2222222222222222222222222222222222222222222222222222222222222222".into(),
            },
            binary_local_path: bin.to_path_buf(),
            binary_sha256: EXPECTED_SHA.into(),
        }
    }

    #[test]
    fn happy_path_drives_saga_to_completed_in_order() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        let t = FakeTransport::healthy();
        let cluster = FakeCluster::joined(plan.assigned_node_id);
        let mut op = ProvisionOp::new(
            Uuid::new_v4(),
            "bare-1".into(),
            "test-node-1".into(),
            "idem".into(),
            0,
        );
        let clock = || 42u64;
        let dur = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
            &cluster,
            fast_timing(),
            &clock,
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(op.state, ProvisionOpState::Completed);
        assert_eq!(op.node_id, Some(plan.assigned_node_id));
        assert!(dur < 5_000);
        let calls = t.calls.borrow();
        // The exact remote command sequence the SshProvisionTransport will run.
        let joined = calls.join("\n");
        assert!(joined.contains("run true"), "reachability probe");
        assert!(
            joined.contains("put_file /tmp/sentinel-daemon.new"),
            "binary push"
        );
        assert!(joined.contains("generate-control-identity"));
        assert!(joined.contains(REMOTE_CONTROL_CERT));
        assert!(joined.contains(REMOTE_CONTROL_KEY));
        assert_eq!(cluster.authorized.borrow().len(), 1);
        assert_eq!(
            cluster.authorized.borrow()[0].node_id,
            plan.assigned_node_id
        );
        assert_eq!(
            cluster.authorized.borrow()[0].cert_fingerprint,
            TEST_FINGERPRINT
        );
        assert_eq!(
            cluster.checked.borrow().as_slice(),
            &[plan.assigned_node_id]
        );
        // config/agents must be created (the daemon read_dirs it on startup).
        assert!(
            joined.contains("/opt/sentinel/config/agents"),
            "config/agents dir created"
        );
        // Privileged files are staged to /tmp then `sudo install`ed (not scp'd
        // directly into the root-owned dir).
        assert!(
            joined.contains("put_text /tmp/sentinel-stage-"),
            "config staged to /tmp"
        );
        assert!(
            joined.contains("sudo install -D -m 0644 /tmp/sentinel-stage-"),
            "staged file sudo-installed"
        );
        assert!(joined.contains(REMOTE_CONFIG), "daemon.toml dest");
        assert!(joined.contains(REMOTE_UNIT), "systemd unit dest");
        assert!(joined.contains("token-gate.conf"), "token-gate drop-in");
        assert!(joined.contains("systemctl daemon-reload"));
        assert!(joined.contains("systemctl enable --now sentinel-daemon.service"));
        assert!(joined.contains("is-active"));
        assert!(
            joined.contains(&format!("sudo rm -f {REMOTE_QUARANTINE}")),
            "stale quarantine marker cleared only after authenticated join"
        );
        // A token-gate drop-in is installed for EACH LLM-touching unit.
        for unit in TOKEN_GATE_UNITS {
            assert!(joined.contains(&format!("/etc/systemd/system/{unit}.d/token-gate.conf")));
        }
    }

    #[test]
    fn missing_host_key_pin_fails_before_any_io() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        let t = FakeTransport::healthy();
        let cluster = FakeCluster::joined(plan.assigned_node_id);
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let clock = || 1u64;
        // Empty pinned host key → AC-S1 precondition fails, no transport calls.
        let err = execute_provision_node(
            &mut op,
            &pending("bare-1", "  "),
            &plan,
            &t,
            &cluster,
            fast_timing(),
            &clock,
            &|_| Ok(()),
        );
        assert!(err.is_err());
        assert_eq!(op.state, ProvisionOpState::Failed);
        assert!(op.failure_reason.as_deref().unwrap().contains("host key"));
        assert!(t.calls.borrow().is_empty(), "no IO before host-key check");
    }

    #[test]
    fn transport_error_quarantines_the_op() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        // Fail when starting the daemon → op must end Failed (AC-B6), never alive.
        let t = FakeTransport::failing("enable --now");
        let cluster = FakeCluster::joined(plan.assigned_node_id);
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let clock = || 7u64;
        let err = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
            &cluster,
            fast_timing(),
            &clock,
            &|_| Ok(()),
        );
        assert!(err.is_err());
        assert_eq!(op.state, ProvisionOpState::Failed);
        assert!(op
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("start daemon"));
        assert_eq!(
            cluster.revoked.borrow().as_slice(),
            &[plan.assigned_node_id]
        );
    }

    #[test]
    fn binary_sha_mismatch_fails_push() {
        let mut bin = tempfile::NamedTempFile::new().unwrap();
        bin.write_all(b"not empty").unwrap(); // sha != EXPECTED_SHA
        let plan = plan_for(bin.path());
        let t = FakeTransport::healthy();
        let cluster = FakeCluster::joined(plan.assigned_node_id);
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let clock = || 3u64;
        let err = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
            &cluster,
            fast_timing(),
            &clock,
            &|_| Ok(()),
        );
        assert!(err.is_err());
        assert_eq!(op.state, ProvisionOpState::Failed);
        assert!(op
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("sha256 mismatch"));
        let calls = t.calls.borrow().join("\n");
        assert!(calls.contains("disable --now sentinel-daemon.service"));
        assert!(calls.contains("put_text /tmp/sentinel-stage-"));
        assert_eq!(
            cluster.revoked.borrow().as_slice(),
            &[plan.assigned_node_id]
        );
    }

    #[test]
    fn binary_push_failure_is_durably_quarantined() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        let t = FakeTransport::failing("put_file /tmp/sentinel-daemon.new");
        let cluster = FakeCluster::joined(plan.assigned_node_id);
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let result = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
            &cluster,
            fast_timing(),
            &|| 4,
            &|_| Ok(()),
        );
        assert!(result.is_err());
        assert_eq!(op.state, ProvisionOpState::Failed);
        let calls = t.calls.borrow().join("\n");
        assert!(calls.contains("disable --now sentinel-daemon.service"));
        assert!(calls.contains("rm -f /tmp/sentinel-daemon.new"));
        assert!(calls.contains("put_text /tmp/sentinel-stage-"));
        assert_eq!(
            cluster.revoked.borrow().as_slice(),
            &[plan.assigned_node_id]
        );
    }

    #[test]
    fn unhealthy_target_fails_after_polls() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        let t = FakeTransport::never_active();
        let cluster = FakeCluster::joined(plan.assigned_node_id);
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let clock = || 9u64;
        let err = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
            &cluster,
            fast_timing(),
            &clock,
            &|_| Ok(()),
        );
        assert!(err.is_err());
        assert_eq!(op.state, ProvisionOpState::Failed);
        assert!(op
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("did not become active"));
        assert_eq!(
            cluster.revoked.borrow().as_slice(),
            &[plan.assigned_node_id]
        );
    }

    #[test]
    fn active_process_without_membership_join_never_completes() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        let t = FakeTransport::healthy();
        let cluster = FakeCluster::absent();
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let err = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
            &cluster,
            fast_timing(),
            &|| 10,
            &|_| Ok(()),
        );
        assert!(err.is_err());
        assert_eq!(op.state, ProvisionOpState::Failed);
        assert!(op
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("authenticated membership"));
        assert_eq!(
            cluster.revoked.borrow().as_slice(),
            &[plan.assigned_node_id]
        );
        assert!(t
            .calls
            .borrow()
            .iter()
            .any(|call| call.contains("disable --now sentinel-daemon")));
    }

    #[test]
    fn quarantine_marker_must_clear_before_completion() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        let t = FakeTransport::failing(REMOTE_QUARANTINE);
        let cluster = FakeCluster::joined(plan.assigned_node_id);
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let result = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
            &cluster,
            fast_timing(),
            &|| 11,
            &|_| Ok(()),
        );
        assert!(result.is_err());
        assert_eq!(op.state, ProvisionOpState::Failed);
        assert!(op
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("clear quarantine marker"));
        assert_eq!(
            cluster.revoked.borrow().as_slice(),
            &[plan.assigned_node_id]
        );
    }

    #[test]
    fn render_daemon_toml_is_member_config() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        let toml = plan.render_daemon_toml();
        assert!(toml.contains("seed = false"));
        assert!(toml.contains(&format!("node_id = \"{}\"", plan.assigned_node_id)));
        assert!(toml.contains(&format!("cluster_id = \"{}\"", plan.cluster_id)));
        assert!(toml.contains(&format!("chef_node_id = \"{}\"", plan.seed_peer.node_id)));
        assert!(toml.contains("control_bind = \"0.0.0.0:8085\""));
        assert!(toml.contains("[[daemon.cluster.control_peers]]"));
        assert!(toml.contains(&format!("node_id = \"{}\"", plan.seed_peer.node_id)));
        assert!(toml.contains("addr = \"10.0.0.241:8085\""));
        assert!(toml.contains(&plan.seed_peer.cert_fingerprint));
        assert!(!toml.contains("seed_endpoint"));
        // It must parse back as a valid daemon cluster config.
        assert!(toml.starts_with("[daemon]"));
        // #568: a provisioned member must not enable the Platform LLM Analyzer.
        assert!(toml.contains("[daemon.platform_controlplane]"));
        assert!(toml.contains("monitored_services = []"));
        assert!(toml.contains("llm_enabled = false"));
        // And it must parse back to a config whose analyzer and absent-service monitoring
        // are both disabled.
        let parsed: crate::config::DaemonConfigFile =
            toml::from_str(&toml).expect("rendered member daemon.toml must parse");
        let cluster = parsed.daemon.cluster.as_ref().unwrap();
        assert_eq!(cluster.control_bind.as_deref(), Some("0.0.0.0:8085"));
        assert_eq!(cluster.control_peers, vec![plan.seed_peer.clone()]);
        assert_eq!(cluster.chef_node_id, Some(plan.seed_peer.node_id));
        assert!(!parsed.daemon.platform_controlplane.llm_enabled);
        assert!(parsed
            .daemon
            .platform_controlplane
            .monitored_services
            .is_empty());
    }

    #[test]
    fn sanitize_alias_rejects_injection() {
        assert_eq!(
            sanitize_alias("test-node_1").as_deref(),
            Some("test-node_1")
        );
        assert_eq!(sanitize_alias("  node2  ").as_deref(), Some("node2"));
        assert!(sanitize_alias("bad\"name").is_none());
        assert!(sanitize_alias("a\nb").is_none());
        assert!(sanitize_alias("").is_none());
        assert!(sanitize_alias(&"x".repeat(64)).is_none());
    }

    #[test]
    fn token_gate_dropin_is_the_repo_template() {
        // The embedded drop-in must carry the boot-gate condition (AC-B7 mechanism).
        assert!(TOKEN_GATE_DROPIN.contains("ConditionPathExists=/etc/sentinel/allow-llm"));
        assert!(SYSTEMD_UNIT.contains("ExecStart=/opt/sentinel/bin/sentinel-daemon"));
    }
}
