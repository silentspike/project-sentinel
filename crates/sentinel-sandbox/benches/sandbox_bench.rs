//! Benchmarks fuer sentinel-sandbox Operationen.
//!
//! Misst Sandbox-Setup-Overhead:
//! - BwrapConfig to_args() Generierung
//! - Landlock Ruleset Erstellung
//! - PSI String-Parsing
//! - cgroup create + remove Zyklus (VM only)
//! - SandboxEnforcer setup + teardown (VM only)
//! - NetworkNsConfig creation + nftables generation (Tier 1)
//! - veth/nftables setup + teardown (Tier 2, VM only)
//!
//! WICHTIG: Die VM-only Benchmarks liefern nur auf der Deployment-VM
//! (10.0.0.240) aussagekraeftige Ergebnisse. Auf anderen Systemen
//! messen sie nur den Config-Overhead.

use criterion::{criterion_group, criterion_main, Criterion};
use sentinel_sandbox::BwrapConfig;

fn bench_bwrap_config_to_args(c: &mut Criterion) {
    c.bench_function("bwrap_config_to_args", |b| {
        let config = BwrapConfig::for_agent("bench-agent");
        b.iter(|| {
            std::hint::black_box(config.to_args());
        });
    });
}

fn bench_landlock_ruleset_creation(c: &mut Criterion) {
    use sentinel_sandbox::LandlockRuleset;

    c.bench_function("landlock_ruleset_creation", |b| {
        b.iter(|| {
            std::hint::black_box(LandlockRuleset::for_agent("bench-agent"));
        });
    });
}

fn bench_psi_parse(c: &mut Criterion) {
    let sample = "some avg10=1.50 avg60=2.30 avg300=0.10 total=12345\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0";

    c.bench_function("psi_parse", |b| {
        b.iter(|| {
            std::hint::black_box(sentinel_sandbox::cgroups::parse_psi(sample).unwrap());
        });
    });
}

fn bench_cgroup_create_remove(c: &mut Criterion) {
    use sentinel_sandbox::{CgroupLimits, SandboxEnforcer};

    let (enforcer, _) = SandboxEnforcer::detect();

    if !enforcer.has_cgroups() {
        // On systems without cgroup access, benchmark the limits creation only
        c.bench_function("cgroup_create_remove [no cgroup — limits only]", |b| {
            b.iter(|| {
                std::hint::black_box(CgroupLimits::default());
            });
        });
        return;
    }

    let limits = CgroupLimits::default();
    c.bench_function("cgroup_create_remove", |b| {
        b.iter(|| {
            sentinel_sandbox::cgroups::create_cgroup("bench-cgroup", &limits).unwrap();
            sentinel_sandbox::cgroups::remove_cgroup("bench-cgroup").unwrap();
        });
    });
}

fn bench_enforcer_setup_teardown(c: &mut Criterion) {
    use sentinel_sandbox::{CgroupLimits, SandboxEnforcer};
    use std::sync::Arc;

    let (enforcer, _) = SandboxEnforcer::detect();

    if !enforcer.has_cgroups() {
        c.bench_function("enforcer_setup_teardown [no cgroup — detect only]", |b| {
            b.iter(|| {
                std::hint::black_box(SandboxEnforcer::detect());
            });
        });
        return;
    }

    let enforcer = Arc::new(enforcer);
    let limits = CgroupLimits::default();

    c.bench_function("enforcer_setup_teardown", |b| {
        b.iter(|| {
            let handle = enforcer.setup_agent("bench-enforcer", &limits).unwrap();
            enforcer.teardown_agent(&handle).unwrap();
        });
    });
}

// --- Tier 1: Network Namespace config benchmarks (run everywhere) ---

fn bench_netns_config_creation(c: &mut Criterion) {
    use sentinel_sandbox::NetworkNsConfig;

    c.bench_function("netns_config_creation", |b| {
        b.iter(|| {
            std::hint::black_box(NetworkNsConfig::for_agent("bench-agent", 5));
        });
    });
}

