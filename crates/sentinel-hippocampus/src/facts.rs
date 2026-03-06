//! FactRetriever - Trigger-based JIT retrieval of company knowledge.
//!
//! When an agent's context contains certain trigger words, relevant facts
//! are loaded on-demand from a fact store (backed by redb in production).

use std::collections::HashMap;

/// Trigger words and their associated fact keys.
///
/// Wenn ein Trigger-Wort im Kontext vorkommt, wird der zugehoerige Fakt
/// aus dem Store nachgeladen.
pub const FACT_TRIGGERS: &[(&str, &str)] = &[
    ("Projekt Aurora", "facts/projects/aurora"),
    ("Redesign", "facts/projects/current-redesign"),
    ("Budget", "facts/finance/budget-q1"),
    ("Kunde", "facts/clients/active"),
    ("Betriebsrat", "facts/hr/betriebsrat"),
    ("Urlaub", "facts/hr/vacation-policy"),
    ("Scrum", "facts/process/scrum-rules"),
    ("Sprint", "facts/process/current-sprint"),
];

/// Trait for fact storage backends (redb in production, HashMap for tests).
pub trait FactStore {
    /// Retrieve a fact by key. Returns None if the key doesn't exist.
    fn get_fact(&self, key: &str) -> anyhow::Result<Option<String>>;
}

/// Trigger-based fact retriever with a generic storage backend.
///
/// Combines the 8 default `FACT_TRIGGERS` with dynamically added custom triggers.
/// Custom triggers are checked AFTER defaults, allowing agent-specific or
/// night-run-derived facts to be injected at runtime.
pub struct FactRetriever<S: FactStore> {
    store: S,
    custom_triggers: Vec<(String, String)>,
}

impl<S: FactStore> FactRetriever<S> {
    pub fn new(store: S) -> Self {
        Self {
            store,
            custom_triggers: Vec::new(),
        }
    }

    /// Create a retriever with initial custom triggers on top of the defaults.
    pub fn with_triggers(store: S, triggers: &[(&str, &str)]) -> Self {
        let custom_triggers = triggers
            .iter()
            .map(|(t, k)| (t.to_string(), k.to_string()))
            .collect();
        Self {
            store,
            custom_triggers,
        }
    }

    /// Add custom triggers (extends, does not replace defaults or existing custom triggers).
    pub fn add_triggers(&mut self, triggers: &[(&str, &str)]) {
        for (trigger, key) in triggers {
            self.custom_triggers
                .push((trigger.to_string(), key.to_string()));
        }
    }

    /// Check if the current context triggers any fact retrieval.
    ///
    /// Returns a list of facts whose trigger words were found in the context.
    /// Matching is case-insensitive. Checks default triggers first, then custom.
    pub fn check_triggers(&self, context: &str) -> Vec<String> {
        let lower = context.to_lowercase();
        let mut facts = Vec::new();

        // Default triggers
        for (trigger, key) in FACT_TRIGGERS {
            if lower.contains(&trigger.to_lowercase()) {
                if let Ok(Some(fact)) = self.store.get_fact(key) {
                    facts.push(fact);
                }
            }
        }

        // Custom triggers
        for (trigger, key) in &self.custom_triggers {
            if lower.contains(&trigger.to_lowercase()) {
                if let Ok(Some(fact)) = self.store.get_fact(key) {
                    facts.push(fact);
                }
            }
        }

        facts
    }

    /// Get the underlying store reference.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Number of custom triggers (excludes the 8 defaults).
    pub fn custom_trigger_count(&self) -> usize {
        self.custom_triggers.len()
    }
}

/// In-memory fact store for testing and development.
pub struct InMemoryFactStore {
    facts: HashMap<String, String>,
}

impl InMemoryFactStore {
    pub fn new() -> Self {
        Self {
            facts: HashMap::new(),
        }
    }

    /// Insert a fact into the store.
    pub fn insert(&mut self, key: &str, value: &str) {
        self.facts.insert(key.to_string(), value.to_string());
    }

    /// Number of facts in the store.
    pub fn len(&self) -> usize {
        self.facts.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }
}

