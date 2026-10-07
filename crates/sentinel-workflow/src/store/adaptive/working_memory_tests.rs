#![cfg(test)]

use super::*;
use crate::adaptive::continuation_tests::{authorization, effect, grant, NOW};
use crate::adaptive::work_funding_tests::{epoch_for, funded_authorization};
use crate::{AdaptiveCursorV1, AdaptiveObservationRefV1};
use sentinel_common::{TextReplacement, WorkbenchTool};

struct Fixture {
    root: tempfile::TempDir,
    store: WorkflowStore,
    session: AdaptiveSessionV1,
}

impl Fixture {
    fn new() -> Self {
        Self::with_capability_padding(0)
    }

    fn with_capability_padding(count: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(root.path().join("memory.sqlite")).unwrap();
        let mut grant = grant();
        grant.max_model_calls = 64;
        grant.max_tool_calls = 64;
        for index in 0..count {
            grant
                .authority
                .capabilities
                .insert(format!("memory-{index}-{}", "x".repeat(100)));
        }
        let session = store
            .begin_adaptive_session(&grant, &grant.authority, NOW)
            .unwrap()
            .1;
        Self {
            root,
            store,
            session,
        }
    }

    fn advance(&mut self, command: AdaptiveTransitionV1) {
        self.session = self
            .store
            .advance_adaptive_session(
                self.session.grant.session_id,
                self.session.version,
                // Reverse operation UUID order must never become history order.
                Uuid::from_u128(100_000 - u128::from(self.session.version)),
                &command,
                &self.session.grant.authority,
                self.session.updated_at_ms + 1,
            )
            .unwrap()
            .1;
    }

    fn claim_model(&mut self) -> AdaptiveEffectV1 {
        let mut model = effect(1);
        model.id =
            working_memory_model_effect_id(self.session.grant.session_id, self.session.version);
        self.advance(AdaptiveTransitionV1::ClaimModel {
            effect: model.clone(),
            previous_observation_digest: self
                .session
                .last_observation
                .as_ref()
                .map(|observation| observation.observation_digest.clone()),
        });
        model
    }

    fn resolve_tool(&mut self, tool: WorkbenchTool) -> String {
        let model = self.claim_model();
        let tool_digest = crate::adaptive_tool_digest(&tool).unwrap();
        self.advance(AdaptiveTransitionV1::ResolveModel {
            effect: model,
            result_digest: "a".repeat(64),
            decision: AdaptiveModelDecisionV1::Tool {
                tool,
                tool_digest: tool_digest.clone(),
            },
        });
        tool_digest
    }

    fn complete(&mut self, tool: WorkbenchTool) {
        let tool_digest = self.resolve_tool(tool);
        let tool_effect = effect(5_000 + u128::from(self.session.version));
        self.advance(AdaptiveTransitionV1::ClaimTool {
            effect: tool_effect.clone(),
            tool_digest,
        });
        self.advance(AdaptiveTransitionV1::ObserveTool {
            observation: AdaptiveObservationRefV1 {
                effect: tool_effect,
                observation_digest: format!("{:064x}", self.session.version),
            },
        });
    }

    fn source(
        &self,
        version: u64,
        effect_id: Uuid,
    ) -> Result<Option<AdaptiveWorkingMemorySourceV1>, WorkflowError> {
        self.store.adaptive_working_memory_source(
            self.session.grant.session_id,
            version,
            effect_id,
            &self.session.grant.authority,
        )
    }

    fn ready_source(&self) -> AdaptiveWorkingMemorySourceV1 {
        self.source(
            self.session.version,
            working_memory_model_effect_id(self.session.grant.session_id, self.session.version),
        )
        .unwrap()
        .unwrap()
    }
}

fn inspect(index: u16) -> WorkbenchTool {
    WorkbenchTool::InspectFile {
        path: format!("src/file-{index}.js"),
        max_bytes: 1024,
    }
}

fn test_tool() -> WorkbenchTool {
    WorkbenchTool::RunTests {
        suite_id: "node-qa-v1".into(),
        program: "sentinel-coding-qa".into(),
        args: vec!["PRIVATE_ARGUMENT_MUST_NOT_APPEAR".into()],
    }
}

