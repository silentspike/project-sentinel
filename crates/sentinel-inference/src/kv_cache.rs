use std::collections::HashMap;

/// Manages shared KV-Cache prefixes across agents within a shift.
///
/// Architecture:
/// - One shared prefix (Firmenkontext) is identical for all 15
///   agents in a shift
/// - Per-agent caches store individual conversation history
/// - FIFO eviction when max_per_agent is exceeded
///
/// This is a prompt-level optimization: the full context
/// (shared + individual) is assembled here before being sent to
/// BitNet. Kernel-level KV-Cache sharing via shared memory is a
/// future optimization.
pub struct KvCacheManager {
    /// Shared prefix content (Firmenkontext: Rollen, Projekte,
    /// Firmenwissen). Identical for all agents in the same shift.
    shared_prefix: Option<String>,
    /// Per-agent cache extensions (Konversationshistorie,
    /// individuelle Erinnerungen).
    agent_caches: HashMap<String, Vec<String>>,
    /// Maximum entries per agent cache before FIFO eviction.
    max_per_agent: usize,
}

impl KvCacheManager {
    pub fn new(max_per_agent: usize) -> Self {
        Self {
            shared_prefix: None,
            agent_caches: HashMap::new(),
            max_per_agent,
        }
    }

    /// Set the shared company context prefix.
    /// Called once per shift start with the full Firmenkontext.
    pub fn set_shared_prefix(&mut self, prefix: String) {
        self.shared_prefix = Some(prefix);
    }

    /// Get the full prompt context for an agent (shared prefix +
    /// agent-specific history). This assembled context is what
    /// gets sent to BitNet for inference.
    pub fn get_context(&self, agent_name: &str) -> String {
        let mut context = self.shared_prefix.clone().unwrap_or_default();
        if let Some(cache) = self.agent_caches.get(agent_name) {
            for entry in cache {
                context.push('\n');
                context.push_str(entry);
            }
        }
        context
    }

    /// Add a conversation entry for a specific agent.
    /// Applies FIFO eviction when max_per_agent is exceeded.
    pub fn add_entry(&mut self, agent_name: &str, entry: String) {
        let cache = self.agent_caches.entry(agent_name.to_string()).or_default();
        cache.push(entry);
        if cache.len() > self.max_per_agent {
            cache.remove(0); // FIFO: aeltester Eintrag wird entfernt
        }
    }

    /// Clear agent-specific cache (Schichtwechsel, Agent
    /// schlaeft ein). Shared prefix remains intact.
    pub fn clear_agent(&mut self, agent_name: &str) {
        self.agent_caches.remove(agent_name);
    }

    /// Get cache statistics:
    /// (shared_prefix_bytes, total_agent_cache_bytes).
    /// Useful for monitoring memory usage and cache efficiency.
    pub fn cache_stats(&self) -> (usize, usize) {
        let shared_len = self.shared_prefix.as_ref().map_or(0, |s| s.len());
        let total_agent_len: usize = self
            .agent_caches
            .values()
            .map(|c| c.iter().map(|s| s.len()).sum::<usize>())
            .sum();
        (shared_len, total_agent_len)
    }

    /// Number of agents with active caches.
    pub fn active_agents(&self) -> usize {
        self.agent_caches.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_prefix() {
        let mut cache = KvCacheManager::new(10);
        cache.set_shared_prefix("PixelPerfekt GmbH Firmenkontext".to_string());

        let ctx_thomas = cache.get_context("Thomas Schmidt");
        let ctx_lisa = cache.get_context("Lisa Weber");
        assert!(ctx_thomas.starts_with("PixelPerfekt GmbH Firmenkontext"));
        assert_eq!(ctx_thomas, ctx_lisa);
    }

    #[test]
    fn test_agent_specific_entries() {
        let mut cache = KvCacheManager::new(10);
        cache.set_shared_prefix("Shared".to_string());

        cache.add_entry("Thomas", "Thomas sagt Hallo".to_string());
        cache.add_entry("Lisa", "Lisa arbeitet an Design".to_string());

        let ctx_thomas = cache.get_context("Thomas");
        let ctx_lisa = cache.get_context("Lisa");

        assert!(ctx_thomas.contains("Thomas sagt Hallo"));
        assert!(!ctx_thomas.contains("Lisa arbeitet an Design"));
        assert!(ctx_lisa.contains("Lisa arbeitet an Design"));
        assert!(!ctx_lisa.contains("Thomas sagt Hallo"));

        assert!(ctx_thomas.starts_with("Shared"));
        assert!(ctx_lisa.starts_with("Shared"));
    }

    #[test]
    fn test_fifo_eviction() {
        let mut cache = KvCacheManager::new(3);

        cache.add_entry("Agent", "entry-1".to_string());
        cache.add_entry("Agent", "entry-2".to_string());
        cache.add_entry("Agent", "entry-3".to_string());
        cache.add_entry("Agent", "entry-4".to_string());

        let ctx = cache.get_context("Agent");
        assert!(!ctx.contains("entry-1"));
        assert!(ctx.contains("entry-2"));
        assert!(ctx.contains("entry-3"));
        assert!(ctx.contains("entry-4"));
    }

    #[test]
    fn test_clear_agent() {
        let mut cache = KvCacheManager::new(10);
        cache.set_shared_prefix("Shared".to_string());

        cache.add_entry("Thomas", "Eintrag 1".to_string());
        cache.add_entry("Lisa", "Eintrag 2".to_string());

        cache.clear_agent("Thomas");

        assert_eq!(cache.get_context("Thomas"), "Shared");
        assert!(cache.get_context("Lisa").contains("Eintrag 2"));
        assert_eq!(cache.active_agents(), 1);
    }

    #[test]
    fn test_cache_stats() {
        let mut cache = KvCacheManager::new(10);
        cache.set_shared_prefix("SHARED_PREFIX".to_string());

        cache.add_entry("A", "hello".to_string());
        cache.add_entry("B", "world".to_string());

        let (shared, agent) = cache.cache_stats();
        assert_eq!(shared, 13);
        assert_eq!(agent, 10);
    }
}
