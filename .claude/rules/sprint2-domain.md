---
id: SPRINT2-DOMAIN
status: Stable
description: Domain Knowledge fuer Sprint 2 (World Simulation)
---

# Sprint 2 - Domain Knowledge (World Simulation)

## Architektur-Ueberblick

Neuro-Symbolischer Ansatz: **ECS** (deterministische Weltregeln) + **LLM** (probabilistische Agent-Entscheidungen).
- ECS berechnet Bio-Zustaende, Physik, Raeume → deterministisch, reproduzierbar
- LLM empfaengt Wahrnehmungs-Texte, entscheidet Aktionen → kreativ, nicht-deterministisch
- Agents wissen NICHT dass sie simuliert werden (Fourth-Wall-Prinzip)

## ECS (Entity Component System) - sentinel-ecs

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

## Bio-Engine - sentinel-bio

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

## Physics Engine - sentinel-physics

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

## Room System - config/rooms.toml

**15 Raeume, 2 Stockwerke:**
- EG (7): empfang, flur-eg, kueche, buero-dev-1, buero-dev-2, meetingraum-01, toilette-eg
- Verbindung: treppenhaus (floor=-1, stockwerk-uebergreifend)
- OG (7): flur-og, buero-design-1, buero-design-2, buero-ceo, meetingraum-02, meetingraum-03, toilette-og

**Raum-Typen:** office, meeting, common, break, transit, bathroom
**Adjacency:** MUSS bidirektional sein (wenn A→B, dann B→A)
**Validierung:** `BuildingConfig::validate(min_capacity=15)` prueft Referenz-Integritaet

## Schichtmodell (54 Agents)

| Set | Schicht | Agenten | Stunden |
|-----|---------|---------|---------|
| 1 | Frueh | AGENT-01 bis 15 | 06-14 |
| 2 | Mittel | AGENT-16 bis 30 | 14-22 |
| 3 | Spaet | AGENT-31 bis 45 | 22-06 |
| 0 | Sonder | AGENT-46 bis 54 | 24/7 |

Max 15+9=24 Agents gleichzeitig (eine Schicht + Sonder-Set).

## Crate-Dependency-Map (Sprint 2)

```
sentinel-ecs ──────→ sentinel-common, bevy_ecs
sentinel-bio ──────→ sentinel-ecs (fuer BioState, Personality, WorkContext Typen)
sentinel-physics ──→ sentinel-common (fuer RoomId, Tick), sentinel-ecs (fuer Position)
sentinel-common ───→ toml (fuer rooms.toml Parsing), serde, anyhow
```

**Reihenfolge:** common → ecs → bio + physics (bio und physics sind parallel moeglich)

## Naming Conventions (Sprint 2)

| Konzept | Rust Identifier | Beispiel |
|---------|-----------------|---------|
| Raum-ID | snake_case String | `"buero-dev-1"`, `"kueche"` |
| Agent-ID | `AGENT-XX` Pattern | `"AGENT-01"`, `"AGENT-46"` |
| ECS Component | PascalCase Struct | `BioState`, `AgentIdentity` |
| ECS System | snake_case Funktion | `bio_system`, `transit_system` |
| SystemSet | PascalCase Enum | `SimulationPhase::Biology` |
| Bio-Parameter | snake_case f32 | `hunger`, `caffeine_mg`, `social_need` |
| Physics-Parameter | snake_case f32/f64 | `noise_db`, `temperature_c`, `co2_ppm` |

## Performance Constraints
- **Tick-Rate:** >100 ticks/s (brauchen nur 1-10, aber massive Reserve halten)
- **Hot Path:** Keine Heap-Allocations in Systems (Arena-Allokatoren wenn noetig)
- **Float-Typ:** `f32` (NICHT f64) fuer alle Bio/Physics-Werte (Cache-Effizienz)
- **ECS Layout:** bevy_ecs nutzt SoA (Struct of Arrays) - Cache-optimiert
- **Tick Duration:** <500µs Ziel, geloggt via tracing
- **Benchmark-Pflicht:** Performance-Tests fuer 100 Ticks mit 15 Agents

## Testing Guidance (Sprint 2)
- **Float-Vergleiche:** IMMER `approx` Crate nutzen (`assert_relative_eq!`, epsilon=1.0)
- **Bio-Formeln:** Zeitschritte in 1-Minuten-Inkrementen simulieren (dt=60.0)
- **Performance-Tests:** `std::time::Instant` fuer Tick-Rate-Messung, >100 ticks/s Schwellenwert
- **Config-Tests:** `CARGO_MANIFEST_DIR` fuer relativen Pfad zu `config/rooms.toml`
- **Adjacency:** Bidirektionalitaet programmatisch testen (jede Referenz in beide Richtungen)
- **ECS-Tests:** Direkt `World::new()` + `spawn_agent()` + `Schedule::run()` (kein bevy App-Runner)
- **Neue Dependencies:** `bevy_ecs`, `approx` (dev), `rand`, `toml` - bei Einfuehrung `cargo deny check` + deny.toml pruefen
