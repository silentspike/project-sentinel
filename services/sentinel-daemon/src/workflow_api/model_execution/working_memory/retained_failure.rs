//! Derived private text, never execution authority or a substitute for its full observation.

use sentinel_common::{
    NativeQaOutcome, WorkbenchCommandStatus, WorkbenchNativeQaStatus, WorkbenchOutcome,
    WorkbenchPrivateObservation, WORKBENCH_MAX_CALLER_RESULT_BYTES,
};
use sentinel_workflow::{AdaptiveWorkingMemoryCompletedRowV1, AdaptiveWorkingMemoryToolKindV1};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::WorkingMemoryOutcomeV1;

const MAX_STREAM_BYTES: usize = 2_048;
const MAX_SERIALIZED_BYTES: usize = 4_096;
const INVALID: &str = "retained failure output invalid";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RetainedTextExcerptV1 {
    pub(super) retained_text_bytes: usize,
    pub(super) omitted_prefix_bytes: usize,
    pub(super) text: String,
}

impl std::fmt::Debug for RetainedTextExcerptV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetainedTextExcerptV1")
            .field("retained_text_bytes", &self.retained_text_bytes)
            .field("omitted_prefix_bytes", &self.omitted_prefix_bytes)
            .finish_non_exhaustive()
    }
}

impl RetainedTextExcerptV1 {
    fn from_text(text: &str) -> Self {
        let start = suffix_start(text, MAX_STREAM_BYTES);
        Self {
            retained_text_bytes: text.len() - start,
            omitted_prefix_bytes: start,
            text: text[start..].to_owned(),
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.text.is_empty()
            || self.retained_text_bytes != self.text.len()
            || self.retained_text_bytes > MAX_STREAM_BYTES
            || self
                .retained_text_bytes
                .checked_add(self.omitted_prefix_bytes)
                .is_none_or(|total| total > WORKBENCH_MAX_CALLER_RESULT_BYTES)
        {
            return Err(INVALID);
        }
        Ok(())
    }

    fn shrink_to(&mut self, bytes: usize) -> Result<(), &'static str> {
        let start = suffix_start(&self.text, bytes);
        self.omitted_prefix_bytes = self
            .omitted_prefix_bytes
            .checked_add(start)
            .ok_or(INVALID)?;
        self.text.replace_range(..start, "");
        self.retained_text_bytes = self.text.len();
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedFailureOutputV1 {
    pub(super) effect_id: Uuid,
    pub(super) observation_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) stdout: Option<RetainedTextExcerptV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) stderr: Option<RetainedTextExcerptV1>,
}

impl std::fmt::Debug for RetainedFailureOutputV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetainedFailureOutputV1")
            .field("effect_id", &self.effect_id)
            .field("observation_digest", &self.observation_digest)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish_non_exhaustive()
    }
}

