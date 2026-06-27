# #443 - TOGAF Handoff (MAIN SESSION ONLY)

This Worker PR intentionally contains no TOGAF HTML edits:

- No edit to `docs/architecture/togaf-architecture-guide.html`
- No edit to `/home/jan/togaf-llm-architecture-guide.html`
- No copy between language-specific TOGAF sources

## Implemented Scope

`crates/sentinel-gaia-memory` adds Gaia Console Memory as a standalone local data layer for the reactive Gaia Console process. It is not `sentinel-gaia` (#415 deterministic generator), not `gaia_json` in simulation snapshots, and not Voice-of-Gaia runtime thought injection.

The implementation provides:

- A bi-temporal redb graph with valid-time and transaction-time fact versions.
- A Markdown memory file at `gaia-memory.md`.
- A thin, confirmation-gated CLI binary named `sentinel-gaia-memory`.
- Read-only rehydration from existing stores using public read APIs.
- Crate-local export/restore of `gaia_console_memory.redb` and `gaia-memory.md`.
- Read-only Hippocampus integration with no writes to agent tables.
- No vector store, no embedding index, no ANN path, no copied event rows.

## Backup Boundary

The backup path deliberately does not modify the simulation snapshot chain:

- No `WorldSnapshot` field was added.
- No `SCHEMA_VERSION` bump was made.
- No `snapshot_codec` change was made.
- No daemon snapshot/restore routine was modified.

Gaia Console Memory has an exportable/restorable backup path through its own library and CLI. This follows the redb dump/restore pattern at the crate data-plane level while keeping Gaia Console Memory separate from simulation state. The production question of who periodically invokes the export path belongs to #442/Ops.

## Rehydration Boundary

Rehydration is read-only and does not replay events:

- `sentinel-limbo::EventStore::open_readonly` supplies event-store metadata only.
- `sentinel-projection::ReadModelStore::open_readonly` supplies projection read models.
- `sentinel-hippocampus::ReadOnlyHippocampusStore` supplies existing memory facts.
- `gaia-memory.md` is read directly without creating or updating the file.

The returned context records `events_replayed=0`, `event_rows_loaded=0`, and `event_copy_count=0`.

## Main-Session TOGAF Work

Recommended TOGAF update, if the main session decides Cluster 04b should enumerate this feature:

- Add Gaia Console Memory as a local Gaia Console memory layer.
- State that it is separate from deterministic `sentinel-gaia`, `gaia_json`, Voice-of-Gaia, and simulation snapshots.
- State that backup is crate-local export/restore, not `WorldSnapshot`.
- State that rehydration reads EventStore metadata, projection read models, Hippocampus memory, and the Markdown memory file without event replay or event copying.
- State that periodic backup scheduling remains an Ops/#442 integration concern.