impl Default for InMemoryFactStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FactStore for InMemoryFactStore {
    fn get_fact(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self.facts.get(key).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> InMemoryFactStore {
        let mut store = InMemoryFactStore::new();
        store.insert(
            "facts/projects/aurora",
            "Projekt Aurora: Redesign der Firmenwebseite",
        );
        store.insert("facts/finance/budget-q1", "Q1 Budget: 150.000 EUR");
        store.insert(
            "facts/hr/betriebsrat",
            "Betriebsrat: 3 Mitglieder, Sitzung montags",
        );
        store.insert(
            "facts/process/scrum-rules",
            "Sprint-Dauer: 2 Wochen, Daily um 09:30",
        );
        store.insert(
            "facts/process/current-sprint",
            "Sprint 12: Dashboard Redesign",
        );
        store
    }

    #[test]
    fn test_trigger_exact_match() {
        let store = test_store();
        let retriever = FactRetriever::new(store);
        let facts = retriever.check_triggers("Wir besprechen Projekt Aurora heute.");
        assert_eq!(facts.len(), 1);
        assert!(facts[0].contains("Projekt Aurora"));
    }

    #[test]
    fn test_trigger_case_insensitive() {
        let store = test_store();
        let retriever = FactRetriever::new(store);
        let facts = retriever.check_triggers("projekt aurora ist wichtig");
        assert_eq!(facts.len(), 1);
    }

    #[test]
    fn test_no_trigger() {
        let store = test_store();
        let retriever = FactRetriever::new(store);
        let facts = retriever.check_triggers("Wetter ist schoen heute");
        assert!(facts.is_empty());
    }

    #[test]
    fn test_multiple_triggers() {
        let store = test_store();
        let retriever = FactRetriever::new(store);
        let facts = retriever.check_triggers("Projekt Aurora und Budget besprechen");
        assert_eq!(facts.len(), 2);
    }

    #[test]
    fn test_trigger_without_store_entry() {
        // Store has no entry for "Redesign" key
        let store = InMemoryFactStore::new();
        let retriever = FactRetriever::new(store);
        let facts = retriever.check_triggers("Redesign der Startseite");
        assert!(facts.is_empty(), "Missing store entry should be skipped");
    }

    #[test]
    fn test_trigger_list_contents() {
        let triggers: Vec<&str> = FACT_TRIGGERS.iter().map(|(t, _)| *t).collect();
        assert!(triggers.contains(&"Projekt Aurora"));
        assert!(triggers.contains(&"Scrum"));
        assert!(triggers.contains(&"Budget"));
        assert!(triggers.contains(&"Betriebsrat"));
        assert_eq!(triggers.len(), 8);
    }

    #[test]
    fn test_in_memory_store_len() {
        let store = test_store();
        assert_eq!(store.len(), 5);
        assert!(!store.is_empty());
    }

    #[test]
    fn test_in_memory_store_default() {
        let store = InMemoryFactStore::default();
        assert!(store.is_empty());
    }

    #[test]
    fn test_sprint_trigger_matches_current_sprint() {
        let store = test_store();
        let retriever = FactRetriever::new(store);
        let facts = retriever.check_triggers("Was ist im aktuellen Sprint geplant?");
        assert!(!facts.is_empty());
        assert!(facts.iter().any(|f| f.contains("Sprint 12")));
    }

    #[test]
    fn test_scrum_and_sprint_both_trigger() {
        let store = test_store();
        let retriever = FactRetriever::new(store);
        let facts = retriever.check_triggers("Scrum Sprint Review morgen");
        assert!(facts.len() >= 2);
    }

    // === Extensible Trigger Tests ===

    #[test]
    fn test_custom_triggers() {
        let mut store = test_store();
        store.insert("facts/custom/server", "Server: 10 VMs im Cluster");

        let retriever =
            FactRetriever::with_triggers(store, &[("Serverraum", "facts/custom/server")]);

        let facts = retriever.check_triggers("Wartung im Serverraum geplant");
        assert_eq!(facts.len(), 1);
        assert!(facts[0].contains("10 VMs"));
    }

    #[test]
    fn test_add_triggers_extends_defaults() {
        let mut store = test_store();
        store.insert("facts/custom/deploy", "Deploy: freitags 16 Uhr");

        let mut retriever = FactRetriever::new(store);
        assert_eq!(retriever.custom_trigger_count(), 0);

        retriever.add_triggers(&[("Deployment", "facts/custom/deploy")]);
        assert_eq!(retriever.custom_trigger_count(), 1);

        // Custom trigger works
        let facts = retriever.check_triggers("Deployment Pipeline laeuft");
        assert_eq!(facts.len(), 1);
        assert!(facts[0].contains("freitags"));

        // Default triggers still work
        let facts2 = retriever.check_triggers("Budget fuer Projekt Aurora pruefen");
        assert_eq!(facts2.len(), 2);
    }

    #[test]
    fn test_custom_and_default_triggers_combine() {
        let mut store = test_store();
        store.insert("facts/custom/meeting", "Meeting: Raum 3, 14 Uhr");

        let retriever = FactRetriever::with_triggers(store, &[("Standup", "facts/custom/meeting")]);

        // Context hits both a default ("Sprint") and custom ("Standup") trigger
        let facts = retriever.check_triggers("Sprint Standup morgen frueh");
        assert_eq!(facts.len(), 2);
    }
}
