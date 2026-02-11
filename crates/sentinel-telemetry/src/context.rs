//! Correlation context for distributed tracing across Sentinel components.
//!
//! TraceContext carries a correlation ID, optional agent origin, and tick.
//! Propagated through Zenoh messages and tracing spans.

use sentinel_common::{AgentId, Tick};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ──────────────────────────────────────────────
// Zenoh Telemetry Topics
// ──────────────────────────────────────────────

/// Zenoh topic for aggregated metrics snapshots.
/// Publishing logic will be added with the Dashboard (Phase 6).
pub const TELEMETRY_METRICS: &str = "sentinel/telemetry/metrics";

/// Zenoh topic for health check results.
/// Publishing logic will be added with the Dashboard (Phase 6).
pub const TELEMETRY_HEALTH: &str = "sentinel/telemetry/health";

/// Zenoh topic for trace context propagation.
/// Publishing logic will be added with the Dashboard (Phase 6).
pub const TELEMETRY_TRACES: &str = "sentinel/telemetry/traces";

/// Zenoh topic for classified error events.
/// Publishing logic will be added with the Dashboard (Phase 6).
pub const TELEMETRY_ERRORS: &str = "sentinel/telemetry/errors";

// ──────────────────────────────────────────────
// TraceContext
// ──────────────────────────────────────────────

/// Correlation context propagated through Zenoh messages and spans.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceContext {
    /// Unique correlation ID for this request chain.
    pub correlation_id: String,
    /// Agent that originated this request (if applicable).
    pub origin_agent: Option<AgentId>,
    /// Simulation tick at which this request was created.
    pub origin_tick: Option<Tick>,
}

impl TraceContext {
    /// Create a new TraceContext with a generated correlation ID.
    pub fn new() -> Self {
        Self {
            correlation_id: Uuid::new_v4().to_string(),
            origin_agent: None,
            origin_tick: None,
        }
    }

    /// Create a TraceContext with agent and tick context.
    pub fn with_agent(agent_id: AgentId, tick: Tick) -> Self {
        Self {
            correlation_id: Uuid::new_v4().to_string(),
            origin_agent: Some(agent_id),
            origin_tick: Some(tick),
        }
    }

    /// Create a tracing span with this context's fields.
    pub fn as_span(&self) -> tracing::Span {
        match (&self.origin_agent, &self.origin_tick) {
            (Some(agent), Some(tick)) => {
                tracing::info_span!(
                    "trace_context",
                    correlation_id = %self.correlation_id,
                    agent_id = %agent,
                    tick = %tick,
                )
            }
            (Some(agent), None) => {
                tracing::info_span!(
                    "trace_context",
                    correlation_id = %self.correlation_id,
                    agent_id = %agent,
                )
            }
            _ => {
                tracing::info_span!(
                    "trace_context",
                    correlation_id = %self.correlation_id,
                )
            }
        }
    }
}

impl Default for TraceContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_generates_unique_ids() {
        let ctx1 = TraceContext::new();
        let ctx2 = TraceContext::new();
        assert_ne!(ctx1.correlation_id, ctx2.correlation_id);
    }

    #[test]
    fn test_new_has_no_agent_or_tick() {
        let ctx = TraceContext::new();
        assert!(ctx.origin_agent.is_none());
        assert!(ctx.origin_tick.is_none());
    }

    #[test]
    fn test_with_agent() {
        let agent = AgentId(1);
        let tick = Tick(42);
        let ctx = TraceContext::with_agent(agent, tick);

        assert_eq!(ctx.origin_agent, Some(AgentId(1)));
        assert_eq!(ctx.origin_tick, Some(Tick(42)));
        assert!(!ctx.correlation_id.is_empty());
    }

    #[test]
    fn test_serialization_roundtrip() {
        let ctx = TraceContext::with_agent(AgentId(7), Tick(100));
        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: TraceContext = serde_json::from_str(&json).unwrap();

        assert_eq!(ctx, deserialized);
    }

    #[test]
    fn test_serialization_without_agent() {
        let ctx = TraceContext::new();
        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: TraceContext = serde_json::from_str(&json).unwrap();

        assert_eq!(ctx, deserialized);
        assert!(deserialized.origin_agent.is_none());
    }

    #[test]
    fn test_as_span_does_not_panic() {
        let ctx1 = TraceContext::new();
        let _span1 = ctx1.as_span();

        let ctx2 = TraceContext::with_agent(AgentId(1), Tick(0));
        let _span2 = ctx2.as_span();
    }

    #[test]
    fn test_default_same_as_new() {
        let ctx = TraceContext::default();
        assert!(ctx.origin_agent.is_none());
        assert!(ctx.origin_tick.is_none());
        assert!(!ctx.correlation_id.is_empty());
    }

    #[test]
    fn test_telemetry_topics() {
        assert_eq!(TELEMETRY_METRICS, "sentinel/telemetry/metrics");
        assert_eq!(TELEMETRY_HEALTH, "sentinel/telemetry/health");
        assert_eq!(TELEMETRY_TRACES, "sentinel/telemetry/traces");
        assert_eq!(TELEMETRY_ERRORS, "sentinel/telemetry/errors");
    }

    #[test]
    fn test_correlation_id_is_valid_uuid() {
        let ctx = TraceContext::new();
        // UUID v4 format: 8-4-4-4-12 hex chars
        let parsed = Uuid::parse_str(&ctx.correlation_id);
        assert!(parsed.is_ok(), "correlation_id should be a valid UUID");
        assert_eq!(parsed.unwrap().get_version_num(), 4);
    }
}
