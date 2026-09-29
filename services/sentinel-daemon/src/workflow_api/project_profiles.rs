use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use sentinel_workflow::{CompanyRoleV1, WorkProfileBindingV1, WorkflowPortError};

const WEB_BEFORE_OBSERVATION_GENERATION: u64 = 1;
const WEB_BEFORE_OBSERVATION_BYTES: &[u8] = include_bytes!(
    "../../../../config/work-profiles/history/web-project-v1-before-observation.toml"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ProjectFamily {
    Web,
    Python,
    Node,
}

impl ProjectFamily {
    const ALL: [Self; 3] = [Self::Web, Self::Python, Self::Node];

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Web => "web-project-v1",
            Self::Python => "python-project-v1",
            Self::Node => "node-project-v1",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Web => include_bytes!("../../../../config/work-profiles/web-project-v1.toml"),
            Self::Python => {
                include_bytes!("../../../../config/work-profiles/python-project-v1.toml")
            }
            Self::Node => include_bytes!("../../../../config/work-profiles/node-project-v1.toml"),
        }
    }

    pub(super) fn parse(id: &str) -> Result<Self, WorkflowPortError> {
        Self::ALL
            .into_iter()
            .find(|family| family.id() == id)
            .ok_or(WorkflowPortError::AuthorityConflict)
    }

    pub(super) fn technical_qa_profile(self) -> &'static str {
        match self {
            Self::Web => "web-qa-v1",
            Self::Python | Self::Node => "coding-qa-v1",
        }
    }

    pub(super) fn execution_profile(
        self,
        role: CompanyRoleV1,
    ) -> Result<&'static str, WorkflowPortError> {
        match role {
            CompanyRoleV1::Designer => Ok("web-authoring-v1"),
            CompanyRoleV1::Developer => Ok(match self {
                Self::Web => "web-authoring-v1",
                Self::Python => "python-coding-v1",
                Self::Node => "node-coding-v1",
            }),
            CompanyRoleV1::Qa => Ok("web-review-v1"),
            _ => Err(WorkflowPortError::AuthorityConflict),
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct ProjectProfileCatalog {
    digests: BTreeMap<ProjectFamily, String>,
}

impl ProjectProfileCatalog {
    pub(super) fn load(config_dir: &Path) -> anyhow::Result<Self> {
        let mut catalog = Self::default();
        for family in ProjectFamily::ALL {
            let path = config_dir
                .join("work-profiles")
                .join(format!("{}.toml", family.id()));
            match fs::symlink_metadata(&path) {
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && family != ProjectFamily::Web =>
                {
                    continue
                }
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            let bytes = super::read_principal_bindings_file(&path)?;
            anyhow::ensure!(
                bytes == family.bytes(),
                "immutable project profile differs from release"
            );
            catalog.digests.insert(family, super::hex_sha256(&bytes));
        }
        Ok(catalog)
    }

    pub(super) fn binding(&self, id: &str) -> Result<WorkProfileBindingV1, WorkflowPortError> {
        let family = ProjectFamily::parse(id)?;
        Ok(WorkProfileBindingV1 {
            profile_id: family.id().to_owned(),
            generation: super::PROFILE_GENERATION,
            digest: self
                .digests
                .get(&family)
                .cloned()
                .ok_or(WorkflowPortError::Unavailable)?,
        })
    }

    pub(super) fn family(
        &self,
        binding: &WorkProfileBindingV1,
    ) -> Result<ProjectFamily, WorkflowPortError> {
        if self.binding(&binding.profile_id)? != *binding {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        ProjectFamily::parse(&binding.profile_id)
    }

    /// Recognizes recorded profiles only; execution admission must use `family()`.
    pub(super) fn resolve_recorded(
        &self,
        binding: &WorkProfileBindingV1,
    ) -> Result<ProjectFamily, WorkflowPortError> {
        let installed = self.binding(&binding.profile_id)?;
        let family = ProjectFamily::parse(&binding.profile_id)?;
        if installed == *binding
            || (family == ProjectFamily::Web
                && binding.generation == WEB_BEFORE_OBSERVATION_GENERATION
                && binding.digest == super::hex_sha256(WEB_BEFORE_OBSERVATION_BYTES))
        {
            return Ok(family);
        }
        Err(WorkflowPortError::AuthorityConflict)
    }

    #[cfg(test)]
    pub(super) fn embedded() -> Self {
        Self {
            digests: ProjectFamily::ALL
                .into_iter()
                .map(|family| (family, super::hex_sha256(family.bytes())))
                .collect(),
        }
    }

    #[cfg(test)]
    pub(super) fn test_web_digest(digest: String) -> Self {
        Self {
            digests: BTreeMap::from([(ProjectFamily::Web, digest)]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn historical_web_binding() -> WorkProfileBindingV1 {
        WorkProfileBindingV1 {
            profile_id: ProjectFamily::Web.id().to_owned(),
            generation: WEB_BEFORE_OBSERVATION_GENERATION,
            digest: super::super::hex_sha256(WEB_BEFORE_OBSERVATION_BYTES),
        }
    }

    #[test]
    fn recorded_project_catalog_recognizes_current_installed_bindings() {
        let catalog = ProjectProfileCatalog::embedded();
        for family in ProjectFamily::ALL {
            let binding = catalog.binding(family.id()).unwrap();
            assert_eq!(catalog.resolve_recorded(&binding).unwrap(), family);
            assert_eq!(catalog.family(&binding).unwrap(), family);
        }
    }

    #[test]
    fn recorded_web_history_is_exact_and_not_execution_admission() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work-profiles");
        fs::create_dir(&root).unwrap();
        let path = root.join("web-project-v1.toml");
        fs::write(&path, ProjectFamily::Web.bytes()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let catalog = ProjectProfileCatalog::load(temp.path()).unwrap();
        let historical = historical_web_binding();
        assert_eq!(
            historical.digest,
            "8573b066e5b7205e3af66a58352c4c698387b6f17e657f7f57f6daa9edadb1c6"
        );
        assert_eq!(
            catalog.resolve_recorded(&historical).unwrap(),
            ProjectFamily::Web
        );
        assert!(matches!(
            catalog.family(&historical),
            Err(WorkflowPortError::AuthorityConflict)
        ));
        assert_eq!(
            catalog.binding(ProjectFamily::Web.id()).unwrap().digest,
            super::super::hex_sha256(ProjectFamily::Web.bytes())
        );
    }

    #[test]
    fn recorded_project_catalog_rejects_unknown_digest_generation_and_cross_family() {
        let catalog = ProjectProfileCatalog::embedded();
        for family in ProjectFamily::ALL {
            let binding = catalog.binding(family.id()).unwrap();
            let mut altered = binding.clone();
            altered.digest = "0".repeat(64);
            assert!(matches!(
                catalog.resolve_recorded(&altered),
                Err(WorkflowPortError::AuthorityConflict)
            ));
            altered = binding;
            altered.generation += 1;
            assert!(matches!(
                catalog.resolve_recorded(&altered),
                Err(WorkflowPortError::AuthorityConflict)
            ));
        }
        let historical = historical_web_binding();
        let mut altered = historical.clone();
        altered.generation += 1;
        assert!(matches!(
            catalog.resolve_recorded(&altered),
            Err(WorkflowPortError::AuthorityConflict)
        ));
        for id in [
            ProjectFamily::Python.id(),
            ProjectFamily::Node.id(),
            "unknown-project-v1",
            "web-project-v1-before-observation",
        ] {
            altered = historical.clone();
            altered.profile_id = id.to_owned();
            assert!(matches!(
                catalog.resolve_recorded(&altered),
                Err(WorkflowPortError::AuthorityConflict)
            ));
        }
        let mut altered = catalog.binding(ProjectFamily::Python.id()).unwrap();
        altered.profile_id = ProjectFamily::Node.id().to_owned();
        assert!(matches!(
            catalog.resolve_recorded(&altered),
            Err(WorkflowPortError::AuthorityConflict)
        ));
    }

    #[test]
    fn recorded_project_catalog_requires_installed_family() {
        let empty = ProjectProfileCatalog::default();
        let embedded = ProjectProfileCatalog::embedded();
        for family in ProjectFamily::ALL {
            assert!(matches!(
                empty.resolve_recorded(&embedded.binding(family.id()).unwrap()),
                Err(WorkflowPortError::Unavailable)
            ));
        }
        let historical = historical_web_binding();
        assert!(matches!(
            empty.resolve_recorded(&historical),
            Err(WorkflowPortError::Unavailable)
        ));
        let without_web = ProjectProfileCatalog {
            digests: BTreeMap::from([(
                ProjectFamily::Python,
                super::super::hex_sha256(ProjectFamily::Python.bytes()),
            )]),
        };
        assert!(matches!(
            without_web.resolve_recorded(&historical),
            Err(WorkflowPortError::Unavailable)
        ));
        let web_only = ProjectProfileCatalog::test_web_digest(super::super::hex_sha256(
            ProjectFamily::Web.bytes(),
        ));
        assert!(matches!(
            web_only.resolve_recorded(&embedded.binding(ProjectFamily::Python.id()).unwrap()),
            Err(WorkflowPortError::Unavailable)
        ));
    }

    #[test]
    fn installed_project_catalog_rejects_changed_and_linked_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work-profiles");
        fs::create_dir(&root).unwrap();
        for family in ProjectFamily::ALL {
            let path = root.join(format!("{}.toml", family.id()));
            fs::write(&path, family.bytes()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let catalog = ProjectProfileCatalog::load(temp.path()).unwrap();
        for family in ProjectFamily::ALL {
            assert_eq!(
                catalog.binding(family.id()).unwrap(),
                ProjectProfileCatalog::embedded()
                    .binding(family.id())
                    .unwrap()
            );
        }
        let python_path = root.join("python-project-v1.toml");
        fs::remove_file(&python_path).unwrap();
        let catalog = ProjectProfileCatalog::load(temp.path()).unwrap();
        assert!(matches!(
            catalog.binding(ProjectFamily::Python.id()),
            Err(WorkflowPortError::Unavailable)
        ));
        assert!(catalog.binding(ProjectFamily::Web.id()).is_ok());
        fs::write(&python_path, b"changed profile").unwrap();
        assert!(ProjectProfileCatalog::load(temp.path()).is_err());
        fs::remove_file(&python_path).unwrap();
        symlink(root.join("node-project-v1.toml"), &python_path).unwrap();
        assert!(ProjectProfileCatalog::load(temp.path()).is_err());
        fs::remove_file(&python_path).unwrap();
        fs::hard_link(root.join("node-project-v1.toml"), &python_path).unwrap();
        assert!(ProjectProfileCatalog::load(temp.path()).is_err());
    }

    #[test]
    fn project_family_binding_rejects_unknown_digest_generation_and_cross_family() {
        let catalog = ProjectProfileCatalog::embedded();
        for family in ProjectFamily::ALL {
            let binding = catalog.binding(family.id()).unwrap();
            assert_eq!(catalog.family(&binding).unwrap(), family);
            let mut altered = binding.clone();
            altered.digest = "0".repeat(64);
            assert!(catalog.family(&altered).is_err());
            altered = binding;
            altered.generation += 1;
            assert!(catalog.family(&altered).is_err());
        }
        let mut binding = catalog.binding(ProjectFamily::Python.id()).unwrap();
        binding.profile_id = ProjectFamily::Node.id().to_owned();
        assert!(catalog.family(&binding).is_err());
        assert!(catalog.binding("unknown-project-v1").is_err());
        assert!(ProjectProfileCatalog::default()
            .binding(ProjectFamily::Python.id())
            .is_err());
    }

    #[test]
    fn family_role_selection_preserves_web_and_separates_coding_from_qa() {
        assert_eq!(
            ProjectFamily::Web
                .execution_profile(CompanyRoleV1::Developer)
                .unwrap(),
            "web-authoring-v1"
        );
        assert_eq!(
            ProjectFamily::Python
                .execution_profile(CompanyRoleV1::Developer)
                .unwrap(),
            "python-coding-v1"
        );
        assert_eq!(
            ProjectFamily::Node
                .execution_profile(CompanyRoleV1::Developer)
                .unwrap(),
            "node-coding-v1"
        );
        for family in ProjectFamily::ALL {
            assert_eq!(
                family.execution_profile(CompanyRoleV1::Designer).unwrap(),
                "web-authoring-v1"
            );
            assert_eq!(
                family.execution_profile(CompanyRoleV1::Qa).unwrap(),
                "web-review-v1"
            );
            assert!(family.execution_profile(CompanyRoleV1::Sales).is_err());
        }
    }
}
