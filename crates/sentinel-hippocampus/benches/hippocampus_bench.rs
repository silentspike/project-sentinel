//! Benchmarks fuer sentinel-hippocampus Operationen (Issue #23).
//!
//! Misst Latenz der persistenten Memory-Operationen:
//! - Episode Serialisierung Roundtrip (serde_json)
//! - redb Store/Load Latenz (Einzel- und Batch-Operationen)
//! - NMDA Score Berechnung
//! - Konsolidierungs-Durchlauf (SleepCycle + Narrative + Persist)
//! - Priorisierte Retrieval-Latenz (NMDA-sortiert)
//! - Fact Store/Load Latenz
//!
//! WICHTIG: Diese Benchmarks MUESSEN auf der Deployment-VM ausgefuehrt werden
//! (NICHT auf dem Build-Server/LXC). Siehe CLAUDE.md.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use redb::ReadableDatabase;
use sentinel_hippocampus::{nmda_score, Episode, HippocampusService, HippocampusStore};

fn make_episode(
    id: u64,
    agent: &str,
    summary: &str,
    relevance: f64,
    emotion: f64,
    repetitions: u32,
    hours_ago: f64,
) -> Episode {
    Episode {
        id,
        agent_name: agent.to_string(),
        summary: summary.to_string(),
        relevance,
        emotion,
        repetitions,
        hours_ago,
        participants: vec!["Lisa".to_string(), "Andreas".to_string()],
        tags: vec!["meeting".to_string(), "important".to_string()],
    }
}

fn make_realistic_episodes(agent: &str, count: usize) -> Vec<Episode> {
    (0..count)
        .map(|i| {
            let relevance = 0.1 + (i as f64 % 9.0) * 0.1;
            let emotion = 0.05 + (i as f64 % 10.0) * 0.09;
            let repetitions = 1 + (i as u32 % 4);
            let hours_ago = 0.5 + (i as f64) * 0.3;
            make_episode(
                i as u64,
                agent,
                &format!("Episode {} mit Kontext und Details", i),
                relevance,
                emotion,
                repetitions,
                hours_ago,
            )
        })
        .collect()
}

fn temp_store() -> (tempfile::TempDir, HippocampusStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench-hippocampus.redb");
    let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
    (dir, store)
}

fn temp_service() -> (tempfile::TempDir, HippocampusService) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench-hippocampus-svc.redb");
    let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
    (dir, service)
}

// ──────────────────────────────────────────────
// Episode Serialisierung (serde_json Roundtrip)
// ──────────────────────────────────────────────

fn bench_episode_serialization(c: &mut Criterion) {
    let episode = make_episode(
        1,
        "Thomas",
        "Wichtiges Strategiemeeting mit Stakeholdern",
        0.9,
        0.85,
        2,
        1.0,
    );

    c.bench_function("hippocampus.episode_serialize_json", |b| {
        b.iter(|| {
            let json = serde_json::to_vec(black_box(&episode)).unwrap();
            black_box(json);
        })
    });

    c.bench_function("hippocampus.episode_deserialize_json", |b| {
        let json = serde_json::to_vec(&episode).unwrap();
        b.iter(|| {
            let ep: Episode = serde_json::from_slice(black_box(&json)).unwrap();
            black_box(ep);
        })
    });

    // Batch: 10 episodes (typical daily load per agent)
    let episodes = make_realistic_episodes("Thomas", 10);

    c.bench_function("hippocampus.episode_batch_10_serialize", |b| {
        b.iter(|| {
            let json = serde_json::to_vec(black_box(&episodes)).unwrap();
            black_box(json);
        })
    });

    c.bench_function("hippocampus.episode_batch_10_deserialize", |b| {
        let json = serde_json::to_vec(&episodes).unwrap();
        b.iter(|| {
            let eps: Vec<Episode> = serde_json::from_slice(black_box(&json)).unwrap();
            black_box(eps);
        })
    });
}

// ──────────────────────────────────────────────
// NMDA Score Berechnung
// ──────────────────────────────────────────────