impl RetainedFailureOutputV1 {
    fn validate_shape(&self) -> Result<(), &'static str> {
        if self.effect_id.is_nil()
            || !super::sha256(&self.observation_digest)
            || (self.stdout.is_none() && self.stderr.is_none())
        {
            return Err(INVALID);
        }
        for excerpt in [&self.stdout, &self.stderr].into_iter().flatten() {
            excerpt.validate()?;
        }
        Ok(())
    }

    fn serialized_bytes(&self) -> Result<usize, &'static str> {
        // Some(value) has the same JSON representation as value; the parent owns its field key.
        serde_json::to_vec(&Some(self))
            .map(|bytes| bytes.len())
            .map_err(|_| "retained failure output encoding failed")
    }

    /// Structural/binding checks only. Authentic text requires full-observation projection
    /// and exact retained-context reconstruction; a digest alone cannot verify a suffix.
    pub(super) fn validate(
        &self,
        row: &AdaptiveWorkingMemoryCompletedRowV1,
        outcome: &WorkingMemoryOutcomeV1,
    ) -> Result<(), &'static str> {
        self.validate_shape()?;
        if !command_kind(row.tool_kind)
            || self.effect_id != row.observation.effect.id
            || self.effect_id != outcome.effect_id
            || self.observation_digest != row.observation.observation_digest
            || !super::sha256(&row.observation.effect.request_digest)
            || !outcome.available
            || outcome.outcome.is_none()
            || outcome
                .exit_code
                .is_some_and(|code| !(-1..=255).contains(&code))
            || (outcome.native_test_outcome.is_some()
                && row.tool_kind != AdaptiveWorkingMemoryToolKindV1::RunTests)
            || !explicit_failure(
                outcome.outcome,
                outcome.exit_code,
                outcome.native_test_outcome,
            )
            || self.serialized_bytes()? > MAX_SERIALIZED_BYTES
        {
            return Err(INVALID);
        }
        Ok(())
    }

    pub(super) fn fit_within(
        mut self,
        serialized_budget: usize,
    ) -> Result<Option<Self>, &'static str> {
        self.validate_shape()?;
        let budget = serialized_budget.min(MAX_SERIALIZED_BYTES);
        if self.serialized_bytes()? <= budget {
            return Ok(Some(self));
        }
        // Keep a useful one-character fallback. Different JSON escaping costs mean
        // raw text length alone cannot decide which stream has the smallest minimum.
        let mut minimum: Option<(usize, Self)> = None;
        for stderr in [true, false] {
            let excerpt = if stderr { &self.stderr } else { &self.stdout };
            let Some(mut excerpt) = excerpt.as_ref().cloned() else {
                continue;
            };
            let last = excerpt.text.chars().next_back().ok_or(INVALID)?.len_utf8();
            excerpt.shrink_to(last)?;
            let mut candidate = Self {
                effect_id: self.effect_id,
                observation_digest: self.observation_digest.clone(),
                stdout: None,
                stderr: None,
            };
            if stderr {
                candidate.stderr = Some(excerpt);
            } else {
                candidate.stdout = Some(excerpt);
            }
            let bytes = candidate.serialized_bytes()?;
            if bytes <= budget && minimum.as_ref().is_none_or(|(size, _)| bytes < *size) {
                minimum = Some((bytes, candidate));
            }
        }
        let Some((_, minimum)) = minimum else {
            return Ok(None);
        };
        loop {
            let bytes = self.serialized_bytes()?;
            if bytes <= budget {
                return Ok(Some(self));
            }
            let stdout_bytes = self.stdout.as_ref().map_or(0, |value| value.text.len());
            let stderr_bytes = self.stderr.as_ref().map_or(0, |value| value.text.len());
            let selected = if stdout_bytes >= stderr_bytes {
                &mut self.stdout
            } else {
                &mut self.stderr
            };
            let Some(excerpt) = selected.as_mut() else {
                return Ok(Some(minimum));
            };
            let keep = excerpt.text.len().saturating_sub(bytes - budget);
            excerpt.shrink_to(keep)?;
            if excerpt.text.is_empty() {
                *selected = None;
            }
            if self.stdout.is_none() && self.stderr.is_none() {
                return Ok(Some(minimum));
            }
        }
    }
}

