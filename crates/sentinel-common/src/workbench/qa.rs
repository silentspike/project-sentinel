//! Bounded, public-safe progress from the provisioned native evaluator only.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{WorkbenchCommandStatus, WorkbenchOutcome, WorkbenchRequest, WorkbenchTool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeQaOutcome {
    Pass,
    Fail,
    Error,
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeQaStage {
    pub outcome: NativeQaOutcome,
    pub planned: u16,
    pub completed: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeQaProgress {
    pub schema_version: u16,
    pub family: String,
    pub suite_id: String,
    pub outcome: NativeQaOutcome,
    pub inventory: NativeQaStage,
    pub syntax: NativeQaStage,
    pub tests: NativeQaStage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeQaInputBinding {
    pub files: u16,
    pub digest: String,
}

impl NativeQaInputBinding {
    pub fn from_request(request: &WorkbenchRequest) -> Result<Self, &'static str> {
        if !WorkbenchNativeQaStatus::requested(request) || request.inputs.len() > 64 {
            return Err("native QA input binding invalid");
        }
        Ok(Self {
            files: u16::try_from(request.inputs.len())
                .map_err(|_| "native QA input count invalid")?,
            digest: WorkbenchNativeQaStatus::expected_input_inventory_digest(request)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchNativeQaStatus {
    pub invocation_id: String,
    pub input_digest: String,
    pub inventory_digest: Option<String>,
    pub input_inventory_digest: Option<String>,
    pub code: String,
    pub progress: NativeQaProgress,
}

impl WorkbenchNativeQaStatus {
    pub const OUTPUT_KEY: &'static str = "native_qa_status";

    pub fn validate_inputs(&self, binding: &NativeQaInputBinding) -> Result<(), &'static str> {
        if binding.files > 64
            || binding.files != self.progress.inventory.planned
            || binding.digest.len() != 64
            || !binding
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || (self.progress.inventory.outcome == NativeQaOutcome::Pass
                && self.input_inventory_digest.as_deref() != Some(binding.digest.as_str()))
        {
            return Err("native QA reserved input inventory changed");
        }
        Ok(())
    }

    pub fn expected_input_inventory_digest(
        request: &WorkbenchRequest,
    ) -> Result<String, &'static str> {
        let mut entries: Vec<_> = request
            .inputs
            .iter()
            .map(|input| (&input.mount_path, &input.sha256))
            .collect();
        entries.sort_unstable();
        let bytes =
            serde_json::to_vec(&entries).map_err(|_| "native QA inventory encoding invalid")?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn validate_command(&self, command: &WorkbenchCommandStatus) -> Result<(), &'static str> {
        if command.stdout_bytes == 0
            || command.stdout_bytes > 4096
            || command.stderr_bytes != 0
            || !matches!(
                (self.progress.outcome, command.exit_code),
                (NativeQaOutcome::Pass, 0)
                    | (NativeQaOutcome::Fail, 1)
                    | (NativeQaOutcome::Error, 2)
            )
        {
            return Err("native QA command evidence invalid");
        }
        Ok(())
    }

    pub fn requested(request: &WorkbenchRequest) -> bool {
        matches!(&request.tool, WorkbenchTool::RunTests { suite_id, program, args }
            if request.tool_profile == "coding-qa-v1" && program == "sentinel-coding-qa"
                && matches!((suite_id.as_str(), args.first().map(String::as_str)),
                    ("python-qa-v1", Some("python-project-v1"))
                    | ("node-qa-v1", Some("node-project-v1"))))
    }

    pub fn from_runner(
        bytes: &[u8],
        request: &WorkbenchRequest,
        exit_code: i32,
    ) -> Result<Self, &'static str> {
        // Deserialize the struct directly: duplicate known keys are rejected,
        // unlike an intermediate serde_json::Value map.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            schema_version: u16,
            family: String,
            suite_id: String,
            outcome: NativeQaOutcome,
            native_status: NativeQaProgress,
            code: String,
            files: Option<u16>,
            bytes: Option<u64>,
            inventory_sha256: Option<String>,
            input_inventory_sha256: Option<String>,
            syntax_files: Option<u16>,
            tests: Option<u16>,
            skipped: Option<u16>,
        }
        if bytes.len() > 4096 || !Self::requested(request) {
            return Err("native QA runner binding invalid");
        }
        let envelope: Envelope =
            serde_json::from_slice(bytes).map_err(|_| "native QA summary invalid")?;
        let status = Self {
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            inventory_digest: envelope.inventory_sha256.clone(),
            input_inventory_digest: envelope.input_inventory_sha256.clone(),
            code: envelope.code.clone(),
            progress: envelope.native_status,
        };
        let WorkbenchTool::RunTests { suite_id, args, .. } = &request.tool else {
            unreachable!()
        };
        if envelope.schema_version != 1
            || envelope.family != args[0]
            || envelope.suite_id != *suite_id
            || envelope.family != status.progress.family
            || envelope.suite_id != status.progress.suite_id
            || envelope.outcome != status.progress.outcome
        {
            return Err("native QA summary binding invalid");
        }
        if !matches!(
            envelope.code.as_str(),
            "checks_passed"
                | "arguments_denied"
                | "family_denied"
                | "input_path_contract"
                | "input_collision"
                | "input_file_contract"
                | "input_total_limit"
                | "input_changed"
                | "tool_timeout"
                | "tool_output_limit"
                | "tool_terminated"
                | "source_missing"
                | "python_compile_failed"
                | "node_syntax_failed"
                | "tests_failed"
                | "tests_missing"
                | "test_plan_invalid"
                | "behavioral_assertion_failed"
                | "io_or_tool_error"
                | "runner_error"
        ) || match envelope.code.as_str() {
            "checks_passed" => envelope.outcome != NativeQaOutcome::Pass,
            "tests_missing" | "test_plan_invalid" | "tool_timeout" | "tool_output_limit"
            | "tool_terminated" | "io_or_tool_error" | "runner_error" => {
                envelope.outcome != NativeQaOutcome::Error
            }
            _ => envelope.outcome != NativeQaOutcome::Fail,
        } || envelope
            .files
            .is_some_and(|files| files != status.progress.inventory.planned)
            || envelope.bytes.is_some_and(|bytes| bytes > 64 * 1024 * 1024)
            || envelope.inventory_sha256.as_ref().is_some_and(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            || (status.progress.inventory.outcome == NativeQaOutcome::Pass
                && (envelope.files.is_none()
                    || envelope.bytes.is_none()
                    || envelope.inventory_sha256.is_none()
                    || envelope.input_inventory_sha256.as_deref()
                        != Some(Self::expected_input_inventory_digest(request)?.as_str())))
            || envelope
                .syntax_files
                .is_some_and(|files| files != status.progress.syntax.planned)
            || (status.progress.syntax.outcome == NativeQaOutcome::Pass
                && envelope.syntax_files.is_none())
            || envelope
                .tests
                .is_some_and(|tests| tests != status.progress.tests.completed)
            || envelope.skipped.is_some_and(|skipped| skipped != 0)
            || (status.progress.tests.outcome == NativeQaOutcome::Pass
                && (envelope.tests.is_none() || envelope.skipped.is_none()))
        {
            return Err("native QA summary metrics invalid");
        }
        status.validate(
            &request.invocation_id,
            &request.input_digest,
            request_outcome(exit_code)?,
            exit_code,
        )?;
        status.validate_inputs(&NativeQaInputBinding::from_request(request)?)?;
        Ok(status)
    }

    pub fn validate(
        &self,
        invocation_id: &str,
        input_digest: &str,
        outcome: WorkbenchOutcome,
        exit_code: i32,
    ) -> Result<(), &'static str> {
        use NativeQaOutcome::{Error, Fail, NotRun, Pass};
        let p = &self.progress;
        if self.invocation_id != invocation_id
            || self.input_digest != input_digest
            || p.schema_version != 1
            || request_outcome(exit_code)? != outcome
            || !matches!(
                (p.family.as_str(), p.suite_id.as_str()),
                ("python-project-v1", "python-qa-v1") | ("node-project-v1", "node-qa-v1")
            )
            || !matches!((p.outcome, exit_code), (Pass, 0) | (Fail, 1) | (Error, 2))
        {
            return Err("native QA status binding invalid");
        }
        for digest in [&self.inventory_digest, &self.input_inventory_digest] {
            if digest.as_ref().is_some_and(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }) || (p.inventory.outcome == Pass && digest.is_none())
            {
                return Err("native QA inventory evidence invalid");
            }
        }
        let code_matches_stage = match self.code.as_str() {
            "checks_passed" => p.outcome == Pass,
            "arguments_denied"
            | "family_denied"
            | "input_path_contract"
            | "input_collision"
            | "input_file_contract"
            | "input_total_limit"
            | "input_changed" => p.inventory.outcome == Fail,
            "source_missing" => p.syntax.outcome == Fail,
            "python_compile_failed" => p.family == "python-project-v1" && p.syntax.outcome == Fail,
            "node_syntax_failed" => p.family == "node-project-v1" && p.syntax.outcome == Fail,
            "tests_failed" | "behavioral_assertion_failed" => p.tests.outcome == Fail,
            "tests_missing" | "test_plan_invalid" => {
                p.tests.outcome == Error && p.syntax.outcome == NotRun
            }
            "tool_timeout" | "tool_output_limit" | "tool_terminated" | "io_or_tool_error"
            | "runner_error" => p.outcome == Error,
            _ => false,
        };
        if !code_matches_stage {
            return Err("native QA failure code contradicts stages");
        }
        let mut stopped = false;
        let mut terminal = Pass;
        // Plan validation precedes all interpreter invocations. Its failure is
        // recorded on tests without claiming syntax execution.
        if p.inventory.outcome == Pass && p.syntax.outcome == NotRun && p.tests.outcome == Error {
            if p.inventory.planned == 0
                || p.inventory.planned > 64
                || p.inventory.completed != p.inventory.planned
                || p.syntax.planned != 0
                || p.syntax.completed != 0
                || p.tests.planned != 0
                || p.tests.completed != 0
                || p.outcome != Error
            {
                return Err("native QA preflight progress invalid");
            }
            return Ok(());
        }
        for stage in [&p.inventory, &p.syntax, &p.tests] {
            if stage.planned > 64
                || stage.completed > stage.planned
                || (stage.outcome == NotRun && (stage.planned != 0 || stage.completed != 0))
                || (stage.outcome == Pass
                    && (stage.planned == 0 || stage.completed != stage.planned))
                || (stopped && stage.outcome != NotRun)
                || (!stopped && stage.outcome == NotRun)
            {
                return Err("native QA progress invalid");
            }
            if !stopped && stage.outcome != Pass {
                stopped = true;
                terminal = stage.outcome;
            }
        }
        if terminal != p.outcome {
            return Err("native QA outcome invalid");
        }
        Ok(())
    }

    pub fn complete(&self) -> bool {
        [
            &self.progress.inventory,
            &self.progress.syntax,
            &self.progress.tests,
        ]
        .iter()
        .all(|stage| {
            matches!(stage.outcome, NativeQaOutcome::Pass | NativeQaOutcome::Fail)
                && stage.planned > 0
                && stage.completed == stage.planned
        })
    }

    pub fn from_output(
        output: &BTreeMap<String, String>,
        invocation_id: &str,
        input_digest: &str,
        outcome: WorkbenchOutcome,
    ) -> Result<Option<Self>, &'static str> {
        let Some(value) = output.get(Self::OUTPUT_KEY) else {
            return Ok(None);
        };
        if value.len() > 4096 {
            return Err("native QA status too large");
        }
        let status: Self = serde_json::from_str(value).map_err(|_| "native QA status invalid")?;
        let command = WorkbenchCommandStatus::from_output(output)?
            .ok_or("native QA command status missing")?;
        status.validate_command(&command)?;
        status.validate(invocation_id, input_digest, outcome, command.exit_code)?;
        Ok(Some(status))
    }

    pub fn insert_output(&self, output: &mut BTreeMap<String, String>) {
        output.insert(
            Self::OUTPUT_KEY.to_owned(),
            serde_json::to_string(self).expect("native QA status serializes"),
        );
    }
}

