# Issue #486 Evidence

Scope: repo-internal, non-TOGAF documentation and comments only. The TOGAF HTML copies are intentionally untouched and handed off to the main session.

## AC-1 / AC-2: redb Snapshot Coverage

Command:

```bash
rg -n -i "11 .*table|11.*tables|all 11|alle 11" crates services docs console CHANGELOG.md
```

Output after the repo-internal redb comment fixes:

```text
docs/architecture/togaf-architecture-guide.html:2406:                                <strong>Persistence Snapshot:</strong> The system serializes the ECS world and redb hot state (11 tables) to a file
docs/architecture/togaf-architecture-guide.html:2445:                            apply all 11 tables in ONE atomic write transaction.
crates/sentinel-ecs/src/lib.rs:61:        // Alle 11 Components muessen vorhanden sein
```

The remaining table-count hits are TOGAF HTML handoff items. The ECS hit is about components, not redb tables.

Command:

```bash
rg -n -i "11 .*[Tt]able" crates services docs
```

Output:

```text
docs/architecture/togaf-architecture-guide.html:2406:                    <div class="font-mono font-bold text-[0.7rem] text-neon-green uppercase tracking-wider mb-2">redb hot state (11 tables)</div>
docs/architecture/togaf-architecture-guide.html:2445:                <div class="flex gap-3 py-2 border-b border-border/60"><strong class="text-bright whitespace-nowrap w-8 shrink-0">4.</strong><span>redb <code class="text-neon-blue text-[0.8em]">restore_all_tables()</code> &mdash; all 11 tables in ONE atomic write transaction</span></div>
```

Command:

```bash
rg -n -i "11 .*components|11.*component|alle 11 components" crates/sentinel-ecs/src/world.rs crates/sentinel-ecs/src/lib.rs
```

Output:

```text
crates/sentinel-ecs/src/lib.rs:3://! Definiert 11 Components, 10 Systems (in strikter Reihenfolge via SimulationPhase),
crates/sentinel-ecs/src/lib.rs:61:        // Alle 11 Components muessen vorhanden sein
crates/sentinel-ecs/src/world.rs:1248:/// Spawnt einen Agenten mit allen 11 Components und Default-Werten.
```

Command:

```bash
awk 'NR>=568 && NR<=586 {printf "%d:%s\n", NR, $0}' crates/sentinel-common/src/types.rs
```

Output:

```text
568:}
569:
570:/// Dump aller 12 redb-Tables inklusive api_patterns (Key-Value Paare als Bytes).
571:#[derive(Debug, Clone, Serialize, Deserialize)]
572:pub struct RedbDump {
573:    pub agent_states: Vec<(u16, Vec<u8>)>,
574:    pub relationships: Vec<(u16, Vec<u8>)>,
575:    pub personalities: Vec<(u16, Vec<u8>)>,
576:    pub room_states: Vec<(String, Vec<u8>)>,
577:    pub voice_styles: Vec<(u16, Vec<u8>)>,
578:    pub behavioral_notes: Vec<(u16, Vec<u8>)>,
579:    pub narrative_summaries: Vec<(u16, Vec<u8>)>,
580:    pub evolution_versions: Vec<(u16, Vec<u8>)>,
581:    pub evolution_batches: Vec<(u16, Vec<u8>)>,
582:    pub agent_facts: Vec<(u16, Vec<u8>)>,
583:    pub nmda_scores: Vec<(u16, Vec<u8>)>,
584:    pub api_patterns: Vec<(String, Vec<u8>)>,
585:    pub sim_meta: Vec<(String, Vec<u8>)>,
586:}
```

Command:

```bash
awk 'NR>=721 && NR<=756 {printf "%d:%s\n", NR, $0}' crates/sentinel-redb/src/lib.rs
```

Output:

