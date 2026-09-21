//! Private tool feedback, never an event, public DTO, or authority source.

use super::*;

/// Retention requires the same capability intersection as tool execution.
pub const WORKBENCH_RETAIN_OBSERVATION: &str = "observation.retain_private";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchPrivateObservation {
    schema_version: u16,
    invocation_id: String,
    request_digest: String,
    outcome: WorkbenchOutcome,
    artifacts: Vec<WorkbenchArtifactRef>,
    output: BTreeMap<String, String>,
    error: Option<WorkbenchErrorInfo>,
    digest: String,
}

impl std::fmt::Debug for WorkbenchPrivateObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkbenchPrivateObservation")
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl WorkbenchPrivateObservation {
    pub fn from_result(message: &WorkbenchMessage) -> Result<Self, &'static str> {
        let WorkbenchMessage::Result {
            schema_version,
            invocation_id,
            input_digest,
            outcome,
            artifacts,
            output,
            error,
            ..
        } = message
        else {
            return Err("observation requires a terminal result");
        };
        if *schema_version != WORKBENCH_SCHEMA_VERSION {
            return Err("unsupported observation result version");
        }
        let mut value = Self {
            schema_version: 1,
            invocation_id: invocation_id.clone(),
            request_digest: input_digest.clone(),
            outcome: *outcome,
            artifacts: artifacts.clone(),
            output: output.clone(),
            error: error.clone(),
            digest: String::new(),
        };
        value.digest = value.canonical_digest()?;
        value.validate_result(message)?;
        Ok(value)
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Only an authorized internal consumer may place this data in model context.
    pub fn output(&self) -> &BTreeMap<String, String> {
        &self.output
    }

    pub fn outcome(&self) -> WorkbenchOutcome {
        self.outcome
    }

    pub fn artifacts(&self) -> &[WorkbenchArtifactRef] {
        &self.artifacts
    }

    pub fn error(&self) -> Option<&WorkbenchErrorInfo> {
        self.error.as_ref()
    }

    pub fn validate(&self, invocation_id: &str, request_digest: &str) -> Result<(), &'static str> {
        if self.schema_version != 1
            || self.invocation_id != invocation_id
            || self.request_digest != request_digest
            || Uuid::parse_str(invocation_id).is_err()
            || request_digest.len() != 64
            || !request_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.digest != self.canonical_digest()?
        {
            return Err("private observation binding is invalid");
        }
        WorkbenchCommandStatus::from_output(&self.output)?;
        if self.artifacts.len() > 64
            || self
                .artifacts
                .iter()
                .any(|artifact| !valid_artifact(artifact))
            || !valid_terminal_shape(self.outcome, &self.artifacts, self.error.as_ref())
            || self.error.as_ref().is_some_and(|error| !valid_error(error))
        {
            return Err("private observation result is invalid");
        }
        Ok(())
    }

    pub fn validate_result(&self, message: &WorkbenchMessage) -> Result<(), &'static str> {
        self.validate_terminal_projection(message)?;
        let WorkbenchMessage::Result { output, .. } = message else {
            unreachable!();
        };
        if self.output != *output {
            return Err("private observation output binding changed");
        }
        Ok(())
    }

    pub fn validate_terminal_projection(
        &self,
        message: &WorkbenchMessage,
    ) -> Result<(), &'static str> {
        let WorkbenchMessage::Result {
            invocation_id,
            input_digest,
            outcome,
            artifacts,
            error,
            ..
        } = message
        else {
            return Err("observation requires a terminal result");
        };
        self.validate(invocation_id, input_digest)?;
        if self.outcome != *outcome || self.artifacts != *artifacts || self.error != *error {
            return Err("private observation result binding changed");
        }
        Ok(())
    }

    fn canonical_digest(&self) -> Result<String, &'static str> {
        let encoded = serde_json::to_vec(&(
            "sentinel.workbench.private-observation.v1",
            self.schema_version,
            &self.invocation_id,
            &self.request_digest,
            self.outcome,
            &self.artifacts,
            &self.output,
            &self.error,
        ))
        .map_err(|_| "private observation encoding failed")?;
        if encoded.len() > WORKBENCH_MAX_CALLER_RESULT_BYTES {
            return Err("private observation exceeds its bound");
        }
        Ok(format!("{:x}", Sha256::digest(&encoded)))
    }
}

fn valid_artifact(artifact: &WorkbenchArtifactRef) -> bool {
    artifact.sha256.len() == 64
        && artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && artifact.artifact_id == format!("sha256:{}", artifact.sha256)
        && valid_identifier(&artifact.artifact_kind)
        && artifact.media_type.len() <= 128
        && artifact.media_type.contains('/')
        && artifact.media_type.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'+' | b'-')
        })
        && artifact.manifest_path == format!("{}.manifest.json", artifact.sha256)
}