fn bench_nmda_scoring(c: &mut Criterion) {
    let episode = make_episode(1, "Thomas", "Konflikt mit Lieferant", 0.9, 0.85, 2, 1.0);

    c.bench_function("hippocampus.nmda_score_single", |b| {
        b.iter(|| {
            black_box(nmda_score(black_box(&episode)));
        })
    });

    // Score + sort for 10 episodes (typical retrieval)
    let episodes = make_realistic_episodes("Thomas", 10);

    c.bench_function("hippocampus.nmda_score_sort_10", |b| {
        b.iter(|| {
            let mut scored: Vec<(&Episode, f64)> =
                episodes.iter().map(|ep| (ep, nmda_score(ep))).collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            black_box(scored);
        })
    });
}

// ──────────────────────────────────────────────
// redb Store/Load Latenz
// ──────────────────────────────────────────────

fn bench_redb_store_load(c: &mut Criterion) {
    // Single episode store/load
    {
        let (_dir, store) = temp_store();
        let episode = make_episode(1, "Thomas", "Benchmark episode", 0.9, 0.8, 1, 0.5);

        c.bench_function("hippocampus.redb_store_1_episode", |b| {
            b.iter(|| {
                store
                    .store_episodes("Thomas", black_box(std::slice::from_ref(&episode)))
                    .unwrap();
            })
        });

        // Pre-store for load benchmark
        store.store_episodes("Thomas", &[episode]).unwrap();

        c.bench_function("hippocampus.redb_load_1_episode", |b| {
            b.iter(|| {
                let eps = store.load_episodes(black_box("Thomas")).unwrap();
                black_box(eps);
            })
        });
    }

    // Batch 10 episodes (typical daily load)
    {
        let (_dir, store) = temp_store();
        let episodes = make_realistic_episodes("Thomas", 10);

        c.bench_function("hippocampus.redb_store_10_episodes", |b| {
            b.iter(|| {
                store
                    .store_episodes("Thomas", black_box(&episodes))
                    .unwrap();
            })
        });

        store.store_episodes("Thomas", &episodes).unwrap();

        c.bench_function("hippocampus.redb_load_10_episodes", |b| {
            b.iter(|| {
                let eps = store.load_episodes(black_box("Thomas")).unwrap();
                black_box(eps);
            })
        });
    }

    // Append single episode (read-modify-write pattern)
    {
        let (_dir, store) = temp_store();
        let episodes = make_realistic_episodes("Thomas", 5);
        store.store_episodes("Thomas", &episodes).unwrap();
        let new_ep = make_episode(99, "Thomas", "Neues Event", 0.8, 0.7, 1, 0.1);

        c.bench_function("hippocampus.redb_append_1_to_5", |b| {
            b.iter(|| {
                // Reset to 5 episodes before each append
                store.store_episodes("Thomas", &episodes).unwrap();
                store
                    .append_episodes("Thomas", black_box(std::slice::from_ref(&new_ep)))
                    .unwrap();
            })
        });
    }
}

// ──────────────────────────────────────────────
// Fact Store/Load Latenz
// ──────────────────────────────────────────────

fn bench_fact_operations(c: &mut Criterion) {
    let (_dir, store) = temp_store();

    c.bench_function("hippocampus.redb_store_fact", |b| {
        b.iter(|| {
            store
                .store_fact(
                    black_box("facts/projects/aurora"),
                    black_box("Projekt Aurora: Webseite Redesign Phase 2"),
                )
                .unwrap();
        })
    });

    store
        .store_fact(
            "facts/projects/aurora",
            "Projekt Aurora: Webseite Redesign Phase 2",
        )
        .unwrap();

    c.bench_function("hippocampus.redb_load_fact", |b| {
        b.iter(|| {
            let fact = store.load_fact(black_box("facts/projects/aurora")).unwrap();
            black_box(fact);
        })
    });
}

// ──────────────────────────────────────────────
// Narrative Store/Load Latenz
// ──────────────────────────────────────────────

fn bench_narrative_operations(c: &mut Criterion) {
    let (_dir, store) = temp_store();
    let state = sentinel_hippocampus::NarrativeState {
        agent_name: "Thomas".to_string(),
        summary: "- Wichtiges Strategiemeeting (Score: 0.72)\n- Konflikt mit Lieferant (Score: 0.65)\n- Kaffee in der Kueche (Score: 0.02)".to_string(),
        episode_count: 3,
    };

    c.bench_function("hippocampus.redb_store_narrative", |b| {
        b.iter(|| {
            store.store_narrative("Thomas", black_box(&state)).unwrap();
        })
    });

    store.store_narrative("Thomas", &state).unwrap();

    c.bench_function("hippocampus.redb_load_narrative", |b| {
        b.iter(|| {
            let n = store.load_narrative(black_box("Thomas")).unwrap();
            black_box(n);
        })
    });
}

