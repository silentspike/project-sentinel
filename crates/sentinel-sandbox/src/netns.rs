//! Per-Agent Network Namespace Isolation.
//!
//! Erstellt pro Agent einen isolierten Network-Namespace mit:
//! - veth-Paar: Agent-NS <-> Host-Bridge
//! - nftables: Erlaubt NUR Zenoh + Cortex Gateway Ports
//! - Graceful Fallback auf --share-net wenn CAP_NET_ADMIN fehlt
//!
//! Architektur:
//! ```text
//! Host: br-sentinel (10.42.0.1/16)
//!   +-- veth-{agent} ---- vp-{agent} (Agent-NS, 10.42.0.{idx+2})
//!       nftables: ALLOW 10.42.0.1:7447, 10.42.0.1:8080, DROP rest
//! ```

use std::process::Command;

use anyhow::{Context, Result};
use tracing::{info, warn};

/// Default bridge name for sentinel agents.
pub const DEFAULT_BRIDGE: &str = "br-sentinel";

/// Bridge IP address (host-side).
pub const BRIDGE_IP: &str = "10.42.0.1";

/// Bridge subnet prefix length.
pub const BRIDGE_PREFIX: u8 = 16;

/// Default Zenoh port.
pub const ZENOH_PORT: u16 = 7447;

/// Default Cortex Gateway port.
pub const CORTEX_PORT: u16 = 8080;

/// Maximum Linux interface name length (IFNAMSIZ - 1 for null terminator).
const MAX_IFNAME: usize = 15;

/// Network Namespace configuration for a single agent.
#[derive(Debug, Clone)]
pub struct NetworkNsConfig {
    pub agent_name: String,
    pub agent_index: u8,
    pub zenoh_port: u16,
    pub cortex_port: u16,
    pub bridge_name: String,
    pub bridge_ip: String,
    pub bridge_prefix: u8,
}

impl NetworkNsConfig {
    /// Creates a default config for an agent.
    ///
    /// `index` determines the agent's IP: 10.42.0.{index+2}
    /// (0 and 1 are reserved for bridge and broadcast).
    pub fn for_agent(name: &str, index: u8) -> Self {
        Self {
            agent_name: name.to_string(),
            agent_index: index,
            zenoh_port: ZENOH_PORT,
            cortex_port: CORTEX_PORT,
            bridge_name: DEFAULT_BRIDGE.to_string(),
            bridge_ip: BRIDGE_IP.to_string(),
            bridge_prefix: BRIDGE_PREFIX,
        }
    }

    /// Agent IP address within the bridge subnet.
    pub fn agent_ip(&self) -> String {
        // index + 2 to avoid .0 (network) and .1 (bridge)
        let octet = u16::from(self.agent_index) + 2;
        format!("10.42.0.{octet}")
    }

    /// Host-side veth interface name, truncated to IFNAMSIZ.
    pub fn veth_host_name(&self) -> String {
        let raw = format!("veth-{}", self.agent_name);
        truncate_ifname(&raw)
    }

    /// Peer-side veth interface name (inside agent NS), truncated to IFNAMSIZ.
    pub fn veth_peer_name(&self) -> String {
        let raw = format!("vp-{}", self.agent_name);
        truncate_ifname(&raw)
    }

    /// Generates nftables ruleset for the agent's network namespace.
    ///
    /// Policy: DROP all, ALLOW only:
    /// - loopback (lo)
    /// - outgoing TCP to bridge_ip:zenoh_port
    /// - outgoing TCP to bridge_ip:cortex_port
    /// - established/related return traffic
    pub fn generate_nftables_rules(&self) -> String {
        format!(
            r#"table inet sentinel-agent {{
  chain input {{
    type filter hook input priority 0; policy drop;
    iif "lo" accept
    ct state established,related accept
  }}
  chain output {{
    type filter hook output priority 0; policy drop;
    oif "lo" accept
    ip daddr {bridge} tcp dport {zenoh} accept
    ip daddr {bridge} tcp dport {cortex} accept
    ct state established,related accept
  }}
}}"#,
            bridge = self.bridge_ip,
            zenoh = self.zenoh_port,
            cortex = self.cortex_port,
        )
    }
}

/// Truncate interface name to Linux IFNAMSIZ limit (15 chars).
fn truncate_ifname(name: &str) -> String {
    if name.len() > MAX_IFNAME {
        name[..MAX_IFNAME].to_string()
    } else {
        name.to_string()
    }
}

