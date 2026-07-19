use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use sentinel_common::AgentId;
use sentinel_redb::StateStore;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

pub const DEFAULT_EVOLUTION_CHANNEL_CAPACITY: usize = 32;
pub const DEFAULT_EVOLUTION_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_CONCURRENT_JOBS: usize = 8;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EvolutionSource {
    ShiftTransition,
    Nightrun,
}

impl EvolutionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShiftTransition => "shift_transition",
            Self::Nightrun => "nightrun",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvolutionJob {
    pub agent_id: AgentId,
    pub agent_name: String,
    pub agent_role: String,
    pub narrative: String,
    pub source: EvolutionSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvolutionResult {
    pub agent_id: AgentId,
    pub agent_name: String,
    pub source: EvolutionSource,
    pub voice_style: Option<Vec<u8>>,
    pub behavioral_notes: Option<Vec<u8>>,
    pub narrative: String,
}

#[derive(Clone)]
pub struct EvolutionTaskConfig {
    pub gateway_url: String,
    pub timeout: Duration,
    pub max_concurrent_jobs: usize,
    pub credential: String,
}

impl EvolutionTaskConfig {
    pub fn from_env() -> Self {
        Self {
            gateway_url: std::env::var("CORTEX_GATEWAY_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_string()),
            timeout: DEFAULT_EVOLUTION_TIMEOUT,
            max_concurrent_jobs: DEFAULT_MAX_CONCURRENT_JOBS,
            // The composition root injects the owner-only credential after
            // validating its file. This module never performs a weaker read.
            credential: String::new(),
        }
    }
}

pub fn spawn_evolution_background_task(
    config: EvolutionTaskConfig,
    result_tx: mpsc::Sender<EvolutionResult>,
) -> tokio::sync::mpsc::Sender<EvolutionJob> {
    let (job_tx, job_rx) = tokio::sync::mpsc::channel(DEFAULT_EVOLUTION_CHANNEL_CAPACITY);
    tokio::spawn(evolution_background_task(job_rx, result_tx, config));
    job_tx
}

pub async fn evolution_background_task(
    mut job_rx: tokio::sync::mpsc::Receiver<EvolutionJob>,
    result_tx: mpsc::Sender<EvolutionResult>,
    config: EvolutionTaskConfig,
) {
    let concurrency = config.max_concurrent_jobs.max(1);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));

    #[cfg(feature = "llm")]
    let client = match reqwest::Client::builder().timeout(config.timeout).build() {
        Ok(client) => Some(client),
        Err(error) => {
            warn!(error = %error, "Evolution LLM Client erstellen fehlgeschlagen");
            None
        }
    };

    while let Some(job) = job_rx.recv().await {
        let permit = match semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(error) => {
                warn!(error = %error, "Evolution Background-Semaphore geschlossen");
                break;
            }
        };
        let result_tx = result_tx.clone();
        let config = config.clone();

        #[cfg(feature = "llm")]
        let client = client.clone();

        tokio::spawn(async move {
            let _permit = permit;

            #[cfg(feature = "llm")]
            let result = match client {
                Some(client) => {
                    run_evolution_job(&client, &config.gateway_url, &config.credential, job).await
                }
                None => fail_safe_result(job),
            };

            #[cfg(not(feature = "llm"))]
            let result = {
                warn!(
                    agent = %job.agent_name,
                    source = job.source.as_str(),
                    "Evolution LLM deaktiviert, schreibe Narrative ohne Voice/Behavioral Notes"
                );
                fail_safe_result(job)
            };

            if result_tx.send(result).is_err() {
                warn!("Evolution-Ergebnis konnte nicht an ECS Tick-Loop gesendet werden");
            }
        });
    }
}

#[cfg(feature = "llm")]
async fn run_evolution_job(
    client: &reqwest::Client,
    gateway_url: &str,
    credential: &str,
    job: EvolutionJob,
) -> EvolutionResult {
    let url = format!("{}/internal/llm", gateway_url.trim_end_matches('/'));
    info!(
        agent = %job.agent_name,
        source = job.source.as_str(),
        "Evolution Background-Job gestartet"
    );

    let voice_user_prompt =
        voice_style_user_prompt(&job.agent_name, &job.agent_role, &job.narrative);
    let behavioral_user_prompt =
        behavioral_notes_user_prompt(&job.agent_name, &job.agent_role, &job.narrative);

    let (voice_style, behavioral_notes) = tokio::join!(
        llm_evolution_call(
            client,
            &url,
            credential,
            &job.agent_name,
            voice_style_system_prompt(),
            &voice_user_prompt,
        ),
        llm_evolution_call(
            client,
            &url,
            credential,
            &job.agent_name,
            behavioral_notes_system_prompt(),
            &behavioral_user_prompt,
        ),
    );

    info!(
        agent = %job.agent_name,
        source = job.source.as_str(),
        voice_style = voice_style.is_some(),
        behavioral_notes = behavioral_notes.is_some(),
        "Evolution Background-Job abgeschlossen"
    );

    EvolutionResult {
        agent_id: job.agent_id,
        agent_name: job.agent_name,
        source: job.source,
        voice_style,
        behavioral_notes,
        narrative: job.narrative,
    }
}

