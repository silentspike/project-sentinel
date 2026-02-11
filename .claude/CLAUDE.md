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

## DOMAIN KNOWLEDGE (Sprint 2 - World Simulation)

### Architektur-Ueberblick

Neuro-Symbolischer Ansatz: **ECS** (deterministische Weltregeln) + **LLM** (probabilistische Agent-Entscheidungen).
- ECS berechnet Bio-Zustaende, Physik, Raeume → deterministisch, reproduzierbar
- LLM empfaengt Wahrnehmungs-Texte, entscheidet Aktionen → kreativ, nicht-deterministisch
- Agents wissen NICHT dass sie simuliert werden (Fourth-Wall-Prinzip)

### ECS (Entity Component System) - sentinel-ecs

**Framework:** `bevy_ecs` (NICHT bevy full! Nur ECS-Kern, kein Rendering)

**10 Components pro Agent:**
`AgentIdentity`, `Position`, `BioState`, `Personality`, `Mood`, `Perception`, `WorkContext`, `Relationships`, `LlmConfig`, `ShiftInfo`

**9 Systems (EXAKTE Reihenfolge via SystemSets):**
1. `input_system` → 2. `bio_system` → 3. `physics_system` → 4. `transit_system` → 5. `chaos_system` → 6. `mood_system` → 7. `perception_system` → 8. `output_system` → 9. `persist_system`

**Tick Rate:** 1-10 Hz (konfigurierbar). Performance-Ziel: >100 ticks/s (massive Reserve).

**Regeln:**
- Components sind Data-Only Structs (kein Business-Logic in Components)
- Systems operieren auf Component-Queries (`Query<(&mut BioState, &Personality)>`)
- System-Reihenfolge via `SimulationPhase` enum + `configure_sets`
- Keine `App`/`World`-Erstellung ausserhalb von `world.rs`

### Bio-Engine - sentinel-bio

**6 biologische Parameter mit Formeln:**

| Parameter | Modell | Rate | Bereich |
|-----------|--------|------|---------|
| Hunger | Linear | +12.5/h | 0-100 |
| Energie | Circadian + Penalties | tageszeit-abhaengig | 0-100 |
| Koffein | Exponential-Decay | t½ = 5.7h (20520s) | 0-∞ mg |
| Blasendrang | Linear + Koffein-Multiplikator | +12/h, ×1.5 bei >50mg | 0-100 |
| Stress | Gewichteter Multi-Faktor | 0.3×Meeting + 0.3×Deadline + 0.2×Conflict + 0.2×Bio | 0-100 |
| Sozial | Persoenlichkeits-abhaengig | Extra: +10/h, Intro: -5/h | 0-100 |

**3 Action-Funktionen:** `drink_coffee()` (+95mg), `eat_meal()` (hunger=0), `use_bathroom()` (bladder=0)

**Wichtig:**
- Alle Werte `f32`, IMMER `.clamp(0.0, 100.0)` (ausser Koffein)
- Koffein-Decay: `C(t) = C0 × e^(-ln(2)/20520 × dt)` - NICHT linear!
- Neurotizismus skaliert Stress-Sensitivitaet: `0.5 + neuroticism × 0.5`
- Morning-Person vs Night-Owl hat unterschiedliche Energie-Kurven
- Tests nutzen `approx` Crate fuer Floating-Point-Vergleiche (epsilon=1.0)

### Physics Engine - sentinel-physics

**5 Sub-Systeme:**

| System | Berechnet | Einheit | Formel-Typ |
|--------|-----------|---------|------------|
| Acoustics | Laermpegel pro Raum | dB | 30 + agents×5 + activity |
| Temperature | Raumtemperatur | °C | base + body_heat + window |
| CO2 | Luftqualitaet | ppm | base(400) + agents×40/h - ventilation |
| Smell | Geruchs-Propagation | 0-1 | intensity - decay_per_room × distance |
| Chaos | Zufallsereignisse | Events | Poisson-verteilt (PhoneRing, PrinterBroken, ...) |

**Akustik-Schwellenwerte:** <35dB ruhig, 35-50 normal, 50-65 laut, 65-80 sehr laut, >80 unertraeglich
**CO2-Schwellenwerte:** <600ppm unsichtbar, 600-1000 frische Luft, 1000-1500 stickig, >1500 Schwindel

