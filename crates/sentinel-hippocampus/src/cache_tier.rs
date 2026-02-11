//! KV-Cache tiering interface for hot/cold agent context management.
//!
//! Hot = in RAM (active agent), Cold = on NVMe via io_uring (inactive).
//! This module defines the trait and provides an in-memory implementation
//! for testing. Production implementation with io_uring is in sentinel-runtime.

use std::collections::HashSet;
use std::sync::RwLock;

/// Errors during cache tier operations.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("Agent '{0}' not found in cache")]
    AgentNotFound(String),
    #[error("Agent '{0}' already in target tier")]
    AlreadyInTier(String),
    #[error("Cache operation failed: {0}")]
    OperationFailed(String),
}

/// Interface for KV-Cache tiering between hot (RAM) and cold (NVMe) storage.
pub trait KvCacheTier {
    /// Move an agent's KV-cache from hot to cold storage.
    fn offload_to_cold(&self, agent_name: &str) -> Result<(), CacheError>;
    /// Restore an agent's KV-cache from cold to hot storage.
    fn restore_to_hot(&self, agent_name: &str) -> Result<(), CacheError>;
    /// Check if an agent is in the hot cache.
    fn is_hot(&self, agent_name: &str) -> bool;
}

/// In-memory KV-cache implementation for testing and development.
///
/// Thread-safe via RwLock. In production, this would be backed by
/// io_uring for NVMe offload.
pub struct InMemoryKvCache {
    hot: RwLock<HashSet<String>>,
}

impl InMemoryKvCache {
    pub fn new() -> Self {
        Self {
            hot: RwLock::new(HashSet::new()),
        }
    }

    /// Add an agent to the hot cache.
    pub fn add_agent(&self, agent_name: &str) {
        self.hot.write().unwrap().insert(agent_name.to_string());
    }

    /// Number of agents in the hot cache.
    pub fn hot_count(&self) -> usize {
        self.hot.read().unwrap().len()
    }
}

impl Default for InMemoryKvCache {
    fn default() -> Self {
        Self::new()
    }
}

impl KvCacheTier for InMemoryKvCache {
    fn offload_to_cold(&self, agent_name: &str) -> Result<(), CacheError> {
        let mut hot = self.hot.write().unwrap();
        if hot.remove(agent_name) {
            Ok(())
        } else {
            Err(CacheError::AgentNotFound(agent_name.to_string()))
        }
    }

    fn restore_to_hot(&self, agent_name: &str) -> Result<(), CacheError> {
        let mut hot = self.hot.write().unwrap();
        if !hot.insert(agent_name.to_string()) {
            Err(CacheError::AlreadyInTier(agent_name.to_string()))
        } else {
            Ok(())
        }
    }

    fn is_hot(&self, agent_name: &str) -> bool {
        self.hot.read().unwrap().contains(agent_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_agent_not_hot() {
        let cache = InMemoryKvCache::new();
        assert!(!cache.is_hot("Thomas"));
    }

    #[test]
    fn test_add_agent_becomes_hot() {
        let cache = InMemoryKvCache::new();
        cache.add_agent("Thomas");
        assert!(cache.is_hot("Thomas"));
    }

    #[test]
    fn test_offload_to_cold() {
        let cache = InMemoryKvCache::new();
        cache.add_agent("Thomas");
        assert!(cache.is_hot("Thomas"));

        cache.offload_to_cold("Thomas").unwrap();
        assert!(!cache.is_hot("Thomas"));
    }

    #[test]
    fn test_restore_to_hot() {
        let cache = InMemoryKvCache::new();
        cache.restore_to_hot("Thomas").unwrap();
        assert!(cache.is_hot("Thomas"));
    }

    #[test]
    fn test_offload_unknown_agent() {
        let cache = InMemoryKvCache::new();
        let result = cache.offload_to_cold("Unknown");
        assert!(result.is_err());
        match result.unwrap_err() {
            CacheError::AgentNotFound(name) => assert_eq!(name, "Unknown"),
            other => panic!("Expected AgentNotFound, got: {other:?}"),
        }
    }

    #[test]
    fn test_restore_already_hot() {
        let cache = InMemoryKvCache::new();
        cache.add_agent("Thomas");
        let result = cache.restore_to_hot("Thomas");
        assert!(result.is_err());
        match result.unwrap_err() {
            CacheError::AlreadyInTier(name) => assert_eq!(name, "Thomas"),
            other => panic!("Expected AlreadyInTier, got: {other:?}"),
        }
    }

    #[test]
    fn test_round_trip() {
        let cache = InMemoryKvCache::new();
        cache.add_agent("Lisa");
        assert!(cache.is_hot("Lisa"));

        cache.offload_to_cold("Lisa").unwrap();
        assert!(!cache.is_hot("Lisa"));

        cache.restore_to_hot("Lisa").unwrap();
        assert!(cache.is_hot("Lisa"));
    }

    #[test]
    fn test_multiple_agents() {
        let cache = InMemoryKvCache::new();
        cache.add_agent("Thomas");
        cache.add_agent("Lisa");
        cache.add_agent("Andreas");

        assert_eq!(cache.hot_count(), 3);

        cache.offload_to_cold("Lisa").unwrap();
        assert_eq!(cache.hot_count(), 2);
        assert!(cache.is_hot("Thomas"));
        assert!(!cache.is_hot("Lisa"));
        assert!(cache.is_hot("Andreas"));
    }

    #[test]
    fn test_default_constructor() {
        let cache = InMemoryKvCache::default();
        assert_eq!(cache.hot_count(), 0);
    }
}
