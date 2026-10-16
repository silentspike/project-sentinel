//! Scripted store/Workbench results, not evidence of process or provider execution.

use super::*;
use crate::workbench::{
    with_private_observation_store_for_test, WorkbenchCoordinator, WorkbenchInvocationStore,
};
use sentinel_common::{WorkbenchCommandStatus, WorkbenchMessage, WorkbenchOutcome};
use sentinel_workflow::{
    adaptive_tool_digest, AdaptiveEffectV1, AdaptiveObservationRefV1, AdaptiveSessionV1,
    AdaptiveTransitionV1, AdaptiveWorkingMemoryToolKindV1,
};

mod native_qa_contract_tests {
    include!("native_qa_contract_tests.rs");
}

mod adaptive_handoff_composition_tests {
    include!("../work_correction/adaptive_handoff_composition_tests.rs");
}

struct FamilyFixture {
    api: WorkflowApi,
    session: AdaptiveSessionV1,
    tools: Arc<WorkbenchInvocationStore>,
    _temp: tempfile::TempDir,
}

impl FamilyFixture {
    fn new(family: &str, profile_id: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let (mut api, sales) = super::super::tests::fixture(&temp.path().join("company.sqlite"));
        api.workbench = Some(Arc::new(WorkbenchExecutionAdapter {
            store: Arc::clone(&api.store),
            authority: Arc::clone(api.authority.as_ref().unwrap()),
        }));
        let planning = accepted_context(&api, &sales, family);
        let accepted = &planning.source_proposal.binding;
        assert_eq!(accepted.governance.project_profile.profile_id, family);
        let developer = participant(accepted, CompanyRoleV1::Developer);
        assert_eq!(developer.profile.profile_id, profile_id);
        let developer = developer.clone();
        let decision: ProjectPlanningDecision = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "rationale": "Scripted native-family fixture plan.",
            "tasks": [{"key":"implement", "title":"Implement local program",
                "objective":"Implement and test the accepted local program.",
                "role":"developer", "depends_on":[]}]
        }))
        .unwrap();
        let items = api.bind_project_plan(&planning, &decision).unwrap();
        let planner = &planning.binding.grant.planner_principal;
        let apply = |command| {
            let outcome = api
                .core
                .apply_company_command(planner, Uuid::new_v4(), &command, now_unix_ms())
                .unwrap();
            let CompanyWorkflowResponseV1::Project(project) = outcome.response else {
                panic!("scripted fixture project transition")
            };
            *project
        };
        let planned = apply(CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: planning.source_project.project_id.clone(),
            expected_version: planning.source_project.version,
            items,
        });
        let active = apply(CompanyWorkflowCommandV1::ActivateProject {
            project_id: planned.project_id.clone(),
            expected_version: planned.version,
            reason_ref: "scripted retained-failure fixture".into(),
        });
        let assigned = api
            .assign_ready_model_work(planner, active, "family-retained-failure-fixture")
            .unwrap();
        let project = api
            .grant_initial_model_work(
                planner,
                assigned,
                &planning.binding.grant,
                "family-retained-failure-fixture",
            )
            .unwrap();
        assert_eq!(project.governance, accepted.governance);
        let binding = api
            .adaptive_provider_authority_for_claim(developer.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(binding.grant.authority.agent_id, developer.agent_id);
        assert_eq!(
            binding.grant.authority.principal.principal_id,
            developer.principal_id
        );
        assert_eq!(
            binding.grant.authority.profile_id,
            developer.profile.profile_id
        );
        assert_eq!(
            binding.grant.authority.profile_digest,
            developer.profile.digest
        );
        assert_eq!(binding.grant.authority.project_id, project.project_id);
        let session = api
            .store
            .adaptive_session(binding.grant.session_id, &binding.grant.authority)
            .unwrap()
            .unwrap();
        let tools =
            Arc::new(WorkbenchInvocationStore::open(temp.path().join("workbench.redb")).unwrap());
        Self {
            api,
            session,
            tools,
            _temp: temp,
        }
    }

    fn advance(&mut self, command: AdaptiveTransitionV1, now: u64) {
        self.session = self
            .api
            .store
            .advance_adaptive_session(
                self.session.grant.session_id,
                self.session.version,
                Uuid::new_v4(),
                &command,
                &self.session.grant.authority,
                now,
            )
            .unwrap()
            .1;
    }

    fn observe(
        &mut self,
        tool: WorkbenchTool,
        output: BTreeMap<String, String>,
    ) -> WorkbenchPrivateObservation {
        let now = now_unix_ms();
        let model = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "a".repeat(64),
        };
        self.advance(
            AdaptiveTransitionV1::ClaimModel {
                effect: model.clone(),
                previous_observation_digest: self
                    .session
                    .last_observation
                    .as_ref()
                    .map(|observation| observation.observation_digest.clone()),
            },
            now,
        );
        let tool_digest = adaptive_tool_digest(&tool).unwrap();
        self.advance(
            AdaptiveTransitionV1::ResolveModel {
                effect: model,
                result_digest: "b".repeat(64),
                decision: AdaptiveModelDecisionV1::Tool {
                    tool: tool.clone(),
                    tool_digest: tool_digest.clone(),
                },
            },
            now,
        );
        let adapter = self.api.workbench.as_ref().unwrap();
        let effect = adapter.adaptive_tool_effect(&self.session, &tool).unwrap();
        let request = adapter
            .build_adaptive_request(&self.session, &effect, &tool)
            .unwrap();
        assert_eq!(
            request.tool_profile,
            self.session.grant.authority.profile_id
        );
        assert_eq!(
            request.tool_profile_digest,
            self.session.grant.authority.profile_digest
        );
        assert_eq!(
            request.caller_id,
            self.session.grant.authority.principal.principal_id
        );
        assert_eq!(request.agent_id, self.session.grant.authority.agent_id);
        assert_eq!(
            request.project_id,
            self.session.grant.authority.project_id.0
        );
        assert_eq!(
            request.work_item_id,
            self.session.grant.authority.work_item_id.0
        );
        self.advance(
            AdaptiveTransitionV1::ClaimTool {
                effect: effect.clone(),
                tool_digest,
            },
            now,
        );
        self.tools.reserve(&request, now).unwrap();
        self.tools
            .mark_executing(&request.invocation_id, &request.input_digest, now)
            .unwrap();
        self.tools
            .accept_result(
                &WorkbenchMessage::Result {
                    schema_version: WORKBENCH_SCHEMA_VERSION,
                    invocation_id: request.invocation_id.clone(),
                    input_digest: request.input_digest.clone(),
                    // Transport succeeded; the scripted process status may still be nonzero.
                    outcome: WorkbenchOutcome::Succeeded,
                    resources: sentinel_common::WorkbenchResourceUsage::default(),
                    artifacts: Vec::new(),
                    error: None,
                    output,
                },
                now,
            )
            .unwrap();
        let authority = self.api.authority.as_ref().unwrap();
        let (profile, digest) = authority
            .profile_for_binding(&request.tool_profile)
            .unwrap();
        let observation = WorkbenchCoordinator::new(&self.tools, profile, digest)
            .private_observation(&request.invocation_id, authority.as_ref())
            .unwrap()
            .unwrap();
        observation
            .validate(&request.invocation_id, &request.input_digest)
            .unwrap();
        self.advance(
            AdaptiveTransitionV1::ObserveTool {
                observation: AdaptiveObservationRefV1 {
                    effect,
                    observation_digest: observation.digest().into(),
                },
            },
            now,
        );
        observation
    }

    fn inspect(&mut self, path: &str, content: &str) -> WorkbenchPrivateObservation {
        self.observe(
            WorkbenchTool::InspectFile {
                path: path.into(),
                max_bytes: 1024,
            },
            BTreeMap::from([("content".into(), content.into())]),
        )
    }

    fn run_tests(
        &mut self,
        tool: WorkbenchTool,
        exit_code: i32,
        stderr: &str,
    ) -> WorkbenchPrivateObservation {
        let mut output = WorkbenchCommandStatus {
            exit_code,
            stdout_bytes: 0,
            stderr_bytes: stderr.len() as u64,
        }
        .output();
        output.insert("stdout".into(), String::new());
        output.insert("stderr".into(), stderr.into());
        self.observe(tool, output)
    }

    fn context(&self) -> AdaptiveModelContext {
        let binding = self
            .api
            .adaptive_provider_authority_for_claim(self.session.grant.authority.agent_id)
            .unwrap()
            .unwrap();
        let (profile, digest) = self
            .api
            .authority
            .as_ref()
            .unwrap()
            .profile_for_binding(&binding.grant.authority.profile_id)
            .unwrap();
        with_private_observation_store_for_test(
            Arc::clone(&self.tools),
            profile.clone(),
            digest.into(),
            || self.api.prepare_adaptive_model(&binding).unwrap(),
        )
    }
}