/// Detects whether network namespace isolation is supported.
///
/// Checks for:
/// - `ip` command available
/// - `nft` command available
/// - CAP_NET_ADMIN capability (via dummy link probe)
pub fn detect_netns_support() -> bool {
    // Check ip command
    let ip_ok = Command::new("ip")
        .args(["link", "show"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !ip_ok {
        warn!("Network namespace isolation: 'ip' command not available");
        return false;
    }

    // Check nft command
    let nft_ok = Command::new("nft")
        .args(["list", "tables"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !nft_ok {
        warn!("Network namespace isolation: 'nft' command not available or no permissions");
        return false;
    }

    // Probe CAP_NET_ADMIN: try to create and immediately delete a dummy link
    let probe_name = "sentinel-probe";
    let create_ok = Command::new("ip")
        .args(["link", "add", probe_name, "type", "dummy"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if create_ok {
        // Cleanup probe
        let _ = Command::new("ip")
            .args(["link", "del", probe_name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        info!("Network namespace isolation: CAP_NET_ADMIN confirmed");
        true
    } else {
        warn!("Network namespace isolation: CAP_NET_ADMIN not available (dummy link probe failed)");
        false
    }
}

/// Sets up the sentinel bridge (idempotent).
///
/// Creates `br-sentinel` with IP 10.42.0.1/16 and brings it up.
/// Safe to call multiple times — skips if bridge already exists.
pub fn setup_bridge(config: &NetworkNsConfig) -> Result<()> {
    // Check if bridge already exists
    let exists = Command::new("ip")
        .args(["link", "show", &config.bridge_name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if exists {
        info!("Bridge {} already exists", config.bridge_name);
        return Ok(());
    }

    // Create bridge
    run_cmd(
        "ip",
        &["link", "add", &config.bridge_name, "type", "bridge"],
    )
    .context("Failed to create bridge")?;

    // Assign IP
    let cidr = format!("{}/{}", config.bridge_ip, config.bridge_prefix);
    run_cmd("ip", &["addr", "add", &cidr, "dev", &config.bridge_name])
        .context("Failed to assign IP to bridge")?;

    // Bring up
    run_cmd("ip", &["link", "set", &config.bridge_name, "up"])
        .context("Failed to bring bridge up")?;

    info!(
        "Created bridge {} with IP {}",
        config.bridge_name, config.bridge_ip
    );
    Ok(())
}

/// Sets up network namespace isolation for a running agent process.
///
/// Creates veth pair, moves peer into agent's network namespace (identified by PID),
/// configures IP addresses and routes, and loads nftables rules.
///
/// Must be called AFTER bwrap spawns (needs PID for nsenter).
pub fn setup_netns(pid: u32, config: &NetworkNsConfig) -> Result<()> {
    let veth_host = config.veth_host_name();
    let veth_peer = config.veth_peer_name();
    let agent_ip = config.agent_ip();
    let pid_str = pid.to_string();

    // 1. Create veth pair
    run_cmd(
        "ip",
        &[
            "link", "add", &veth_host, "type", "veth", "peer", "name", &veth_peer,
        ],
    )
    .with_context(|| format!("Failed to create veth pair {veth_host}<->{veth_peer}"))?;

    // 2. Attach host-side to bridge
    run_cmd(
        "ip",
        &["link", "set", &veth_host, "master", &config.bridge_name],
    )
    .with_context(|| format!("Failed to attach {veth_host} to bridge"))?;

    // 3. Bring host-side up
    run_cmd("ip", &["link", "set", &veth_host, "up"])
        .with_context(|| format!("Failed to bring {veth_host} up"))?;

    // 4. Move peer into agent's network namespace
    run_cmd("ip", &["link", "set", &veth_peer, "netns", &pid_str])
        .with_context(|| format!("Failed to move {veth_peer} into PID {pid} netns"))?;

    // 5. Configure peer inside agent NS (via nsenter)
    let cidr = format!("{agent_ip}/{}", config.bridge_prefix);
    nsenter_cmd(pid, &["ip", "addr", "add", &cidr, "dev", &veth_peer])
        .context("Failed to assign IP to veth peer")?;

    nsenter_cmd(pid, &["ip", "link", "set", &veth_peer, "up"])
        .context("Failed to bring veth peer up")?;

    nsenter_cmd(pid, &["ip", "link", "set", "lo", "up"]).context("Failed to bring loopback up")?;

    // 6. Add default route via bridge
    nsenter_cmd(
        pid,
        &["ip", "route", "add", "default", "via", &config.bridge_ip],
    )
    .context("Failed to add default route")?;

    // 7. Load nftables rules inside agent NS
    let rules = config.generate_nftables_rules();
    nsenter_nft(pid, &rules).context("Failed to load nftables rules")?;

    info!(
        "Network namespace configured for agent {} (PID {pid}, IP {agent_ip})",
        config.agent_name
    );
    Ok(())
}

/// Tears down network namespace resources for an agent.
///
/// Deletes the host-side veth interface. The peer inside the agent NS
/// is automatically removed when the host-side is deleted.
pub fn teardown_netns(config: &NetworkNsConfig) -> Result<()> {
    let veth_host = config.veth_host_name();

    // Deleting host-side veth automatically removes the peer
    let result = Command::new("ip")
        .args(["link", "del", &veth_host])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match result {
        Ok(status) if status.success() => {
            info!("Removed veth {veth_host} for agent {}", config.agent_name);
        }
        _ => {
            // Non-fatal: veth might already be gone (agent died)
            warn!("Could not remove veth {veth_host} (may already be cleaned up)");
        }
    }

    Ok(())
}

/// Run a command, returning Ok(()) on success or Err on failure.
fn run_cmd(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .with_context(|| format!("Failed to execute: {program} {}", args.join(" ")))?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "{} {} exited with status {}",
            program,
            args.join(" "),
            status
        );
    }
}

/// Run a command inside an agent's network namespace via nsenter.
fn nsenter_cmd(pid: u32, args: &[&str]) -> Result<()> {
    let pid_str = pid.to_string();
    let mut full_args = vec!["-t", &pid_str, "-n", "--"];
    full_args.extend(args);
    run_cmd("nsenter", &full_args)
}

/// Load nftables rules inside an agent's network namespace via nsenter + nft -f.
fn nsenter_nft(pid: u32, rules: &str) -> Result<()> {
    let pid_str = pid.to_string();
    let output = Command::new("nsenter")
        .args(["-t", &pid_str, "-n", "--", "nft", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn nsenter for nft")?;

    use std::io::Write;
    let mut child = output;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(rules.as_bytes())
            .context("Failed to write nftables rules to stdin")?;
    }

    let status = child.wait().context("Failed to wait for nsenter nft")?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("nft rules load failed with status {status}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let config = NetworkNsConfig::for_agent("thomas", 0);
        assert_eq!(config.agent_name, "thomas");
        assert_eq!(config.agent_index, 0);
        assert_eq!(config.zenoh_port, 7447);
        assert_eq!(config.cortex_port, 8080);
        assert_eq!(config.bridge_name, "br-sentinel");
        assert_eq!(config.bridge_ip, "10.42.0.1");
        assert_eq!(config.bridge_prefix, 16);
    }

    #[test]
    fn agent_ip_computation() {
        assert_eq!(NetworkNsConfig::for_agent("a", 0).agent_ip(), "10.42.0.2");
        assert_eq!(NetworkNsConfig::for_agent("b", 1).agent_ip(), "10.42.0.3");
        assert_eq!(NetworkNsConfig::for_agent("c", 52).agent_ip(), "10.42.0.54");
        assert_eq!(
            NetworkNsConfig::for_agent("d", 253).agent_ip(),
            "10.42.0.255"
        );
    }

    #[test]
    fn veth_host_name_truncation() {
        // Short name — no truncation
        let config = NetworkNsConfig::for_agent("thomas", 0);
        assert_eq!(config.veth_host_name(), "veth-thomas");
        assert!(config.veth_host_name().len() <= MAX_IFNAME);

        // Long name — must truncate to 15 chars
        let long = NetworkNsConfig::for_agent("agent-01-thomas-ceo", 0);
        let name = long.veth_host_name();
        assert_eq!(name.len(), MAX_IFNAME);
        assert!(name.starts_with("veth-"));
    }

    #[test]
    fn veth_peer_name_truncation() {
        let config = NetworkNsConfig::for_agent("thomas", 0);
        assert_eq!(config.veth_peer_name(), "vp-thomas");
        assert!(config.veth_peer_name().len() <= MAX_IFNAME);

        let long = NetworkNsConfig::for_agent("agent-01-thomas-ceo", 0);
        let name = long.veth_peer_name();
        assert_eq!(name.len(), MAX_IFNAME);
        assert!(name.starts_with("vp-"));
    }

    #[test]
    fn nftables_contains_allowed_ports() {
        let config = NetworkNsConfig::for_agent("thomas", 0);
        let rules = config.generate_nftables_rules();
        assert!(rules.contains("tcp dport 7447 accept"));
        assert!(rules.contains("tcp dport 8080 accept"));
        assert!(rules.contains(&config.bridge_ip));
    }

    #[test]
    fn nftables_policy_is_drop() {
        let config = NetworkNsConfig::for_agent("thomas", 0);
        let rules = config.generate_nftables_rules();
        assert!(rules.contains("policy drop"));
        // Should appear twice (input + output chains)
        let drop_count = rules.matches("policy drop").count();
        assert_eq!(
            drop_count, 2,
            "Expected DROP policy on both input and output chains"
        );
    }

    #[test]
    fn different_agents_different_ips() {
        let a = NetworkNsConfig::for_agent("thomas", 0);
        let b = NetworkNsConfig::for_agent("lisa", 1);
        let c = NetworkNsConfig::for_agent("andreas", 2);
        assert_ne!(a.agent_ip(), b.agent_ip());
        assert_ne!(b.agent_ip(), c.agent_ip());
        assert_ne!(a.agent_ip(), c.agent_ip());
    }
}
