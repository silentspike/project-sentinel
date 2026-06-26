//! Handler fuer die #427 Cost-Read-Models (`cost_by_agent`, `cost_by_tier`,
//! `cost_timeseries`).
//!
//! Aggregiert `AgentLlmUsage`-Events (cache-aware Token + Kosten pro Agent/Tier)
//! in drei materialisierte Views. Die Cost-Information lebt EINMAL als Event-Sequenz
//! im Event-Store (SSOT); diese Projektion ist ihre materialisierte Sicht (1:n, kein
//! zweiter Puffer). Der Agent-Key ist die `aggregate_id` ("AGENT-NN"), nicht die
//! numerische `agent_id` im Payload (konsistent mit allen agent-Events).

use sentinel_common::{DomainEvent, DomainEventPayload};

use crate::store::{LlmCostUpdate, ReadModelTransaction};

use super::ProjectionHandler;

pub struct CostHandler;

impl ProjectionHandler for CostHandler {
    fn handle(
        &self,
        row_id: i64,
        event: &DomainEvent,
        payload: &DomainEventPayload,
        txn: &ReadModelTransaction<'_>,
    ) -> anyhow::Result<()> {
        if let DomainEventPayload::AgentLlmUsage {
            tier,
            input_tokens,
            output_tokens,
            cache_read,
            cache_creation,
            cost_usd,
            ..
        } = payload
        {
            let update = LlmCostUpdate {
                // Design point F (#429-R3): key by the aggregate_id "AGENT-NN", not the
                // numeric payload agent_id.
                agent_id: &event.aggregate_id,
                tier,
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
                cache_read: *cache_read,
                cache_creation: *cache_creation,
                cost_usd: *cost_usd,
                bucket_ms: event.timestamp_ms,
            };
            txn.record_llm_cost(&update, row_id)?;
        }
        Ok(())
    }
}
