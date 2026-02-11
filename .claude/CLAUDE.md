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
- Files editieren ohne sie vorher zu lesen (Read before Edit!)
- "production ready" behaupten ohne Evidence
- Architektur-Entscheidungen treffen die vom Plan abweichen ohne User-Freigabe
- GitHub Actions auf Tags referenzieren (`@v4`) - IMMER SHA-Pins verwenden
- `allow(unused)` ohne Kommentar warum
- Raten bei fehlenden Informationen - FRAGE stattdessen
- Fehler oder Warnungen stillschweigend ignorieren
- Destructive git commands ohne explizite User-Freigabe (`push --force`, `reset --hard`, `clean -fd`)

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
- Lessons-Check nach jedem abgeschlossenen Schritt (siehe PROJEKT-LEARNINGS)
- Neue Features = neue Tests, Bug Fixes = Regression Test

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

### Architektur-Mindset
- NIEMALS in Phasen denken - IMMER den Endausbau im Blick behalten
- Endausbau = Claude Code + Cortex Gateway (permanent, NICHT Migration weg von CC)
- System muss modell-agnostisch sein (Claude, Qwen3, BitNet als Provider hinter Cortex Gateway)

### Dependency- und Tool-Management (PFLICHT)
- IMMER die **aktuellste Version** aller Tools, Libraries, Actions und Configs verwenden
- Nach JEDEM Feature-Install: `cargo update`, Go-Module updaten, Action-SHAs pruefen
- `cargo update` vor jedem PR, GitHub Actions SHA-Pins vor PR pruefen
- Bei Major-Version-Bumps: Breaking Changes pruefen, Config migrieren
- deny.toml: cargo-deny v2 Format, golangci-lint: `.golangci.yml` v2-Format
- Nach Updates IMMER verifizieren: `make ci`

### Teammate-Management
- Teammates die Tasks wiederholt ignorieren: Frueh terminieren
- Selbst fixen ist oft schneller als wiederholtes Anweisen
- Jeder Teammate arbeitet nach DIESER CLAUDE.md - sie ist autoritativ

---

## REQUIRED GUIDELINES
### Code Quality
- Hot Path: keine Allocations, Arena-Allokatoren (Details siehe Plan)
- Serialisierung intern: Zero-Copy, extern: MessagePack (Dashboard, Logs)
- Code-Identifier: Englisch, Kommentare: Deutsch erlaubt bei Domain-Logik
- Keine Magic Numbers - Konstanten definieren
- Kein toter Code - aufraeumen statt auskommentieren

### Code Style
- Rust: `cargo fmt` + `cargo clippy -- -D warnings` + `RUSTDOCFLAGS="-D warnings" cargo doc` (zero warnings)
- Rust: `rustfmt.toml` (max_width=100), `clippy.toml` (Thresholds)
- Go: `gofmt` + `go vet` + `golangci-lint` (`.golangci.yml`, v2-Format!)
- TypeScript: Bun-native, kein extra Formatter
- Alle: `typos` Spellcheck (`typos.toml`) - keine Tippfehler in Code oder Docs

### Testing
- Neue Features = neue Tests (Unit + ggf. Integration)
- Bug Fixes = Regression Test der den Bug reproduziert
- Refactoring = alle bestehenden Tests muessen gruen bleiben
- Float-Vergleiche: IMMER `approx` Crate (`assert_relative_eq!`)
- Performance-Tests fuer Hot-Path Code (>100 ticks/s Schwellenwert)

### Supply-Chain-Security
- Alle GitHub Actions auf volle Commit-SHAs gepinnt (nie `@v4`, immer `@sha # v4.x.y`)
- `deny.toml`: erlaubte Licenses, Advisory-Policy, Crate-Bans, Source-Restrictions
- Bei neuen Dependencies: `cargo deny check` + ggf. `deny.toml` anpassen

---

## WORKFLOWS
### PR Workflow
1. Branch: `feat/beschreibung` oder `fix/beschreibung`
2. Lokal: `make ci` (muss gruen sein - inkl. deny check)
3. Push + PR erstellen (Conventional Commit Titel)
4. CI laeuft automatisch (nur betroffene Jobs via path-filter)
5. Review → Merge → Branch loeschen