```text
721:    /// Dumpt alle 12 Tables inklusive api_patterns in einer Read-Transaktion.
722:    pub fn dump_all_tables(&self) -> anyhow::Result<sentinel_common::RedbDump> {
723:        let txn = self.db.begin_read()?;
724:        Ok(sentinel_common::RedbDump {
725:            agent_states: Self::dump_table_u16(&txn, AGENT_STATES)?,
726:            relationships: Self::dump_table_u16(&txn, RELATIONSHIPS)?,
727:            personalities: Self::dump_table_u16(&txn, PERSONALITIES)?,
728:            room_states: Self::dump_table_str(&txn, ROOM_STATES)?,
729:            voice_styles: Self::dump_table_u16(&txn, VOICE_STYLES)?,
730:            behavioral_notes: Self::dump_table_u16(&txn, BEHAVIORAL_NOTES)?,
731:            narrative_summaries: Self::dump_table_u16(&txn, NARRATIVE_SUMMARIES)?,
732:            evolution_versions: Self::dump_table_u16(&txn, EVOLUTION_VERSIONS)?,
733:            evolution_batches: Self::dump_table_u16(&txn, EVOLUTION_BATCHES)?,
734:            agent_facts: Self::dump_table_u16(&txn, AGENT_FACTS)?,
735:            nmda_scores: Self::dump_table_u16(&txn, NMDA_SCORES)?,
736:            api_patterns: Self::dump_str_bytes(&txn, API_PATTERNS)?,
737:            sim_meta: Self::dump_str_bytes(&txn, SIM_META)?,
738:        })
739:    }
740:
741:    /// Restored alle 12 Tables inklusive api_patterns aus einem Dump in einer atomaren Write-Transaktion.
742:    pub fn restore_all_tables(&self, dump: &sentinel_common::RedbDump) -> anyhow::Result<()> {
743:        let txn =
744:            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World))?;
745:        Self::restore_table_u16(&txn, AGENT_STATES, &dump.agent_states)?;
746:        Self::restore_table_u16(&txn, RELATIONSHIPS, &dump.relationships)?;
747:        Self::restore_table_u16(&txn, PERSONALITIES, &dump.personalities)?;
748:        Self::restore_table_str(&txn, ROOM_STATES, &dump.room_states)?;
749:        Self::restore_table_u16(&txn, VOICE_STYLES, &dump.voice_styles)?;
750:        Self::restore_table_u16(&txn, BEHAVIORAL_NOTES, &dump.behavioral_notes)?;
751:        Self::restore_table_u16(&txn, NARRATIVE_SUMMARIES, &dump.narrative_summaries)?;
752:        Self::restore_table_u16(&txn, EVOLUTION_VERSIONS, &dump.evolution_versions)?;
753:        Self::restore_table_u16(&txn, EVOLUTION_BATCHES, &dump.evolution_batches)?;
754:        Self::restore_table_u16(&txn, AGENT_FACTS, &dump.agent_facts)?;
755:        Self::restore_table_u16(&txn, NMDA_SCORES, &dump.nmda_scores)?;
756:        Self::restore_str_bytes(&txn, API_PATTERNS, &dump.api_patterns)?;
```

## RoomPhysicsState and ArtifactPlane Scope

Command:

```bash
awk 'NR>=1443 && NR<=1555 {printf "%d:%s\n", NR, $0}' crates/sentinel-ecs/src/world.rs
```

Output excerpt:

```text
1443:pub fn snapshot_ecs_state(world: &mut World) -> sentinel_common::EcsSnapshot {
1444:    let mut positions = Vec::new();
1445:    let mut bio_states = Vec::new();
1446:    let mut personalities = Vec::new();
1447:    let mut moods = Vec::new();
1448:    let mut perception_states = Vec::new();
1449:    let mut work_contexts = Vec::new();
1450:    let mut agent_capabilities = Vec::new();
1451:    let mut event_queues = Vec::new();
1452:    let mut identities = Vec::new();
1453:    let mut shift_infos = Vec::new();
...
1511:    sentinel_common::EcsSnapshot {
1512:        positions,
1513:        bio_states,
1514:        personalities,
1515:        moods,
1516:        perception_states,
1517:        work_contexts,
1518:        agent_capabilities,
1519:        event_queues,
1520:        identities,
1521:        shift_infos,
1522:        relationships: relationships_vec,
1523:        llm_configs: llm_configs_vec,
1524:        task_states,
1525:        sim_tick: sim_time.map(|t| t.tick.0).unwrap_or(0),
1526:        sim_hour: sim_time.map(|t| t.sim_hour).unwrap_or(0.0),
1527:        sim_delta_seconds: sim_time.map(|t| t.delta_seconds).unwrap_or(1.0),
1528:        active_chaos_json: world
1529:            .get_resource::<ActiveChaos>()
1530:            .and_then(|c| serde_json::to_vec(c).ok())
1531:            .unwrap_or_default(),
1532:        active_stimuli_json: world
1533:            .get_resource::<ActiveRoomStimuli>()
1534:            .and_then(|s| serde_json::to_vec(s).ok())
1535:            .unwrap_or_default(),
1536:        autonomy_cooldowns,
1537:        // #491 (TM-3): bisher beim Restore verworfene ephemere Resources erfassen.
1538:        smells_json: world
1539:            .get_resource::<ActiveSmells>()
1540:            .and_then(|r| serde_json::to_vec(r).ok())
1541:            .unwrap_or_default(),
1542:        room_chat_json: world
1543:            .get_resource::<RoomChatBuffer>()
1544:            .and_then(|r| serde_json::to_vec(r).ok())
1545:            .unwrap_or_default(),
1546:        gaia_json: world
1547:            .get_resource::<GaiaBuffer>()
1548:            .and_then(|r| serde_json::to_vec(r).ok())
1549:            .unwrap_or_default(),
1550:        broadcast_json: world
1551:            .get_resource::<BroadcastBuffer>()
1552:            .and_then(|r| serde_json::to_vec(r).ok())
1553:            .unwrap_or_default(),
1554:    }
1555:}
```

