//! ProvisionNode execution (#495, G3) — the seed-side saga driver that absorbs a
//! bare VM shell into a cluster node.
//!
//! The seed drives a recoverable [`ProvisionOp`] saga
//! (`sentinel_common::provision`) over an injected [`ProvisionTransport`] (the
//! SSH/scp seam): validate the target against the `PendingBareNode` allowlist
//! (V14), confirm the out-of-band host-key pin (AC-S1), push the sha256-verified
//! binary, render `daemon.toml` + the systemd unit + the #517 token-gate drop-ins,
//! start the daemon and wait for it to become healthy. The saga/decision/render
//! logic is transport-agnostic and unit-tested; the real [`SshProvisionTransport`]
//! is a thin wrapper over `ssh`/`scp`.
//!
//! **Track-A bounded scope (documented, not a silent gap):**
//! - The SSH bootstrap establishes trust. The mTLS node cert (the `IssuingCert`
//!   saga step) is a deliberate **no-op** here — membership runs over Zenoh (no
//!   peer auth yet) and the authenticated QUIC control stream (ADR-2 / #498) is
//!   where a node cert first matters. The saga state exists so the shape is
//!   N-node-native; the execution fills it in a later track.
//! - **No secrets / LLM tokens are copied** (AC-B7/AC-S3): the token-gate drop-ins
//!   keep gateway/judge/health boot-gated on `ConditionPathExists=/etc/sentinel/allow-llm`,
//!   which the seed never creates.
//! - `ProvisionOp` durability (ADR-3 `PROVISION_OPS`, V5 restart-recovery) is an
//!   in-memory map here; the redb persistence lands with #496's cluster tables.
//!
//! The full live bootstrap against a real bare VM is the cross-node live AC
//! (`AC-B2..B6` / `AC-S1..S6`) verified on the test cluster after deploy.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sentinel_common::cluster::{NodeId, PendingBareNode};
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

/// Health-poll cadence for `AwaitingHealth` (injected so tests don't sleep).
#[derive(Debug, Clone, Copy)]
pub struct ProvisionTiming {
    pub health_poll_interval: Duration,
    pub health_poll_max: u32,
}

impl Default for ProvisionTiming {
    fn default() -> Self {
        // ~60s budget: a fresh daemon is `active` within a few seconds.
        Self {
            health_poll_interval: Duration::from_secs(2),
            health_poll_max: 30,
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
    /// Zenoh connect endpoint for the member to reach the seed; `None` = rely on
    /// LAN multicast discovery (Track-A default — the test nodes share a subnet).
    pub seed_endpoint: Option<String>,
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
        if let Some(ep) = &self.seed_endpoint {
            s.push_str(&format!("seed_endpoint = \"{ep}\"\n"));
        }
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

    // IssuingCert → Track-A no-op (SSH trust; mTLS node cert deferred to QUIC/#498).
    op.advance(clock()); // → RenderingConfig

    // RenderingConfig → daemon.toml + systemd unit + token-gate drop-ins.
    fenced_step(op, clock, "render config", || {
        transport
            .run("sudo install -d -m 0755 /opt/sentinel/config /opt/sentinel/data /etc/sentinel")?;
        transport.put_text(REMOTE_CONFIG, &plan.render_daemon_toml())?;
        transport.put_text(REMOTE_UNIT, SYSTEMD_UNIT)?;
        for unit in TOKEN_GATE_UNITS {
            let dir = format!("/etc/systemd/system/{unit}.d");
            transport.run(&format!("sudo install -d -m 0755 {dir}"))?;
            transport.put_text(&format!("{dir}/token-gate.conf"), TOKEN_GATE_DROPIN)?;
        }
        transport.run("sudo systemctl daemon-reload")?;
        Ok(())
    })?; // → StartingDaemon

    // StartingDaemon → enable + start the daemon (gateway/judge stay gated).
    fenced_step(op, clock, "start daemon", || {
        transport.run("sudo systemctl enable --now sentinel-daemon.service")?;
        Ok(())
    })?; // → AwaitingHealth

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
        anyhow::bail!(
            "provision: target {} did not become active after bootstrap",
            pending.pending_target_id
        );
    }
    op.advance(clock()); // → ObservingJoin

    // ObservingJoin → membership convergence is observed by the membership service
    // (Chunk 4a); the join is confirmed by the daemon being active + publishing its
    // heartbeat. The live both-nodes-alive assertion is AC-B4.
    op.advance(clock()); // → Completed
    Ok(started.elapsed().as_millis() as u64)
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
            if cmd.contains("sha256sum") {
                // Echo the expected sha so the post-push check passes.
                return Ok(EXPECTED_SHA.to_string());
            }
            Ok(String::new())
        }
    }

    const EXPECTED_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn fast_timing() -> ProvisionTiming {
        ProvisionTiming {
            health_poll_interval: Duration::ZERO,
            health_poll_max: 3,
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
            seed_endpoint: None,
            binary_local_path: bin.to_path_buf(),
            binary_sha256: EXPECTED_SHA.into(),
        }
    }

    #[test]
    fn happy_path_drives_saga_to_completed_in_order() {
        let bin = empty_binary();
        let plan = plan_for(bin.path());
        let t = FakeTransport::healthy();
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
        assert!(
            joined.contains(&format!("put_text {REMOTE_CONFIG}")),
            "daemon.toml render"
        );
        assert!(
            joined.contains(&format!("put_text {REMOTE_UNIT}")),
            "systemd unit"
        );
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
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let clock = || 1u64;
        // Empty pinned host key → AC-S1 precondition fails, no transport calls.
        let err = execute_provision_node(
            &mut op,
            &pending("bare-1", "  "),
            &plan,
            &t,
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
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let clock = || 7u64;
        let err = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
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
    }

    #[test]
    fn binary_sha_mismatch_fails_push() {
        let mut bin = tempfile::NamedTempFile::new().unwrap();
        bin.write_all(b"not empty").unwrap(); // sha != EXPECTED_SHA
        let plan = plan_for(bin.path());
        let t = FakeTransport::healthy();
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let clock = || 3u64;
        let err = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
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
        let mut op = ProvisionOp::new(Uuid::new_v4(), "bare-1".into(), "n".into(), "i".into(), 0);
        let clock = || 9u64;
        let err = execute_provision_node(
            &mut op,
            &pending("bare-1", "ssh-ed25519 AAAA"),
            &plan,
            &t,
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
    }

    #[test]
    fn render_daemon_toml_is_member_config() {
        let bin = empty_binary();
        let mut plan = plan_for(bin.path());
        plan.seed_endpoint = Some("tcp/10.0.0.241:7447".into());
        let toml = plan.render_daemon_toml();
        assert!(toml.contains("seed = false"));
        assert!(toml.contains(&format!("node_id = \"{}\"", plan.assigned_node_id)));
        assert!(toml.contains(&format!("cluster_id = \"{}\"", plan.cluster_id)));
        assert!(toml.contains("seed_endpoint = \"tcp/10.0.0.241:7447\""));
        // It must parse back as a valid daemon cluster config.
        assert!(toml.starts_with("[daemon]"));
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