### Complex Task Workflow (fuer Refactoring, neue Features, Bug-Fixing)
1. **ANALYZE**: Alle relevanten Files lesen, Dependencies identifizieren
2. **PLAN**: Aenderungen auflisten, Risiken identifizieren, Erfolgskriterien definieren
3. **EXECUTE**: Plan Schritt fuer Schritt ausfuehren, nach jeder Aenderung testen
4. **VERIFY**: Tests + Lints + manuelle Verifikation, Confidence Level angeben

### Debug Workflow
1. Fehler reproduzieren (exakter Command + Output)
2. Hypothesen aufstellen (maximal 3)
3. Systematisch eingrenzen (bisect, logs, minimale Reproduktion)
4. Fix implementieren + Regression Test schreiben

### VERIFY nach jedem Schritt (PFLICHT)
```
□ Tests ausgefuehrt? → Command + Output
□ Lints bestanden? → cargo fmt --check + clippy + deny + typos + doc
□ Manuell verifiziert? → Was geprueft?
□ Lessons-Check: Unerwartetes Verhalten?
  → JA: Sofort in PROJEKT-LEARNINGS dokumentieren
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
| **Docs** | `make doc` |
| **Unused Deps** | `make machete` |
| **FlatBuffer Gen** | `make generate` |
| **Security Audit** | `make security` |
| **Benchmarks** | `make bench` |

### Verzeichnisstruktur
```
crates/              # Rust Workspace
cmd/cortex-gateway/  # Go LLM Proxy
dashboard/           # Bun + Hono Frontend/Backend
schemas/             # FlatBuffer Definitionen (.fbs)
config/              # Raum-Layout, Agent-Defs, Simulations-Parameter
.claude/rules/       # Modulare Regeln (Domain Knowledge, etc.)
```

### CI/CD Workflows (alle Actions SHA-gepinnt, Concurrency-Groups aktiv)
- **ci.yml**: Path-filtered CI (lint, typos, rust+doc+machete, go, dashboard, schemas, config)
- **deny.yml**: cargo-deny (Advisories, Licenses, Bans, Sources) - parallel
- **coverage.yml**: cargo-tarpaulin + Codecov
- **scorecard.yml**: OSSF Scorecard (woechentlich)
- **security.yml**: cargo audit + govulncheck + npm audit
- **pr-lint.yml**: Conventional Commits
- **release.yml**: Tag-triggered Release + SBOM (CycloneDX)

---

## REFERENCES (SSOT)
| Was | Authoritative Quelle |
|-----|---------------------|
| Architektur + Tech-Entscheidungen | `/home/jan/.claude/plans/peaceful-splashing-willow.md` |
| Bio-Engine Formeln + Schwellenwerte | Plan Sektion 2b + `.claude/rules/sprint2-domain.md` |
| Sprint 2 Domain Knowledge | `.claude/rules/sprint2-domain.md` |
| Raum-Layout + Adjacency | `config/rooms.toml` |
| Agent-Persoenlichkeiten | `config/agents/AGENT-XX-NAME.toml` |
| FlatBuffer Schemas | `schemas/*.fbs` |
| Supply-Chain Policy | `deny.toml` |
| Rust Format/Clippy Config | `rustfmt.toml`, `clippy.toml` |
| Go Lint Config | `.golangci.yml` |
| Typos Config | `typos.toml` |
| Audit Ignores | `.cargo/audit.toml` |
| Changelog | `CHANGELOG.md` |
| Cortex Gateway Config | `config/cortex-gateway.toml` |
| Go Package Layout | `cmd/cortex-gateway/internal/` |
| Sprint 3 Domain Knowledge | `.claude/rules/sprint3-domain.md` |

**Regel:** Plan ist SSOT fuer Architektur. Wenn Plan und Code divergieren → Plan gewinnt.

---

## PROJEKT-LEARNINGS

_Erkenntnisse aus der Implementierung. Format: Datum, Kontext, was gelernt._

### NIEMALS (gelernt)
- 2026-02-11: PR mergen ohne `make ci` (fmt+clippy+test) lokal/remote verifiziert zu haben.
- 2026-02-11: GitHub Actions mit Tags (`@v4`) referenzieren - Supply-Chain-Attacke moeglich.

### IMMER (gelernt)
- 2026-02-11: `cargo fmt --all -- --check` + `cargo clippy -- -D warnings` VOR Push. Nicht nur `cargo test`.
- 2026-02-11: CHANGELOG.md bei JEDEM PR aktualisieren, nicht erst nachtraeglich.
- 2026-02-11: Consumer-Crates: Feature-Gates in eigener Cargo.toml deklarieren: `telemetry = ["sentinel-telemetry/telemetry"]`
- 2026-02-11: `cargo remote -- fmt` synct NICHT zurueck. `cargo fmt` lokal ausfuehren.
- 2026-02-11: deny.toml IMMER aktualisieren bei neuen Dependencies.

### IMMER (gelernt, Sprint 2)
- 2026-02-11: Zirkulaere Crate-Abhaengigkeiten frueh erkennen. Component-Typen gehoeren in die unterste Schicht (sentinel-common), NICHT in sentinel-ecs.
- 2026-02-11: ECS Component-Name darf NICHT mit Message-Struct kollidieren. Loesung: `PerceptionState` (Component) vs `Perception` (Message in types.rs).
- 2026-02-11: bevy_ecs `Res<T>` fuer System-Parameter: Resource MUSS in World inserted sein BEVOR Schedule::run(), sonst Panic.
- 2026-02-11: Deutsche Woerter in Rust-Strings (perception text) muessen ALLE in typos.toml stehen - CI-typos-Check prueft auch String-Literale.

### Kontext-Wissen
- 2026-02-11: BioStateUpdate::new() hat bewusst viele Args (10) - `#[allow(clippy::too_many_arguments)]` ist ok.
- 2026-02-11: `ring` Crate: spezielle License-Clarification in deny.toml (MIT AND ISC AND OpenSSL).
- 2026-02-11: cargo-deny v2: `vulnerability`/`notice` entfernt, `unmaintained`/`unsound` sind Scope-Werte.
- 2026-02-11: golangci-lint v2: `version: "2"` noetig, `gosimple` in `staticcheck` gemerged.
- 2026-02-11: GitHub Actions Major-Bumps koennen Breaking Changes haben. IMMER Release Notes pruefen.

### IMMER (gelernt, Sprint 3)
- 2026-02-11: Go structs mit `sync.RWMutex` duerfen NICHT by-value kopiert/serialisiert werden. Loesung: DTO-Struct ohne Mutex (z.B. `ConfigSnapshot`).
- 2026-02-11: Prometheus `init()` MustRegister panikt bei doppelter Registrierung in Tests. Alternative: `promauto.New*` oder Custom Registry.
- 2026-02-11: Go HTTP Handler: IMMER `io.LimitReader` fuer Request/Response Bodies (Defense-in-Depth).
- 2026-02-11: HTTP Client Timeouts setzen (nicht 0 = unendlich). LLM-Calls brauchen laengere Timeouts (5min).
- 2026-02-11: Koffein-Entzug Schwellenwert (caffeine<20 + tolerance>0.3) kollidiert mit default_bio() wo caffeine=0. Tests muessen caffeine_mg explizit setzen.
- 2026-02-11: `go.work` im Repo-Root fuer Go Workspace (cmd/cortex-gateway ist ein separates Go Module).
- 2026-02-11: Deutsche Regex-Patterns in Go-Strings: typos.toml muss ALLE deutschen Woerter enthalten.
- 2026-02-11: UTF-8-sichere String-Truncation in Go: `string[:N]` kann Multi-Byte Runes kaputt machen. `utf8.Valid` pruefen oder Rune-basiert truncaten.

### Kontext-Wissen (Sprint 2)
- 2026-02-11: Dependency-Graph Sprint 2: common(+bevy_ecs) → bio + physics → ecs. NICHT umgekehrt.
- 2026-02-11: sentinel-ecs re-exportiert Components aus sentinel-common. Externe Crates koennen beides nutzen.
- 2026-02-11: SimulationTime Resource muss AUSSERHALB des Schedules aktualisiert werden (vor schedule.run()).
- 2026-02-11: Physics-Berechnungen sind pro RAUM (nicht pro Agent). Agenten pro Raum zaehlen via Position-Query.
- 2026-02-11: Mood-System nutzt Valenz-Arousal-Modell. Quadranten-Mapping auf 10 Emotionen (Emotion enum in types.rs).