fn snapshot(store: &WorkflowStore) -> Vec<u8> {
    let connection = store.lock().unwrap();
    let mut statement = connection.prepare(
        "SELECT operation_namespace,operation_id,request_digest,response,created_at_ms FROM workflow_operations ORDER BY operation_namespace,operation_id",
    ).unwrap();
    let operations = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut statement = connection.prepare(
        "SELECT tenant_id,project_id,work_item_id,agent_id,authority_digest,session_id,version,updated_at_ms FROM workflow_adaptive_heads ORDER BY tenant_id,project_id,work_item_id,agent_id,authority_digest",
    ).unwrap();
    let heads = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let changes: i64 = connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    serde_json::to_vec(&(operations, heads, changes)).unwrap()
}

#[test]
fn ready_and_claimed_sources_are_byte_identical_and_reads_do_not_write() {
    let mut fixture = Fixture::new();
    fixture.complete(inspect(1));
    fixture.complete(test_tool());
    let before = snapshot(&fixture.store);
    let source = fixture.ready_source();
    assert_eq!(snapshot(&fixture.store), before);
    let bytes = serde_json::to_vec(&source).unwrap();
    let provider_version = fixture.session.version;
    let model = fixture.claim_model();
    let claimed = snapshot(&fixture.store);
    let after = fixture.source(provider_version, model.id).unwrap().unwrap();
    assert_eq!(serde_json::to_vec(&after).unwrap(), bytes);
    assert_eq!(after.head_version, provider_version);
    assert_eq!(after.model_calls, 2);
    assert_eq!(fixture.session.model_calls, 3);
    assert_eq!(after.last_observation, source.last_observation);
    assert_eq!(snapshot(&fixture.store), claimed);
    let (_, entry) = evidence_entry(
        &fixture.store.lock().unwrap(),
        &namespace(source.session_id),
        source.head_version,
    )
    .unwrap();
    assert_eq!(
        source.head_entry_digest,
        canonical_sha256("sentinel.workflow.adaptive-entry.v1", &entry).unwrap()
    );
}

