use std::sync::atomic::{AtomicUsize, Ordering};

use sentinel_common::{AppendDispositionV2, EventSchemaRegistry};
use sentinel_limbo::{AuthenticatedEventCallerV1, EventStore};
use sentinel_workflow::CollaborationPublicationV1;

pub(super) fn publish_collaboration_publications(
    event_store: &EventStore,
    registry: &EventSchemaRegistry,
    publications: &[CollaborationPublicationV1],
    pending: &AtomicUsize,
) -> Result<Vec<AppendDispositionV2>, String> {
    publish_inner(
        event_store,
        registry,
        publications,
        pending,
        #[cfg(test)]
        &|_| {},
    )
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observation {
    BeforeProbe(usize),
    AfterProbe(usize),
    Classified,
    BeforeAppend(usize),
    AfterAppend(usize),
}

// The caller supplies a validated complete ledger and fences its snapshot and
// publication-producing mutations. Replay validation is not pending publication.
fn publish_inner(
    event_store: &EventStore,
    registry: &EventSchemaRegistry,
    publications: &[CollaborationPublicationV1],
    pending: &AtomicUsize,
    #[cfg(test)] observe: &dyn Fn(Observation),
) -> Result<Vec<AppendDispositionV2>, String> {
    let result = (|| {
        let gateway = event_store.append_gateway(registry);
        let mut classified = Vec::with_capacity(publications.len());
        let mut missing = 0;
        for publication in publications {
            #[cfg(test)]
            let index = classified.len();
            #[cfg(test)]
            observe(Observation::BeforeProbe(index));
            let authority_scope_digest = publication
                .proposal
                .causal_context
                .authority_scope_digest()
                .map_err(|error| format!("collaboration authority is invalid: {error}"))?;
            let caller = AuthenticatedEventCallerV1 {
                service_id: "sentinel-daemon-workflow".to_owned(),
                producer: publication.proposal.producer.clone(),
                authority_scope_digest,
            };
            let replay = gateway
                .lookup_exact_replay(&caller, &publication.proposal)
                .map_err(|error| {
                    super::log_reconciliation_failure(
                        "collaboration_probe",
                        "canonical receipt rejected",
                    );
                    format!("collaboration publication verification failed: {error}")
                })?;
            if replay.is_none() {
                missing += 1;
            }
            classified.push((caller, replay.map(|outcome| outcome.disposition)));
            #[cfg(test)]
            observe(Observation::AfterProbe(index));
        }

        pending.store(missing, Ordering::Release);
        #[cfg(test)]
        observe(Observation::Classified);
        let mut dispositions = Vec::with_capacity(publications.len());
        for (publication, (caller, replay)) in publications.iter().zip(classified) {
            #[cfg(test)]
            let index = dispositions.len();
            let disposition = match replay {
                Some(disposition) => disposition,
                None => {
                    #[cfg(test)]
                    observe(Observation::BeforeAppend(index));
                    let outcome =
                        gateway
                            .append(&caller, &publication.proposal)
                            .map_err(|error| {
                                super::log_reconciliation_failure(
                                    "collaboration_append",
                                    "canonical append rejected",
                                );
                                format!("collaboration publication failed: {error}")
                            })?;
                    missing -= 1;
                    pending.store(missing, Ordering::Release);
                    #[cfg(test)]
                    observe(Observation::AfterAppend(index));
                    outcome.disposition
                }
            };
            dispositions.push(disposition);
        }
        Ok(dispositions)
    })();
    if result.is_err() {
        pending.store(usize::MAX, Ordering::Release);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    use sentinel_common::{AppendOutcomeV2, ExpectedStreamRevision};
    use sentinel_limbo::rusqlite::{params, Connection};
    use sentinel_workflow::{
        collaboration_event_schema_registry, AgentId, CompanyRoleV1, ProjectId, TenantId,
        WorkItemId,
    };
    use uuid::Uuid;

    use super::*;

    fn publication(
        tenant: &str,
        operation: u128,
        expected_stream_revision: ExpectedStreamRevision,
    ) -> CollaborationPublicationV1 {
        let authority_digest = "a".repeat(64);
        let sequence = match expected_stream_revision {
            ExpectedStreamRevision::NoStream => 1,
            ExpectedStreamRevision::Exact(revision) => revision + 1,
        };
        let tenant_id = TenantId::parse(tenant).unwrap();
        let project_id = ProjectId::parse("project-collaboration").unwrap();
        let work_item_id = WorkItemId::parse("work-collaboration").unwrap();
        let mut session = sentinel_workflow::CollaborationSessionV1 {
            schema_version: sentinel_workflow::COLLABORATION_SCHEMA_VERSION,
            session_id: "collaboration-session".to_owned(),
            tenant_id: tenant_id.clone(),
            project_id: project_id.clone(),
            work_item_id: Some(work_item_id.clone()),
            admission_id: None,
            admission_contract_digest: None,
            collaboration_generation: None,
            admission_routes: Vec::new(),
            organization_generation: 1,
            organization_digest: authority_digest.clone(),
            assignment_id: "assignment-collaboration".to_owned(),
            assignment_version: 1,
            assignment_digest: authority_digest.clone(),
            policy_version: 1,
            policy_digest: authority_digest.clone(),
            subject_ref: "publish one collaboration transition".to_owned(),
            input_digest: authority_digest.clone(),
            mode: sentinel_workflow::CollaborationModeV1::IndependentReview,
            budget: sentinel_workflow::CollaborationBudgetV1 {
                max_participants: 2,
                max_claims: 2,
                max_handoffs: 1,
                max_clarification_rounds: 1,
                max_transitions: 64,
                deadline_unix_ms: 100,
            },
            participants: vec![
                sentinel_workflow::CollaborationParticipantV1 {
                    agent_id: AgentId(1),
                    permanent_role: CompanyRoleV1::ProjectManager,
                    mandate: sentinel_workflow::BehaviorMandateV1::Synthesize,
                    capability_snapshot_digest: authority_digest.clone(),
                    capabilities: BTreeSet::from(["coordination".to_owned()]),
                    privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
                },
                sentinel_workflow::CollaborationParticipantV1 {
                    agent_id: AgentId(2),
                    permanent_role: CompanyRoleV1::Developer,
                    mandate: sentinel_workflow::BehaviorMandateV1::Implement,
                    capability_snapshot_digest: authority_digest.clone(),
                    capabilities: BTreeSet::from(["rust".to_owned()]),
                    privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
                },
            ],
            state: sentinel_workflow::CollaborationSessionStateV1::Planned,
            transition_sequence: sequence,
            publication_revision: sequence,
            binding_digest: String::new(),
            claims: Vec::new(),
            transition_history: Vec::new(),
            created_by: AgentId(1),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
        };
        session.binding_digest = session.expected_binding_digest().unwrap();
        let payload_value = sentinel_workflow::CollaborationEventPayloadV1 {
            schema_version: sentinel_workflow::COLLABORATION_SCHEMA_VERSION,
            tenant_id: tenant_id.clone(),
            project_id: project_id.clone(),
            session_id: session.session_id.clone(),
            transition_sequence: sequence,
            command_digest: authority_digest.clone(),
            record: sentinel_workflow::CollaborationEventRecordV1::Session(session.clone()),
        };
        let payload = sentinel_common::canonical_json(&payload_value).unwrap();
        let payload_digest = sentinel_common::sha256_hex(&payload);
        let operation_id = Uuid::from_u128(operation);
        let proposal = sentinel_common::AppendProposalV2 {
            proposal_version: sentinel_common::EVENT_PROPOSAL_VERSION_V2,
            requested_event_id: None,
            event_type: sentinel_workflow::COLLABORATION_EVENT_TYPE.to_owned(),
            schema_version: sentinel_workflow::COLLABORATION_EVENT_SCHEMA_VERSION,
            payload_codec: sentinel_common::EventPayloadCodec::Json,
            payload_digest: payload_digest.clone(),
            payload,
            causal_context: sentinel_common::CausalContextV1 {
                schema_version: sentinel_common::CAUSAL_CONTEXT_VERSION_V1,
                tenant: sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::Tenant,
                    id: tenant_id.0,
                    authority_generation: 1,
                    authority_digest: authority_digest.clone(),
                },
                company: sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::Company,
                    id: "virtual-company".to_owned(),
                    authority_generation: 1,
                    authority_digest: authority_digest.clone(),
                },
                project: sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::Project,
                    id: project_id.0,
                    authority_generation: 1,
                    authority_digest: authority_digest.clone(),
                },
                workflow: Some(sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::Workflow,
                    id: session.session_id.clone(),
                    authority_generation: 1,
                    authority_digest: session.binding_digest,
                }),
                work_item: Some(sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::WorkItem,
                    id: work_item_id.0,
                    authority_generation: 1,
                    authority_digest: authority_digest.clone(),
                }),
                request_id: operation_id.to_string(),
                request_digest: authority_digest,
                correlation_id: session.session_id.clone(),
                causation_event_id: None,
                operation_id: operation_id.to_string(),
                attempt: 1,
                source_generation: sequence,
                source_digest: payload_digest.clone(),
                invocation_id: None,
                agent_id: Some("agent-1".to_owned()),
                tick: None,
                artifact_id: None,
                artifact_digest: None,
                qa_run_id: None,
                release_id: None,
                delivery_id: None,
                diagnostic_trace_id: None,
                diagnostic_span_id: None,
            },
            producer: sentinel_workflow::COLLABORATION_EVENT_PRODUCER.to_owned(),
            owner_term: None,
            tick: None,
            requested_durability: sentinel_common::EventDurability::Authoritative,
            expected_stream_revision,
            delivery_intents: vec![sentinel_common::DeliveryIntentV1 {
                intent_id: format!("collaboration-delivery-{tenant}-{operation_id}"),
                topic: sentinel_workflow::COLLABORATION_DELIVERY_TOPIC.to_owned(),
                payload_digest,
            }],
            effect_reservations: Vec::new(),
        };
        CollaborationPublicationV1 {
            operation_id,
            session_id: session.session_id,
            transition_sequence: sequence,
            proposal,
        }
    }

    fn caller(publication: &CollaborationPublicationV1) -> AuthenticatedEventCallerV1 {
        AuthenticatedEventCallerV1 {
            service_id: "sentinel-daemon-workflow".to_owned(),
            producer: publication.proposal.producer.clone(),
            authority_scope_digest: publication
                .proposal
                .causal_context
                .authority_scope_digest()
                .unwrap(),
        }
    }

    fn append(events: &EventStore, publication: &CollaborationPublicationV1) -> AppendOutcomeV2 {
        events
            .append_gateway(&collaboration_event_schema_registry().unwrap())
            .append(&caller(publication), &publication.proposal)
            .unwrap()
    }

    fn counts(path: &std::path::Path) -> (i64, i64, i64) {
        let conn = Connection::open(path).unwrap();
        (
            conn.query_row("SELECT count(*) FROM events_v2", [], |row| row.get(0))
                .unwrap(),
            conn.query_row("SELECT count(*) FROM event_operations_v2", [], |row| {
                row.get(0)
            })
            .unwrap(),
            conn.query_row("SELECT count(*) FROM delivery_intents_v2", [], |row| {
                row.get(0)
            })
            .unwrap(),
        )
    }

    #[test]
    fn complete_canonical_history_never_becomes_pending_during_real_probes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.sqlite");
        let events = EventStore::open(path.to_str().unwrap()).unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        let publications = (0..27)
            .map(|index| {
                publication(
                    "tenant-collaboration",
                    739 + u128::from(index),
                    if index == 0 {
                        ExpectedStreamRevision::NoStream
                    } else {
                        ExpectedStreamRevision::Exact(index)
                    },
                )
            })
            .collect::<Vec<_>>();
        let wire = serde_json::to_vec(&publications).unwrap();
        let pending = AtomicUsize::new(usize::MAX);
        assert_eq!(
            publish_collaboration_publications(&events, &registry, &publications, &pending)
                .unwrap(),
            vec![AppendDispositionV2::Appended; 27]
        );
        for previous in [0, 7, usize::MAX] {
            pending.store(previous, Ordering::Release);
            let probes = RefCell::new(Vec::new());
            let replay =
                publish_inner(
                    &events,
                    &registry,
                    &publications,
                    &pending,
                    &|stage| match stage {
                        Observation::BeforeProbe(_) | Observation::AfterProbe(_) => {
                            assert_eq!(pending.load(Ordering::Acquire), previous);
                            probes.borrow_mut().push(stage);
                        }
                        Observation::Classified => assert_eq!(pending.load(Ordering::Acquire), 0),
                        Observation::BeforeAppend(_) | Observation::AfterAppend(_) => {
                            panic!("canonical history must not be appended again")
                        }
                    },
                )
                .unwrap();
            assert_eq!(
                replay,
                vec![AppendDispositionV2::ReplayOfPriorOperation; 27]
            );
            assert_eq!(probes.borrow().len(), 54);
            assert_eq!(pending.load(Ordering::Acquire), 0);
        }
        assert_eq!(counts(&path), (27, 27, 27));
        assert_eq!(serde_json::to_vec(&publications).unwrap(), wire);
    }

    #[test]
    fn mixed_receipts_count_only_missing_and_preserve_all_disposition_positions() {
        let events = EventStore::open(":memory:").unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        let first = publication("tenant-first", 739, ExpectedStreamRevision::NoStream);
        let second = publication("tenant-first", 740, ExpectedStreamRevision::Exact(1));
        let other = publication("tenant-other", 741, ExpectedStreamRevision::NoStream);
        let fourth = publication("tenant-first", 742, ExpectedStreamRevision::Exact(2));
        append(&events, &first);
        append(&events, &other);
        let pending = AtomicUsize::new(usize::MAX);
        let observations = RefCell::new(Vec::new());
        let result = publish_inner(
            &events,
            &registry,
            &[first, second.clone(), other, fourth.clone()],
            &pending,
            &|stage| {
                observations
                    .borrow_mut()
                    .push((stage, pending.load(Ordering::Acquire)))
            },
        )
        .unwrap();
        assert_eq!(
            result,
            [
                AppendDispositionV2::ReplayOfPriorOperation,
                AppendDispositionV2::Appended,
                AppendDispositionV2::ReplayOfPriorOperation,
                AppendDispositionV2::Appended,
            ]
        );
        let publishing = observations
            .into_inner()
            .into_iter()
            .filter(|(stage, _)| {
                !matches!(
                    stage,
                    Observation::BeforeProbe(_) | Observation::AfterProbe(_)
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            publishing,
            [
                (Observation::Classified, 2),
                (Observation::BeforeAppend(1), 2),
                (Observation::AfterAppend(1), 1),
                (Observation::BeforeAppend(3), 1),
                (Observation::AfterAppend(3), 0),
            ]
        );
        assert_eq!(append(&events, &second).envelope.stream_revision, 2);
        assert_eq!(append(&events, &fourth).envelope.stream_revision, 3);
    }

    #[test]
    fn failed_append_preserves_durable_prefix_and_unknown_until_exact_retry() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.sqlite");
        let events = EventStore::open(path.to_str().unwrap()).unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        let publications = [
            publication(
                "tenant-collaboration",
                739,
                ExpectedStreamRevision::NoStream,
            ),
            publication(
                "tenant-collaboration",
                740,
                ExpectedStreamRevision::Exact(1),
            ),
        ];
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TRIGGER fail_second_publication BEFORE INSERT ON event_operations_v2
             WHEN NEW.operation_id = '{}'
             BEGIN SELECT RAISE(ABORT, 'injected publication failure'); END;",
            publications[1].operation_id
        ))
        .unwrap();
        let pending = AtomicUsize::new(0);
        let observations = RefCell::new(Vec::new());
        assert!(
            publish_inner(&events, &registry, &publications, &pending, &|stage| {
                observations
                    .borrow_mut()
                    .push((stage, pending.load(Ordering::Acquire)));
            })
            .is_err()
        );
        assert_eq!(pending.load(Ordering::Acquire), usize::MAX);
        assert!(observations
            .borrow()
            .contains(&(Observation::BeforeAppend(0), 2)));
        assert!(observations
            .borrow()
            .contains(&(Observation::AfterAppend(0), 1)));
        assert!(observations
            .borrow()
            .contains(&(Observation::BeforeAppend(1), 1)));
        assert_eq!(counts(&path), (1, 1, 1));
        conn.execute_batch("DROP TRIGGER fail_second_publication;")
            .unwrap();
        assert_eq!(
            publish_collaboration_publications(&events, &registry, &publications, &pending)
                .unwrap(),
            [
                AppendDispositionV2::ReplayOfPriorOperation,
                AppendDispositionV2::Appended
            ]
        );
        assert_eq!(pending.load(Ordering::Acquire), 0);
        assert_eq!(counts(&path), (2, 2, 2));
    }

    #[test]
    fn later_conflict_prevents_any_missing_publication_from_being_appended() {
        let events = EventStore::open(":memory:").unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        let committed = publication("tenant-committed", 739, ExpectedStreamRevision::NoStream);
        let original = append(&events, &committed);
        let missing = publication("tenant-missing", 740, ExpectedStreamRevision::NoStream);
        let mut conflict = committed.clone();
        conflict.proposal.tick = Some(8);
        conflict.proposal.causal_context.tick = Some(8);
        registry.validate_proposal(&conflict.proposal).unwrap();
        for previous in [0, 7, usize::MAX] {
            let pending = AtomicUsize::new(previous);
            let stages = RefCell::new(Vec::new());
            assert!(publish_inner(
                &events,
                &registry,
                &[missing.clone(), conflict.clone()],
                &pending,
                &|stage| stages.borrow_mut().push(stage),
            )
            .is_err());
            assert_eq!(pending.load(Ordering::Acquire), usize::MAX);
            assert!(!stages.borrow().contains(&Observation::Classified));
            assert!(events
                .append_gateway(&registry)
                .lookup_exact_replay(&caller(&missing), &missing.proposal)
                .unwrap()
                .is_none());
        }
        assert_eq!(append(&events, &committed).envelope, original.envelope);
    }

    #[test]
    fn canonical_receipt_does_not_bypass_current_producer_authorization() {
        let events = EventStore::open(":memory:").unwrap();
        let publication = publication(
            "tenant-collaboration",
            739,
            ExpectedStreamRevision::NoStream,
        );
        append(&events, &publication);
        let registry = EventSchemaRegistry::new([sentinel_common::EventSchemaDefinition {
            event_type: sentinel_workflow::COLLABORATION_EVENT_TYPE.to_owned(),
            schema_version: sentinel_workflow::COLLABORATION_EVENT_SCHEMA_VERSION,
            durability: sentinel_common::EventDurability::Authoritative,
            payload_codec: sentinel_common::EventPayloadCodec::Json,
            causation_policy: sentinel_common::CausationPolicyV1::RootRequired,
            allowed_producers: BTreeSet::from(["another-producer".to_owned()]),
            deterministic_event_id_producers: BTreeSet::new(),
            validator_id: "fixture-json-v1".to_owned(),
            validate_payload: |_| Ok(()),
            upcast: None,
        }])
        .unwrap();
        let pending = AtomicUsize::new(0);
        let error =
            publish_collaboration_publications(&events, &registry, &[publication], &pending)
                .unwrap_err();
        assert!(error.contains("producer"));
        assert_eq!(pending.load(Ordering::Acquire), usize::MAX);
    }

    #[test]
    fn malformed_later_payload_prevents_a_valid_missing_append() {
        let events = EventStore::open(":memory:").unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        let missing = publication("tenant-first", 739, ExpectedStreamRevision::NoStream);
        let mut malformed = publication("tenant-other", 740, ExpectedStreamRevision::NoStream);
        malformed.proposal.payload = b"{}".to_vec();
        malformed.proposal.payload_digest =
            sentinel_common::sha256_hex(&malformed.proposal.payload);
        malformed.proposal.causal_context.source_digest = malformed.proposal.payload_digest.clone();
        let pending = AtomicUsize::new(0);
        assert!(publish_collaboration_publications(
            &events,
            &registry,
            &[missing.clone(), malformed],
            &pending,
        )
        .is_err());
        assert_eq!(pending.load(Ordering::Acquire), usize::MAX);
        assert!(events
            .append_gateway(&registry)
            .lookup_exact_replay(&caller(&missing), &missing.proposal)
            .unwrap()
            .is_none());
    }

    #[test]
    fn corrupt_or_missing_receipt_target_fails_closed_without_a_new_append() {
        for sql in [
            "UPDATE events_v2 SET payload = X'7B7D' WHERE event_id = ?1",
            "UPDATE event_operations_v2 SET outcome_digest = ?2 WHERE event_id = ?1",
            "UPDATE event_operations_v2 SET event_id = ?2 WHERE event_id = ?1",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("events.sqlite");
            let events = EventStore::open(path.to_str().unwrap()).unwrap();
            let registry = collaboration_event_schema_registry().unwrap();
            let committed = publication("tenant-committed", 739, ExpectedStreamRevision::NoStream);
            let outcome = append(&events, &committed);
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
            if sql.contains("?2") {
                conn.execute(sql, params![outcome.envelope.event_id, "b".repeat(64)])
                    .unwrap();
            } else {
                conn.execute(sql, params![outcome.envelope.event_id])
                    .unwrap();
            }
            let missing = publication("tenant-missing", 740, ExpectedStreamRevision::NoStream);
            let pending = AtomicUsize::new(0);
            assert!(publish_collaboration_publications(
                &events,
                &registry,
                &[missing, committed],
                &pending,
            )
            .is_err());
            assert_eq!(pending.load(Ordering::Acquire), usize::MAX);
            assert_eq!(counts(&path), (1, 1, 1));
        }
    }

    #[test]
    fn receipt_read_failure_is_unknown_not_missing_or_zero() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.sqlite");
        let events = EventStore::open(path.to_str().unwrap()).unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        let publication = publication(
            "tenant-collaboration",
            739,
            ExpectedStreamRevision::NoStream,
        );
        append(&events, &publication);
        Connection::open(&path)
            .unwrap()
            .execute_batch("ALTER TABLE event_operations_v2 RENAME TO unavailable_operations_v2;")
            .unwrap();
        let pending = AtomicUsize::new(0);
        assert!(
            publish_collaboration_publications(&events, &registry, &[publication], &pending)
                .is_err()
        );
        assert_eq!(pending.load(Ordering::Acquire), usize::MAX);
    }

    #[test]
    fn readonly_store_verifies_replays_but_missing_append_stays_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.sqlite");
        let registry = collaboration_event_schema_registry().unwrap();
        let committed = publication(
            "tenant-collaboration",
            739,
            ExpectedStreamRevision::NoStream,
        );
        {
            let events = EventStore::open(path.to_str().unwrap()).unwrap();
            append(&events, &committed);
        }
        let readonly = EventStore::open_readonly(path.to_str().unwrap()).unwrap();
        let pending = AtomicUsize::new(usize::MAX);
        assert_eq!(
            publish_collaboration_publications(
                &readonly,
                &registry,
                std::slice::from_ref(&committed),
                &pending,
            )
            .unwrap(),
            [AppendDispositionV2::ReplayOfPriorOperation]
        );
        assert_eq!(pending.load(Ordering::Acquire), 0);
        let missing = publication(
            "tenant-collaboration",
            740,
            ExpectedStreamRevision::Exact(1),
        );
        assert!(publish_collaboration_publications(
            &readonly,
            &registry,
            &[committed, missing],
            &pending,
        )
        .is_err());
        assert_eq!(pending.load(Ordering::Acquire), usize::MAX);
        assert_eq!(counts(&path), (1, 1, 1));
    }

    #[test]
    fn reopen_rederives_receipts_and_missing_work_from_immutable_history() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.sqlite");
        let registry = collaboration_event_schema_registry().unwrap();
        let publications = [
            publication(
                "tenant-collaboration",
                739,
                ExpectedStreamRevision::NoStream,
            ),
            publication(
                "tenant-collaboration",
                740,
                ExpectedStreamRevision::Exact(1),
            ),
        ];
        let wire = serde_json::to_vec(&publications).unwrap();
        {
            let events = EventStore::open(path.to_str().unwrap()).unwrap();
            append(&events, &publications[0]);
        }
        for expected in [
            [
                AppendDispositionV2::ReplayOfPriorOperation,
                AppendDispositionV2::Appended,
            ],
            [AppendDispositionV2::ReplayOfPriorOperation; 2],
        ] {
            let events = EventStore::open(path.to_str().unwrap()).unwrap();
            let pending = AtomicUsize::new(usize::MAX);
            assert_eq!(
                publish_collaboration_publications(&events, &registry, &publications, &pending)
                    .unwrap(),
                expected
            );
            assert_eq!(pending.load(Ordering::Acquire), 0);
            assert_eq!(counts(&path), (2, 2, 2));
        }
        assert_eq!(serde_json::to_vec(&publications).unwrap(), wire);
    }

    #[test]
    fn same_operation_in_another_tenant_is_not_an_existing_receipt() {
        let events = EventStore::open(":memory:").unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        let first = publication("tenant-first", 739, ExpectedStreamRevision::NoStream);
        let other = publication("tenant-other", 739, ExpectedStreamRevision::NoStream);
        append(&events, &first);
        let pending = AtomicUsize::new(0);
        assert_eq!(
            publish_collaboration_publications(&events, &registry, &[first, other], &pending)
                .unwrap(),
            [
                AppendDispositionV2::ReplayOfPriorOperation,
                AppendDispositionV2::Appended
            ]
        );
        assert_eq!(pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn raced_missing_append_accepts_exact_canonical_success() {
        let events = EventStore::open(":memory:").unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        let publication = publication(
            "tenant-collaboration",
            739,
            ExpectedStreamRevision::NoStream,
        );
        let pending = AtomicUsize::new(usize::MAX);
        let result = publish_inner(
            &events,
            &registry,
            std::slice::from_ref(&publication),
            &pending,
            &|stage| {
                if stage == Observation::BeforeAppend(0) {
                    assert_eq!(pending.load(Ordering::Acquire), 1);
                    append(&events, &publication);
                }
            },
        )
        .unwrap();
        assert_eq!(result, [AppendDispositionV2::ReplayOfPriorOperation]);
        assert_eq!(pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn empty_complete_ledger_clears_unknown_or_previous_pending() {
        let events = EventStore::open(":memory:").unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        for previous in [0, 7, usize::MAX] {
            let pending = AtomicUsize::new(previous);
            assert!(
                publish_collaboration_publications(&events, &registry, &[], &pending)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(pending.load(Ordering::Acquire), 0);
        }
    }
}