No `RoomPhysicsState` is serialized in `snapshot_ecs_state`.

Command:

```bash
awk 'NR>=1443 && NR<=1555 {printf "%d:%s\n", NR, $0}' crates/sentinel-ecs/src/world.rs | rg -n "RoomPhysicsState"
```

Output:

```text
(no output)
```

Command:

```bash
awk 'NR>=3138 && NR<=3152 {printf "%d:%s\n", NR, $0}' services/sentinel-daemon/src/orchestrator.rs
awk 'NR>=7590 && NR<=7602 {printf "%d:%s\n", NR, $0}' services/sentinel-daemon/src/orchestrator.rs
```

Output:

```text
3138:                mood_str,
3139:                max_event_id,
3140:                now_ms,
3141:            ],
3142:        )
3143:        .with_context(|| format!("agent_live_view seed fuer AGENT-{id:02}"))?;
3144:        report.agents_seeded += 1;
3145:    }
3146:
3147:    // RoomPhysicsState ist nicht Teil des WorldSnapshot. Restore rekonstruiert
3148:    // room_live_view deshalb bewusst nur aus Occupancy der Agent-Positionen.
3149:    let mut room_occupancy: HashMap<String, u32> = HashMap::new();
3150:    for (_, pos) in &snapshot.ecs.positions {
3151:        if !pos.in_transit {
3152:            *room_occupancy.entry(pos.room_id.clone()).or_default() += 1;
7590:                        row.get(1)?,
7591:                        row.get(2)?,
7592:                        row.get(3)?,
7593:                        row.get(4)?,
7594:                    ))
7595:                },
7596:            )
7597:            .unwrap();
7598:        assert_eq!(room_row.0, 1);
7599:        assert_eq!(room_row.1, None, "RoomPhysicsState bleibt out of scope");
7600:        assert_eq!(room_row.2, None, "RoomPhysicsState bleibt out of scope");
7601:        assert_eq!(room_row.3, None, "RoomPhysicsState bleibt out of scope");
7602:        assert_eq!(room_row.4, None, "Room smells bleiben out of scope");
```

Command:

```bash
awk 'NR>=1 && NR<=64 {printf "%d:%s\n", NR, $0}' crates/sentinel-fs/src/artifact.rs
```

Output:

```text
1://! Artifact Plane data model: 6 redb tables for content-defined chunked storage.
2://!
3://! Tables:
4://! - `FS_OBJECTS`: ObjectId -> ObjectMetadata (size, mime, created_at, chunk_count)
5://! - `FS_MANIFESTS`: ObjectId -> JSON-serialized `Vec<[u8;16]>` (ordered chunk list)
6://! - `FS_CHUNKS`: `[u8;16]` (BLAKE3-128) -> zstd-compressed chunk data
7://! - `FS_CHUNK_REFCOUNT`: `[u8;16]` -> u32 (how many manifests reference this chunk)
8://! - `FS_OBJECT_REFS`: &str (name) -> u64 (ObjectId, named references)
9://! - `FS_INGEST_SESSIONS`: session_id (u64) -> JSON-serialized IngestSessionState
...
38:// --- Table Definitions ---
39:
40:/// Object metadata: ObjectId -> JSON-serialized ObjectMetadata.
41:pub const FS_OBJECTS: TableDefinition<u64, &[u8]> = TableDefinition::new("fs_objects");
42:
43:/// Manifests: ObjectId -> JSON-serialized list of chunk hashes (ordered).
44:pub const FS_MANIFESTS: TableDefinition<u64, &[u8]> = TableDefinition::new("fs_manifests");
45:
46:/// Chunk index: BLAKE3-128 fingerprint -> ChunkLocation (segment_id + offset + len).
47:/// Actual compressed data is stored in segment pack files, not in redb.
48:pub const FS_CHUNKS: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new("fs_chunks");
```

## TOGAF Handoff

Command:

```bash
awk 'NR>=2398 && NR<=2448 {printf "%d:%s\n", NR, $0}' docs/architecture/togaf-architecture-guide.html
awk 'NR>=2398 && NR<=2448 {printf "%d:%s\n", NR, $0}' /home/jan/togaf-llm-architecture-guide.html
```

Output excerpt:

```text
docs/architecture/togaf-architecture-guide.html:2406 includes "redb hot state (11 tables)"
docs/architecture/togaf-architecture-guide.html:2408-2411 omit api_patterns from the listed redb tables
docs/architecture/togaf-architecture-guide.html:2420 includes "ActiveSmells, RoomPhysicsState"
docs/architecture/togaf-architecture-guide.html:2445 includes "all 11 tables"
/home/jan/togaf-llm-architecture-guide.html:2406 includes "redb Hot-State (11 Tables)"
/home/jan/togaf-llm-architecture-guide.html:2408-2411 omit api_patterns from the listed redb tables
/home/jan/togaf-llm-architecture-guide.html:2420 includes "ActiveSmells, RoomPhysicsState"
/home/jan/togaf-llm-architecture-guide.html:2445 includes "alle 11 Tables"
```

Command:

```bash
rg -n "Soft affinity|Soft-Affinity|nano-container is the unit|Nano-Container ist die Einheit" docs/architecture/togaf-architecture-guide.html /home/jan/togaf-llm-architecture-guide.html
```

Output:

```text
/home/jan/togaf-llm-architecture-guide.html:2548:                <!-- Live-Migration (Fenced State Transfer) + Soft-Affinity -->
/home/jan/togaf-llm-architecture-guide.html:2556:                            <strong>Soft-Affinity:</strong> der Nano-Container ist die Einheit, nicht die Firma
docs/architecture/togaf-architecture-guide.html:2548:                <!-- Live-Migration (Fenced State Transfer) + Soft-Affinity -->
docs/architecture/togaf-architecture-guide.html:2556:                            <strong>Soft affinity:</strong> the nano-container is the unit, not the company
```

## Verification

Initial remote check failed because this isolated worktree had no local cargo-remote config:

Command:

```bash
cargo remote -c -- doc --no-deps -p sentinel-common
```

Output:

```text
2026-06-26 17:57:40,004 INFO  [cargo_remote] Project dir: "/work/company/ps-486-snapshot-coverage"
2026-06-26 17:57:40,004 ERROR [cargo_remote] No remote build server was defined (use config file or the --remote flags)
```

Then the ignored worktree-local `.cargo-remote.toml` was copied from the main checkout and the remote checks were rerun.

Command:

```bash
cargo remote -c -- doc --no-deps -p sentinel-common
```

Output:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 30.40s
Generated /tmp/builds/5613544798438574209/target/doc/sentinel_common/index.html
```

Command:

```bash
cargo remote -c -- test -p sentinel-redb test_dump_restore_includes_api_patterns
```

Output:

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 38.56s
Running unittests src/lib.rs (/tmp/builds/5613544798438574209/target/debug/deps/sentinel_redb-e9895c8661f06655)

running 1 test
test tests::test_dump_restore_includes_api_patterns ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 23 filtered out; finished in 0.45s

Running tests/acceptance.rs (/tmp/builds/5613544798438574209/target/debug/deps/acceptance-73ec88de4667b48b)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s
```

Benchmarks: not applicable. This PR changes comments, markdown, changelog text, and committed evidence only.