fn retained_family_failure(
    family: &str,
    profile: &str,
    path: &str,
    tool: WorkbenchTool,
    failure: &str,
) {
    let mut fixture = FamilyFixture::new(family, profile);
    let grant = fixture.session.grant.clone();
    fixture.inspect(path, "scripted initial source\n");
    let failed = fixture.run_tests(tool.clone(), 1, failure);
    let failed_ref = fixture.session.last_observation.clone().unwrap();
    assert_eq!(failed_ref.observation_digest, failed.digest());
    assert_eq!(fixture.context().observation.unwrap(), failed);
    let current = fixture.inspect(path, "scripted corrected source\n");
    let context = fixture.context();
    context.validate_dispatch(now_unix_ms()).unwrap();
    assert_eq!(context.binding.grant, grant);
    assert_eq!(context.observation.as_ref().unwrap(), &current);
    assert!(!context.fresh_observation_required);
    let memory = context.working_memory.as_ref().unwrap();
    memory.validate(&context.binding).unwrap();
    assert_eq!(memory.source.authority, grant.authority);
    assert_eq!(memory.source.session_id, grant.session_id);
    let row = memory
        .source
        .rows
        .iter()
        .find(|row| row.observation == failed_ref)
        .unwrap();
    assert_eq!(row.tool_kind, AdaptiveWorkingMemoryToolKindV1::RunTests);
    let WorkbenchTool::RunTests {
        suite_id, program, ..
    } = &tool
    else {
        panic!("test suite")
    };
    assert_eq!(row.suite_id.as_deref(), Some(suite_id.as_str()));
    assert_eq!(row.program.as_deref(), Some(program.as_str()));
    let outcome = memory
        .outcomes
        .iter()
        .find(|outcome| outcome.effect_id == failed_ref.effect.id)
        .unwrap();
    assert!(outcome.available);
    assert_eq!(outcome.exit_code, Some(1));
    let value = serde_json::to_value(memory).unwrap();
    assert_eq!(
        value["retained_failure_output"]["effect_id"],
        failed_ref.effect.id.to_string()
    );
    assert_eq!(
        value["retained_failure_output"]["observation_digest"],
        failed.digest()
    );
    assert_eq!(value["retained_failure_output"]["stderr"]["text"], failure);
    assert_eq!(
        value["retained_failure_output"]["stderr"]["omitted_prefix_bytes"],
        0
    );
    let encoded_failure = serde_json::to_string(failure).unwrap();
    assert!(context.prompt().unwrap().contains(&encoded_failure));
    let debug = format!("{context:?}");
    for line in failure.lines().filter(|line| !line.is_empty()) {
        assert!(!debug.contains(line));
    }

    fixture.run_tests(tool, 0, "");
    assert!(fixture
        .context()
        .working_memory
        .unwrap()
        .retained_failure_output
        .is_none());
    fixture.inspect(path, "scripted source after passing test\n");
    let passing = fixture.context();
    assert!(passing
        .working_memory
        .as_ref()
        .unwrap()
        .retained_failure_output
        .is_none());
    assert!(!passing.prompt().unwrap().contains(&encoded_failure));
    assert_eq!(fixture.session.grant, grant);
    assert_eq!(
        (fixture.session.model_calls, fixture.session.tool_calls),
        (5, 5)
    );
    assert_eq!(
        fixture
            .api
            .store
            .adaptive_session(grant.session_id, &grant.authority)
            .unwrap()
            .unwrap(),
        fixture.session
    );
    let authority = fixture.api.authority.as_ref().unwrap();
    let (profile, digest) = authority
        .profile_for_binding(&grant.authority.profile_id)
        .unwrap();
    assert_eq!(
        WorkbenchCoordinator::new(&fixture.tools, profile, digest)
            .private_observation(&failed_ref.effect.id.to_string(), authority.as_ref())
            .unwrap()
            .unwrap(),
        failed
    );
}

#[test]
fn accepted_python_unittest_failure_survives_inspection_until_a_passing_test() {
    retained_family_failure(
        "python-project-v1",
        "python-coding-v1",
        "test_app.py",
        WorkbenchTool::RunTests {
            suite_id: "python-unittest".into(),
            program: "python3".into(),
            args: ["-I", "-m", "unittest", "discover"]
                .map(str::to_owned)
                .to_vec(),
        },
        "FAIL: test_result (test_app.ProgramTests.test_result)\nAssertionError: 2 != 3\n",
    );
}

#[test]
fn accepted_node_tests_failure_survives_inspection_until_a_passing_test() {
    retained_family_failure(
        "node-project-v1",
        "node-coding-v1",
        "app.test.mjs",
        WorkbenchTool::RunTests {
            suite_id: "node-tests".into(),
            program: "node".into(),
            args: ["--test", "--", "app.test.mjs"].map(str::to_owned).to_vec(),
        },
        "not ok 1 - program result\nAssertionError [ERR_ASSERTION]: 2 !== 3\n",
    );
}