fn suffix_start(text: &str, bytes: usize) -> usize {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

fn command_kind(kind: AdaptiveWorkingMemoryToolKindV1) -> bool {
    matches!(
        kind,
        AdaptiveWorkingMemoryToolKindV1::RunCommand | AdaptiveWorkingMemoryToolKindV1::RunTests
    )
}

fn explicit_failure(
    outcome: Option<WorkbenchOutcome>,
    exit_code: Option<i32>,
    native: Option<NativeQaOutcome>,
) -> bool {
    outcome.is_some()
        && (matches!(
            outcome,
            Some(WorkbenchOutcome::Failed | WorkbenchOutcome::TimedOut)
        ) || exit_code.is_some_and(|code| code != 0)
            || matches!(native, Some(NativeQaOutcome::Fail | NativeQaOutcome::Error)))
}

pub(super) fn from_observation(
    row: &AdaptiveWorkingMemoryCompletedRowV1,
    observation: &WorkbenchPrivateObservation,
) -> Result<Option<RetainedFailureOutputV1>, &'static str> {
    observation.validate(
        &row.observation.effect.id.to_string(),
        &row.observation.effect.request_digest,
    )?;
    if row.observation.effect.id.is_nil()
        || observation.digest() != row.observation.observation_digest
    {
        return Err("retained failure private observation changed");
    }
    if !command_kind(row.tool_kind) {
        return Ok(None);
    }
    let exit_code =
        WorkbenchCommandStatus::from_output(observation.output())?.map(|status| status.exit_code);
    let native = if row.tool_kind == AdaptiveWorkingMemoryToolKindV1::RunTests {
        WorkbenchNativeQaStatus::from_output(
            observation.output(),
            &row.observation.effect.id.to_string(),
            &row.observation.effect.request_digest,
            observation.outcome(),
        )?
        .map(|status| status.progress.outcome)
    } else {
        None
    };
    if !explicit_failure(Some(observation.outcome()), exit_code, native) {
        return Ok(None);
    }
    let excerpt = |key: &str| {
        observation
            .output()
            .get(key)
            .filter(|text| !text.is_empty())
            .map(|text| RetainedTextExcerptV1::from_text(text))
    };
    let value = RetainedFailureOutputV1 {
        effect_id: row.observation.effect.id,
        observation_digest: row.observation.observation_digest.clone(),
        stdout: excerpt("stdout"),
        stderr: excerpt("stderr"),
    };
    if value.stdout.is_none() && value.stderr.is_none() {
        return Ok(None);
    }
    value.fit_within(MAX_SERIALIZED_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::{
        NativeQaProgress, NativeQaStage, WorkbenchErrorClass, WorkbenchErrorInfo, WorkbenchMessage,
        WorkbenchResourceUsage, WORKBENCH_SCHEMA_VERSION,
    };
    use sentinel_workflow::{AdaptiveEffectV1, AdaptiveObservationRefV1};
    use std::collections::BTreeMap;

    fn observation(
        outcome: WorkbenchOutcome,
        output: BTreeMap<String, String>,
    ) -> WorkbenchPrivateObservation {
        WorkbenchPrivateObservation::from_result(&WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: Uuid::from_u128(856).to_string(),
            input_digest: "a".repeat(64),
            outcome,
            resources: WorkbenchResourceUsage::default(),
            artifacts: Vec::new(),
            output,
            error: (outcome != WorkbenchOutcome::Succeeded).then(|| WorkbenchErrorInfo {
                class: WorkbenchErrorClass::Tool,
                code: "test_failed".into(),
                safe_message: "bounded test diagnostic".into(),
                retryable: false,
            }),
        })
        .unwrap()
    }

    fn fixture(
        kind: AdaptiveWorkingMemoryToolKindV1,
        status: WorkbenchOutcome,
        exit: Option<i32>,
        stdout: &str,
        stderr: &str,
    ) -> (
        AdaptiveWorkingMemoryCompletedRowV1,
        WorkbenchPrivateObservation,
        WorkingMemoryOutcomeV1,
    ) {
        let mut output = BTreeMap::from([
            ("stdout".into(), stdout.into()),
            ("stderr".into(), stderr.into()),
        ]);
        if let Some(exit_code) = exit {
            output.extend(
                WorkbenchCommandStatus {
                    exit_code,
                    stdout_bytes: stdout.len() as u64,
                    stderr_bytes: stderr.len() as u64,
                }
                .output(),
            );
        }
        let observation = observation(status, output);
        let row = AdaptiveWorkingMemoryCompletedRowV1 {
            session_version: 4,
            entry_digest: "b".repeat(64),
            recorded_at_ms: 1_900_000_000_000,
            tool_kind: kind,
            tool_digest: "c".repeat(64),
            target: None,
            program: Some("python3".into()),
            suite_id: (kind == AdaptiveWorkingMemoryToolKindV1::RunTests)
                .then(|| "python-unittest".into()),
            labels_omitted: false,
            observation: AdaptiveObservationRefV1 {
                effect: AdaptiveEffectV1 {
                    id: Uuid::from_u128(856),
                    request_digest: "a".repeat(64),
                },
                observation_digest: observation.digest().to_owned(),
            },
        };
        let outcome = WorkingMemoryOutcomeV1 {
            effect_id: row.observation.effect.id,
            available: true,
            outcome: Some(status),
            exit_code: exit,
            native_test_outcome: None,
            artifact_digests: Vec::new(),
            workspace_content_digest: None,
        };
        (row, observation, outcome)
    }

    #[test]
    fn command_and_test_nonzero_exit_preserve_exact_private_text() {
        for kind in [
            AdaptiveWorkingMemoryToolKindV1::RunCommand,
            AdaptiveWorkingMemoryToolKindV1::RunTests,
        ] {
            let (row, observation, outcome) = fixture(
                kind,
                WorkbenchOutcome::Succeeded,
                Some(1),
                "expected 2, got 1",
                "AssertionError: bad result\n",
            );
            let value = from_observation(&row, &observation).unwrap().unwrap();
            value.validate(&row, &outcome).unwrap();
            assert_eq!(value.stdout.as_ref().unwrap().text, "expected 2, got 1");
            assert_eq!(
                value.stderr.as_ref().unwrap().text,
                "AssertionError: bad result\n"
            );
            assert_eq!(value.stderr.as_ref().unwrap().omitted_prefix_bytes, 0);
            assert_eq!(value.effect_id, row.observation.effect.id);
            assert_eq!(value.observation_digest, observation.digest());
        }
    }

    #[test]
    fn explicit_terminal_failures_do_not_require_a_process_exit() {
        for status in [WorkbenchOutcome::Failed, WorkbenchOutcome::TimedOut] {
            let (row, observation, outcome) = fixture(
                AdaptiveWorkingMemoryToolKindV1::RunTests,
                status,
                None,
                "",
                "process diagnostic",
            );
            let value = from_observation(&row, &observation).unwrap().unwrap();
            value.validate(&row, &outcome).unwrap();
            assert!(value.stdout.is_none());
        }
    }

    #[test]
    fn passing_unknown_and_unrelated_tools_do_not_supply_failure_output() {
        for (status, exit) in [
            (WorkbenchOutcome::Succeeded, Some(0)),
            (WorkbenchOutcome::Succeeded, None),
            (WorkbenchOutcome::Cancelled, None),
            (WorkbenchOutcome::DigestConflict, None),
        ] {
            let (row, observation, _) = fixture(
                AdaptiveWorkingMemoryToolKindV1::RunTests,
                status,
                exit,
                "not failure evidence",
                "",
            );
            assert!(from_observation(&row, &observation).unwrap().is_none());
        }
        for kind in [
            AdaptiveWorkingMemoryToolKindV1::ListDirectory,
            AdaptiveWorkingMemoryToolKindV1::InspectFile,
            AdaptiveWorkingMemoryToolKindV1::WriteFile,
            AdaptiveWorkingMemoryToolKindV1::ApplyPatch,
            AdaptiveWorkingMemoryToolKindV1::PackageArtifact,
        ] {
            let (row, observation, _) = fixture(
                kind,
                WorkbenchOutcome::Failed,
                Some(1),
                "private content",
                "",
            );
            assert!(from_observation(&row, &observation).unwrap().is_none());
        }
        let (row, observation, _) = fixture(
            AdaptiveWorkingMemoryToolKindV1::RunTests,
            WorkbenchOutcome::Failed,
            Some(1),
            "",
            "",
        );
        assert!(from_observation(&row, &observation).unwrap().is_none());
        let mut wire = serde_json::to_value(&observation).unwrap();
        wire["outcome"] = "unknown_outcome".into();
        assert!(serde_json::from_value::<WorkbenchPrivateObservation>(wire).is_err());
    }

    #[test]
    fn utf8_suffixes_and_counters_remain_exact_through_further_fitting() {
        let text = format!("prefix{}", "\u{20ac}\u{1f600}".repeat(700));
        let (row, observation, outcome) = fixture(
            AdaptiveWorkingMemoryToolKindV1::RunTests,
            WorkbenchOutcome::Succeeded,
            Some(1),
            "",
            &text,
        );
        let value = from_observation(&row, &observation).unwrap().unwrap();
        for budget in [512, 1024, 4096, usize::MAX] {
            let fitted = value.clone().fit_within(budget).unwrap().unwrap();
            fitted.validate(&row, &outcome).unwrap();
            let excerpt = fitted.stderr.as_ref().unwrap();
            assert!(text.ends_with(&excerpt.text));
            assert_eq!(excerpt.retained_text_bytes, excerpt.text.len());
            assert_eq!(
                excerpt.retained_text_bytes + excerpt.omitted_prefix_bytes,
                text.len()
            );
            assert!(excerpt.retained_text_bytes <= MAX_STREAM_BYTES);
            assert!(fitted.serialized_bytes().unwrap() <= budget.min(MAX_SERIALIZED_BYTES));
        }
    }

    #[test]
    fn json_escaping_is_charged_to_the_entire_optional_value() {
        let stdout = "\0\n\"\\".repeat(1000);
        let stderr = "\u{0001}\t".repeat(2000);
        let (row, observation, outcome) = fixture(
            AdaptiveWorkingMemoryToolKindV1::RunCommand,
            WorkbenchOutcome::Succeeded,
            Some(1),
            &stdout,
            &stderr,
        );
        let value = from_observation(&row, &observation).unwrap().unwrap();
        for budget in [512, 2048, 4095, 4096, usize::MAX] {
            let fitted = value.clone().fit_within(budget).unwrap().unwrap();
            fitted.validate(&row, &outcome).unwrap();
            assert!(
                serde_json::to_vec(&Some(&fitted)).unwrap().len()
                    <= budget.min(MAX_SERIALIZED_BYTES)
            );
            for (excerpt, original) in [(&fitted.stdout, &stdout), (&fitted.stderr, &stderr)] {
                if let Some(excerpt) = excerpt {
                    assert!(original.ends_with(&excerpt.text));
                    assert_eq!(
                        excerpt.retained_text_bytes + excerpt.omitted_prefix_bytes,
                        original.len()
                    );
                }
            }
            assert_eq!(fitted.clone().fit_within(budget).unwrap(), Some(fitted));
        }
    }

    #[test]
    fn exact_minimum_budget_and_unfittable_values_are_distinct() {
        let (row, observation, outcome) = fixture(
            AdaptiveWorkingMemoryToolKindV1::RunTests,
            WorkbenchOutcome::Failed,
            None,
            "x",
            "",
        );
        let value = from_observation(&row, &observation).unwrap().unwrap();
        let bytes = value.serialized_bytes().unwrap();
        assert!(value.clone().fit_within(bytes - 1).unwrap().is_none());
        assert!(value.clone().fit_within(0).unwrap().is_none());
        let fitted = value.fit_within(bytes).unwrap().unwrap();
        fitted.validate(&row, &outcome).unwrap();
        assert_eq!(fitted.serialized_bytes().unwrap(), bytes);
    }

    #[test]
    fn cheaper_minimal_stream_survives_different_escaping_costs() {
        let (row, observation, outcome) = fixture(
            AdaptiveWorkingMemoryToolKindV1::RunTests,
            WorkbenchOutcome::Failed,
            None,
            "\u{00e9}",
            "\0",
        );
        let value = from_observation(&row, &observation).unwrap().unwrap();
        let mut stdout_only = value.clone();
        stdout_only.stderr = None;
        let budget = stdout_only.serialized_bytes().unwrap();
        let fitted = value.fit_within(budget).unwrap().unwrap();
        fitted.validate(&row, &outcome).unwrap();
        assert_eq!(fitted, stdout_only);
    }

    #[test]
    fn private_observation_forgeries_reject_before_output_selection() {
        let (row, observation, _) = fixture(
            AdaptiveWorkingMemoryToolKindV1::RunTests,
            WorkbenchOutcome::Succeeded,
            Some(1),
            "private failure",
            "",
        );
        for field in ["effect", "request", "digest"] {
            let mut changed = row.clone();
            match field {
                "effect" => changed.observation.effect.id = Uuid::from_u128(857),
                "request" => changed.observation.effect.request_digest = "d".repeat(64),
                _ => changed.observation.observation_digest = "e".repeat(64),
            }
            assert!(from_observation(&changed, &observation).is_err());
        }
        let mut wire = serde_json::to_value(&observation).unwrap();
        wire["output"]["stdout"] = "changed private bytes".into();
        let tampered: WorkbenchPrivateObservation = serde_json::from_value(wire).unwrap();
        assert!(from_observation(&row, &tampered).is_err());
    }

    #[test]
    fn validation_rejects_foreign_unavailable_passing_and_malformed_projections() {
        let (row, observation, outcome) = fixture(
            AdaptiveWorkingMemoryToolKindV1::RunTests,
            WorkbenchOutcome::Succeeded,
            Some(1),
            "failure",
            "",
        );
        let value = from_observation(&row, &observation).unwrap().unwrap();
        for change in 0..10 {
            let mut projection = value.clone();
            match change {
                0 => projection.effect_id = Uuid::nil(),
                1 => projection.effect_id = Uuid::from_u128(857),
                2 => projection.observation_digest = "e".repeat(64),
                3 => projection.observation_digest = "A".repeat(64),
                4 => projection.stdout.as_mut().unwrap().retained_text_bytes += 1,
                5 => projection.stdout.as_mut().unwrap().omitted_prefix_bytes = usize::MAX,
                6 => projection.stdout.as_mut().unwrap().text.clear(),
                7 => projection.stdout = None,
                8 => {
                    let excerpt = projection.stdout.as_mut().unwrap();
                    excerpt.text = "x".repeat(MAX_STREAM_BYTES + 1);
                    excerpt.retained_text_bytes = excerpt.text.len();
                }
                _ => {
                    projection.stdout.as_mut().unwrap().omitted_prefix_bytes =
                        WORKBENCH_MAX_CALLER_RESULT_BYTES
                }
            }
            assert!(projection.validate(&row, &outcome).is_err());
            if matches!(change, 0 | 3..=9) {
                assert!(projection.fit_within(0).is_err());
            }
        }
        for change in 0..6 {
            let mut numeric = outcome.clone();
            match change {
                0 => numeric.effect_id = Uuid::from_u128(857),
                1 => numeric.available = false,
                2 => numeric.outcome = None,
                3 => numeric.exit_code = Some(0),
                4 => numeric.exit_code = Some(256),
                _ => numeric.native_test_outcome = Some(NativeQaOutcome::NotRun),
            }
            if change == 5 {
                numeric.exit_code = None;
            }
            assert!(value.validate(&row, &numeric).is_err());
        }
        let mut command = row.clone();
        command.tool_kind = AdaptiveWorkingMemoryToolKindV1::RunCommand;
        let mut native = outcome.clone();
        native.native_test_outcome = Some(NativeQaOutcome::Fail);
        assert!(value.validate(&command, &native).is_err());
        let mut oversized = value;
        let excerpt = oversized.stdout.as_mut().unwrap();
        excerpt.text = "\0".repeat(MAX_STREAM_BYTES);
        excerpt.retained_text_bytes = excerpt.text.len();
        assert!(oversized.validate(&row, &outcome).is_err());
    }

    #[test]
    fn native_fail_and_error_are_validated_against_full_native_status() {
        for (family, suite) in [
            ("python-project-v1", "python-qa-v1"),
            ("node-project-v1", "node-qa-v1"),
        ] {
            for (native, exit) in [(NativeQaOutcome::Fail, 1), (NativeQaOutcome::Error, 2)] {
                let (mut row, original, mut numeric) = fixture(
                    AdaptiveWorkingMemoryToolKindV1::RunTests,
                    WorkbenchOutcome::Failed,
                    Some(exit),
                    "retained native diagnostic",
                    "",
                );
                let mut output = original.output().clone();
                let status = WorkbenchNativeQaStatus {
                    invocation_id: row.observation.effect.id.to_string(),
                    input_digest: row.observation.effect.request_digest.clone(),
                    inventory_digest: Some("d".repeat(64)),
                    input_inventory_digest: Some("e".repeat(64)),
                    code: if native == NativeQaOutcome::Fail {
                        "tests_failed"
                    } else {
                        "runner_error"
                    }
                    .into(),
                    progress: NativeQaProgress {
                        schema_version: 1,
                        family: family.into(),
                        suite_id: suite.into(),
                        outcome: native,
                        inventory: NativeQaStage {
                            outcome: NativeQaOutcome::Pass,
                            planned: 1,
                            completed: 1,
                        },
                        syntax: NativeQaStage {
                            outcome: NativeQaOutcome::Pass,
                            planned: 1,
                            completed: 1,
                        },
                        tests: NativeQaStage {
                            outcome: native,
                            planned: 1,
                            completed: 0,
                        },
                    },
                };
                status.insert_output(&mut output);
                let observed = observation(WorkbenchOutcome::Failed, output.clone());
                row.observation.observation_digest = observed.digest().to_owned();
                numeric.native_test_outcome = Some(native);
                let value = from_observation(&row, &observed).unwrap().unwrap();
                value.validate(&row, &numeric).unwrap();
                let mut changed = status;
                changed.invocation_id = Uuid::from_u128(857).to_string();
                changed.insert_output(&mut output);
                let observed = observation(WorkbenchOutcome::Failed, output);
                row.observation.observation_digest = observed.digest().to_owned();
                assert!(from_observation(&row, &observed).is_err());
            }
        }
    }

    #[test]
    fn serde_is_strict_and_debug_never_contains_private_text() {
        let (row, observation, outcome) = fixture(
            AdaptiveWorkingMemoryToolKindV1::RunTests,
            WorkbenchOutcome::Failed,
            None,
            "PRIVATE-STDOUT-SENTINEL",
            "PRIVATE-STDERR-SENTINEL",
        );
        let value = from_observation(&row, &observation).unwrap().unwrap();
        let encoded = serde_json::to_vec(&value).unwrap();
        let restored: RetainedFailureOutputV1 = serde_json::from_slice(&encoded).unwrap();
        restored.validate(&row, &outcome).unwrap();
        assert_eq!(restored, value);
        let debug = format!("{value:?}");
        assert!(!debug.contains("PRIVATE-STDOUT-SENTINEL"));
        assert!(!debug.contains("PRIVATE-STDERR-SENTINEL"));
        assert!(!format!("{:?}", value.stdout.unwrap()).contains("PRIVATE-STDOUT-SENTINEL"));
        for nested in [false, true] {
            let mut wire = serde_json::to_value(&restored).unwrap();
            if nested {
                wire["stdout"]["unknown"] = true.into();
            } else {
                wire["unknown"] = true.into();
            }
            assert!(serde_json::from_value::<RetainedFailureOutputV1>(wire).is_err());
        }
    }

    #[test]
    fn shape_validation_cannot_replace_exact_private_context_reconstruction() {
        let (row, observation, outcome) = fixture(
            AdaptiveWorkingMemoryToolKindV1::RunTests,
            WorkbenchOutcome::Succeeded,
            Some(1),
            "failure",
            "",
        );
        let value = from_observation(&row, &observation).unwrap().unwrap();
        let mut forged = value.clone();
        forged.stdout.as_mut().unwrap().text = "forgery".into();
        // Same-length forged text passes shape checks, but changes the serialized
        // context that the parent must compare with the exact reserved digest.
        forged.validate(&row, &outcome).unwrap();
        assert_ne!(
            serde_json::to_vec(&value).unwrap(),
            serde_json::to_vec(&forged).unwrap()
        );
        assert_eq!(from_observation(&row, &observation).unwrap(), Some(value));
    }
}
