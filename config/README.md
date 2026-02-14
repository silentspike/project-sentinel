# Config

Konfigurationsdateien fuer die Simulation. Alle Dateien sind TOML-Format.

## Dateien

| Datei | Steuert | Validierung |
|-------|---------|-------------|
| `rooms.toml` | 15 Raeume (2 Stockwerke), Adjacency, Kapazitaeten, Raum-Typen | `cargo test -p sentinel-common -- acceptance_rooms` |
| `agents/AGENT-XX-NAME.toml` | Persoenlichkeit, Schicht, Bio-Defaults pro Agent | `cargo test -p sentinel-common -- acceptance_agents` |
| `simulation.toml` | Tick-Rate, Persistenz-Intervall (`persist_every_n_ticks`), Simulation-Flags | ECS-Startup |
| `cortex-gateway.toml` | Server-Ports (8080/8081), Provider-Config, Pipeline-Flags, Timeouts | Gateway-Startup |
| `company.toml` | Firmenname, Schichtmodell (54 Agents, 3 Schichten + Sonder-Set) | Agent-Loader |
| `observatory.toml` | Observatory-Metriken, Report-Intervalle | Observatory-Module |

## Raum-Layout (rooms.toml)

- 15 Raeume, 2 Stockwerke (EG + OG), verbunden via Treppenhaus
- Raum-Typen: `office`, `meeting`, `common`, `break`, `transit`, `bathroom`
- Adjacency muss bidirektional sein (A→B impliziert B→A)
- Kapazitaetssumme >= 15 (eine volle Schicht)

## Agent-Definitionen (agents/)

- Format: `AGENT-XX-NAME.toml` (z.B. `AGENT-01-Max.toml`)
- Enthaelt: Big-Five Persoenlichkeit, Schicht-Zuordnung, Morning/Night-Owl, Rolle
- Aktuell 5 Agents migriert, Ziel: 54

## Validierung

```bash
# Raum-Config testen (Adjacency, Kapazitaet, Referenz-Integritaet)
cargo remote -- test -p sentinel-common -- acceptance_rooms

# Agent-Config testen (Loader, Format)
cargo remote -- test -p sentinel-common -- acceptance_agents
```