pub fn apply_evolution_result(store: &StateStore, result: &EvolutionResult) -> Result<u64> {
    store.set_evolution_batch(
        result.agent_id,
        result.voice_style.as_deref(),
        result.behavioral_notes.as_deref(),
        Some(result.narrative.as_bytes()),
        None,
    )
}

fn fail_safe_result(job: EvolutionJob) -> EvolutionResult {
    EvolutionResult {
        agent_id: job.agent_id,
        agent_name: job.agent_name,
        source: job.source,
        voice_style: None,
        behavioral_notes: None,
        narrative: job.narrative,
    }
}

#[cfg(feature = "llm")]
fn evolution_request(
    client: &reqwest::Client,
    url: &str,
    credential: &str,
    body: &serde_json::Value,
) -> reqwest::RequestBuilder {
    client.post(url).bearer_auth(credential).json(body)
}

#[cfg(feature = "llm")]
async fn llm_evolution_call(
    client: &reqwest::Client,
    url: &str,
    credential: &str,
    agent_name: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Option<Vec<u8>> {
    let body = serde_json::json!({
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt}
        ],
        "temperature": 0.3,
        "max_tokens": 500,
        "metadata": {
            "subject_agent_name": agent_name,
            "request_type": "evolution_analysis"
        }
    });

    match evolution_request(client, url, credential, &body)
        .send()
        .await
    {
        Ok(response) => {
            if !response.status().is_success() {
                warn!(
                    agent = agent_name,
                    status = %response.status(),
                    "Evolution LLM Call fehlgeschlagen (HTTP)"
                );
                return None;
            }
            match response.json::<serde_json::Value>().await {
                Ok(json) => extract_evolution_content(&json).map(|content| content.into_bytes()),
                Err(error) => {
                    warn!(
                        agent = agent_name,
                        error = %error,
                        "Evolution LLM Response parse fehlgeschlagen"
                    );
                    None
                }
            }
        }
        Err(error) => {
            warn!(
                agent = agent_name,
                error = %error,
                "Evolution LLM Call fehlgeschlagen"
            );
            None
        }
    }
}

#[cfg(feature = "llm")]
fn extract_evolution_content(json: &serde_json::Value) -> Option<String> {
    if let Some(content) = json.get("content").and_then(|value| value.as_str()) {
        if !content.is_empty() {
            return Some(content.to_string());
        }
    }

    let content = json
        .get("choices")
        .and_then(|value| value.as_array())
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

#[cfg(feature = "llm")]
fn voice_style_system_prompt() -> &'static str {
    "Du bist ein linguistischer Analyst fuer eine Firmen-Simulation. \
     Analysiere den Sprachstil des Agenten basierend auf seiner Schicht-Zusammenfassung. \
     Antworte AUSSCHLIESSLICH als valides JSON."
}

#[cfg(feature = "llm")]
fn voice_style_user_prompt(agent_name: &str, agent_role: &str, narrative: &str) -> String {
    format!(
        "Agent \"{agent_name}\" (Rolle: {agent_role}) hatte folgende Schicht-Erfahrungen:\n\n\
         {narrative}\n\n\
         Analysiere den Sprachstil. Antwort als JSON:\n\
         {{\"phrases\": [\"phrase1\"], \"sentence_style\": \"kurz|mittel|lang\", \"formality\": 0.X}}"
    )
}

#[cfg(feature = "llm")]
fn behavioral_notes_system_prompt() -> &'static str {
    "Du bist ein Verhaltensanalyst fuer eine Firmen-Simulation. \
     Analysiere Verhaltensmuster des Agenten basierend auf seiner Schicht-Zusammenfassung. \
     Antworte AUSSCHLIESSLICH als valides JSON."
}

