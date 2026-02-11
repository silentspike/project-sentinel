# PROJECT SENTINEL - Claude Code Konfiguration

## Projekt
Synthetische Enterprise-Reality: 54 KI-Agents glauben echte Mitarbeiter zu sein.
Polyglot: Rust (ECS, Hot Path), Go (Cortex Gateway), Bun/TypeScript (Dashboard).

## Quick Reference

| Was | Command |
|-----|---------|
| **Alles pruefen** | `make ci` |
| **Quick Lint** | `make check` |
| **Full Lint** | `make lint-all` |
| **Tests** | `make test` |
| **Build** | `make build` |
| **Format** | `make fmt` |
| **FlatBuffer Gen** | `make generate` |
| **Security Audit** | `make security` |
| **Benchmarks** | `make bench` |

### Einzelne Targets

| Was | Command |
|-----|---------|
| Rust Build (remote!) | `cargo remote -- build` |
| Rust Tests | `cargo remote -- test` |
| Rust Clippy | `cargo remote -- clippy -- -D warnings` |
| Rust Format | `cargo fmt --all -- --check` |
| Go Build | `cd cmd/cortex-gateway && go build ./...` |
| Go Tests | `cd cmd/cortex-gateway && go test ./...` |
| Go Lint | `cd cmd/cortex-gateway && golangci-lint run` |
| Dashboard | `cd dashboard && bun install && bun test` |
| FlatBuffer | `flatc --rust -o crates/sentinel-common/src/generated schemas/*.fbs` |

## Verzeichnisstruktur

```
crates/              # Rust Workspace (10 Crates)
  sentinel-ecs/      # ECS-Kern (bevy_ecs), World Simulation
  sentinel-bio/      # Bio-Engine (Differenzialgleichungen)
  sentinel-physics/  # Akustik, Olfaktorik, Thermodynamik
  sentinel-zenoh/    # Zenoh Pub/Sub Integration
  sentinel-redb/     # redb KV-Store
  sentinel-limbo/    # Limbo async SQLite
  sentinel-sandbox/  # bwrap + Landlock + cgroups v2
  sentinel-ebpf/     # eBPF Monitoring (aya-rs)
  sentinel-wasm/     # Wasmtime + Extism Tool Runtime
  sentinel-common/   # Shared Types, FlatBuffer Schemas
cmd/cortex-gateway/  # Go LLM Proxy (Perception Injection, Fourth-Wall Detection)
dashboard/           # Bun + Hono (Backend + Vanilla JS Frontend)
schemas/             # FlatBuffer Definitionen (.fbs)
config/              # Agent-Defs, rooms.toml, simulation.toml
  agents/            # 54 AGENT-XX-NAME.toml Definitionen
bitnet/              # BitNet b1.58 CPU-Inference
deploy/              # VM-Config, systemd, init.sh
```

## Konventionen

### Commits
Conventional Commits: `feat:`, `fix:`, `perf:`, `refactor:`, `docs:`, `test:`, `ci:`, `chore:`, `deps:`

### Code Style
- Rust: `cargo fmt` + `cargo clippy -- -D warnings` (zero warnings policy)
- Go: `gofmt` + `go vet` + `golangci-lint`
- TypeScript: Bun-native, kein extra Formatter
- Code-Identifier: Englisch
- Kommentare: Deutsch erlaubt bei Domain-Logik (Bio-Engine, Agent-Persoenlichkeiten)

### Architektur-Regeln
- Hot Path (ECS Tick): Nur Rust, keine Allocations, Arena-Allokatoren
- Serialisierung intern: FlatBuffers (Zero-Copy)
- Serialisierung extern: MessagePack (Dashboard, Logs)
- Kommunikation: Zenoh Pub/Sub (nie Dateisystem!)
- State: redb (Hot), Limbo/SQLite (Cold)

## NIEMALS
- Secrets committen (.env, *.key, API Keys)
- `cargo build` lokal ausfuehren (IMMER `cargo remote`)
- Dateisystem fuer Inter-Agent-Kommunikation nutzen (Zenoh!)
- innerHTML verwenden (textContent!)
- SQL-Strings konkatenieren (Prepared Statements!)
- eval() in jeglicher Sprache

## IMMER
- `make ci` vor Push (lokale CI = Lint + Tests, schneller als GitHub Actions abwarten)
- Clippy clean (`cargo remote -- clippy -- -D warnings`)
- FlatBuffer Schema validieren bei Schema-Aenderungen
- Performance-kritischen Code benchmarken (criterion.rs)
- IOPS-Impact bedenken (DRAM-lose NVMe!)
- Conventional Commits fuer PR-Titel und Commits
- CHANGELOG.md bei user-facing Aenderungen aktualisieren

## Remote Infrastruktur
- Build-Server: `root@10.0.0.155` (LXC rustbuild, 8 Cores, 12GB RAM)
- Runtime-Host: `root@10.0.0.70` (LXC pixelperfekt-runtime)
- Proxmox: `root@10.0.0.69`

## PR Workflow
1. Branch: `feat/beschreibung` oder `fix/beschreibung`
2. Lokal: `make ci` (muss gruen sein)
3. Push + PR erstellen (Conventional Commit Titel)
4. CI laeuft automatisch (nur betroffene Jobs via path-filter)
5. Review → Merge → Branch loeschen

## CI/CD Uebersicht
- **ci.yml**: Smart path-filtered CI (Rust/Go/Dashboard/Schemas nur wenn betroffen)
- **pr-lint.yml**: Conventional Commits Validierung
- **auto-label.yml**: Automatische Labels auf Issues und PRs
- **security.yml**: Woechentlich cargo audit + govulncheck
- **codeql.yml**: Woechentlich CodeQL fuer Go + TypeScript
- **release.yml**: Tag-triggered Release mit Changelog
- **labels.yml**: Label-Sync aus .github/labels.yml

## Vollstaendiger Implementierungsplan
Siehe: `/home/jan/.claude/plans/peaceful-splashing-willow.md`