fn valid_terminal_shape(
    outcome: WorkbenchOutcome,
    artifacts: &[WorkbenchArtifactRef],
    error: Option<&WorkbenchErrorInfo>,
) -> bool {
    match outcome {
        WorkbenchOutcome::Succeeded => error.is_none(),
        WorkbenchOutcome::Failed
        | WorkbenchOutcome::Cancelled
        | WorkbenchOutcome::TimedOut
        | WorkbenchOutcome::DigestConflict => artifacts.is_empty() && error.is_some(),
    }
}

fn valid_error(error: &WorkbenchErrorInfo) -> bool {
    valid_identifier(&error.code)
        && !error.safe_message.is_empty()
        && error.safe_message.len() <= 4096
        && !error
            .safe_message
            .bytes()
            .any(|byte| byte.is_ascii_control())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> WorkbenchMessage {
        WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: "018f3f32-4f01-7f2c-a6c1-f6f4a81b2801".into(),
            input_digest: "a".repeat(64),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage::default(),
            artifacts: vec![WorkbenchArtifactRef {
                artifact_id: format!("sha256:{}", "b".repeat(64)),
                sha256: "b".repeat(64),
                artifact_kind: "source_tree".into(),
                media_type: "application/vnd.sentinel.source-tree+json".into(),
                size_bytes: 42,
                manifest_path: format!("{}.manifest.json", "b".repeat(64)),
            }],
            output: BTreeMap::from([("content".into(), "PRIVATE-OBSERVATION".into())]),
            error: None,
        }
    }

    #[test]
    fn private_observation_is_bound_bounded_and_debug_safe() {
        let value = WorkbenchPrivateObservation::from_result(&result()).unwrap();
        value
            .validate(&value.invocation_id, &value.request_digest)
            .unwrap();
        assert!(!format!("{value:?}").contains("PRIVATE-OBSERVATION"));
        assert_eq!(value.outcome(), WorkbenchOutcome::Succeeded);
        assert_eq!(value.artifacts()[0].sha256, "b".repeat(64));
        assert!(value.error().is_none());
        assert!(value
            .validate(&value.invocation_id, &"b".repeat(64))
            .is_err());
        assert!(value
            .validate(
                "018f3f32-4f01-7f2c-a6c1-f6f4a81b2802",
                &value.request_digest
            )
            .is_err());
        let mut tampered = value.clone();
        tampered.output.insert("content".into(), "changed".into());
        assert!(tampered
            .validate(&value.invocation_id, &value.request_digest)
            .is_err());
        let mut tampered_artifact = value.clone();
        tampered_artifact.artifacts[0].sha256 = "c".repeat(64);
        tampered_artifact.digest = tampered_artifact.canonical_digest().unwrap();
        assert!(tampered_artifact
            .validate(&value.invocation_id, &value.request_digest)
            .is_err());
        tampered.output.insert(
            "content".into(),
            "x".repeat(WORKBENCH_MAX_CALLER_RESULT_BYTES),
        );
        assert!(tampered.canonical_digest().is_err());
        let roundtrip: WorkbenchPrivateObservation =
            serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(roundtrip, value);
    }

    #[test]
    fn failed_observation_retains_only_safe_terminal_diagnostics() {
        let message = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: "018f3f32-4f01-7f2c-a6c1-f6f4a81b2802".into(),
            input_digest: "d".repeat(64),
            outcome: WorkbenchOutcome::Failed,
            resources: WorkbenchResourceUsage::default(),
            artifacts: Vec::new(),
            output: BTreeMap::from([
                ("exit_code".into(), "1".into()),
                ("stdout_bytes".into(), "0".into()),
                ("stderr_bytes".into(), "12".into()),
                ("stderr".into(), "bounded diagnostic".into()),
            ]),
            error: Some(WorkbenchErrorInfo {
                class: WorkbenchErrorClass::Tool,
                code: "command_failed".into(),
                safe_message: "the command failed".into(),
                retryable: false,
            }),
        };
        let observation = WorkbenchPrivateObservation::from_result(&message).unwrap();
        assert_eq!(observation.outcome(), WorkbenchOutcome::Failed);
        assert!(observation.artifacts().is_empty());
        assert_eq!(observation.error().unwrap().code, "command_failed");
        assert_eq!(
            observation.output().get("stderr").map(String::as_str),
            Some("bounded diagnostic")
        );
    }
}
