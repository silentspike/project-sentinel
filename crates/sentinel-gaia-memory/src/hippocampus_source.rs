//! Read-only Hippocampus source adapter for Gaia Console Memory.

use std::path::{Path, PathBuf};

use sentinel_hippocampus::HippocampusStore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HippocampusReadRequest {
    pub path: PathBuf,
    pub agent_name: Option<String>,
    pub fact_keys: Vec<String>,
    pub max_episodes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HippocampusFactSummary {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HippocampusWakeMemory {
    pub status: String,
    pub path: PathBuf,
    pub agent_name: Option<String>,
    pub narrative_summary: Option<String>,
    pub live_episode_summaries: Vec<String>,
    pub archived_episode_summaries: Vec<String>,
    pub facts: Vec<HippocampusFactSummary>,
    pub notes: Vec<String>,
}

impl HippocampusReadRequest {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            agent_name: None,
            fact_keys: Vec::new(),
            max_episodes: 8,
        }
    }
}

pub fn read_hippocampus(request: &HippocampusReadRequest) -> anyhow::Result<HippocampusWakeMemory> {
    if !request.path.exists() {
        return Ok(unavailable(
            request,
            format!("{} is missing", request.path.display()),
        ));
    }

    let store = match HippocampusStore::open_readonly(&request.path.to_string_lossy()) {
        Ok(store) => store,
        Err(error) => {
            return Ok(unavailable(
                request,
                format!("read-only open failed: {error}"),
            ));
        }
    };

    let mut notes = Vec::new();
    let mut narrative_summary = None;
    let mut live_episode_summaries = Vec::new();
    let mut archived_episode_summaries = Vec::new();

    if let Some(agent) = &request.agent_name {
        match store.load_narrative(agent) {
            Ok(Some(narrative)) => narrative_summary = Some(narrative.summary),
            Ok(None) => notes.push(format!("no narrative for agent {agent}")),
            Err(error) => notes.push(format!("narrative read failed for {agent}: {error}")),
        }

        match store.load_episodes(agent) {
            Ok(episodes) => {
                live_episode_summaries = episodes
                    .into_iter()
                    .take(request.max_episodes)
                    .map(|episode| episode.summary)
                    .collect();
            }
            Err(error) => notes.push(format!("episode read failed for {agent}: {error}")),
        }

        match store.load_archive(agent) {
            Ok(episodes) => {
                archived_episode_summaries = episodes
                    .into_iter()
                    .take(request.max_episodes)
                    .map(|episode| episode.summary)
                    .collect();
            }
            Err(error) => notes.push(format!("archive read failed for {agent}: {error}")),
        }
    } else {
        notes.push("no agent requested; skipped narrative and episode reads".to_string());
    }

    let mut facts = Vec::new();
    for key in &request.fact_keys {
        match store.load_fact(key) {
            Ok(Some(value)) => facts.push(HippocampusFactSummary {
                key: key.clone(),
                value,
            }),
            Ok(None) => notes.push(format!("no fact for key {key}")),
            Err(error) => notes.push(format!("fact read failed for {key}: {error}")),
        }
    }

    Ok(HippocampusWakeMemory {
        status: "ok".to_string(),
        path: request.path.clone(),
        agent_name: request.agent_name.clone(),
        narrative_summary,
        live_episode_summaries,
        archived_episode_summaries,
        facts,
        notes,
    })
}

fn unavailable(request: &HippocampusReadRequest, note: String) -> HippocampusWakeMemory {
    HippocampusWakeMemory {
        status: "unavailable".to_string(),
        path: request.path.clone(),
        agent_name: request.agent_name.clone(),
        narrative_summary: None,
        live_episode_summaries: Vec::new(),
        archived_episode_summaries: Vec::new(),
        facts: Vec::new(),
        notes: vec![note],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_hippocampus::{Episode, HippocampusStore, NarrativeState};

    fn episode(id: u64, summary: &str) -> Episode {
        Episode {
            id,
            agent_name: "Thomas".to_string(),
            summary: summary.to_string(),
            relevance: 1.0,
            emotion: 0.5,
            repetitions: 1,
            hours_ago: 0.0,
            participants: Vec::new(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn hippocampus_missing_file_degrades_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let request = HippocampusReadRequest {
            path: dir.path().join("missing.redb"),
            agent_name: Some("Thomas".to_string()),
            fact_keys: vec!["facts/projects/aurora".to_string()],
            max_episodes: 4,
        };

        let memory = read_hippocampus(&request).unwrap();
        assert_eq!(memory.status, "unavailable");
        assert!(memory.notes[0].contains("missing"));
    }

    #[test]
    fn hippocampus_readonly_adapter_loads_requested_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hippocampus.redb");
        {
            let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
            store
                .store_narrative(
                    "Thomas",
                    &NarrativeState {
                        agent_name: "Thomas".to_string(),
                        summary: "Knows the console backup boundary".to_string(),
                        episode_count: 1,
                    },
                )
                .unwrap();
            store
                .store_episodes("Thomas", &[episode(1, "Live memory")])
                .unwrap();
            store
                .store_archive("Thomas", &[episode(2, "Archived memory")])
                .unwrap();
            store
                .store_fact("facts/projects/aurora", "Aurora is active")
                .unwrap();
        }

        let request = HippocampusReadRequest {
            path,
            agent_name: Some("Thomas".to_string()),
            fact_keys: vec!["facts/projects/aurora".to_string()],
            max_episodes: 4,
        };

        let memory = read_hippocampus(&request).unwrap();
        assert_eq!(memory.status, "ok");
        assert_eq!(
            memory.narrative_summary.as_deref(),
            Some("Knows the console backup boundary")
        );
        assert_eq!(memory.live_episode_summaries, vec!["Live memory"]);
        assert_eq!(memory.archived_episode_summaries, vec!["Archived memory"]);
        assert_eq!(memory.facts[0].value, "Aurora is active");
    }
}
