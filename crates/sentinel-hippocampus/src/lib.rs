//! sentinel-hippocampus: Multi-tier memory system for agent simulation.
//!
//! Implements a biologically-inspired memory architecture:
//! - **Episode + NMDA Scoring**: Event recording with consolidation scoring
//! - **NarrativeMemory**: Running summary of important daily events
//! - **FactRetriever**: Trigger-based JIT retrieval of company knowledge
//! - **KvCacheTier**: Hot/cold tiering interface for KV-cache offload
//! - **GOLF Framework**: Goal-Oriented Life Tasks for long-term agent tracking
//! - **HippocampusStore**: Persistent redb storage for all memory data
//! - **HippocampusService**: Central facade for recording, consolidation, retrieval

pub mod cache_tier;
pub mod episode;
pub mod facts;
pub mod golf;
pub mod narrative;
pub mod selection;
pub mod service;
pub mod sleep;
pub mod store;

pub use cache_tier::{CacheError, InMemoryKvCache, KvCacheTier};
pub use episode::{nmda_score, Episode};
pub use facts::{FactRetriever, FactStore, InMemoryFactStore, FACT_TRIGGERS};
pub use golf::{default_goals_for_role, Goal, GoalStatus, GoalType};
pub use narrative::NarrativeMemory;
pub use selection::{
    selection_decision, should_consolidate, NmdaSelectionDecision, NmdaSelectionProfile,
    CALIBRATED_NMDA_SELECTION_PROFILE, NMDA_CONSOLIDATION_THRESHOLD,
    NMDA_MAX_CONSOLIDATION_EPISODES, NMDA_NARRATIVE_INCLUSION_THRESHOLD, NMDA_SELECTION_RATIONALE,
};
pub use service::{ConsolidationResult, HippocampusService};
pub use sleep::{SleepCycle, SleepPhase};
pub use store::{HippocampusStore, NarrativeState, ReadOnlyHippocampusStore, RedbFactStore};
