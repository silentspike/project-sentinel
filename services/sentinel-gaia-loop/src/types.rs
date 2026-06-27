use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GaiaAlert {
    pub alert_id: String,
    pub source_event_id: String,
    pub tick: u64,
    pub timestamp_ms: u64,
    pub trigger: String,
    pub severity: String,
    pub target: String,
    pub summary: String,
    pub recommendation: String,
    pub unresolved_keys: Vec<String>,
}

impl GaiaAlert {
    pub fn dedupe_key(&self) -> String {
        format!("platform_analysis:{}", self.source_event_id)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GaiaSessionKind {
    Deep,
    SetupInterview,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GaiaSessionStatus {
    Started,
    Succeeded,
    Failed,
    TimedOut,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ClaudeUsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub total_cost_usd: Option<f64>,
}

impl ClaudeUsageSummary {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_read_input_tokens
            + self.cache_creation_input_tokens
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GaiaSessionIndexEntry {
    pub gaia_session_id: String,
    pub claude_session_id: Option<String>,
    pub kind: GaiaSessionKind,
    pub status: GaiaSessionStatus,
    pub stream_path: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub usage: ClaudeUsageSummary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_dedupe_key_is_event_scoped() {
        let alert = GaiaAlert {
            alert_id: "gaia-alert-1".to_string(),
            source_event_id: "event-1".to_string(),
            tick: 42,
            timestamp_ms: 1000,
            trigger: "scheduled".to_string(),
            severity: "critical".to_string(),
            target: "system".to_string(),
            summary: "Projection stuck".to_string(),
            recommendation: "Inspect manually".to_string(),
            unresolved_keys: vec!["projection:system".to_string()],
        };
        assert_eq!(alert.dedupe_key(), "platform_analysis:event-1");
    }

    #[test]
    fn usage_total_includes_cache_breakdown() {
        let usage = ClaudeUsageSummary {
            input_tokens: 10,
            output_tokens: 3,
            cache_read_input_tokens: 7,
            cache_creation_input_tokens: 2,
            total_cost_usd: Some(0.01),
        };
        assert_eq!(usage.total_tokens(), 22);
    }
}
