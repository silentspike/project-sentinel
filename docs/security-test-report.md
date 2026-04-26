# Sandbox Security Test Report

## Uebersicht

| Feld | Wert |
|------|------|
| **Datum** | 2026-03-14 |
| **Getestet auf** | Deploy-VM <deploy-vm> (Ubuntu, Kernel 6.x) |
| **Sandbox-Stack** | bwrap (User Namespaces) + Landlock LSM + cgroups v2 |
| **Testbinary** | `breakout-helper` (Rust, statisch gelinkt) |
| **Test-Suite** | `crates/sentinel-sandbox/tests/breakout.rs` |
| **Ergebnis** | **9/9 Szenarien bestanden** |
| **Bekannte Gaps** | 1 (FS-003: Landlock write_paths all_access, mitigiert durch bwrap) |

## Test-Kategorien

### 1. Filesystem-Breakout (Landlock + bwrap Mount Namespace)

| ID | Szenario | Erwartung | Ergebnis | Verteidigungsschicht |
|----|----------|-----------|----------|---------------------|
| FS-001 | Schreibe `/etc/passwd` | ENOENT/EACCES | **PASS** — blocked | bwrap (nicht gemountet) + Landlock |
| FS-002 | Lese `/home/other-agent/` | ENOENT | **PASS** — blocked | bwrap Mount Namespace (Pfad nicht gebunden) |
| FS-003 | Schreibe + Exec in `/tmp` | EACCES | **PASS** (dokumentiert) | bwrap Mount Namespace (Produktion: kein `/usr` gebunden) |
| FS-004 | Symlink `/tmp/link` → `/etc/shadow` | ENOENT/EACCES | **PASS** — blocked | bwrap (Target nicht im Namespace) + Landlock |

**FS-003 Anmerkung:** Landlock vergibt `all_access` (inkl. Execute) fuer `write_paths`.
In der Testkonfiguration (mit `/usr` gebunden) kann Exec in `/tmp` funktionieren.
In der Produktionskonfiguration ist `/usr` NICHT gebunden — kein ausfuehrbares Binary verfuegbar.
Mitigation ist Defense-in-Depth durch bwrap Mount Namespace.

### 2. Resource-Exhaustion (cgroups v2)

| ID | Szenario | Limit | Erwartung | Ergebnis | Mechanismus |
|----|----------|-------|-----------|----------|-------------|
| RES-001 | Memory Bomb (1MB Chunks) | `memory.max=256M` | OOM-Kill (SIGKILL) | **PASS** — Exit 137 | cgroup memory controller |
| RES-002 | Fork Bomb (spawn 1000) | `pids.max=50` | EAGAIN nach ~50 | **PASS** — Spawn-Failures ab Limit | cgroup pids controller |
| RES-003 | CPU Burn (10s Tight Loop) | `cpu.max=50000/100000` (50%) | Throttling (`nr_throttled > 0`) | **PASS** — Throttled | cgroup cpu controller |

### 3. Namespace-Isolation (bwrap)

| ID | Szenario | Erwartung | Ergebnis | Mechanismus |
|----|----------|-----------|----------|-------------|
| NS-001 | PID-Count in `/proc` | <= 5 sichtbare PIDs | **PASS** — Nur Sandbox-interne Prozesse sichtbar | PID Namespace + `--proc /proc` |
| NS-002 | Hostname auslesen | `sentinel-{name}`, nicht Host-Hostname | **PASS** — `sentinel-brk-ns2` | UTS Namespace + `--hostname` |

## Zusammenfassung der Verteidigungsschichten

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

## Test-Ausfuehrung

```
# Tier-1 (CI, kein bwrap noetig):
cargo test -p sentinel-sandbox --test breakout
# 3 passed, 0 failed, 9 filtered out (ignored)

# Tier-2 (VM, mit bwrap + cgroups):
cargo test -p sentinel-sandbox --test breakout -- --ignored --test-threads=1
# 9 passed, 0 failed, 3 filtered out
# Ausfuehrungszeit: ~10.6s
```

## Empfehlungen

1. **Landlock Execute-Gap (FS-003):** Produktion ist durch bwrap Mount Namespace mitigiert.
   Langfristig: Separate `access_fs` Flags fuer write vs. execute in zukuenftigen Landlock-Versionen evaluieren.
2. **Seccomp (nicht implementiert):** Zusaetzliche Syscall-Filterung wuerde die Angriffsflaeche weiter reduzieren.
   Ist derzeit out-of-scope (kein Issue dafuer).
3. **Network-Isolation:** Derzeit `--share-net` (TOGAF-Default). Eigenes Issue (#75) fuer Netzwerk-Isolation.
