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
//! - `ProvisionOp` durability (ADR-3 `PROVISION_OPS`, V5 restart-recovery) is an
//!   in-memory map here; the redb persistence lands with #496's cluster tables.
//!
//! The full bare-VM bootstrap remains a destructive cross-node acceptance drill.
//! Unit tests cover the complete transport sequence and fail-closed join behavior;
//! live correction evidence exercises target-local identity generation and the
//! authenticated membership boundary without reprovisioning an active node.

use std::path::{Path, PathBuf};
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
) -> anyhow::Result<u64> {
    let started = Instant::now();

    // VerifyingTarget → the allowlist host key must be pinned (AC-S1 precondition).
    if pending.expected_host_key.trim().is_empty() {
        op.fail("no pinned host key for target (AC-S1)", clock());
        anyhow::bail!(
            "provision: target {} has no pinned host key",
            pending.pending_target_id
        );
    }
    op.assign_node(plan.assigned_node_id, clock());
    op.advance(clock()); // → PinningHostKey

    // PinningHostKey → the transport was constructed with the pinned key for strict
    // checking; confirm reachability over that pinned channel.
    fenced_step(op, clock, "reachability", || {
        transport.run("true").map(|_| ())
    })?; // → PushingBinary

    // PushingBinary → verify local sha256 == expected, push, verify remote sha256.
    fenced_step(op, clock, "push binary", || {
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
    })?; // → IssuingCert

    // IssuingCert: the verified target binary generates its private key locally. Only
    // the public certificate fingerprint returns over the pinned SSH channel.
    let mut target_fingerprint = None;
    fenced_step(op, clock, "issue control identity", || {
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
    })?; // → RenderingConfig
    let target_fingerprint = target_fingerprint.expect("identity step produced a fingerprint");

    // RenderingConfig → daemon.toml + systemd unit + token-gate drop-ins.
    // `config/agents` MUST exist — the daemon `read_dir`s it on startup (an absent
    // dir is fatal). Privileged files are staged to a writable `/tmp` path and then
    // `sudo install`ed (the SSH user is unprivileged; a direct scp into a root-owned
    // dir is denied — same staging pattern as the binary push above).
    let mut peer_authorized = false;
    fenced_step(op, clock, "render config", || {
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
        peer_authorized = true;
        Ok(())
    })?; // → StartingDaemon

    // StartingDaemon → enable + start the daemon (gateway/judge stay gated).
    if let Err(error) = fenced_step(op, clock, "start daemon", || {
        transport.run("sudo systemctl enable --now sentinel-daemon.service")?;
        Ok(())
    }) {
        cleanup_failed_join(cluster, transport, plan.assigned_node_id, peer_authorized);
        return Err(error);
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
        cleanup_failed_join(cluster, transport, plan.assigned_node_id, peer_authorized);
        anyhow::bail!(
            "provision: target {} did not become active after bootstrap",
            pending.pending_target_id
        );
    }
    op.advance(clock()); // → ObservingJoin

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
        cleanup_failed_join(cluster, transport, plan.assigned_node_id, peer_authorized);
        anyhow::bail!(
            "provision: target {} did not join authenticated membership",
            pending.pending_target_id
        );
    }
    op.advance(clock()); // → Completed
    Ok(started.elapsed().as_millis() as u64)
}

fn cleanup_failed_join(
    cluster: &impl ProvisionCluster,
    transport: &impl ProvisionTransport,
    node_id: NodeId,
    peer_authorized: bool,
) {
    let _ = transport.run("sudo systemctl disable --now sentinel-daemon.service");
    if peer_authorized {
        let _ = cluster.revoke_peer(node_id);
    }
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
    label: &str,
    f: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    match f() {
        Ok(()) => {
            op.advance(clock());
            Ok(())
        }
        Err(e) => {
            op.fail(format!("{label}: {e}"), clock());
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
            self.calls
                .borrow_mut()
                .push(format!("put_file {remote_path}"));
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
        );
        assert!(err.is_err());
        assert_eq!(op.state, ProvisionOpState::Failed);
        assert!(op
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("sha256 mismatch"));
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
    fn render_daemon_toml_is_member_config() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        let toml = plan.render_daemon_toml();
        assert!(toml.contains("seed = false"));
        assert!(toml.contains(&format!("node_id = \"{}\"", plan.assigned_node_id)));
        assert!(toml.contains(&format!("cluster_id = \"{}\"", plan.cluster_id)));
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
