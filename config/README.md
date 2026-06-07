# Config

Configuration files for the simulation. All files use TOML format.

## Files

| File | Controls | Validation |
|-------|---------|-------------|
| `rooms.toml` | 15 rooms (2 floors), adjacency, capacities, room types | `cargo test -p sentinel-common -- acceptance_rooms` |
| `agents/AGENT-XX-NAME.toml` | Personality, shift, bio defaults per agent | `cargo test -p sentinel-common -- acceptance_agents` |
| `simulation.toml` | Tick rate, persistence interval (`persist_every_n_ticks`), simulation flags | ECS startup |
| `cortex-gateway.toml` | Server ports (8080/8081), provider config, pipeline flags, timeouts | Gateway startup |
| `company.toml` | Company name, shift model (54 agents, 3 shifts + special set) | Agent loader |
| `observatory.toml` | Observatory metrics, report intervals | Observatory module |

## Room Layout (rooms.toml)

- 15 rooms, 2 floors (ground floor + upper floor), connected via stairwell
- Room types: `office`, `meeting`, `common`, `break`, `transit`, `bathroom`
- Adjacency must be bidirectional (A->B implies B->A)
- Capacity sum >= 15 (one full shift)

## Agent Definitions (agents/)

- Format: `AGENT-XX-NAME.toml` (for example `AGENT-01-Max.toml`)
- Contains: Big Five personality, shift assignment, morning/night owl, role
- Currently 5 agents migrated, target: 54

## Validation

```bash
# Raum-Config testen (Adjacency, Kapazitaet, Referenz-Integritaet)
cargo remote -- test -p sentinel-common -- acceptance_rooms

# Agent-Config testen (Loader, Format)
cargo remote -- test -p sentinel-common -- acceptance_agents
```