#[test]
fn working_memory_cache_reuses_exact_bytes_without_replaying_or_writing() {
    let mut fixture = Fixture::new();
    fixture.complete(inspect(1));
    fixture.complete(test_tool());
    let before = snapshot(&fixture.store);
    let (source, first) = crate::domain_store::validation_scope::with_completed_validations(|| {
        fixture.ready_source()
    });
    assert!(first.iter().any(|scope| scope
        .get("adaptive-journal")
        .is_some_and(|count| *count > 0)));
    let bytes = serde_json::to_vec(&source).unwrap();
    let (_, repeated) = crate::domain_store::validation_scope::with_completed_validations(|| {
        for _ in 0..3 {
            assert_eq!(serde_json::to_vec(&fixture.ready_source()).unwrap(), bytes);
        }
    });
    assert!(
        repeated.is_empty(),
        "unchanged SQL inputs should reuse the proof"
    );
    assert_eq!(snapshot(&fixture.store), before);
    assert!(fixture.store.lock().unwrap().is_autocommit());
    let reopened = WorkflowStore::open(fixture.root.path().join("memory.sqlite")).unwrap();
    assert_eq!(
        serde_json::to_vec(
            &reopened
                .adaptive_working_memory_source(
                    source.session_id,
                    source.provider_version,
                    source.effect_id,
                    &source.authority,
                )
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        bytes
    );
}

#[test]
fn working_memory_cache_oversized_authority_key_falls_back_to_replay() {
    let fixture = Fixture::with_capability_padding(80);
    let key = encode(&(
        fixture.session.grant.session_id,
        fixture.session.version,
        working_memory_model_effect_id(fixture.session.grant.session_id, fixture.session.version),
        &fixture.session.grant.authority,
    ))
    .unwrap();
    assert!(key.len() > 4096);
    let before = snapshot(&fixture.store);
    let (source, first) = crate::domain_store::validation_scope::with_completed_validations(|| {
        fixture.ready_source()
    });
    let (_, repeated) = crate::domain_store::validation_scope::with_completed_validations(|| {
        assert_eq!(fixture.ready_source(), source);
    });
    assert!(!first.is_empty());
    assert!(
        !repeated.is_empty(),
        "oversized keys must not retain a proof"
    );
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn working_memory_cache_does_not_hide_a_new_session_after_cached_absence() {
    let fixture = Fixture::new();
    let mut grant = fixture.session.grant.clone();
    grant.session_id = Uuid::from_u128(999);
    grant.authority.work_item_id = crate::WorkItemId::parse("new-work").unwrap();
    let effect_id = working_memory_model_effect_id(grant.session_id, 1);
    let read = || {
        fixture.store.adaptive_working_memory_source(
            grant.session_id,
            1,
            effect_id,
            &grant.authority,
        )
    };
    assert!(read().unwrap().is_none());
    let (_, repeated) = crate::domain_store::validation_scope::with_completed_validations(|| {
        assert!(read().unwrap().is_none());
    });
    assert!(repeated.is_empty());
    fixture
        .store
        .begin_adaptive_session(&grant, &grant.authority, NOW)
        .unwrap();
    let before = snapshot(&fixture.store);
    let (source, scopes) =
        crate::domain_store::validation_scope::with_completed_validations(|| {
            read().unwrap().unwrap()
        });
    assert_eq!(source.session_id, grant.session_id);
    assert_eq!(source.authority, grant.authority);
    assert!(source.rows.is_empty());
    assert!(
        !scopes.is_empty(),
        "new SQL rows must invalidate cached absence"
    );
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn chronology_latest_test_and_omission_counts_survive_reopen() {
    let mut fixture = Fixture::new();
    fixture.complete(test_tool());
    for index in 1..20 {
        fixture.complete(inspect(index));
    }
    let source = fixture.ready_source();
    source.validate().unwrap();
    assert_eq!(source.rows.len(), 16);
    assert_eq!(source.completed_tool_count, 20);
    assert_eq!(source.omitted_count, 4);
    assert_eq!(
        source.rows[0].tool_kind,
        AdaptiveWorkingMemoryToolKindV1::RunTests
    );
    assert_eq!(source.rows[0].session_version, 5);
    assert!(source
        .rows
        .windows(2)
        .all(|rows| rows[0].session_version < rows[1].session_version));
    assert_eq!(
        source.rows.last().unwrap().observation,
        fixture.session.last_observation.clone().unwrap()
    );
    assert_eq!(source.model_calls, 20);
    assert_eq!(source.tool_calls, 20);
    assert_eq!(source.root_model_ceiling, 64);
    assert_eq!(source.root_tool_ceiling, 64);
    assert_eq!(source.active_model_ceiling, 64);
    assert_eq!(source.continuation_windows, 0);
    assert!(serde_json::to_vec(&source).unwrap().len() <= ADAPTIVE_WORKING_MEMORY_MAX_BYTES);
    let reopened = WorkflowStore::open(fixture.root.path().join("memory.sqlite")).unwrap();
    let before = snapshot(&reopened);
    assert_eq!(
        reopened
            .adaptive_working_memory_source(
                source.session_id,
                source.provider_version,
                source.effect_id,
                &source.authority,
            )
            .unwrap()
            .unwrap(),
        source
    );
    assert_eq!(snapshot(&reopened), before);
}

#[test]
fn all_tool_variants_are_projected_without_content_arguments_or_package_paths() {
    let mut fixture = Fixture::new();
    let tools = [
        WorkbenchTool::ListDirectory {
            path: ".".into(),
            after: None,
            max_entries: 64,
        },
        inspect(1),
        WorkbenchTool::WriteFile {
            path: "src/main.js".into(),
            content: "PRIVATE_FILE_CONTENT_MUST_NOT_APPEAR".into(),
            expected_sha256: None,
        },
        WorkbenchTool::ApplyPatch {
            path: "src/main.js".into(),
            expected_sha256: "a".repeat(64),
            replacements: vec![TextReplacement {
                old: "PRIVATE_OLD_PATCH".into(),
                new: "PRIVATE_NEW_PATCH".into(),
                expected_occurrences: 1,
            }],
        },
        WorkbenchTool::RunCommand {
            program: "node".into(),
            args: vec!["PRIVATE_COMMAND_ARGUMENT".into()],
        },
        test_tool(),
        WorkbenchTool::PackageArtifact {
            artifact_kind: "report".into(),
            media_type: "text/plain".into(),
            paths: vec!["PRIVATE_PACKAGE_PATH".into()],
        },
    ];
    for tool in tools {
        fixture.complete(tool);
    }
    let source = fixture.ready_source();
    let encoded = serde_json::to_string(&source).unwrap();
    assert!(!encoded.contains("PRIVATE_"));
    assert!(!encoded.contains("stdout"));
    assert!(!encoded.contains("content"));
    assert!(!encoded.contains("args"));
    assert_eq!(source.rows.len(), 7);
    assert_eq!(source.rows[0].target.as_deref(), Some("."));
    assert_eq!(source.rows[4].program.as_deref(), Some("node"));
    assert_eq!(source.rows[5].suite_id.as_deref(), Some("node-qa-v1"));
    assert!(source.rows[6].target.is_none());
    assert!(source.rows[6].labels_omitted);
    for row in source.rows {
        assert_eq!(row.tool_digest.len(), 64);
        assert_eq!(row.entry_digest.len(), 64);
        assert_eq!(row.observation.observation_digest.len(), 64);
    }
}

#[test]
fn labels_are_utf8_byte_bounded_and_never_misleadingly_truncated() {
    let mut fixture = Fixture::new();
    let exact = "\u{e9}".repeat(128);
    let oversized = "\u{e9}".repeat(129);
    let control = "line\nfile".to_string();
    for path in [&exact, &oversized, &control] {
        fixture.complete(WorkbenchTool::InspectFile {
            path: path.clone(),
            max_bytes: 1,
        });
    }
    let source = fixture.ready_source();
    assert_eq!(source.rows[0].target.as_deref(), Some(exact.as_str()));
    assert!(!source.rows[0].labels_omitted);
    for row in &source.rows[1..] {
        assert!(row.target.is_none());
        assert!(row.labels_omitted);
        assert_eq!(row.tool_digest.len(), 64);
    }
    source.validate().unwrap();
}

#[test]
fn stale_foreign_nil_and_wrong_effect_requests_are_read_only_failures() {
    let fixture = Fixture::new();
    let id = fixture.session.grant.session_id;
    let version = fixture.session.version;
    let effect_id = working_memory_model_effect_id(id, version);
    let before = snapshot(&fixture.store);
    let expected = fixture.ready_source();
    for (version, effect_id) in [
        (0, effect_id),
        (version + 1, effect_id),
        (version, Uuid::nil()),
        (version, Uuid::from_u128(19)),
    ] {
        assert_eq!(fixture.ready_source(), expected);
        assert!(fixture.source(version, effect_id).is_err());
    }
    for index in 0..20 {
        assert_eq!(fixture.ready_source(), expected);
        let mut foreign = fixture.session.grant.authority.clone();
        match index {
            0 => foreign.agent_id = crate::AgentId(99),
            1 => foreign.project_id = crate::ProjectId::parse("foreign-project").unwrap(),
            2 => foreign.work_item_id = crate::WorkItemId::parse("foreign-work").unwrap(),
            3 => foreign.assignment_version += 1,
            4 => foreign.tenant_id = crate::TenantId::parse("foreign-tenant").unwrap(),
            5 => foreign.assignment_digest = "a".repeat(64),
            6 => foreign.organization_generation += 1,
            7 => foreign.organization_digest = "a".repeat(64),
            8 => foreign.principal.principal_generation += 1,
            9 => foreign.profile_id = "foreign-profile".into(),
            10 => foreign.profile_generation += 1,
            11 => foreign.profile_digest = "a".repeat(64),
            12 => foreign.runtime_key = "foreign-runtime".into(),
            13 => foreign.runtime_generation += 1,
            14 => foreign.runtime_digest = "a".repeat(64),
            15 => foreign.policy_generation += 1,
            16 => foreign.policy_digest = "a".repeat(64),
            17 => {
                foreign.capabilities.insert("foreign-capability".into());
            }
            18 => foreign.principal.principal_id = "foreign-principal".into(),
            _ => foreign.principal.authority_digest = "a".repeat(64),
        }
        assert_ne!(foreign, expected.authority);
        assert!(fixture
            .store
            .adaptive_working_memory_source(id, version, effect_id, &foreign)
            .is_err());
    }
    assert_eq!(fixture.ready_source(), expected);
    assert!(fixture
        .store
        .adaptive_working_memory_source(Uuid::nil(), version, effect_id, &expected.authority)
        .is_err());
    assert_eq!(fixture.ready_source(), expected);
    assert!(fixture
        .store
        .adaptive_working_memory_source(
            Uuid::from_u128(999),
            version,
            effect_id,
            &fixture.session.grant.authority,
        )
        .unwrap()
        .is_none());
    assert_eq!(fixture.ready_source(), expected);
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn pending_and_unknown_require_exact_effect_and_only_the_allowed_prefix() {
    let mut fixture = Fixture::new();
    fixture.complete(inspect(1));
    let ready_version = fixture.session.version;
    let ready = fixture.ready_source();
    let model = fixture.claim_model();
    assert!(fixture.source(ready_version, Uuid::from_u128(17)).is_err());
    assert_eq!(
        fixture.source(ready_version, model.id).unwrap(),
        Some(ready.clone())
    );
    assert_eq!(
        fixture.source(ready_version, model.id).unwrap(),
        Some(ready)
    );
    let pending_version = fixture.session.version;
    fixture.advance(AdaptiveTransitionV1::MarkUnknown {
        effect: model.clone(),
    });
    let before = snapshot(&fixture.store);
    assert!(fixture.source(ready_version, model.id).is_err());
    assert!(fixture
        .source(pending_version, Uuid::from_u128(17))
        .is_err());
    let source = fixture.source(pending_version, model.id).unwrap().unwrap();
    assert_eq!(
        fixture.source(pending_version, model.id).unwrap(),
        Some(source.clone())
    );
    assert_eq!(source.head_version, pending_version);
    assert_eq!(source.completed_tool_count, 1);
    assert_eq!(source.model_calls, 2);
    assert_eq!(snapshot(&fixture.store), before);
    let late_resolution = AdaptiveTransitionV1::ResolveModel {
        effect: model.clone(),
        result_digest: "a".repeat(64),
        decision: AdaptiveModelDecisionV1::Tool {
            tool: inspect(2),
            tool_digest: crate::adaptive_tool_digest(&inspect(2)).unwrap(),
        },
    };
    assert_eq!(
        fixture
            .store
            .advance_adaptive_session(
                fixture.session.grant.session_id,
                fixture.session.version,
                Uuid::from_u128(100_000 - u128::from(fixture.session.version)),
                &late_resolution,
                &fixture.session.grant.authority,
                fixture.session.updated_at_ms + 1,
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict,
    );
    assert_eq!(snapshot(&fixture.store), before);
    assert_eq!(
        fixture.source(pending_version, model.id).unwrap(),
        Some(source)
    );

    // A normally resolved pending result also invalidates its old memory request.
    let mut resolved_fixture = Fixture::new();
    resolved_fixture.complete(inspect(1));
    let resolved_provider_version = resolved_fixture.session.version;
    let resolved_model = resolved_fixture.claim_model();
    assert!(resolved_fixture
        .source(resolved_provider_version, resolved_model.id)
        .unwrap()
        .is_some());
    resolved_fixture.advance(AdaptiveTransitionV1::ResolveModel {
        effect: resolved_model.clone(),
        result_digest: "a".repeat(64),
        decision: AdaptiveModelDecisionV1::Tool {
            tool: inspect(2),
            tool_digest: crate::adaptive_tool_digest(&inspect(2)).unwrap(),
        },
    });
    let resolved = snapshot(&resolved_fixture.store);
    assert!(resolved_fixture
        .source(resolved_provider_version, resolved_model.id)
        .is_err());
    assert_eq!(snapshot(&resolved_fixture.store), resolved);
}

#[test]
fn unobserved_tool_states_are_not_completed_memory_or_model_admission() {
    let mut fixture = Fixture::new();
    let tool_digest = fixture.resolve_tool(inspect(1));
    assert!(matches!(
        fixture.session.cursor,
        AdaptiveCursorV1::ReadyForTool { .. }
    ));
    let tool_effect = effect(600);
    assert!(fixture
        .source(fixture.session.version, tool_effect.id)
        .is_err());
    fixture.advance(AdaptiveTransitionV1::ClaimTool {
        effect: tool_effect.clone(),
        tool_digest,
    });
    assert!(fixture
        .source(fixture.session.version, tool_effect.id)
        .is_err());
    fixture.advance(AdaptiveTransitionV1::MarkUnknown {
        effect: tool_effect.clone(),
    });
    assert!(fixture
        .source(fixture.session.version, tool_effect.id)
        .is_err());
    fixture.advance(AdaptiveTransitionV1::ObserveTool {
        observation: AdaptiveObservationRefV1 {
            effect: tool_effect,
            observation_digest: "b".repeat(64),
        },
    });
    let source = fixture.ready_source();
    assert_eq!(source.completed_tool_count, 1);
    assert_eq!(source.rows.len(), 1);
}

#[test]
fn byte_budget_evicts_old_rows_but_never_latest_test_or_latest_observation() {
    let mut fixture = Fixture::with_capability_padding(80);
    fixture.complete(test_tool());
    for index in 1..20 {
        fixture.complete(inspect(index));
    }
    let source = fixture.ready_source();
    assert!(source.rows.len() < ADAPTIVE_WORKING_MEMORY_MAX_ROWS);
    assert_eq!(
        source.rows[0].tool_kind,
        AdaptiveWorkingMemoryToolKindV1::RunTests
    );
    assert_eq!(
        source.rows.last().unwrap().observation,
        fixture.session.last_observation.clone().unwrap()
    );
    assert_eq!(source.completed_tool_count, 20);
    assert_eq!(usize::from(source.omitted_count), 20 - source.rows.len());
    assert!(serde_json::to_vec(&source).unwrap().len() <= ADAPTIVE_WORKING_MEMORY_MAX_BYTES);
    source.validate().unwrap();
}

#[test]
fn foreign_project_history_is_not_followed_and_empty_history_is_valid() {
    let mut fixture = Fixture::new();
    let empty = fixture.ready_source();
    assert!(empty.rows.is_empty());
    assert!(empty.last_observation.is_none());
    assert_eq!(empty.completed_tool_count, 0);
    fixture.complete(inspect(1));
    let mut foreign = fixture.session.grant.clone();
    foreign.session_id = Uuid::from_u128(77_777);
    foreign.authority.project_id = crate::ProjectId::parse("other-project").unwrap();
    fixture
        .store
        .begin_adaptive_session(&foreign, &foreign.authority, NOW)
        .unwrap();
    let source = fixture
        .store
        .adaptive_working_memory_source(
            foreign.session_id,
            1,
            working_memory_model_effect_id(foreign.session_id, 1),
            &foreign.authority,
        )
        .unwrap()
        .unwrap();
    assert_eq!(source.session_id, foreign.session_id);
    assert_eq!(source.authority, foreign.authority);
    assert!(source.rows.is_empty());
    assert_eq!(source.completed_tool_count, 0);
    assert_eq!(fixture.ready_source().completed_tool_count, 1);
}

#[test]
fn same_session_continuation_preserves_observations_and_counts_windows_not_reviews() {
    let mut fixture = Fixture::new();
    fixture.complete(inspect(1));
    let model = fixture.claim_model();
    fixture.advance(AdaptiveTransitionV1::MarkUnknown {
        effect: model.clone(),
    });
    let mut auth = authorization(&fixture.session);
    auth.abandoned_model_effect = Some(model);
    auth.additional_model_calls = 3;
    let command = AdaptiveTransitionV1::ContinueGoverned {
        authorization: auth.clone(),
    };
    let continued = fixture
        .session
        .transition(&command, auth.issued_at_ms)
        .unwrap();
    {
        // Valid historical journal fixture, not a live continuation authorization.
        let mut connection = fixture.store.lock().unwrap();
        let tx = immediate(&mut connection).unwrap();
        let (_, digest) = load(&tx, fixture.session.grant.session_id)
            .unwrap()
            .unwrap();
        append(
            &tx,
            &namespace(fixture.session.grant.session_id),
            &Entry {
                previous_digest: Some(digest),
                command: Some(command),
                session: continued.clone(),
                recovery_feedback: None,
            },
        )
        .unwrap();
        update_head(&tx, &fixture.session, &continued).unwrap();
        tx.commit().unwrap();
    }
    fixture.session = continued;
    let source = fixture.ready_source();
    assert_eq!(source.continuation_windows, 1);
    assert_eq!(source.completed_tool_count, 1);
    assert_eq!(source.model_calls, 2);
    assert_eq!(source.active_model_ceiling, 5);
    assert_eq!(source.root_model_ceiling, 64);
    assert_eq!(source.last_observation, fixture.session.last_observation);
    assert!(fixture.session.requires_fresh_observation());
    let before = snapshot(&fixture.store);
    assert_eq!(fixture.ready_source(), source);
    assert_eq!(snapshot(&fixture.store), before);
    assert!(fixture.session.requires_fresh_observation());
}

#[test]
fn corrupted_digest_resealed_transition_foreign_entry_and_head_are_rejected() {
    for corruption in 0..8 {
        let mut fixture = Fixture::new();
        fixture.complete(inspect(1));
        let source = fixture.ready_source();
        assert_eq!(fixture.ready_source(), source);
        {
            let connection = fixture.store.lock().unwrap();
            let ns = namespace(fixture.session.grant.session_id);
            let key = format!("{:020}", fixture.session.version);
            if corruption == 0 {
                connection.execute("UPDATE workflow_operations SET request_digest=?1 WHERE operation_namespace=?2 AND operation_id=?3",
                    params!["f".repeat(64), ns, key]).unwrap();
            } else if corruption == 3 {
                connection
                    .execute("UPDATE workflow_adaptive_heads SET version=version+1", [])
                    .unwrap();
            } else if corruption == 4 {
                connection
                    .execute(
                        "UPDATE workflow_adaptive_heads SET updated_at_ms=updated_at_ms+1",
                        [],
                    )
                    .unwrap();
            } else if corruption == 5 {
                connection
                    .execute(
                        "UPDATE workflow_adaptive_heads SET authority_digest=?1",
                        params!["a".repeat(64)],
                    )
                    .unwrap();
            } else if corruption == 6 {
                connection
                    .execute("DELETE FROM workflow_adaptive_heads", [])
                    .unwrap();
            } else if corruption == 7 {
                connection.execute("UPDATE workflow_operations SET response=?1 WHERE operation_namespace=?2 AND operation_id=?3",
                    params![b"{}".as_slice(), ns, "00000000000000000001"]).unwrap();
            } else {
                let (_, mut entry) =
                    evidence_entry(&connection, &ns, fixture.session.version).unwrap();
                if corruption == 1 {
                    entry.session.tool_calls += 1;
                } else {
                    entry.session.grant.session_id = Uuid::from_u128(12345);
                }
                let digest =
                    canonical_sha256("sentinel.workflow.adaptive-entry.v1", &entry).unwrap();
                connection.execute("UPDATE workflow_operations SET request_digest=?1,response=?2 WHERE operation_namespace=?3 AND operation_id=?4",
                    params![digest, serde_json::to_vec(&entry).unwrap(), ns, key]).unwrap();
            }
        }
        let before = snapshot(&fixture.store);
        assert!(fixture
            .source(
                fixture.session.version,
                working_memory_model_effect_id(
                    fixture.session.grant.session_id,
                    fixture.session.version,
                )
            )
            .is_err());
        assert_eq!(snapshot(&fixture.store), before);
    }
}

#[test]
fn working_memory_cache_rejects_forged_funding_even_for_a_cached_prefix() {
    let root = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(root.path().join("memory.sqlite")).unwrap();
    let grant = grant();
    let session = store
        .begin_adaptive_session(&grant, &grant.authority, NOW)
        .unwrap()
        .1;
    let fixture = Fixture {
        root,
        store,
        session,
    };
    let source = fixture.ready_source();
    assert_eq!(fixture.ready_source(), source);
    {
        let mut connection = fixture.store.lock().unwrap();
        let mut epoch = epoch_for(&fixture.session, 10);
        let ns = namespace(source.session_id);
        epoch.receipt.request.source.resume_source.root_entry_digest =
            evidence_entry(&connection, &ns, 1).unwrap().0;
        epoch.receipt.request.source.resume_source.head_entry_digest =
            source.head_entry_digest.clone();
        epoch.binding = epoch.receipt.binding(1).unwrap();
        let auth = funded_authorization(&fixture.session, epoch, fixture.session.grant.deadline_ms);
        // A resealed descriptive epoch is not authoritative funding membership.
        assert!(
            require_funding_authorization_membership(&connection, &auth, &source.authority,)
                .is_err()
        );
        let command = AdaptiveTransitionV1::ContinueGoverned {
            authorization: auth.clone(),
        };
        let next = fixture
            .session
            .transition(&command, auth.issued_at_ms)
            .unwrap();
        let tx = immediate(&mut connection).unwrap();
        append(
            &tx,
            &ns,
            &Entry {
                previous_digest: Some(source.head_entry_digest.clone()),
                command: Some(command),
                session: next.clone(),
                recovery_feedback: None,
            },
        )
        .unwrap();
        update_head(&tx, &fixture.session, &next).unwrap();
        tx.commit().unwrap();
    }
    let before = snapshot(&fixture.store);
    for _ in 0..2 {
        assert!(fixture
            .source(source.provider_version, source.effect_id)
            .is_err());
    }
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn working_memory_cache_rechecks_outer_transaction_and_recovers_after_rollback() {
    let fixture = Fixture::new();
    let source = fixture.ready_source();
    let before = snapshot(&fixture.store);
    fixture
        .store
        .lock()
        .unwrap()
        .execute_batch("BEGIN IMMEDIATE; UPDATE workflow_adaptive_heads SET version=version+1")
        .unwrap();
    assert!(fixture
        .source(source.provider_version, source.effect_id)
        .is_err());
    {
        let connection = fixture.store.lock().unwrap();
        assert!(!connection.is_autocommit());
        connection.execute_batch("ROLLBACK").unwrap();
    }
    let (after, scopes) = crate::domain_store::validation_scope::with_completed_validations(|| {
        fixture.ready_source()
    });
    assert_eq!(after, source);
    assert!(
        !scopes.is_empty(),
        "rollback must not retain a transient proof"
    );
    let (_, repeated) = crate::domain_store::validation_scope::with_completed_validations(|| {
        assert_eq!(fixture.ready_source(), source);
    });
    assert!(repeated.is_empty());
    let after = snapshot(&fixture.store);
    // total_changes() includes the rolled-back fixture update, not any source read.
    let before: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let after: serde_json::Value = serde_json::from_slice(&after).unwrap();
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1], before[1]);
    assert_eq!(after[2].as_i64(), before[2].as_i64().map(|count| count + 1));
}

#[test]
fn working_memory_cache_rejects_index_only_damage_with_unchanged_rows() {
    use std::io::{Seek, SeekFrom, Write};

    let mut fixture = Fixture::new();
    fixture.complete(inspect(1));
    fixture.store.lock().unwrap().execute_batch(
        "CREATE INDEX memory_payload_index ON workflow_operations(created_at_ms); PRAGMA wal_checkpoint(TRUNCATE)",
    ).unwrap();
    let source = fixture.ready_source();
    assert_eq!(fixture.ready_source(), source);
    let before = snapshot(&fixture.store);
    {
        let connection = fixture.store.lock().unwrap();
        let page_size: u32 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        let root: u32 = connection
            .query_row(
                "SELECT rootpage FROM sqlite_schema WHERE name='memory_payload_index'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection.execute_batch("PRAGMA shrink_memory").unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.root.path().join("memory.sqlite"))
            .unwrap();
        file.seek(SeekFrom::Start(u64::from(root - 1) * u64::from(page_size)))
            .unwrap();
        file.write_all(&vec![0; page_size as usize]).unwrap();
        file.sync_all().unwrap();
    }
    assert_eq!(snapshot(&fixture.store), before);
    assert!(fixture
        .source(source.provider_version, source.effect_id)
        .is_err());
    assert!(fixture
        .source(source.provider_version, source.effect_id)
        .is_err());
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn source_shape_validation_rejects_bad_refs_labels_counts_and_order() {
    let mut fixture = Fixture::new();
    fixture.complete(inspect(1));
    fixture.complete(test_tool());
    let source = fixture.ready_source();
    for mutation in 0..8 {
        let mut invalid = source.clone();
        match mutation {
            0 => invalid.rows.reverse(),
            1 => invalid.rows[0].observation.observation_digest = "not-a-digest".into(),
            2 => invalid.rows[0].target = Some("../foreign".into()),
            3 => invalid.omitted_count += 1,
            4 => invalid.head_version += 1,
            5 => invalid.rows[0].target = Some("x".repeat(257)),
            6 => invalid.last_observation = None,
            _ => invalid.rows[0].program = Some("node".into()),
        }
        assert!(invalid.validate().is_err(), "mutation {mutation}");
    }
}
