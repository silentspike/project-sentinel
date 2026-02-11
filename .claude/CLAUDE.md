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
- GitHub Actions auf Tags referenzieren (`@v4`) - IMMER SHA-Pins verwenden
- `allow(unused)` ohne Kommentar warum

### IMMER
- `make ci` vor Push (lokale CI = Lint + Tests + cargo deny)
- Read before Edit - jedes File vor Bearbeitung lesen
- Conventional Commits fuer PR-Titel und Commits
- CHANGELOG.md bei user-facing Aenderungen aktualisieren
- `cargo deny check` vor Push (Licenses + Advisories)
- `cargo update` vor PR (neueste kompatible Dependency-Versionen)
- Aktuellste Versionen aller Tools/Actions/Libraries verwenden
- Nach Feature-Install: Updates pruefen, Configs verifizieren, `make ci` ausfuehren
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

## TEAM-REGELN (Claude Code Teammates)

### Modell-Zuweisung
- **Lead-Rollen** (Teamlead, Architect): IMMER Opus Modell (`model: "opus"`)
- **Worker-Rollen** (Implementierung, Tests): IMMER Sonnet Modell (`model: "sonnet"`)

### Verifikation + Push
- Push to main: NUR die Hauptsession (User/Owner), KEINE Delegation an Teammates
- Teammates erstellen PRs, aber mergen NICHT selbst

### Lessons Learned (PFLICHT)
- Teamlead MUSS nach Abschluss JEDER Task alle Teammates nach Lessons Learned fragen
- Erkenntnisse werden SOFORT in dieser Datei unter PROJEKT-LEARNINGS eingetragen
- Unerwartetes Verhalten, Fehler, Workarounds - alles dokumentieren

### Architektur-Mindset
- NIEMALS in Phasen denken - IMMER den Endausbau im Blick behalten
- Endausbau = Claude Code + Cortex Gateway (permanent, NICHT Migration weg von CC)
- Cortex Gateway ist permanenter Middleware-Layer UEBER Claude Code
- System muss modell-agnostisch sein (Claude, Qwen3, BitNet als Provider hinter Cortex Gateway)
- BitNet = optionaler Cost-Saving-Layer fuer triviale Interaktionen, NICHT das Brain

### Dependency- und Tool-Management (PFLICHT)
- IMMER die **aktuellste Version** aller Tools, Libraries, Actions und Configs verwenden
- Nach JEDEM Feature-Install / Konfigurationsaenderung: `cargo update`, Go-Module updaten, Action-SHAs pruefen
- `cargo update` vor jedem PR ausfuehren (holt neueste kompatible Patch-Versionen)
- GitHub Actions: Vor PR pruefen ob neuere Major/Minor-Releases verfuegbar sind (`gh api repos/OWNER/REPO/releases/latest`)
- Bei Major-Version-Bumps von Actions: Breaking Changes pruefen, Config migrieren falls noetig
- golangci-lint: `.golangci.yml` hat `version: "2"` - bei Updates Format-Kompatibilitaet pruefen
- deny.toml: cargo-deny v2 Format (NICHT v1!) - `unmaintained`/`unsound` sind Scope-Werte (all/workspace/transitive/none)
- Nie veraltete Versionen deployen - lieber kurz recherchieren als mit altem Stand arbeiten
- Nach Updates IMMER verifizieren: `make ci` (oder mindestens die betroffenen Checks)

### Teammate-Management
- Teammates die Tasks wiederholt ignorieren oder nicht nach Instruktion arbeiten: Frueh terminieren
- Selbst fixen ist oft schneller als wiederholtes Anweisen
- Jeder Teammate arbeitet nach DIESER CLAUDE.md - sie ist autoritativ

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
- Rust: `RUSTDOCFLAGS="-D warnings" cargo doc` (zero doc-warnings)
- Rust: `rustfmt.toml` definiert max_width=100, `clippy.toml` definiert Thresholds
- Go: `gofmt` + `go vet` + `golangci-lint` (Config: `.golangci.yml`, v2-Format!)
- TypeScript: Bun-native, kein extra Formatter
- Alle: `typos` Spellcheck (Config: `typos.toml`) - keine Tippfehler in Code oder Docs

### Supply-Chain-Security
- Alle GitHub Actions MUESSEN auf volle Commit-SHAs gepinnt sein (nie `@v4`, immer `@sha # v4.x.y`)
- `deny.toml` definiert erlaubte Licenses, Advisory-Policy, Crate-Bans, Source-Restrictions
- Bei neuen Dependencies: `cargo deny check` ausfuehren und ggf. `deny.toml` anpassen
- Dependabot PRs nutzen Conventional Commit Messages (`deps(scope): ...`)

---

## WORKFLOWS

