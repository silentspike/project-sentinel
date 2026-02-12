//! sentinel-hippocampus: Multi-tier memory system for agent simulation.
//!
//! Implements a biologically-inspired memory architecture:
//! - **Episode + NMDA Scoring**: Event recording with consolidation scoring
//! - **NarrativeMemory**: Running summary of important daily events
//! - **FactRetriever**: Trigger-based JIT retrieval of company knowledge
//! - **KvCacheTier**: Hot/cold tiering interface for KV-cache offload

pub mod cache_tier;
pub mod episode;
pub mod facts;
pub mod narrative;
pub mod sleep;

pub use cache_tier::{CacheError, InMemoryKvCache, KvCacheTier};
pub use episode::{nmda_score, Episode};
pub use facts::{FactRetriever, FactStore, InMemoryFactStore, FACT_TRIGGERS};
pub use narrative::NarrativeMemory;
pub use sleep::{SleepCycle, SleepPhase};