#[cfg(feature = "llm")]
fn behavioral_notes_user_prompt(agent_name: &str, agent_role: &str, narrative: &str) -> String {
    format!(
        "Agent \"{agent_name}\" (Rolle: {agent_role}) hatte folgende Schicht-Erfahrungen:\n\n\
         {narrative}\n\n\
         Identifiziere Verhaltensmuster. Antwort als JSON:\n\
         {{\"habits\": [\"habit1\"], \"interaction_style\": \"proaktiv|reaktiv|gemischt\", \
         \"decision_style\": \"schnell|zoegerlich|ausgewogen\", \"anomalies\": []}}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evolution_job_roundtrip() {
        let (tx, rx) = mpsc::channel();
        let job = EvolutionJob {
            agent_id: AgentId(7),
            agent_name: "Test Agent".to_string(),
            agent_role: "Tester".to_string(),
            narrative: "Observed a regression and wrote a report".to_string(),
            source: EvolutionSource::ShiftTransition,
        };

        tx.send(job.clone()).unwrap();

        assert_eq!(rx.recv().unwrap(), job);
    }

    #[test]
    fn test_evolution_result_applied_to_redb() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(dir.path().join("state.redb").to_str().unwrap()).unwrap();
        let result = EvolutionResult {
            agent_id: AgentId(5),
            agent_name: "Ada".to_string(),
            source: EvolutionSource::Nightrun,
            voice_style: Some(br#"{"phrases":["klar"]}"#.to_vec()),
            behavioral_notes: Some(br#"{"habits":["audit"]}"#.to_vec()),
            narrative: "Ada consolidated a shift".to_string(),
        };

        let version = apply_evolution_result(&store, &result).unwrap();

        assert_eq!(version, 1);
        assert_eq!(store.get_evolution_version(AgentId(5)).unwrap(), 1);
        assert_eq!(
            store.get_voice_style(AgentId(5)).unwrap(),
            Some(br#"{"phrases":["klar"]}"#.to_vec())
        );
        assert_eq!(
            store.get_behavioral_notes(AgentId(5)).unwrap(),
            Some(br#"{"habits":["audit"]}"#.to_vec())
        );
        assert_eq!(
            store.get_narrative_summary(AgentId(5)).unwrap(),
            Some(b"Ada consolidated a shift".to_vec())
        );
    }

    #[test]
    fn test_evolution_result_empty_fields_no_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(dir.path().join("state.redb").to_str().unwrap()).unwrap();
        store
            .set_evolution_batch(
                AgentId(9),
                Some(b"existing voice"),
                Some(b"existing notes"),
                Some(b"old narrative"),
                None,
            )
            .unwrap();
        let result = EvolutionResult {
            agent_id: AgentId(9),
            agent_name: "Grace".to_string(),
            source: EvolutionSource::ShiftTransition,
            voice_style: None,
            behavioral_notes: None,
            narrative: "new narrative".to_string(),
        };

        let version = apply_evolution_result(&store, &result).unwrap();

        assert_eq!(version, 2);
        assert_eq!(
            store.get_voice_style(AgentId(9)).unwrap(),
            Some(b"existing voice".to_vec())
        );
        assert_eq!(
            store.get_behavioral_notes(AgentId(9)).unwrap(),
            Some(b"existing notes".to_vec())
        );
        assert_eq!(
            store.get_narrative_summary(AgentId(9)).unwrap(),
            Some(b"new narrative".to_vec())
        );
    }

    #[cfg(feature = "llm")]
    #[test]
    fn test_extract_evolution_content_supports_gateway_shapes() {
        let flat = serde_json::json!({"content": "{\"flat\":true}"});
        let openai = serde_json::json!({
            "choices": [{"message": {"content": "{\"openai\":true}"}}]
        });

        assert_eq!(
            extract_evolution_content(&flat).unwrap(),
            "{\"flat\":true}".to_string()
        );
        assert_eq!(
            extract_evolution_content(&openai).unwrap(),
            "{\"openai\":true}".to_string()
        );
    }

    #[cfg(feature = "llm")]
    #[test]
    fn evolution_request_uses_internal_path_and_bearer_credential() {
        let body = serde_json::json!({"messages": []});
        let request = evolution_request(
            &reqwest::Client::new(),
            "http://127.0.0.1:8080/internal/llm",
            "evolution-test-credential",
            &body,
        )
        .build()
        .unwrap();
        assert_eq!(request.url().path(), "/internal/llm");
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .unwrap(),
            "Bearer evolution-test-credential"
        );
    }

    #[tokio::test]
    async fn test_evolution_background_task_fail_safe_on_gateway_failure() {
        let (result_tx, result_rx) = mpsc::channel();
        let (job_tx, job_rx) = tokio::sync::mpsc::channel(1);
        let config = EvolutionTaskConfig {
            gateway_url: "http://127.0.0.1:9".to_string(),
            timeout: Duration::from_millis(100),
            max_concurrent_jobs: 1,
            credential: "test-evolution-credential".to_string(),
        };

        tokio::spawn(evolution_background_task(job_rx, result_tx, config));
        job_tx
            .send(EvolutionJob {
                agent_id: AgentId(11),
                agent_name: "Linus".to_string(),
                agent_role: "Engineer".to_string(),
                narrative: "Network failed but the tick loop must continue".to_string(),
                source: EvolutionSource::Nightrun,
            })
            .await
            .unwrap();
        drop(job_tx);

        let result =
            tokio::task::spawn_blocking(move || result_rx.recv_timeout(Duration::from_secs(2)))
                .await
                .unwrap()
                .unwrap();

        assert_eq!(result.agent_id, AgentId(11));
        assert_eq!(result.voice_style, None);
        assert_eq!(result.behavioral_notes, None);
        assert_eq!(
            result.narrative,
            "Network failed but the tick loop must continue"
        );
    }
}
