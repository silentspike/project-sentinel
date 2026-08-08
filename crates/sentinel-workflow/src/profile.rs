use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{ActorRole, WorkItemSpec, WorkProfileRef, WorkflowError, WorkflowErrorCode};

const EMBEDDED_WEB_PROJECT_V1: &[u8] =
    include_bytes!("../../../config/work-profiles/web-project-v1.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct CanonicalWorkProfile {
    schema_version: u32,
    id: String,
    project: ProjectContract,
    roles: Vec<RoleContract>,
    tool_profiles: Vec<ToolProfileContract>,
    required_artifacts: Vec<ArtifactContract>,
    quality_gates: Vec<QualityGateContract>,
    #[serde(skip)]
    digest: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectContract {
    minimum_distinct_internal_roles: usize,
    minimum_worker_specialties: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct RoleContract {
    id: String,
    required_capabilities: BTreeSet<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ToolProfileContract {
    id: String,
    roles: BTreeSet<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArtifactContract {
    kind: String,
    producer_role: String,
    immutable: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct QualityGateContract {
    id: String,
    runner: String,
    required: bool,
}

impl CanonicalWorkProfile {
    pub fn embedded() -> Result<Self, WorkflowError> {
        Self::parse(EMBEDDED_WEB_PROJECT_V1)
    }

    pub fn load_verified(path: impl AsRef<Path>) -> Result<Self, WorkflowError> {
        let bytes = std::fs::read(path)
            .map_err(|_| profile_error("canonical work profile is unavailable"))?;
        let expected = Sha256::digest(EMBEDDED_WEB_PROJECT_V1);
        if Sha256::digest(&bytes) != expected {
            return Err(profile_error(
                "canonical work profile bytes do not match the release SSOT",
            ));
        }
        Self::parse(&bytes)
    }

    pub fn reference(&self) -> WorkProfileRef {
        WorkProfileRef {
            id: self.id.clone(),
            version: self.schema_version,
            digest: self.digest.clone(),
        }
    }

    pub fn require_reference(&self, reference: &WorkProfileRef) -> Result<(), WorkflowError> {
        if *reference == self.reference() {
            Ok(())
        } else {
            Err(profile_error(
                "persisted work profile reference does not match the canonical SSOT",
            ))
        }
    }

    pub fn require_id(&self, id: &str) -> Result<WorkProfileRef, WorkflowError> {
        if id == self.id {
            Ok(self.reference())
        } else {
            Err(profile_error("unknown work profile"))
        }
    }

    pub fn validate_work_graph(&self, items: &[WorkItemSpec]) -> Result<(), WorkflowError> {
        let roles: BTreeSet<ActorRole> = items.iter().map(|item| item.required_role).collect();
        let required_execution_roles = BTreeSet::from([
            ActorRole::ProjectManager,
            ActorRole::TechnicalLead,
            ActorRole::Designer,
            ActorRole::Developer,
            ActorRole::Qa,
            ActorRole::ReleaseManager,
        ]);
        if roles.len() < self.project.minimum_distinct_internal_roles
            || !required_execution_roles.is_subset(&roles)
        {
            return Err(profile_error(
                "work graph does not satisfy the canonical role topology",
            ));
        }

        let role_contracts: BTreeMap<ActorRole, &RoleContract> = self
            .roles
            .iter()
            .filter_map(|role| actor_role(&role.id).map(|actor| (actor, role)))
            .collect();
        for item in items {
            let contract = role_contracts.get(&item.required_role).ok_or_else(|| {
                profile_error("work item role is not declared by the canonical profile")
            })?;
            if !contract
                .required_capabilities
                .is_subset(&item.required_capabilities)
            {
                return Err(profile_error("work item omits a canonical role capability"));
            }
        }

        let worker_roles: BTreeSet<String> = self
            .tool_profiles
            .iter()
            .filter(|profile| profile.id == "web-authoring-v1")
            .flat_map(|profile| profile.roles.iter().cloned())
            .collect();
        let specialties = items
            .iter()
            .filter(|item| worker_roles.contains(role_id(item.required_role)))
            .map(|item| item.required_role)
            .collect::<BTreeSet<_>>();
        if specialties.len() < self.project.minimum_worker_specialties {
            return Err(profile_error(
                "work graph does not satisfy the worker specialty minimum",
            ));
        }

        for artifact in &self.required_artifacts {
            if !artifact.immutable
                || matches!(artifact.producer_role.as_str(), "sales" | "customer")
            {
                continue;
            }
            let producer_role = actor_role(&artifact.producer_role)
                .ok_or_else(|| profile_error("artifact producer role is not canonical"))?;
            if !items.iter().any(|item| {
                item.required_role == producer_role
                    && item.required_output_kinds.contains(&artifact.kind)
            }) {
                return Err(profile_error(
                    "work graph omits a canonical required artifact",
                ));
            }
        }

        let gates: BTreeSet<&str> = items
            .iter()
            .map(|item| item.quality_gate.as_str())
            .collect();
        for gate in self.quality_gates.iter().filter(|gate| gate.required) {
            if !self
                .tool_profiles
                .iter()
                .any(|profile| profile.id == gate.runner)
                || !gates.contains(gate.id.as_str())
            {
                return Err(profile_error(
                    "work graph omits a canonical required quality gate",
                ));
            }
        }
        Ok(())
    }

    pub fn gate_runner(&self, gate_id: &str) -> Option<&str> {
        self.quality_gates
            .iter()
            .find(|gate| gate.required && gate.id == gate_id)
            .map(|gate| gate.runner.as_str())
    }

    fn parse(bytes: &[u8]) -> Result<Self, WorkflowError> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| profile_error("work profile is not UTF-8"))?;
        let mut profile: Self =
            toml::from_str(text).map_err(|_| profile_error("work profile TOML is invalid"))?;
        profile.digest = hex_digest(bytes);
        if profile.schema_version != 1 || profile.id != "web-project-v1" {
            return Err(profile_error("unsupported canonical work profile identity"));
        }
        unique(profile.roles.iter().map(|value| value.id.as_str()), "role")?;
        unique(
            profile.tool_profiles.iter().map(|value| value.id.as_str()),
            "tool profile",
        )?;
        unique(
            profile
                .required_artifacts
                .iter()
                .map(|value| value.kind.as_str()),
            "artifact",
        )?;
        unique(
            profile.quality_gates.iter().map(|value| value.id.as_str()),
            "quality gate",
        )?;
        Ok(profile)
    }
}

fn unique<'a>(values: impl Iterator<Item = &'a str>, kind: &str) -> Result<(), WorkflowError> {
    let values: Vec<&str> = values.collect();
    if values.iter().collect::<BTreeSet<_>>().len() == values.len() {
        Ok(())
    } else {
        Err(profile_error(format!(
            "canonical profile contains duplicate {kind} identifiers"
        )))
    }
}

fn actor_role(id: &str) -> Option<ActorRole> {
    Some(match id {
        "sales" => ActorRole::Sales,
        "project_manager" => ActorRole::ProjectManager,
        "technical_lead" => ActorRole::TechnicalLead,
        "designer" => ActorRole::Designer,
        "developer" => ActorRole::Developer,
        "qa" => ActorRole::Qa,
        "release_manager" => ActorRole::ReleaseManager,
        "gaia" => ActorRole::Gaia,
        _ => return None,
    })
}

fn role_id(role: ActorRole) -> &'static str {
    match role {
        ActorRole::Customer => "customer",
        ActorRole::Sales => "sales",
        ActorRole::ProjectManager => "project_manager",
        ActorRole::TechnicalLead => "technical_lead",
        ActorRole::Designer => "designer",
        ActorRole::Developer => "developer",
        ActorRole::Qa => "qa",
        ActorRole::ReleaseManager => "release_manager",
        ActorRole::Gaia => "gaia",
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}

fn profile_error(message: impl Into<String>) -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::DigestConflict, false, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_profile_rejects_unknown_and_digest_mismatched_references() {
        let profile = CanonicalWorkProfile::embedded().expect("embedded profile");
        assert_eq!(
            profile
                .require_id("unknown-profile")
                .expect_err("unknown profile")
                .code,
            WorkflowErrorCode::DigestConflict
        );
        let mut reference = profile.reference();
        reference.digest = "0".repeat(64);
        assert_eq!(
            profile
                .require_reference(&reference)
                .expect_err("digest mismatch")
                .code,
            WorkflowErrorCode::DigestConflict
        );
    }

    #[test]
    fn verified_profile_loader_rejects_tampered_bytes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("web-project-v1.toml");
        let mut bytes = EMBEDDED_WEB_PROJECT_V1.to_vec();
        bytes.extend_from_slice(b"\n# tampered\n");
        std::fs::write(&path, bytes).expect("write tampered profile");
        assert_eq!(
            CanonicalWorkProfile::load_verified(path)
                .expect_err("tampered bytes")
                .code,
            WorkflowErrorCode::DigestConflict
        );
    }
}