### PR Workflow
1. Branch: `feat/beschreibung` oder `fix/beschreibung`
2. Lokal: `make ci` (muss gruen sein - inkl. deny check)
3. Push + PR erstellen (Conventional Commit Titel)
4. CI laeuft automatisch (nur betroffene Jobs via path-filter)
5. Review → Merge → Branch loeschen

### VERIFY nach jedem Schritt (PFLICHT)
```
□ Tests ausgefuehrt? → Command + Output
□ Lints bestanden? → cargo fmt --check + clippy + deny + typos + doc
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
| **Supply Chain** | `make deny` |
| **Coverage** | `make coverage` |
| **Typos** | `make typos` |
| **Docs (Warnings=Error)** | `make doc` |
| **Unused Deps** | `make machete` |
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

### CI/CD Workflows (alle Actions SHA-gepinnt, Concurrency-Groups aktiv)
- **ci.yml**: Smart path-filtered CI (lint, typos, rust+doc+machete, go, dashboard, schemas)
- **pr-lint.yml**: Conventional Commits Validierung
- **auto-label.yml**: Automatische Labels auf Issues und PRs
- **deny.yml**: cargo-deny (Advisories, Licenses, Bans, Sources) - bei Cargo-Aenderungen + woechentlich
- **coverage.yml**: cargo-tarpaulin + Codecov Upload - bei Rust-Aenderungen
- **scorecard.yml**: OSSF Scorecard (woechentlich) - Security-Posture
- **security.yml**: cargo audit + govulncheck + npm audit - woechentlich
- **codeql.yml**: CodeQL SAST fuer Go + TypeScript - woechentlich
- **release.yml**: Tag-triggered Release mit Changelog + SBOM (CycloneDX)
- **labels.yml**: Label-Sync aus .github/labels.yml

### Remote Infrastruktur
- Build-Server: `root@10.0.0.155` (LXC rustbuild, 8 Cores, 12GB RAM)
- Runtime-Host: `root@10.0.0.70` (LXC pixelperfekt-runtime)
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
| Supply-Chain Policy | `deny.toml` |
| Rust Format Config | `rustfmt.toml` |
| Clippy Config | `clippy.toml` |
| Go Lint Config | `.golangci.yml` |
| Typos Config | `typos.toml` |
| Audit Ignores | `.cargo/audit.toml` |

**Regel:** Plan ist SSOT fuer Architektur. Wenn Plan und Code divergieren → Plan gewinnt, Code anpassen.

---

## PROJEKT-LEARNINGS

_Hier werden Erkenntnisse dokumentiert die waehrend der Implementierung gewonnen werden._
_Format: Datum, Kontext, was gelernt wurde._

### NIEMALS (gelernt)
- 2026-02-11: PR mergen ohne `make ci` (fmt+clippy+test) lokal/remote verifiziert zu haben. CI war FAILING nach Merge.
- 2026-02-11: GitHub Actions mit Tags (`@v4`) referenzieren - Supply-Chain-Attacke ueber gekaperte Tags moeglich. IMMER SHA-Pins.

### IMMER (gelernt)
- 2026-02-11: `cargo fmt --all -- --check` + `cargo clippy -- -D warnings` VOR Push ausfuehren. Nicht nur `cargo test`.
- 2026-02-11: CHANGELOG.md bei JEDEM PR aktualisieren, nicht erst nachtraeglich.
- 2026-02-11: Consumer-Crates muessen Feature-Gates in eigener Cargo.toml deklarieren: `telemetry = ["sentinel-telemetry/telemetry"]`
- 2026-02-11: `cargo remote -- fmt` synct formatierte Files NICHT zurueck. `cargo fmt` lokal ausfuehren (ist kein Build).
- 2026-02-11: deny.toml IMMER aktualisieren wenn neue Dependencies hinzukommen (License-Check).

### Kontext-Wissen
- 2026-02-11: BioStateUpdate::new() hat bewusst viele Args (10) - `#[allow(clippy::too_many_arguments)]` ist ok.
- 2026-02-11: rustfmt Version auf Build-Server und lokal koennen abweichen - IMMER CI-kompatible Version verifizieren.
- 2026-02-11: `ring` Crate hat spezielle License-Clarification in deny.toml (MIT AND ISC AND OpenSSL).
- 2026-02-11: cargo-deny v2 hat Config-Format geaendert: `vulnerability`/`notice` entfernt, `unmaintained`/`unsound` sind jetzt Scope-Werte (all/workspace/transitive/none) statt deny/warn.
- 2026-02-11: golangci-lint v2 hat Config-Format geaendert: `version: "2"` noetig, `gosimple` in `staticcheck` gemerged, `linters-settings` → `linters.settings`, `linters.default: none` statt implizit.
- 2026-02-11: GitHub Actions Major-Version-Bumps koennen Breaking Changes haben (z.B. golangci-lint v6→v9 erfordert golangci-lint v2 Config). IMMER Release Notes pruefen.
