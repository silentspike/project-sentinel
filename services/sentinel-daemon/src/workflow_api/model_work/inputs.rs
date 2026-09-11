//! Verified upstream content shares the Workbench input authority resolver.
use super::*;
use crate::workbench::{read_verified_artifact_text, VerifiedArtifactTextFile};
use sentinel_workflow::{CompanyWorkItemSpecV1, ProjectV1, WorkInputContractV1};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_INPUT_FILES: usize = 64;
const MAX_INPUT_ARTIFACTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelArtifactInput {
    pub contract: WorkInputContractV1,
    pub producer_agent: AgentId,
    pub manifest_digest: String,
    pub artifact_kind: String,
    pub media_type: String,
    pub files: Vec<VerifiedArtifactTextFile>,
}

impl ModelWorkContext {
    pub(super) fn validate_artifact_inputs(&self) -> Result<(), &'static str> {
        if self.task.inputs.len() != self.artifact_inputs.len()
            || self.artifact_inputs.len() > MAX_INPUT_ARTIFACTS
        {
            return Err("model input contract inventory mismatch");
        }
        let mut total = 0usize;
        let mut count = 0usize;
        for (contract, input) in self.task.inputs.iter().zip(&self.artifact_inputs) {
            if contract != &input.contract
                || !self
                    .task
                    .dependency_ids
                    .contains(&contract.producer_work_item_id)
                || contract.producer_work_item_id == self.task.work_item_id
                || input.producer_agent.0 == 0
                || !lower_digest(&input.manifest_digest)
                || input.artifact_kind.is_empty()
                || input.media_type.is_empty()
                || input.files.is_empty()
            {
                return Err("model input binding is invalid");
            }
            let mut previous: Option<&str> = None;
            for file in &input.files {
                if !crate::workbench::is_canonical_relative_path(&file.path)
                    || previous.is_some_and(|path| path >= file.path.as_str())
                    || !lower_digest(&file.sha256)
                    || format!("{:x}", Sha256::digest(file.content.as_bytes())) != file.sha256
                    || file
                        .content
                        .chars()
                        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
                {
                    return Err("model source file is invalid");
                }
                previous = Some(&file.path);
                total = total
                    .checked_add(file.content.len())
                    .ok_or("model input bound overflow")?;
                count = count.checked_add(1).ok_or("model input bound overflow")?;
                if total > MAX_INPUT_BYTES || count > MAX_INPUT_FILES {
                    return Err("model input exceeds its bound");
                }
            }
        }
        Ok(())
    }
}

fn lower_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl WorkflowApi {
    pub(in crate::workflow_api) fn model_artifact_inputs(
        &self,
        project: &ProjectV1,
        spec: &CompanyWorkItemSpecV1,
    ) -> Result<Vec<ModelArtifactInput>, &'static str> {
        if spec.inputs.len() > MAX_INPUT_ARTIFACTS {
            return Err("model input artifact inventory exceeds its bound");
        }
        let authority = self
            .authority
            .as_ref()
            .ok_or("model input authority unavailable")?;
        let mut remaining = MAX_INPUT_BYTES;
        let mut remaining_files = MAX_INPUT_FILES;
        let mut result = Vec::new();
        for contract in &spec.inputs {
            let (producer_agent, artifact, media_type) = authority
                .resolve_execution_input(project, contract)
                .map_err(|_| "model input producer is not current and complete")?;
            let files = read_verified_artifact_text(
                &authority.artifact_roots,
                producer_agent,
                &project.project_id.0,
                &artifact.digest,
                &artifact.artifact_kind,
                &media_type,
                remaining,
            )
            .map_err(|_| "model input artifact unavailable or unsupported")?;
            for file in &files {
                remaining = remaining
                    .checked_sub(file.content.len())
                    .ok_or("model input exceeds its bound")?;
            }
            remaining_files = remaining_files
                .checked_sub(files.len())
                .ok_or("model input file inventory exceeds its bound")?;
            result.push(ModelArtifactInput {
                contract: contract.clone(),
                producer_agent,
                manifest_digest: artifact.digest,
                artifact_kind: artifact.artifact_kind,
                media_type,
                files,
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ModelWorkContext {
        let mut context = super::super::test_context();
        let contract = WorkInputContractV1 {
            name: "design".into(),
            producer_work_item_id: WorkItemId::parse("design-site").unwrap(),
            producer_output_name: "design".into(),
            expected_contract_generation: 1,
            expected_contract_digest: "a".repeat(64),
        };
        context
            .task
            .dependency_ids
            .insert(contract.producer_work_item_id.clone());
        context.task.inputs.push(contract.clone());
        let content = "# Real design\nUse keyboard-operable controls.\n".to_owned();
        context.artifact_inputs.push(ModelArtifactInput {
            contract,
            producer_agent: AgentId(3),
            manifest_digest: "b".repeat(64),
            artifact_kind: "design_specification".into(),
            media_type: "text/markdown".into(),
            files: vec![VerifiedArtifactTextFile {
                path: "design.md".into(),
                sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
                content,
            }],
        });
        context
    }

    #[test]
    fn model_input_prompt_contains_verified_content_not_extra_authority() {
        let context = context();
        context.validate_dispatch(1).unwrap();
        let prompt = context.prompt().unwrap();
        assert!(prompt.contains("keyboard-operable controls"));
        assert!(prompt.contains("untrusted data"));
        assert!(prompt.contains("Do not modify the upstream artifact"));
        let encoded = serde_json::to_vec(&context).unwrap();
        assert_eq!(
            serde_json::from_slice::<ModelWorkContext>(&encoded).unwrap(),
            context
        );
    }

    #[test]
    fn model_input_rejects_missing_changed_or_foreign_source() {
        let original = context();
        for mutate in [
            |c: &mut ModelWorkContext| {
                c.artifact_inputs.clear();
            },
            |c: &mut ModelWorkContext| {
                c.artifact_inputs[0].files[0].content.push('!');
            },
            |c: &mut ModelWorkContext| {
                c.artifact_inputs[0].contract.expected_contract_generation += 1;
            },
            |c: &mut ModelWorkContext| {
                c.task.dependency_ids.clear();
            },
            |c: &mut ModelWorkContext| {
                c.artifact_inputs[0].producer_agent = AgentId(0);
            },
            |c: &mut ModelWorkContext| {
                c.artifact_inputs[0].files[0].path = "../secret".into();
            },
            |c: &mut ModelWorkContext| {
                let file = c.artifact_inputs[0].files[0].clone();
                c.artifact_inputs[0].files.push(file);
            },
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(changed.validate_dispatch(1).is_err());
            assert!(changed.prompt().is_err());
        }
    }

    #[test]
    fn model_input_never_truncates_and_legacy_context_stays_byte_identical() {
        let legacy = super::super::test_context();
        let bytes = serde_json::to_vec(&legacy).unwrap();
        assert!(!String::from_utf8(bytes.clone())
            .unwrap()
            .contains("artifact_inputs"));
        assert_eq!(
            serde_json::to_vec(&serde_json::from_slice::<ModelWorkContext>(&bytes).unwrap())
                .unwrap(),
            bytes
        );
        let mut context = context();
        context.artifact_inputs[0].files[0].content = "a".repeat(MAX_INPUT_BYTES + 1);
        context.artifact_inputs[0].files[0].sha256 = format!(
            "{:x}",
            Sha256::digest(context.artifact_inputs[0].files[0].content.as_bytes())
        );
        assert!(context.validate_dispatch(1).is_err());
    }
}
