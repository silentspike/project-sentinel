//! Benchmarks fuer sentinel-sandbox Operationen.
//!
//! Misst Sandbox-Setup-Overhead:
//! - BwrapConfig to_args() Generierung
//! - Landlock Ruleset Erstellung
//! - PSI String-Parsing
//! - cgroup create + remove Zyklus (VM only)
//! - SandboxEnforcer setup + teardown (VM only)
//! - #75 full-cage netns isolation verifier overhead (readlink + classify)
//! - bwrap full-cage spawn latency (Tier 2, VM only)
//!
//! WICHTIG: Die VM-only Benchmarks liefern nur auf der Deployment-VM
//! (<deploy-vm>) aussagekraeftige Ergebnisse. Auf anderen Systemen
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

// --- #75: full-cage netns isolation verifier (run everywhere) ---

fn bench_netns_verify(c: &mut Criterion) {
    use sentinel_sandbox::SandboxEnforcer;

    // Overhead of the post-spawn isolation check: two readlink(/proc/<pid>/ns/net)
    // + parse + classify. Probing the current PID against the daemon's own netns
    // exercises the exact code path (result is NotIsolated here, which is fine —
    // we measure cost, not outcome).
    let (enforcer, _) = SandboxEnforcer::detect();
    let pid = std::process::id();
    c.bench_function("netns_verify", |b| {
        b.iter(|| {
            std::hint::black_box(enforcer.verify_agent_netns_isolation(pid));
        });
    });
}

// --- Tier 2: bwrap spawn latency (VM only) ---

fn bench_bwrap_spawn_latency(c: &mut Criterion) {
    if !BwrapConfig::test_userns() {
        c.bench_function("bwrap_spawn_latency [no bwrap — config only]", |b| {
            b.iter(|| {
                std::hint::black_box(BwrapConfig::for_agent("bench-spawn").to_args());
            });
        });
        return;
    }

    // Budget: < 10ms for bwrap spawn + "true" + exit
    let config = BwrapConfig::for_agent("bench-spawn");
    c.bench_function("bwrap_spawn_latency", |b| {
        b.iter(|| {
            let mut args = config.to_args();
            args.push("true".to_string());
            let child = std::process::Command::new("bwrap")
                .args(&args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("bwrap spawn failed");
            let status = child.wait_with_output().expect("bwrap wait failed");
            std::hint::black_box(status.status);
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
    // #75 — full-cage isolation verifier overhead
    bench_netns_verify,
    // Tier 2 — bwrap spawn latency, full cage (VM only)
    bench_bwrap_spawn_latency,
);
criterion_main!(benches);
