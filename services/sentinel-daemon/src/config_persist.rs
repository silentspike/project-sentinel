//! Atomarer config_dir-Write-Back der angewandten Firmen-Config (#425).
//!
//! Nach erfolgreichem Runtime-Apply schreibt der Daemon die neue Config zurueck in seinen
//! `config_dir`, damit sie einen Restart ueberlebt (Daemon laedt beim Start aus `config_dir`,
//! orchestrator.rs:579 Agents / 818 Rooms) und die Laufzeit-Welt NICHT von der TOML-SSOT driftet.
//! Der Daemon ist **alleiniger** `config_dir`-Schreiber (#420/ctl liest nur + schickt Inline-JSON).
//!
//! Schreiben ist atomar (Temp-Datei + `rename`) und legt vorher ein Backup an (restore-faehig,
//! zusaetzlich zum ECS-Safety-Snapshot). Dateinamen bleiben stabil: eine existierende Agent-TOML
//! wird per `identity.id` wiederverwendet, neue Agents bekommen `AGENT-<id:02>-<SLUG>.toml`,
//! entfernte Agents werden geloescht (deckt Live-Delta + Fresh-Load ab).

use anyhow::{Context, Result};
use sentinel_common::agent_config::{load_agent_config, AgentConfig};
use sentinel_common::room::BuildingConfig;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Ergebnis eines Write-Backs (fuer Logging / `ConfigApplied`-Event).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PersistResult {
    pub agents_written: usize,
    pub agents_removed: usize,
    pub rooms_written: bool,
    pub backup_dir: Option<PathBuf>,
}

/// Schreibt die angewandte Config atomar in den `config_dir` zurueck.
///
/// Reihenfolge: Backup → Agent-TOMLs atomar schreiben (Namen per id wiederverwenden) → entfernte
/// Agents loeschen → `rooms.toml` atomar schreiben. `backup_label` (z.B. der Tick) macht das
/// Backup-Verzeichnis eindeutig.
pub fn persist_company_config(
    config_dir: &Path,
    agents: &[AgentConfig],
    building: &BuildingConfig,
    backup_label: &str,
) -> Result<PersistResult> {
    let agents_dir = config_dir.join("agents");
    let rooms_path = config_dir.join("rooms.toml");

    // 1. Backup des aktuellen Zustands (vor jeder Mutation).
    let backup_dir = config_dir.join(format!(".backup-{backup_label}"));
    backup_existing(&agents_dir, &rooms_path, &backup_dir)?;

    // 2. id -> existierender Dateipfad, damit Dateinamen stabil bleiben.
    let existing = existing_agent_files(&agents_dir)?;
    std::fs::create_dir_all(&agents_dir)
        .with_context(|| format!("create agents dir {}", agents_dir.display()))?;

    // 3. Agents atomar schreiben.
    let mut new_ids = HashSet::new();
    let mut agents_written = 0usize;
    for agent in agents {
        new_ids.insert(agent.identity.id);
        let target = existing
            .get(&agent.identity.id)
            .cloned()
            .unwrap_or_else(|| agents_dir.join(agent_file_name(agent)));
        let toml = toml::to_string(agent)
            .with_context(|| format!("serialize agent {}", agent.identity.id))?;
        atomic_write(&target, toml.as_bytes())?;
        agents_written += 1;
    }

    // 4. Entfernte Agents loeschen (Live-Despawn + Fresh-Load).
    let mut agents_removed = 0usize;
    for (id, path) in &existing {
        if !new_ids.contains(id) {
            std::fs::remove_file(path)
                .with_context(|| format!("remove stale agent file {}", path.display()))?;
            agents_removed += 1;
        }
    }

    // 5. rooms.toml atomar schreiben.
    let rooms_toml = toml::to_string(building).context("serialize building config")?;
    atomic_write(&rooms_path, rooms_toml.as_bytes())?;

    Ok(PersistResult {
        agents_written,
        agents_removed,
        rooms_written: true,
        backup_dir: Some(backup_dir),
    })
}

/// Dateiname fuer einen neuen Agent: `AGENT-<id:02>-<SLUG>.toml` (Loader matcht auf `AGENT-` Prefix).
fn agent_file_name(agent: &AgentConfig) -> String {
    let slug: String = agent
        .identity
        .name
        .to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("AGENT-{:02}.toml", agent.identity.id)
    } else {
        format!("AGENT-{:02}-{}.toml", agent.identity.id, slug)
    }
}

/// `id -> Pfad` fuer alle existierenden `AGENT-*.toml` (per geparster `identity.id`).
fn existing_agent_files(agents_dir: &Path) -> Result<HashMap<u16, PathBuf>> {
    let mut map = HashMap::new();
    if !agents_dir.exists() {
        return Ok(map);
    }
    for entry in std::fs::read_dir(agents_dir)? {
        let path = entry?.path();
        let is_agent_toml = path.extension().is_some_and(|e| e == "toml")
            && path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("AGENT-"));
        if is_agent_toml {
            if let Ok(cfg) = load_agent_config(&path) {
                map.insert(cfg.identity.id, path);
            }
        }
    }
    Ok(map)
}