fn bench_nftables_rules_generation(c: &mut Criterion) {
    use sentinel_sandbox::NetworkNsConfig;

    let config = NetworkNsConfig::for_agent("bench-agent", 5);
    c.bench_function("nftables_rules_generation", |b| {
        b.iter(|| {
            std::hint::black_box(config.generate_nftables_rules());
        });
    });
}

fn bench_veth_name_computation(c: &mut Criterion) {
    use sentinel_sandbox::NetworkNsConfig;

    let config = NetworkNsConfig::for_agent("agent-01-thomas-ceo", 0);
    c.bench_function("veth_name_computation", |b| {
        b.iter(|| {
            std::hint::black_box(config.veth_host_name());
            std::hint::black_box(config.veth_peer_name());
        });
    });
}

// --- Tier 2: Network Namespace system benchmarks (VM only) ---

fn bench_netns_setup_teardown(c: &mut Criterion) {
    use sentinel_sandbox::{netns, NetworkNsConfig};

    if !netns::detect_netns_support() {
        c.bench_function("netns_setup_teardown [no netns — config only]", |b| {
            b.iter(|| {
                std::hint::black_box(NetworkNsConfig::for_agent("bench-ns", 99));
            });
        });
        return;
    }

    // Budget: < 50ms for full setup + teardown
    let config = NetworkNsConfig::for_agent("bench-ns", 99);
    c.bench_function("netns_setup_teardown", |b| {
        b.iter(|| {
            // Setup bridge is idempotent, include in measurement
            netns::setup_bridge(&config).unwrap();
            // We cannot do full setup_netns without a real PID in a netns,
            // so we measure bridge setup + teardown cycle
            netns::teardown_netns(&config).ok();
        });
    });
}

fn bench_veth_creation(c: &mut Criterion) {
    use sentinel_sandbox::netns;

    if !netns::detect_netns_support() {
        c.bench_function("veth_creation [no netns — skip]", |b| {
            b.iter(|| {
                std::hint::black_box(42);
            });
        });
        return;
    }

    // Budget: < 20ms for veth pair create + delete
    c.bench_function("veth_creation", |b| {
        b.iter(|| {
            let _ = std::process::Command::new("ip")
                .args([
                    "link",
                    "add",
                    "veth-bench",
                    "type",
                    "veth",
                    "peer",
                    "name",
                    "vp-bench",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let _ = std::process::Command::new("ip")
                .args(["link", "del", "veth-bench"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    });
}

fn bench_nftables_load(c: &mut Criterion) {
    use sentinel_sandbox::{netns, NetworkNsConfig};

    if !netns::detect_netns_support() {
        c.bench_function("nftables_load [no netns — generation only]", |b| {
            let config = NetworkNsConfig::for_agent("bench-nft", 99);
            b.iter(|| {
                std::hint::black_box(config.generate_nftables_rules());
            });
        });
        return;
    }

    // Budget: < 10ms for nft ruleset load
    let config = NetworkNsConfig::for_agent("bench-nft", 99);
    c.bench_function("nftables_load", |b| {
        let rules = config.generate_nftables_rules();
        b.iter(|| {
            use std::io::Write;
            let mut child = std::process::Command::new("nft")
                .args(["-f", "-"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(rules.as_bytes()).unwrap();
            }
            child.wait().unwrap();
            // Cleanup: flush the table
            let _ = std::process::Command::new("nft")
                .args(["delete", "table", "inet", "sentinel-agent"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    });
}

criterion_group!(
    benches,
    bench_bwrap_config_to_args,
    bench_landlock_ruleset_creation,
    bench_psi_parse,
    bench_cgroup_create_remove,
    bench_enforcer_setup_teardown,
    // Tier 1 — Network Namespace
    bench_netns_config_creation,
    bench_nftables_rules_generation,
    bench_veth_name_computation,
    // Tier 2 — Network Namespace (VM only)
    bench_netns_setup_teardown,
    bench_veth_creation,
    bench_nftables_load,
);
criterion_main!(benches);