// ──────────────────────────────────────────────
// Konsolidierungs-Durchlauf (Full Cycle)
// ──────────────────────────────────────────────

fn bench_consolidation(c: &mut Criterion) {
    // Single agent, 10 episodes (typical nightly run)
    c.bench_function("hippocampus.consolidate_1_agent_10_eps", |b| {
        b.iter_with_setup(
            || {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("bench-consolidate.redb");
                let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
                let episodes = make_realistic_episodes("Thomas", 10);
                service.record_episodes("Thomas", &episodes).unwrap();
                (dir, service)
            },
            |(_dir, service)| {
                let result = service.consolidate_agent(black_box("Thomas")).unwrap();
                black_box(result);
            },
        )
    });

    // Multi-agent consolidation (5 agents, 10 eps each)
    c.bench_function("hippocampus.consolidate_5_agents_10_eps", |b| {
        b.iter_with_setup(
            || {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("bench-consolidate-multi.redb");
                let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
                for agent in &["Thomas", "Lisa", "Andreas", "Maria", "Stefan"] {
                    let episodes = make_realistic_episodes(agent, 10);
                    service.record_episodes(agent, &episodes).unwrap();
                }
                (dir, service)
            },
            |(_dir, service)| {
                let results = service.consolidate_all().unwrap();
                black_box(results);
            },
        )
    });
}

// ──────────────────────────────────────────────
// Priorisierte Retrieval-Latenz (NMDA-sortiert)
// ──────────────────────────────────────────────

fn bench_retrieval(c: &mut Criterion) {
    // Retrieve from 10 episodes, return top 5
    {
        let (_dir, service) = temp_service();
        let episodes = make_realistic_episodes("Thomas", 10);
        service.record_episodes("Thomas", &episodes).unwrap();

        c.bench_function("hippocampus.retrieve_top5_from_10", |b| {
            b.iter(|| {
                let memories = service
                    .retrieve_memories(black_box("Thomas"), black_box(5))
                    .unwrap();
                black_box(memories);
            })
        });
    }

    // Retrieve from 50 episodes (stress case), return top 10
    {
        let (_dir, service) = temp_service();
        let episodes = make_realistic_episodes("Thomas", 50);
        service.record_episodes("Thomas", &episodes).unwrap();

        c.bench_function("hippocampus.retrieve_top10_from_50", |b| {
            b.iter(|| {
                let memories = service
                    .retrieve_memories(black_box("Thomas"), black_box(10))
                    .unwrap();
                black_box(memories);
            })
        });
    }
}

// ──────────────────────────────────────────────
// Fact Retrieval via Service (Trigger-basiert)
// ──────────────────────────────────────────────

fn bench_fact_retrieval(c: &mut Criterion) {
    let (_dir, service) = temp_service();
    // Store facts that match FACT_TRIGGERS
    service
        .store()
        .store_fact("facts/projects/aurora", "Projekt Aurora: Webseite Redesign")
        .unwrap();
    service
        .store()
        .store_fact("facts/finance/budget-q1", "Q1 Budget: 150k EUR")
        .unwrap();
    service
        .store()
        .store_fact("facts/hr/vacation", "30 Tage pro Jahr")
        .unwrap();

    c.bench_function("hippocampus.fact_retrieval_2_matches", |b| {
        b.iter(|| {
            let facts =
                service.retrieve_facts(black_box("Wir besprechen Projekt Aurora und das Budget"));
            black_box(facts);
        })
    });

    c.bench_function("hippocampus.fact_retrieval_0_matches", |b| {
        b.iter(|| {
            let facts = service.retrieve_facts(black_box("Nichts relevantes hier"));
            black_box(facts);
        })
    });
}

// ──────────────────────────────────────────────
// Skalierung: Variierendes Episode-Count
// ──────────────────────────────────────────────