/// Kopiert das aktuelle `agents/`-Verzeichnis + `rooms.toml` nach `backup_dir` (restore-faehig).
fn backup_existing(agents_dir: &Path, rooms_path: &Path, backup_dir: &Path) -> Result<()> {
    let backup_agents = backup_dir.join("agents");
    std::fs::create_dir_all(&backup_agents)
        .with_context(|| format!("create backup dir {}", backup_agents.display()))?;
    if agents_dir.exists() {
        for entry in std::fs::read_dir(agents_dir)? {
            let path = entry?.path();
            if path.is_file() {
                let name = path.file_name().context("backup source file has no name")?;
                std::fs::copy(&path, backup_agents.join(name))
                    .with_context(|| format!("backup {}", path.display()))?;
            }
        }
    }
    if rooms_path.exists() {
        std::fs::copy(rooms_path, backup_dir.join("rooms.toml"))
            .with_context(|| format!("backup {}", rooms_path.display()))?;
    }
    Ok(())
}

/// Atomarer Schreibvorgang: Temp-Datei im selben Verzeichnis → fsync → `rename`.
fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
    let dir = target.parent().context("target has no parent dir")?;
    let file_name = target.file_name().context("target has no file name")?;
    let tmp = dir.join(format!(".{}.tmp", file_name.to_string_lossy()));
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("create temp {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::agent_config::{
        load_all_agents, BackgroundConfig, CapabilitiesConfig, IdentityConfig, PersonalityConfig,
        PreferencesConfig, RuntimeSelectionConfig,
    };
    use sentinel_common::room::{BuildingMeta, RoomConfig, RoomType};

    fn agent(id: u16, name: &str, role: &str) -> AgentConfig {
        AgentConfig {
            identity: IdentityConfig {
                id,
                name: name.to_string(),
                role: role.to_string(),
                department: "Dev".to_string(),
                shift_set: 1,
                kpis: vec![],
                reports_to: None,
                direct_reports: vec![],
            },
            personality: PersonalityConfig {
                openness: 0.5,
                conscientiousness: 0.5,
                extraversion: 0.5,
                agreeableness: 0.5,
                neuroticism: 0.5,
                caffeine_tolerance: 0.5,
                morning_person: true,
            },
            preferences: PreferencesConfig {
                favorite_room: "empfang".to_string(),
                coffee_preference: "espresso".to_string(),
                lunch_time: "12:30".to_string(),
            },
            background: BackgroundConfig {
                bio: "bio".to_string(),
                quirks: vec![],
            },
            runtime: RuntimeSelectionConfig::default(),
            capabilities: CapabilitiesConfig::default(),
        }
    }

    fn building() -> BuildingConfig {
        BuildingConfig {
            building: BuildingMeta {
                name: "Test GmbH".to_string(),
                address: "Teststr. 1".to_string(),
                floors: 1,
            },
            rooms: vec![RoomConfig {
                id: "empfang".to_string(),
                name: "Empfang".to_string(),
                floor: 0,
                capacity: 10,
                room_type: RoomType::Common,
                adjacent: vec![],
                department: None,
                has_coffee_machine: false,
                has_printer: false,
            }],
        }
    }

    #[test]
    fn persist_writes_atomically_reuses_names_and_removes_stale() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();

        // Initialzustand: id1 + id2
        persist_company_config(
            cfg,
            &[agent(1, "Anna", "Dev"), agent(2, "Bob", "PM")],
            &building(),
            "0",
        )
        .unwrap();
        let id2_path = cfg.join("agents").join("AGENT-02-BOB.toml");
        assert!(id2_path.exists(), "Bob TOML should exist after first apply");

        // Neue Config: id1 geaendert (Rolle), id3 neu, id2 entfernt
        let res = persist_company_config(
            cfg,
            &[agent(1, "Anna", "Lead"), agent(3, "Cara", "QA")],
            &building(),
            "1",
        )
        .unwrap();
        assert_eq!(res.agents_written, 2);
        assert_eq!(res.agents_removed, 1);
        assert!(res.rooms_written);

        // id2 ist weg
        assert!(!id2_path.exists(), "stale Bob TOML must be removed");

        // Reload aus config_dir: nur id1 + id3, id1-Rolle aktualisiert
        let reloaded = load_all_agents(&cfg.join("agents")).unwrap();
        let ids: Vec<u16> = reloaded.iter().map(|a| a.identity.id).collect();
        assert_eq!(ids, vec![1, 3], "only updated + new agent remain");
        assert_eq!(reloaded[0].identity.role, "Lead", "id1 role updated live");

        // Backup des 2. Apply enthaelt den Vor-Apply-Zustand (id1 + id2 + rooms)
        let backup = res.backup_dir.unwrap();
        assert!(backup.join("agents").join("AGENT-01-ANNA.toml").exists());
        assert!(backup.join("agents").join("AGENT-02-BOB.toml").exists());
        assert!(backup.join("rooms.toml").exists());
    }

    #[test]
    fn persist_building_round_trip_identical() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        let b = building();
        persist_company_config(cfg, &[agent(1, "Anna", "Dev")], &b, "0").unwrap();
        let reloaded = BuildingConfig::load(&cfg.join("rooms.toml")).unwrap();
        assert_eq!(
            reloaded, b,
            "rooms.toml write-back must round-trip identical"
        );
    }

    #[test]
    fn persist_into_empty_config_dir_creates_agents() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        let res = persist_company_config(cfg, &[agent(7, "Greta", "Sales")], &building(), "init")
            .unwrap();
        assert_eq!(res.agents_written, 1);
        assert_eq!(res.agents_removed, 0);
        let reloaded = load_all_agents(&cfg.join("agents")).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].identity.id, 7);
    }
}
