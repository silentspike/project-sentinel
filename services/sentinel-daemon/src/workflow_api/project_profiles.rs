use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use sentinel_workflow::{CompanyRoleV1, WorkProfileBindingV1, WorkflowPortError};

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
