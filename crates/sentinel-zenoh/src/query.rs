//! Scoped query types for sentinel-zenoh.
//!
//! Queries haben einen `query_id` (UUIDv7), ein `deadline_ms`, einen
//! `min_tick` fuer Stale-Response-Filterung, und einen `scope`.

use sentinel_common::{AgentId, Tick};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Scope einer Scoped Query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryScope {
    /// Query fuer einen einzelnen Agent.
    Agent(AgentId),
    /// Query fuer einen Raum.
    Room(String),
    /// Globale Query (kein Scope-Filter).
    Global,
}

/// Eine Scoped Query Anfrage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedQuery {
    /// Zeitgeordnete Query-ID (UUIDv7).
    pub query_id: Uuid,
    /// Deadline in Millisekunden ab Query-Erstellung.
    pub deadline_ms: u64,
    /// Minimaler Tick fuer gueltige Responses (Stale-Filter).
    pub min_tick: u64,
    /// Query Scope.
    pub scope: QueryScope,
    /// Absender-Agent.
    pub origin_agent: AgentId,
    /// Request Payload (FlatBuffer Bytes).
    pub payload: Vec<u8>,
    /// Simulations-Tick bei Query-Erstellung.
    pub tick: Tick,
}

/// Eine Response auf eine Scoped Query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResponse {
    /// Die query_id auf die geantwortet wird.
    pub query_id: Uuid,
    /// Tick bei Response-Generierung.
    pub response_tick: u64,
    /// Response Payload (FlatBuffer Bytes).
    pub payload: Vec<u8>,
}

impl ScopedQuery {
    /// Erstellt eine neue Scoped Query mit UUIDv7.
    pub fn new(
        origin_agent: AgentId,
        scope: QueryScope,
        payload: Vec<u8>,
        tick: Tick,
        deadline_ms: u64,
        min_tick: u64,
    ) -> Self {
        Self {
            query_id: Uuid::now_v7(),
            deadline_ms,
            min_tick,
            scope,
            origin_agent,
            payload,
            tick,
        }
    }
}

impl QueryResponse {
    /// Prueft ob diese Response stale ist (response_tick < min_tick).
    pub fn is_stale(&self, min_tick: u64) -> bool {
        self.response_tick < min_tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::Tick;

    #[test]
    fn test_scoped_query_uuid_v7_time_ordered() {
        let q1 = ScopedQuery::new(AgentId(1), QueryScope::Global, vec![], Tick(0), 100, 0);
        // UUIDv7 ist zeitgeordnet: spaetere Queries haben groessere UUIDs
        let q2 = ScopedQuery::new(AgentId(1), QueryScope::Global, vec![], Tick(1), 100, 0);
        assert!(q2.query_id > q1.query_id);
    }

    #[test]
    fn test_scoped_query_serialization_roundtrip() {
        let query = ScopedQuery::new(
            AgentId(42),
            QueryScope::Room("kueche".to_string()),
            vec![1, 2, 3],
            Tick(100),
            200,
            50,
        );
        let json = serde_json::to_string(&query).unwrap();
        let deserialized: ScopedQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.query_id, query.query_id);
        assert_eq!(deserialized.min_tick, 50);
        assert_eq!(deserialized.payload, vec![1, 2, 3]);
    }

    #[test]
    fn test_query_response_stale_detection() {
        let response = QueryResponse {
            query_id: Uuid::now_v7(),
            response_tick: 10,
            payload: vec![],
        };
        assert!(response.is_stale(20));
        assert!(!response.is_stale(10));
        assert!(!response.is_stale(5));
    }

    #[test]
    fn test_query_scope_variants() {
        let scopes = vec![
            QueryScope::Agent(AgentId(1)),
            QueryScope::Room("buero-dev-1".to_string()),
            QueryScope::Global,
        ];
        for scope in scopes {
            let json = serde_json::to_string(&scope).unwrap();
            let deserialized: QueryScope = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, scope);
        }
    }
}