fn request_outcome(exit_code: i32) -> Result<WorkbenchOutcome, &'static str> {
    match exit_code {
        0 => Ok(WorkbenchOutcome::Succeeded),
        1 | 2 => Ok(WorkbenchOutcome::Failed),
        _ => Err("native QA exit status invalid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn progress() -> NativeQaProgress {
        NativeQaProgress {
            schema_version: 1,
            family: "python-project-v1".into(),
            suite_id: "python-qa-v1".into(),
            outcome: NativeQaOutcome::Fail,
            inventory: NativeQaStage {
                outcome: NativeQaOutcome::Pass,
                planned: 2,
                completed: 2,
            },
            syntax: NativeQaStage {
                outcome: NativeQaOutcome::Pass,
                planned: 1,
                completed: 1,
            },
            tests: NativeQaStage {
                outcome: NativeQaOutcome::Fail,
                planned: 2,
                completed: 1,
            },
        }
    }

    #[test]
    fn reserved_inventory_requires_exact_count_and_observed_content() {
        let status = WorkbenchNativeQaStatus {
            invocation_id: "run".into(),
            input_digest: "digest".into(),
            inventory_digest: Some("a".repeat(64)),
            input_inventory_digest: Some("b".repeat(64)),
            code: "behavioral_assertion_failed".into(),
            progress: progress(),
        };
        let binding = NativeQaInputBinding {
            files: 2,
            digest: "b".repeat(64),
        };
        assert!(status.validate_inputs(&binding).is_ok());
        for invalid in [
            NativeQaInputBinding {
                files: 1,
                ..binding.clone()
            },
            NativeQaInputBinding {
                digest: "c".repeat(64),
                ..binding.clone()
            },
            NativeQaInputBinding {
                digest: "B".repeat(64),
                ..binding
            },
        ] {
            assert!(status.validate_inputs(&invalid).is_err());
        }
    }

    #[test]
    fn actual_outcomes_and_partial_progress_are_not_completion() {
        let mut status = WorkbenchNativeQaStatus {
            invocation_id: "run".into(),
            input_digest: "digest".into(),
            inventory_digest: Some("a".repeat(64)),
            input_inventory_digest: Some("b".repeat(64)),
            code: "behavioral_assertion_failed".into(),
            progress: progress(),
        };
        assert!(status
            .validate("run", "digest", WorkbenchOutcome::Failed, 1)
            .is_ok());
        assert!(!status.complete());
        status.progress.tests.completed = 2;
        assert!(status.complete());
        status.code = "python_compile_failed".into();
        assert!(status
            .validate("run", "digest", WorkbenchOutcome::Failed, 1)
            .is_err());
        status.code = "behavioral_assertion_failed".into();
        assert!(status
            .validate_command(&WorkbenchCommandStatus {
                exit_code: 1,
                stdout_bytes: 1,
                stderr_bytes: 0
            })
            .is_ok());
        assert!(status
            .validate_command(&WorkbenchCommandStatus {
                exit_code: 1,
                stdout_bytes: 0,
                stderr_bytes: 0
            })
            .is_err());
        assert!(status
            .validate_command(&WorkbenchCommandStatus {
                exit_code: 1,
                stdout_bytes: 1,
                stderr_bytes: 1
            })
            .is_err());
        assert!(status
            .validate("other", "digest", WorkbenchOutcome::Failed, 1)
            .is_err());
        assert!(status
            .validate("run", "digest", WorkbenchOutcome::Failed, 2)
            .is_err());
        status.progress.syntax.outcome = NativeQaOutcome::Error;
        assert!(status
            .validate("run", "digest", WorkbenchOutcome::Failed, 1)
            .is_err());
        let encoded = serde_json::to_string(&status).unwrap();
        assert!(
            serde_json::from_str::<WorkbenchNativeQaStatus>(&encoded.replace(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1"
            ))
            .is_err()
        );
    }
}
