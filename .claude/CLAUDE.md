# CLAUDE CODE - Project Sentinel

**Sprache:** Deutsch
**Typ:** Polyglot Simulation Engine (Rust, Go, TypeScript)

---

## CRITICAL RULES (NIEMALS verletzen)

### NIEMALS
- Secrets committen (.env, *.key, API Keys, Tokens)
- `cargo build` lokal ausfuehren (IMMER `cargo remote`)
- Dateisystem fuer Inter-Prozess-Kommunikation nutzen (Pub/Sub!)
- innerHTML verwenden (textContent!)
- SQL-Strings konkatenieren (Prepared Statements!)
- eval() in jeglicher Sprache
- Files editieren ohne sie vorher zu lesen
- "production ready" behaupten ohne Evidence
- Architektur-Entscheidungen treffen die vom Plan abweichen ohne User-Freigabe

### IMMER
- `make ci` vor Push (lokale CI = Lint + Tests)
- Read before Edit - jedes File vor Bearbeitung lesen
- Conventional Commits fuer PR-Titel und Commits
- CHANGELOG.md bei user-facing Aenderungen aktualisieren
- IOPS-Impact bedenken (DRAM-lose NVMe! Budget: siehe Plan)
- Performance-kritischen Code benchmarken
- FlatBuffer Schema validieren bei Schema-Aenderungen
- Lessons-Check nach jedem abgeschlossenen Schritt (siehe Sektion unten)

### Data Exposure Rules (Repo wird public!)
ERLAUBT in Code/Docs:
- Projekt-Pfade (/work/company/project-sentinel/...)
- Beispiel-Domains, Placeholder-Daten
- Generische Architektur-Beschreibungen

VERBOTEN in Code/Docs:
- Echte Host-IPs (Proxmox, NAS, Router)
- Echter Hostname, Username, Home-Pfade
- Hardware-spezifische Interface-Namen
- Echte API-Keys, auch in Beispielen
- Interne Firmennamen/Domains die nicht zum Projekt gehoeren

---

## REQUIRED GUIDELINES

### Code Quality
- Hot Path: keine Allocations, Arena-Allokatoren (Details siehe Plan)
- Serialisierung intern: Zero-Copy (Details siehe Plan)
- Serialisierung extern: MessagePack (Dashboard, Logs)
- Code-Identifier: Englisch
- Kommentare: Deutsch erlaubt bei Domain-Logik

### Code Style
- Rust: `cargo fmt` + `cargo clippy -- -D warnings` (zero warnings)
- Go: `gofmt` + `go vet` + `golangci-lint`
- TypeScript: Bun-native, kein extra Formatter

---

## WORKFLOWS

### PR Workflow
1. Branch: `feat/beschreibung` oder `fix/beschreibung`
2. Lokal: `make ci` (muss gruen sein)
3. Push + PR erstellen (Conventional Commit Titel)
4. CI laeuft automatisch (nur betroffene Jobs via path-filter)
5. Review → Merge → Branch loeschen

### VERIFY nach jedem Schritt (PFLICHT)
```
□ Tests ausgefuehrt? → Command + Output
□ Manuell verifiziert? → Was geprueft?
□ Lessons-Check: Unerwartetes Verhalten?
  → JA: Sofort unten in "Projekt-Learnings" dokumentieren
  → NEIN: Weiter
```

---

## PROJECT CONTEXT

### Quick Reference
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
| Go Build | `cd cmd/cortex-gateway && go build ./...` |
| Go Tests | `cd cmd/cortex-gateway && go test ./...` |
| Go Lint | `cd cmd/cortex-gateway && golangci-lint run` |
| Dashboard | `cd dashboard && bun install && bun test` |

### Verzeichnisstruktur
```
crates/              # Rust Workspace
cmd/cortex-gateway/  # Go LLM Proxy
dashboard/           # Bun + Hono Frontend/Backend
schemas/             # FlatBuffer Definitionen (.fbs)
config/              # Agent-Defs, Raum-Layout, Simulations-Parameter
  agents/            # Agent-Definitionen
bitnet/              # CPU-Inference
deploy/              # VM-Config, systemd, init.sh
```

### CI/CD Workflows
- **ci.yml**: Smart path-filtered CI (nur betroffene Sprachen)
- **pr-lint.yml**: Conventional Commits Validierung
- **auto-label.yml**: Automatische Labels auf Issues und PRs
- **security.yml**: Woechentlich cargo audit + govulncheck
- **codeql.yml**: Woechentlich CodeQL fuer Go + TypeScript
- **release.yml**: Tag-triggered Release mit Changelog
- **labels.yml**: Label-Sync aus .github/labels.yml

### Remote Infrastruktur
- Build-Server: `root@192.0.2.155` (LXC rustbuild, 8 Cores, 12GB RAM)
- Runtime-Host: `root@192.0.2.70` (LXC pixelperfekt-runtime)
- Proxmox: `root@10.0.0.69`

---

## REFERENCES (SSOT)

| Was | Authoritative Quelle |
|-----|---------------------|
| Architektur + alle Tech-Entscheidungen | `/home/jan/.claude/plans/peaceful-splashing-willow.md` |
| Hardware-Constraints + IOPS-Budget | Plan Sektion 7d (VM-Konfiguration) |
| Bio-Engine Formeln + Schwellenwerte | Plan Sektion 2b (Bio-Engine) |
| Raum-Layout + Adjacency | `config/rooms.toml` |
| Agent-Persoenlichkeiten | `config/agents/AGENT-XX-NAME.toml` |
| Simulations-Parameter | `config/simulation.toml` |
| FlatBuffer Schemas | `schemas/*.fbs` |
| Changelog | `CHANGELOG.md` |
| Labels | `.github/labels.yml` |

**Regel:** Plan ist SSOT fuer Architektur. Wenn Plan und Code divergieren → Plan gewinnt, Code anpassen.

---

## PROJEKT-LEARNINGS

_Hier werden Erkenntnisse dokumentiert die waehrend der Implementierung gewonnen werden._
_Format: Datum, Kontext, was gelernt wurde._

### NIEMALS (gelernt)
_(noch leer - wird waehrend Implementierung befuellt)_

### IMMER (gelernt)
_(noch leer - wird waehrend Implementierung befuellt)_

### Kontext-Wissen
_(noch leer - wird waehrend Implementierung befuellt)_