fn bench_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("hippocampus.scaling");

    for count in [1, 5, 10, 25, 50] {
        group.bench_with_input(
            BenchmarkId::new("store_episodes", count),
            &count,
            |b, &count| {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("bench-scale.redb");
                let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
                let episodes = make_realistic_episodes("Thomas", count);

                b.iter(|| {
                    store
                        .store_episodes("Thomas", black_box(&episodes))
                        .unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("load_episodes", count),
            &count,
            |b, &count| {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("bench-scale.redb");
                let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
                let episodes = make_realistic_episodes("Thomas", count);
                store.store_episodes("Thomas", &episodes).unwrap();

                b.iter(|| {
                    let eps = store.load_episodes(black_box("Thomas")).unwrap();
                    black_box(eps);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("retrieve_memories", count),
            &count,
            |b, &count| {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("bench-scale.redb");
                let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
                let episodes = make_realistic_episodes("Thomas", count);
                service.record_episodes("Thomas", &episodes).unwrap();

                b.iter(|| {
                    let mems = service
                        .retrieve_memories(black_box("Thomas"), black_box(10))
                        .unwrap();
                    black_box(mems);
                });
            },
        );
    }

    group.finish();
}

// ──────────────────────────────────────────────
// redb Deep-Dive: Transaction, MVCC, Open, Scan
// ──────────────────────────────────────────────

fn bench_redb_deep(c: &mut Criterion) {
    // DB open/create latency (cold start)
    c.bench_function("hippocampus.redb_open_create", |b| {
        b.iter_with_setup(
            || {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("bench-open.redb");
                (dir, path)
            },
            |(_dir, path)| {
                let store = HippocampusStore::open(black_box(path.to_str().unwrap())).unwrap();
                black_box(store);
            },
        )
    });

    // DB reopen latency (warm start — tables already exist)
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench-reopen.redb");
        // Create and populate
        {
            let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
            let episodes = make_realistic_episodes("Thomas", 10);
            store.store_episodes("Thomas", &episodes).unwrap();
        }

        let path_str = path.to_str().unwrap().to_string();
        c.bench_function("hippocampus.redb_reopen_existing", |b| {
            b.iter(|| {
                let store = HippocampusStore::open(black_box(&path_str)).unwrap();
                black_box(store);
            })
        });
    }

    // MVCC read: read while another key was recently written
    // (measures snapshot isolation overhead)
    {
        let (_dir, store) = temp_store();
        // Pre-populate multiple agents
        for i in 0..10 {
            let eps = make_realistic_episodes(&format!("Agent_{i}"), 5);
            store.store_episodes(&format!("Agent_{i}"), &eps).unwrap();
        }

        c.bench_function("hippocampus.redb_mvcc_read_after_write", |b| {
            let mut counter = 0u64;
            b.iter(|| {
                // Write to one key, then read from another (MVCC snapshot isolation)
                let ep = make_episode(counter, "Agent_0", "Update", 0.5, 0.5, 1, 0.1);
                store.store_episodes("Agent_0", &[ep]).unwrap();
                let loaded = store.load_episodes(black_box("Agent_5")).unwrap();
                black_box(loaded);
                counter += 1;
            })
        });
    }

    // Pure transaction overhead: begin_write + commit (empty)
    // Uses raw redb API to isolate transaction cost
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench-txn.redb");
        let db = redb::Database::create(path.to_str().unwrap()).unwrap();
        // Initialize table
        {
            let txn = db.begin_write().unwrap();
            txn.open_table(redb::TableDefinition::<&str, &[u8]>::new("bench"))
                .unwrap();
            txn.commit().unwrap();
        }

        c.bench_function("hippocampus.redb_txn_write_empty_commit", |b| {
            b.iter(|| {
                let txn = db.begin_write().unwrap();
                {
                    let _table = txn
                        .open_table(redb::TableDefinition::<&str, &[u8]>::new("bench"))
                        .unwrap();
                }
                txn.commit().unwrap();
            })
        });

        c.bench_function("hippocampus.redb_txn_read_only", |b| {
            b.iter(|| {
                let txn = db.begin_read().unwrap();
                let table = txn
                    .open_table(redb::TableDefinition::<&str, &[u8]>::new("bench"))
                    .unwrap();
                let _ = table.get("nonexistent").unwrap();
                black_box(());
            })
        });
    }

    // Multi-key scan: list_agents_with_episodes with N agents
    {
        let mut group = c.benchmark_group("hippocampus.redb_agent_scan");
        for agent_count in [5, 15, 54] {
            group.bench_with_input(
                BenchmarkId::new("list_agents", agent_count),
                &agent_count,
                |b, &count| {
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("bench-scan.redb");
                    let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
                    for i in 0..count {
                        let ep = make_episode(
                            i as u64,
                            &format!("Agent_{i}"),
                            "Episode",
                            0.5,
                            0.5,
                            1,
                            1.0,
                        );
                        store.store_episodes(&format!("Agent_{i}"), &[ep]).unwrap();
                    }

                    b.iter(|| {
                        let agents = store.list_agents_with_episodes().unwrap();
                        black_box(agents);
                    });
                },
            );
        }
        group.finish();
    }

    // Cache state throughput: rapid hot/cold toggling
    {
        let (_dir, store) = temp_store();
        c.bench_function("hippocampus.redb_cache_state_toggle", |b| {
            let mut is_hot = true;
            b.iter(|| {
                store
                    .store_cache_state("Thomas", black_box(is_hot))
                    .unwrap();
                is_hot = !is_hot;
            })
        });

        store.store_cache_state("Thomas", true).unwrap();
        c.bench_function("hippocampus.redb_cache_state_read", |b| {
            b.iter(|| {
                let state = store.load_cache_state(black_box("Thomas")).unwrap();
                black_box(state);
            })
        });
    }
}

// ──────────────────────────────────────────────
// 54-Agent Stress-Test (Produktions-Szenario)
// ──────────────────────────────────────────────

fn bench_production_scenario(c: &mut Criterion) {
    // Full 54-agent nightly consolidation
    c.bench_function("hippocampus.production_54_agents_consolidate", |b| {
        b.iter_with_setup(
            || {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("bench-prod.redb");
                let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
                // 54 agents, each with 8-12 episodes (realistic daily load)
                for i in 0..54 {
                    let count = 8 + (i % 5); // 8-12 episodes
                    let episodes = make_realistic_episodes(&format!("Agent_{i}"), count);
                    service
                        .record_episodes(&format!("Agent_{i}"), &episodes)
                        .unwrap();
                }
                (dir, service)
            },
            |(_dir, service)| {
                let results = service.consolidate_all().unwrap();
                black_box(results);
            },
        )
    });

    // Full 54-agent episode recording (day operations batch)
    c.bench_function("hippocampus.production_54_agents_record_batch", |b| {
        b.iter_with_setup(
            || {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("bench-prod-record.redb");
                let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
                (dir, service)
            },
            |(_dir, service)| {
                for i in 0..54 {
                    let ep =
                        make_episode(i, &format!("Agent_{i}"), "Tages-Event", 0.7, 0.6, 1, 0.5);
                    service.record_episode(ep).unwrap();
                }
            },
        )
    });

    // 54-agent retrieval sweep (dashboard/monitoring use case)
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench-prod-retrieve.redb");
        let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
        for i in 0..54 {
            let episodes = make_realistic_episodes(&format!("Agent_{i}"), 10);
            service
                .record_episodes(&format!("Agent_{i}"), &episodes)
                .unwrap();
        }

        c.bench_function("hippocampus.production_54_agents_retrieve_all", |b| {
            b.iter(|| {
                for i in 0..54 {
                    let mems = service.retrieve_memories(&format!("Agent_{i}"), 5).unwrap();
                    black_box(mems);
                }
            })
        });
    }

    // DB file size after 54 agents with 10 episodes each
    // (informational, not timed — prints to stderr)
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench-prod-size.redb");
        let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
        for i in 0..54 {
            let episodes = make_realistic_episodes(&format!("Agent_{i}"), 10);
            service
                .record_episodes(&format!("Agent_{i}"), &episodes)
                .unwrap();
            service
                .store()
                .store_fact(
                    &format!("facts/agent_{i}/info"),
                    &format!("Agent {i} Fakten und Informationen"),
                )
                .unwrap();
        }
        drop(service);
        let file_size = std::fs::metadata(&path).unwrap().len();
        eprintln!(
            "\n[INFO] redb file size (54 agents, 10 eps each + 54 facts): {} bytes ({:.1} KB)",
            file_size,
            file_size as f64 / 1024.0
        );
    }
}

criterion_group!(
    benches,
    bench_episode_serialization,
    bench_nmda_scoring,
    bench_redb_store_load,
    bench_fact_operations,
    bench_narrative_operations,
    bench_consolidation,
    bench_retrieval,
    bench_fact_retrieval,
    bench_scaling,
    bench_redb_deep,
    bench_production_scenario,
);
criterion_main!(benches);
