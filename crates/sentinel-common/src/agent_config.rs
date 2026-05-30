//! Agent configuration parser fuer TOML-basierte Agent-Definitionen.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::{AgentId, AgentIdBounds};

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub identity: IdentityConfig,
    pub personality: PersonalityConfig,
    pub preferences: PreferencesConfig,
    pub background: BackgroundConfig,
    #[serde(default)]
    pub runtime: RuntimeSelectionConfig,
    #[serde(default)]
    pub capabilities: CapabilitiesConfig,
}

/// Tool-Capabilities und Sandbox-Einschraenkungen pro Agent.
/// Leere Capabilities = kein Tool-Zugriff (sicherer Default).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CapabilitiesConfig {
    /// Tool-Namen die der Agent nutzen darf (z.B. "file_read", "chat", "calendar").
    #[serde(default)]
    pub tools: Vec<String>,
    /// Erlaubte Dateisystem-Pfade fuer FileRead/FileWrite.
    #[serde(default)]
    pub sandbox_allowed_paths: Vec<String>,
}

/// Optional Nano-Container runtime selection for this workload.
///
/// Empty means the caller must provide an explicit fallback policy. The parser
/// does not inject a default runtime because DEV-007 makes the contract plural.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RuntimeSelectionConfig {
    #[serde(default)]
    pub nano_runtime: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IdentityConfig {
    pub id: u16,
    pub name: String,
    pub role: String,
    pub department: String,
    pub shift_set: u8,
    #[serde(default)]
    pub kpis: Vec<String>,
    #[serde(default)]
    pub reports_to: Option<String>,
    #[serde(default)]
    pub direct_reports: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PersonalityConfig {
    pub openness: f32,
    pub conscientiousness: f32,
    pub extraversion: f32,
    pub agreeableness: f32,
    pub neuroticism: f32,
    pub caffeine_tolerance: f32,
    pub morning_person: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreferencesConfig {
    pub favorite_room: String,
    pub coffee_preference: String,
    pub lunch_time: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackgroundConfig {
    pub bio: String,
    pub quirks: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AgentConfigValidation {
    pub agent_id_bounds: AgentIdBounds,
}

impl AgentConfigValidation {
    pub fn with_max_agent_id(max_agent_id: u16) -> Self {
        Self {
            agent_id_bounds: AgentIdBounds::new(max_agent_id),
        }
    }
}

impl PersonalityConfig {
    /// Validiert dass alle f32-Werte in [0.0, 1.0] liegen.
    pub fn validate(&self) -> Result<()> {
        let fields = [
            ("openness", self.openness),
            ("conscientiousness", self.conscientiousness),
            ("extraversion", self.extraversion),
            ("agreeableness", self.agreeableness),
            ("neuroticism", self.neuroticism),
            ("caffeine_tolerance", self.caffeine_tolerance),
        ];
        for (name, value) in fields {
            if !(0.0..=1.0).contains(&value) {
                return Err(anyhow!("{name} value {value} out of range [0.0, 1.0]"));
            }
        }
        Ok(())
    }
}

/// Laedt eine einzelne Agent-Config aus einer TOML-Datei.
pub fn load_agent_config(path: &Path) -> Result<AgentConfig> {
    load_agent_config_with_validation(path, AgentConfigValidation::default())
}

/// Laedt eine einzelne Agent-Config mit expliziten Validierungsgrenzen.
pub fn load_agent_config_with_validation(
    path: &Path,
    validation: AgentConfigValidation,
) -> Result<AgentConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read agent config: {}", path.display()))?;
    let config: AgentConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse agent config: {}", path.display()))?;
    config.personality.validate()?;
    AgentId::new_with_bounds(config.identity.id, validation.agent_id_bounds)
        .with_context(|| format!("Invalid agent id in {}", path.display()))?;
    Ok(config)
}

/// Laedt alle AGENT-*.toml Dateien aus einem Verzeichnis, sortiert nach ID.
pub fn load_all_agents(dir: &Path) -> Result<Vec<AgentConfig>> {
    load_all_agents_with_validation(dir, AgentConfigValidation::default())
}

/// Laedt alle AGENT-*.toml Dateien aus einem Verzeichnis mit expliziten Grenzen.
pub fn load_all_agents_with_validation(
    dir: &Path,
    validation: AgentConfigValidation,
) -> Result<Vec<AgentConfig>> {
    let mut agents = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml")
            && path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("AGENT-"))
        {
            agents.push(load_agent_config_with_validation(&path, validation)?);
        }
    }
    agents.sort_by_key(|a| a.identity.id);
    Ok(agents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_dir() -> std::path::PathBuf {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        std::path::PathBuf::from(manifest).join("../../config/agents")
    }

    #[test]
    fn parse_single_agent() {
        let path = config_dir().join("AGENT-01-THOMAS-CEO.toml");
        let config = load_agent_config(&path).unwrap();
        assert_eq!(config.identity.name, "Thomas Mueller");
        assert_eq!(config.identity.role, "CEO / Geschaeftsfuehrer / Gruender");
        assert_eq!(config.identity.id, 1);
    }

    #[test]
    fn validate_personality_valid() {
        let personality = PersonalityConfig {
            openness: 0.8,
            conscientiousness: 0.8,
            extraversion: 0.6,
            agreeableness: 0.7,
            neuroticism: 0.3,
            caffeine_tolerance: 0.7,
            morning_person: true,
        };
        assert!(personality.validate().is_ok());
    }

    #[test]
    fn validate_personality_rejects_invalid() {
        let personality = PersonalityConfig {
            openness: 1.5, // Out of range
            conscientiousness: 0.8,
            extraversion: 0.6,
            agreeableness: 0.7,
            neuroticism: 0.3,
            caffeine_tolerance: 0.7,
            morning_person: true,
        };
        assert!(personality.validate().is_err());
    }

    #[test]
    fn load_all_agents_sorted() {
        let agents = load_all_agents(&config_dir()).unwrap();
        assert_eq!(agents.len(), 60);
        // Check sorted by ID - first and last
        assert_eq!(agents[0].identity.id, 1);
        assert_eq!(agents[59].identity.id, 60);
        // Check monotonically increasing
        for i in 1..agents.len() {
            assert!(agents[i].identity.id > agents[i - 1].identity.id);
        }
    }

    #[test]
    fn no_id_gaps_or_duplicates() {
        let agents = load_all_agents(&config_dir()).unwrap();
        let ids: Vec<u16> = agents.iter().map(|a| a.identity.id).collect();
        let expected: Vec<u16> = (1..=60).collect();
        assert_eq!(ids, expected, "Agent IDs must be 1..=60 without gaps");
    }

    #[test]
    fn shift_distribution() {
        let agents = load_all_agents(&config_dir()).unwrap();
        let set0: Vec<_> = agents
            .iter()
            .filter(|a| a.identity.shift_set == 0)
            .collect();
        let set1: Vec<_> = agents
            .iter()
            .filter(|a| a.identity.shift_set == 1)
            .collect();
        let set2: Vec<_> = agents
            .iter()
            .filter(|a| a.identity.shift_set == 2)
            .collect();
        let set3: Vec<_> = agents
            .iter()
            .filter(|a| a.identity.shift_set == 3)
            .collect();
        assert_eq!(set0.len(), 9, "Sonder-Set (0) should have 9 agents");
        assert_eq!(set1.len(), 17, "Frueh-Set (1) should have 17 agents");
        assert_eq!(set2.len(), 17, "Mittel-Set (2) should have 17 agents");
        assert_eq!(set3.len(), 17, "Spaet-Set (3) should have 17 agents");
    }

    #[test]
    fn all_personality_values_valid() {
        let agents = load_all_agents(&config_dir()).unwrap();
        for agent in &agents {
            agent.personality.validate().unwrap_or_else(|e| {
                panic!("Agent {} personality invalid: {}", agent.identity.id, e);
            });
        }
    }

    #[test]
    fn required_fields_not_empty() {
        let agents = load_all_agents(&config_dir()).unwrap();
        for agent in &agents {
            assert!(
                !agent.identity.name.is_empty(),
                "Agent {} has empty name",
                agent.identity.id
            );
            assert!(
                !agent.identity.role.is_empty(),
                "Agent {} has empty role",
                agent.identity.id
            );
            assert!(
                !agent.identity.department.is_empty(),
                "Agent {} has empty department",
                agent.identity.id
            );
            assert!(
                !agent.preferences.favorite_room.is_empty(),
                "Agent {} has empty favorite_room",
                agent.identity.id
            );
            assert!(
                !agent.background.bio.is_empty(),
                "Agent {} has empty bio",
                agent.identity.id
            );
            assert!(
                !agent.background.quirks.is_empty(),
                "Agent {} has no quirks",
                agent.identity.id
            );
        }
    }

    #[test]
    fn agent_id_range() {
        use tempfile::NamedTempFile;

        fn agent_toml(id: u16) -> String {
            format!(
                r#"[identity]
id = {id}
name = "Test"
role = "Test"
department = "Test"
shift_set = 1

[personality]
openness = 0.5
conscientiousness = 0.5
extraversion = 0.5
agreeableness = 0.5
neuroticism = 0.5
caffeine_tolerance = 0.5
morning_person = true

[preferences]
favorite_room = "test"
coffee_preference = "test"
lunch_time = "12:00"

[background]
bio = "test"
quirks = []
"#
            )
        }

        fn write_agent(id: u16) -> NamedTempFile {
            let mut tmpfile = NamedTempFile::new().unwrap();
            std::io::Write::write_all(&mut tmpfile, agent_toml(id).as_bytes()).unwrap();
            tmpfile
        }

        let zero = write_agent(0);
        assert!(load_agent_config(zero.path()).is_err());

        let default_overflow = write_agent(61);
        assert!(load_agent_config(default_overflow.path()).is_err());

        let custom_valid = write_agent(61);
        let validation = AgentConfigValidation::with_max_agent_id(120);
        let loaded = load_agent_config_with_validation(custom_valid.path(), validation).unwrap();
        assert_eq!(loaded.identity.id, 61);

        let custom_overflow = write_agent(121);
        assert!(load_agent_config_with_validation(custom_overflow.path(), validation).is_err());
    }
}
