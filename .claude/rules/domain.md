---
id: SENTINEL-DOMAIN
status: Stable
---
# Domain-Wissen: PixelPerfekt Simulation
## TL;DR
- 54 Agents, 3 Schichten + 1 Sonder-Set, 17 Raeume, 2 Etagen
- Bio-Engine mit 6 Differentialgleichungen [0.0, 1.0]
- Tick-basierte Simulation mit SimulationTime Resource
- Agent-IDs: AGENT-XX, Room-IDs: kebab-case

## Firma
PixelPerfekt GmbH, Webdesign-Agentur, Nuernberg.
Virtuelle Firmen-Simulation mit 54 Mitarbeitern als Claude-Agents.

## Schichtmodell
| Set | Uhrzeit | Agents | Beispiele |
|-----|---------|--------|-----------|
| 1 (Frueh) | 06-14 | AGENT-01 bis 15 | Thomas CEO, Lisa Design, Andreas Dev |
| 2 (Mittel) | 14-22 | AGENT-16 bis 30 | Michael CEO, Carla Design, Martin Dev |
| 3 (Spaet) | 22-06 | AGENT-31 bis 45 | Sandra CEO, Jens Design, Kevin Dev |
| 0 (Sonder) | 24/7 | AGENT-46 bis 54 | 3 Betriebsrat, 3 Psychologen, 3 Aerzte |

Schicht 0 wird NIEMALS konsolidiert (Nightrun filtert sie raus).

## Raeume (17, config/rooms.toml)
2 Etagen, bidirektionale Adjacency. Room-IDs: kebab-case.
Beispiele: `buero-dev-1`, `kueche-eg`, `konferenz-1`, `flur-eg`, `toilette-eg`

## Bio-Engine (6 Gleichungen)
| Variable | Bereich | Beschreibung |
|----------|---------|--------------|
| Hunger | [0.0, 1.0] | Steigt ueber Zeit, sinkt beim Essen |
| Energy | [0.0, 1.0] | Sinkt bei Arbeit, steigt bei Pause/Schlaf |
| Caffeine | [0.0, 1.0] | Decay-Kurve nach Koffein-Aufnahme |
| Bladder | [0.0, 1.0] | Steigt ueber Zeit + Koffein, sinkt bei Toilette |
| Stress | [0.0, 1.0] | Steigt bei Konflikten/Deadlines, sinkt bei sozialer Interaktion |
| Social Need | [0.0, 1.0] | Steigt bei Isolation, sinkt bei Gespraechen |

`caffeine_tolerance` Feld in Personality Component beeinflusst Decay.

## Mood System
Valence-Arousal Modell mit gewichteten Bio/Stress/Hunger/Social Faktoren.

## NMDA Night-Run (Memory Consolidation)
- Kein Model-Training! Konsolidiert episodische Erinnerungen.
- 6-Phase State Machine: Awake → Collecting → Scoring → Selecting → Consolidating → WakingUp
- NMDA-Score = Relevanz-Metrik fuer Episode-Selektion
- Deterministic Replay mit SHA-256 Hash Chain

## Naming Conventions
| Entitaet | Format | Beispiel |
|----------|--------|---------|
| Agent-IDs | `AGENT-XX` | AGENT-01, AGENT-54 |
| Room-IDs | kebab-case | buero-dev-1, kueche-eg |
| Event Types | snake_case | agent_action_received |
| Rust Funktionen | snake_case | consolidate_agent() |
| Rust Typen | PascalCase | DomainEvent, HashChain |
| Go | Standard Conventions | fourthWallDetector |

## Agent-Definitionen
TOML-Files in `agents/AGENT-XX-NAME.toml`.
Big Five Personality-Werte [0.0, 1.0] mit Validierung.
Felder: identity (name, role, shift_set), personality (openness, conscientiousness, ...).
