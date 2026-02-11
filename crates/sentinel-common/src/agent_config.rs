//! Agent configuration parser fuer TOML-basierte Agent-Definitionen.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub identity: IdentityConfig,
    pub personality: PersonalityConfig,
    pub preferences: PreferencesConfig,
    pub background: BackgroundConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IdentityConfig {
    pub id: u16,
    pub name: String,
    pub role: String,
    pub department: String,
    pub shift_set: u8,
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
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read agent config: {}", path.display()))?;
    let config: AgentConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse agent config: {}", path.display()))?;
    config.personality.validate()?;
    // Validiere id Bereich 1-54
    if config.identity.id == 0 || config.identity.id > 54 {
        return Err(anyhow!(
            "Agent id {} out of range (1-54)",
            config.identity.id
        ));
    }
    Ok(config)
}

/// Laedt alle AGENT-*.toml Dateien aus einem Verzeichnis, sortiert nach ID.
pub fn load_all_agents(dir: &Path) -> Result<Vec<AgentConfig>> {
    let mut agents = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml")
            && path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("AGENT-"))
        {
            agents.push(load_agent_config(&path)?);
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
        assert_eq!(config.identity.role, "CEO / Geschaeftsfuehrer");
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
        assert_eq!(agents.len(), 5);
        // Check sorted by ID
        assert_eq!(agents[0].identity.id, 1);
        assert_eq!(agents[1].identity.id, 2);
        assert_eq!(agents[2].identity.id, 3);
        assert_eq!(agents[3].identity.id, 4);
        assert_eq!(agents[4].identity.id, 5);
    }

    #[test]
    fn agent_id_range() {
        use tempfile::NamedTempFile;

        // Test id = 0 (invalid)
        let mut tmpfile = NamedTempFile::new().unwrap();
        std::io::Write::write_all(
            &mut tmpfile,
            b"[identity]\nid = 0\nname = \"Test\"\nrole = \"Test\"\ndepartment = \"Test\"\nshift_set = 1\n[personality]\nopenness = 0.5\nconscientiousness = 0.5\nextraversion = 0.5\nagreeableness = 0.5\nneuroticism = 0.5\ncaffeine_tolerance = 0.5\nmorning_person = true\n[preferences]\nfavorite_room = \"test\"\ncoffee_preference = \"test\"\nlunch_time = \"12:00\"\n[background]\nbio = \"test\"\nquirks = []\n",
        )
        .unwrap();
        let result = load_agent_config(tmpfile.path());
        assert!(result.is_err());

        // Test id = 55 (invalid)
        let mut tmpfile = NamedTempFile::new().unwrap();
        std::io::Write::write_all(
            &mut tmpfile,
            b"[identity]\nid = 55\nname = \"Test\"\nrole = \"Test\"\ndepartment = \"Test\"\nshift_set = 1\n[personality]\nopenness = 0.5\nconscientiousness = 0.5\nextraversion = 0.5\nagreeableness = 0.5\nneuroticism = 0.5\ncaffeine_tolerance = 0.5\nmorning_person = true\n[preferences]\nfavorite_room = \"test\"\ncoffee_preference = \"test\"\nlunch_time = \"12:00\"\n[background]\nbio = \"test\"\nquirks = []\n",
        )
        .unwrap();
        let result = load_agent_config(tmpfile.path());
        assert!(result.is_err());
    }
}
