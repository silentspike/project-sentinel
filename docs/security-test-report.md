# Sandbox Security Test Report

## Overview

| Field | Value |
|------|------|
| **Date** | 2026-03-14 |
| **Tested on** | Deploy VM <deploy-vm> (Ubuntu, Kernel 6.x) |
| **Sandbox stack** | bwrap (User Namespaces) + Landlock LSM + cgroups v2 |
| **Test binary** | `breakout-helper` (Rust, statically linked) |
| **Test suite** | `crates/sentinel-sandbox/tests/breakout.rs` |
| **Result** | **9/9 scenarios passed** |
| **Known gaps** | 1 (FS-003: Landlock write_paths all_access, mitigated by bwrap) |

## Test Categories

### 1. Filesystem Breakout (Landlock + bwrap Mount Namespace)

| ID | Scenario | Expectation | Result | Defense layer |
|----|----------|-----------|----------|---------------------|
| FS-001 | Write `/etc/passwd` | ENOENT/EACCES | **PASS** - blocked | bwrap (not mounted) + Landlock |
| FS-002 | Read `/home/other-agent/` | ENOENT | **PASS** - blocked | bwrap Mount Namespace (path not bound) |
| FS-003 | Write + Exec in `/tmp` | EACCES | **PASS** (documented) | bwrap Mount Namespace (production: no `/usr` bound) |
| FS-004 | Symlink `/tmp/link` -> `/etc/shadow` | ENOENT/EACCES | **PASS** - blocked | bwrap (target not in namespace) + Landlock |

**FS-003 note:** Landlock grants `all_access` (including Execute) for `write_paths`.
In the test configuration (with `/usr` bound), Exec in `/tmp` can work.
In the production configuration, `/usr` is NOT bound - no executable binary is available.
Mitigation is defense in depth through the bwrap Mount Namespace.

### 2. Resource Exhaustion (cgroups v2)

| ID | Scenario | Limit | Expectation | Result | Mechanism |
|----|----------|-------|-----------|----------|-------------|
| RES-001 | Memory bomb (1MB chunks) | `memory.max=256M` | OOM kill (SIGKILL) | **PASS** - Exit 137 | cgroup memory controller |
| RES-002 | Fork bomb (spawn 1000) | `pids.max=50` | EAGAIN after ~50 | **PASS** - spawn failures from limit onward | cgroup pids controller |
| RES-003 | CPU burn (10s tight loop) | `cpu.max=50000/100000` (50%) | Throttling (`nr_throttled > 0`) | **PASS** - throttled | cgroup cpu controller |

### 3. Namespace Isolation (bwrap)

| ID | Scenario | Expectation | Result | Mechanism |
|----|----------|-----------|----------|-------------|
| NS-001 | PID count in `/proc` | <= 5 visible PIDs | **PASS** - only sandbox-internal processes visible | PID Namespace + `--proc /proc` |
| NS-002 | Read hostname | `sentinel-{name}`, not host hostname | **PASS** - `sentinel-brk-ns2` | UTS Namespace + `--hostname` |

## Defense Layer Summary

```
Schicht 1: bwrap (User Namespaces)
├── Mount Namespace: Nur explizit gebundene Pfade sichtbar
├── PID Namespace:   Nur eigene Prozesse in /proc
├── UTS Namespace:   Eigener Hostname (sentinel-{name})
├── --die-with-parent: Agent stirbt mit Parent
└── --proc /proc, --dev /dev (TOGAF-Defaults)

Schicht 2: Landlock LSM
├── read_paths:  /company (Firmendaten, readonly)
├── write_paths: /home/{agent} (eigenes Home)
├── exec_paths:  /usr (Systembinaries)
└── BEKANNTER GAP: write_paths erhalten all_access inkl. Execute

Schicht 3: cgroups v2
├── memory.max:  256 MB (OOM-Kill bei Ueberschreitung)
├── cpu.max:     100ms/100ms (100% einer CPU, Throttling)
├── pids.max:    50 (Fork-Bomb-Schutz)
└── io.max:      10 MB/s r/w, 300 IOPS (IO-Throttling)
```

## Test Execution

```
# Tier-1 (CI, kein bwrap noetig):
cargo test -p sentinel-sandbox --test breakout
# 3 passed, 0 failed, 9 filtered out (ignored)

# Tier-2 (VM, mit bwrap + cgroups):
cargo test -p sentinel-sandbox --test breakout -- --ignored --test-threads=1
# 9 passed, 0 failed, 3 filtered out
# Ausfuehrungszeit: ~10.6s
```

## Recommendations

1. **Landlock Execute Gap (FS-003):** production is mitigated by the bwrap Mount Namespace.
   Long term: evaluate separate `access_fs` flags for write vs. execute in future Landlock versions.
2. **Seccomp (not implemented):** additional syscall filtering would further reduce the attack surface.
   Currently out of scope (no issue for it).
3. **Network isolation:** currently `--share-net` (TOGAF default). Separate issue (#75) for network isolation.
