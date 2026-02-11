# PROJECT SENTINEL - Claude Code Konfiguration

## Projekt
Synthetische Enterprise-Reality: 54 KI-Agents glauben echte Mitarbeiter zu sein.
Polyglot: Rust (ECS, Hot Path), Go (Cortex Gateway), Bun/TypeScript (Dashboard).

## Build Commands

| Was | Command |
|-----|---------|
| Rust Build | `cargo remote -- build` |
| Rust Tests | `cargo remote -- test` |
| Rust Clippy | `cargo remote -- clippy -- -D warnings` |
| Rust Format | `cargo fmt --all -- --check` |
| Go Build | `cd cmd/cortex-gateway && go build ./...` |
| Go Tests | `cd cmd/cortex-gateway && go test ./...` |
| Go Vet | `cd cmd/cortex-gateway && go vet ./...` |
| Dashboard Install | `cd dashboard && bun install` |
| Dashboard Tests | `cd dashboard && bun test` |
| FlatBuffer Compile | `flatc --rust -o crates/sentinel-common/src/generated schemas/*.fbs` |

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
- Tests vor Commit (`cargo remote -- test && cd cmd/cortex-gateway && go test ./...`)
- Clippy clean (`cargo remote -- clippy -- -D warnings`)
- FlatBuffer Schema validieren bei Schema-Aenderungen
- Performance-kritischen Code benchmarken (criterion.rs)
- IOPS-Impact bedenken (DRAM-lose NVMe!)

## Remote Infrastruktur
- Build-Server: `root@192.0.2.155` (LXC rustbuild, 8 Cores, 12GB RAM)
- Runtime-Host: `root@192.0.2.70` (LXC pixelperfekt-runtime)
- Proxmox: `root@10.0.0.69`

## Vollstaendiger Implementierungsplan
Siehe: `/home/jan/.claude/plans/peaceful-splashing-willow.md`