**Wichtig:**
- Gerueche propagieren ueber `adjacent` Raeume mit Decay pro Raum
- Chaos-Events sind Poisson-verteilt (nicht gleichverteilt!)
- Transit (Raumwechsel) dauert 2-5 Minuten, ~30% Flurbegegnungs-Wahrscheinlichkeit
- Physik-Berechnung ist pro RAUM, nicht pro Agent

### Room System - config/rooms.toml

**15 Raeume, 2 Stockwerke:**
- EG (7): empfang, flur-eg, kueche, buero-dev-1, buero-dev-2, meetingraum-01, toilette-eg
- Verbindung: treppenhaus (floor=-1, stockwerk-uebergreifend)
- OG (7): flur-og, buero-design-1, buero-design-2, buero-ceo, meetingraum-02, meetingraum-03, toilette-og

**Raum-Typen:** office, meeting, common, break, transit, bathroom
**Adjacency:** MUSS bidirektional sein (wenn A→B, dann B→A)
**Validierung:** `BuildingConfig::validate(min_capacity=15)` prueft Referenz-Integritaet

### Schichtmodell (54 Agents)

| Set | Schicht | Agenten | Stunden |
|-----|---------|---------|---------|
| 1 | Frueh | AGENT-01 bis 15 | 06-14 |
| 2 | Mittel | AGENT-16 bis 30 | 14-22 |
| 3 | Spaet | AGENT-31 bis 45 | 22-06 |
| 0 | Sonder | AGENT-46 bis 54 | 24/7 |

Max 15+9=24 Agents gleichzeitig (eine Schicht + Sonder-Set).

### Crate-Dependency-Map (Sprint 2)

```
sentinel-ecs ──────→ sentinel-common, bevy_ecs
sentinel-bio ──────→ sentinel-ecs (fuer BioState, Personality, WorkContext Typen)
sentinel-physics ──→ sentinel-common (fuer RoomId, Tick), sentinel-ecs (fuer Position)
sentinel-common ───→ toml (fuer rooms.toml Parsing), serde, anyhow
```

**Reihenfolge:** common → ecs → bio + physics (bio und physics sind parallel moeglich)

### Naming Conventions (Sprint 2)

| Konzept | Rust Identifier | Beispiel |
|---------|-----------------|---------|
| Raum-ID | snake_case String | `"buero-dev-1"`, `"kueche"` |
| Agent-ID | `AGENT-XX` Pattern | `"AGENT-01"`, `"AGENT-46"` |
| ECS Component | PascalCase Struct | `BioState`, `AgentIdentity` |
| ECS System | snake_case Funktion | `bio_system`, `transit_system` |
| SystemSet | PascalCase Enum | `SimulationPhase::Biology` |
| Bio-Parameter | snake_case f32 | `hunger`, `caffeine_mg`, `social_need` |
| Physics-Parameter | snake_case f32/f64 | `noise_db`, `temperature_c`, `co2_ppm` |

### Performance Constraints

- **Tick-Rate:** >100 ticks/s (brauchen nur 1-10, aber massive Reserve halten)
- **Hot Path:** Keine Heap-Allocations in Systems (Arena-Allokatoren wenn noetig)
- **Float-Typ:** `f32` (NICHT f64) fuer alle Bio/Physics-Werte (Cache-Effizienz)
- **ECS Layout:** bevy_ecs nutzt SoA (Struct of Arrays) - Cache-optimiert
- **Tick Duration:** <500µs Ziel, geloggt via tracing
- **Benchmark-Pflicht:** Performance-Tests fuer 100 Ticks mit 15 Agents

### Testing Guidance (Sprint 2)

- **Float-Vergleiche:** IMMER `approx` Crate nutzen (`assert_relative_eq!`, epsilon=1.0)
- **Bio-Formeln:** Zeitschritte in 1-Minuten-Inkrementen simulieren (dt=60.0)
- **Performance-Tests:** `std::time::Instant` fuer Tick-Rate-Messung, >100 ticks/s Schwellenwert
- **Config-Tests:** `CARGO_MANIFEST_DIR` fuer relativen Pfad zu `config/rooms.toml`
- **Adjacency:** Bidirektionalitaet programmatisch testen (jede Referenz in beide Richtungen)
- **ECS-Tests:** Direkt `World::new()` + `spawn_agent()` + `Schedule::run()` (kein bevy App-Runner)
- **Neue Dependencies:** `bevy_ecs`, `approx` (dev), `rand`, `toml` - bei Einfuehrung `cargo deny check` + deny.toml pruefen

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
