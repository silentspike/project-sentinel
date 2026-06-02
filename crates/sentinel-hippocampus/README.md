# sentinel-hippocampus

## Purpose

`sentinel-hippocampus` is the multi-tier memory subsystem for agents. It records episodes, scores consolidation priority, maintains narrative summaries, retrieves facts, and persists memory state.

## Interfaces

- `HippocampusService` is the facade used by daemon and nightrun paths.
- `Episode`, `nmda_score`, and `selection_decision` rank memory candidates.
- `NarrativeMemory` maintains rolling summaries.
- `FactRetriever`, `FactStore`, and `RedbFactStore` provide JIT context facts.
- `KvCacheTier`, `InMemoryKvCache`, and `SleepCycle` model hot/cold memory behavior.

## Dependencies

- `sentinel-common` for agent identity and shared event contracts.
- `redb` for persistent store tables.
- `serde`, `serde_json`, `anyhow`, and `thiserror`.

## Verify

```bash
cargo remote -c -- test -p sentinel-hippocampus
```

Nightrun and daemon integration should also exercise consolidation through their own service tests before changing memory contracts.
