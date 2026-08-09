//! Persistent storage for hippocampus memory data via redb.
//!
//! Separate database file (`hippocampus.redb`) from the main StateStore.
//! 9 tables: episodes, narratives, facts, cache_state, goals, archive, and the
//! episode projection control, receipt, and quarantine tables.

use redb::{Database, ReadOnlyDatabase, ReadableDatabase, ReadableTable, TableDefinition};
use sentinel_common::types::AgentId;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use crate::episode::Episode;
use crate::facts::FactStore;
use crate::golf::Goal;

// Table definitions — all &str keys with &[u8] values (JSON-serialized)
const EPISODES: TableDefinition<&str, &[u8]> = TableDefinition::new("episodes");
const NARRATIVES: TableDefinition<&str, &[u8]> = TableDefinition::new("narratives");
const FACTS: TableDefinition<&str, &[u8]> = TableDefinition::new("facts");
const CACHE_STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("cache_state");
const GOALS: TableDefinition<&str, &[u8]> = TableDefinition::new("goals");
const ARCHIVE: TableDefinition<&str, &[u8]> = TableDefinition::new("archive");
const EPISODE_PROJECTION_STATE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("episode_projection_state");
const EPISODE_SOURCE_RECEIPTS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("episode_source_receipts");
const EPISODE_QUARANTINE: TableDefinition<&str, &[u8]> = TableDefinition::new("episode_quarantine");

const MAX_EPISODES_PER_AGENT: usize = 1000;
const EPISODE_PROJECTION_CONTROL_KEY: &str = "episode-producer-v1";
const KEY_SEPARATOR: char = '\u{1f}';

#[cfg(test)]
static FAIL_AFTER_EPISODE_WRITE: AtomicBool = AtomicBool::new(false);

/// Schema version for the dependency-independent EpisodeProducer projection.
pub const EPISODE_PROJECTION_VERSION: u32 = 1;

/// Explicit source position used when the projection is first initialized.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EpisodeProjectionStartPolicy {
    Beginning,
    RecoveryCut {
        source_row_id: i64,
        proof_digest: String,
    },
    ExplicitPosition {
        source_row_id: i64,
    },
}

impl EpisodeProjectionStartPolicy {
    fn source_row_id(&self) -> i64 {
        match self {
            Self::Beginning => 0,
            Self::RecoveryCut { source_row_id, .. } | Self::ExplicitPosition { source_row_id } => {
                *source_row_id
            }
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.source_row_id() >= 0,
            "episode projection start position must be non-negative"
        );
        if let Self::RecoveryCut { proof_digest, .. } = self {
            anyhow::ensure!(
                is_sha256_hex(proof_digest),
                "episode projection recovery cut requires a SHA-256 proof digest"
            );
        }
        Ok(())
    }
}

/// Store-owned global source cursor for the episode projection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionControl {
    pub projection_version: u32,
    pub start_policy: EpisodeProjectionStartPolicy,
    pub last_source_row_id: i64,
    pub last_source_event_id: Option<String>,
}

/// Stable authority subject for an episode projection. Display names are not
/// projection identities.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EpisodeProjectionSubject {
    Agent { agent_id: AgentId },
    Building,
}

impl EpisodeProjectionSubject {
    fn validate(self) -> anyhow::Result<()> {
        if let Self::Agent { agent_id } = self {
            AgentId::new(agent_id.0)
                .map_err(|error| anyhow::anyhow!("invalid episode projection agent ID: {error}"))?;
        }
        Ok(())
    }

    fn storage_key(self) -> String {
        match self {
            Self::Agent { agent_id } => format!("agent:{}", agent_id.0),
            Self::Building => "building".to_string(),
        }
    }
}

/// Stable projection subject paired with its immutable M0 episode-bucket name.
/// The subject owns receipt/frontier identity; the name remains a storage
/// locator until episode buckets are migrated to subject-keyed storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeProjectionAgent {
    pub subject: EpisodeProjectionSubject,
    pub agent_name: String,
}

/// Durable per-agent frontier. New agents start at the committed global cursor;
/// subsequent advances require the corresponding episode and source receipt in
/// the same redb transaction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionFrontier {
    pub subject: EpisodeProjectionSubject,
    pub agent_name: String,
    pub projection_version: u32,
    pub start_policy: EpisodeProjectionStartPolicy,
    pub last_source_row_id: i64,
    pub last_source_event_id: Option<String>,
    pub last_request_digest: Option<String>,
    pub applied_count: u64,
}

/// Permanent idempotency receipt for one source event applied to one agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeSourceReceipt {
    pub subject: EpisodeProjectionSubject,
    pub agent_name: String,
    pub source_event_id: String,
    pub source_row_id: i64,
    pub projection_version: u32,
    pub request_digest: String,
    pub episode_id: u64,
}

/// Input to the atomic episode projection write.
#[derive(Debug, Clone)]
pub struct EpisodeProjectionWrite {
    pub subject: EpisodeProjectionSubject,
    pub agent_name: String,
    pub source_event_id: String,
    pub source_row_id: i64,
    pub projection_version: u32,
    pub request_digest: String,
    pub expected_global_frontier: i64,
    pub episode: Episode,
}

/// Result of an atomic episode projection write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpisodeProjectionApplyOutcome {
    Applied {
        receipt: EpisodeSourceReceipt,
        control: EpisodeProjectionControl,
        frontier: EpisodeProjectionFrontier,
    },
    Duplicate {
        receipt: EpisodeSourceReceipt,
        control: EpisodeProjectionControl,
        frontier: EpisodeProjectionFrontier,
    },
}

/// Typed reason for a relevant source event that cannot be projected.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeProjectionQuarantineReason {
    MalformedRelevantPayload,
    EventTypeMismatch,
    UnknownAgent,
}

/// Durable readback for a relevant event that cannot advance its affected
/// per-agent frontier. The global scan cursor may advance atomically with this
/// record so that one poison event cannot block unrelated agents.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionQuarantine {
    /// `Agent` blocks only that subject. `Building` and `None` are global
    /// because no individual agent frontier can safely absorb them.
    pub affected_subject: Option<EpisodeProjectionSubject>,
    pub source_event_id: String,
    pub source_row_id: i64,
    pub event_type: String,
    pub projection_version: u32,
    pub request_digest: String,
    pub reason: EpisodeProjectionQuarantineReason,
    /// Digest of bounded diagnostic context. Raw event payload and diagnostic
    /// text are deliberately excluded from durable quarantine state.
    pub diagnostic_digest: String,
}

/// Typed reason why episode projection readiness is closed for a subject.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EpisodeProjectionReadinessBlock {
    ProjectionUninitialized,
    FrontierMissing,
    SubjectQuarantine {
        quarantine: EpisodeProjectionQuarantine,
    },
    GlobalQuarantine {
        quarantine: EpisodeProjectionQuarantine,
    },
}

/// Read-only readiness material for later orchestrator wiring.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionReadiness {
    pub subject: EpisodeProjectionSubject,
    pub frontier: Option<EpisodeProjectionFrontier>,
    pub blockers: Vec<EpisodeProjectionReadinessBlock>,
}

#[derive(Debug)]
struct PersistedEpisodeReceipt {
    key: String,
    encoded: Vec<u8>,
    receipt: EpisodeSourceReceipt,
}

fn receipt_entry_targets_subject(
    entry: &PersistedEpisodeReceipt,
    subject: EpisodeProjectionSubject,
) -> bool {
    let source_prefix = format!("source{KEY_SEPARATOR}{}{KEY_SEPARATOR}", subject.storage_key());
    let identity_prefix = format!(
        "episode{KEY_SEPARATOR}{}{KEY_SEPARATOR}",
        subject.storage_key()
    );
    entry.receipt.subject == subject
        || entry.key.starts_with(&source_prefix)
        || entry.key.starts_with(&identity_prefix)
}

impl EpisodeProjectionReadiness {
    pub fn is_ready(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Source identity for a non-episode event that may advance only the global
/// projection cursor.
#[derive(Debug, Clone)]
pub struct EpisodeProjectionAdvance {
    pub source_event_id: String,
    pub source_row_id: i64,
    pub projection_version: u32,
    pub request_digest: String,
    pub expected_global_frontier: i64,
}

/// Persistent state for narrative memory (serializable for redb storage).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NarrativeState {
    pub agent_name: String,
    pub summary: String,
    pub episode_count: usize,
}

/// ACID KV-store for hippocampus memory persistence.
///
/// Each agent's episodes, narratives, facts, and cache state are stored
/// in separate redb tables with string keys and JSON-serialized values.
pub struct HippocampusStore {
    db: Database,
}

/// Read-only handle for existing hippocampus.redb files.
///
/// This does not create missing tables and cannot start write transactions.
pub struct ReadOnlyHippocampusStore {
    db: ReadOnlyDatabase,
}

impl HippocampusStore {
    /// Open or create the hippocampus store at the given path.
    ///
    /// Creates all tables if they don't exist.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let db = Database::create(path).map_err(|e| {
            anyhow::anyhow!("Failed to create/open hippocampus.redb at {path}: {e}")
        })?;

        // Initialize all tables
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(EPISODES)?;
            write_txn.open_table(NARRATIVES)?;
            write_txn.open_table(FACTS)?;
            write_txn.open_table(CACHE_STATE)?;
            write_txn.open_table(GOALS)?;
            write_txn.open_table(ARCHIVE)?;
            write_txn.open_table(EPISODE_PROJECTION_STATE)?;
            write_txn.open_table(EPISODE_SOURCE_RECEIPTS)?;
            write_txn.open_table(EPISODE_QUARANTINE)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    /// Open an existing hippocampus store without write access.
    pub fn open_readonly(path: &str) -> anyhow::Result<ReadOnlyHippocampusStore> {
        ReadOnlyHippocampusStore::open(path)
    }

    // === EPISODES ===

    /// Store episodes for an agent (overwrites existing).
    pub fn store_episodes(&self, agent: &str, eps: &[Episode]) -> anyhow::Result<()> {
        let retained = if eps.len() > MAX_EPISODES_PER_AGENT {
            &eps[eps.len() - MAX_EPISODES_PER_AGENT..]
        } else {
            eps
        };
        let json = serde_json::to_vec(retained)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(EPISODES)?;
            table.insert(agent, json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load episodes for an agent. Returns empty vec if none stored.
    pub fn load_episodes(&self, agent: &str) -> anyhow::Result<Vec<Episode>> {
        load_episodes_from(&self.db, agent)
    }

    /// Append episodes to an agent's existing list. Caps at 1000 live episodes per agent.
    pub fn append_episodes(&self, agent: &str, new: &[Episode]) -> anyhow::Result<()> {
        let mut existing = self.load_episodes(agent)?;
        existing.extend_from_slice(new);
        self.store_episodes(agent, &existing)
    }

    /// Clear all episodes for an agent.
    pub fn clear_episodes(&self, agent: &str) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(EPISODES)?;
            table.remove(agent)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    // === EPISODE PROJECTION ===

    /// Initialize the store-owned cursor and durable per-agent frontiers.
    /// Existing state must use the exact same start policy.
    pub fn initialize_episode_projection(
        &self,
        start_policy: &EpisodeProjectionStartPolicy,
        agents: &[EpisodeProjectionAgent],
    ) -> anyhow::Result<EpisodeProjectionControl> {
        start_policy.validate()?;
        let mut subjects = HashSet::new();
        let mut names = HashSet::new();
        for agent in agents {
            agent.subject.validate()?;
            validate_projection_key_part(&agent.agent_name, "agent name")?;
            anyhow::ensure!(
                subjects.insert(agent.subject),
                "duplicate episode projection subject {}",
                agent.subject.storage_key()
            );
            anyhow::ensure!(
                names.insert(agent.agent_name.as_str()),
                "duplicate episode projection agent name {}",
                agent.agent_name
            );
        }

        let write_txn = self.db.begin_write()?;
        let has_existing_episode_data = {
            let episodes = write_txn.open_table(EPISODES)?;
            let mut has_data = false;
            for entry in episodes.iter()? {
                let (_, value) = entry?;
                let stored: Vec<Episode> = serde_json::from_slice(value.value())?;
                if !stored.is_empty() {
                    has_data = true;
                    break;
                }
            }
            has_data
        };
        let has_source_receipts = {
            let receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS)?;
            let mut has_data = false;
            if let Some(entry) = receipts.iter()?.next() {
                entry?;
                has_data = true;
            }
            has_data
        };
        let has_quarantine = {
            let quarantine = write_txn.open_table(EPISODE_QUARANTINE)?;
            let mut has_data = false;
            if let Some(entry) = quarantine.iter()?.next() {
                entry?;
                has_data = true;
            }
            has_data
        };
        let control;
        {
            let mut state = write_txn.open_table(EPISODE_PROJECTION_STATE)?;
            let frontier_prefix = format!("frontier{KEY_SEPARATOR}");
            let mut persisted_frontiers = Vec::new();
            let mut state_entry_count = 0_usize;
            for entry in state.iter()? {
                let (key, value) = entry?;
                state_entry_count += 1;
                if key.value() == EPISODE_PROJECTION_CONTROL_KEY {
                    continue;
                }
                anyhow::ensure!(
                    key.value().starts_with(&frontier_prefix),
                    "unknown episode projection state key"
                );
                let frontier: EpisodeProjectionFrontier =
                    serde_json::from_slice(value.value())?;
                anyhow::ensure!(
                    key.value() == projection_frontier_key(frontier.subject).as_str(),
                    "episode projection frontier key/value subject mismatch"
                );
                persisted_frontiers.push(frontier);
            }
            let mut persisted_subjects = HashSet::new();
            let mut persisted_names = HashSet::new();
            for frontier in &persisted_frontiers {
                anyhow::ensure!(
                    persisted_subjects.insert(frontier.subject),
                    "duplicate persisted episode projection subject {}",
                    frontier.subject.storage_key()
                );
                anyhow::ensure!(
                    persisted_names.insert(frontier.agent_name.as_str()),
                    "duplicate persisted episode bucket name {}",
                    frontier.agent_name
                );
            }
            for agent in agents {
                for frontier in &persisted_frontiers {
                    anyhow::ensure!(
                        frontier.subject != agent.subject
                            || frontier.agent_name == agent.agent_name,
                        "episode bucket name is immutable for subject {}",
                        agent.subject.storage_key()
                    );
                    anyhow::ensure!(
                        frontier.agent_name != agent.agent_name
                            || frontier.subject == agent.subject,
                        "episode bucket name {} is already bound to subject {}",
                        agent.agent_name,
                        frontier.subject.storage_key()
                    );
                }
            }
            let existing_control = state
                .get(EPISODE_PROJECTION_CONTROL_KEY)?
                .map(|guard| guard.value().to_vec());
            control = match existing_control {
                Some(encoded) => {
                    let existing: EpisodeProjectionControl = serde_json::from_slice(&encoded)?;
                    validate_projection_control(&existing)?;
                    anyhow::ensure!(
                        existing.start_policy == *start_policy,
                        "episode projection start policy is already fixed"
                    );
                    existing
                }
                None => {
                    anyhow::ensure!(
                        state_entry_count == 0 && !has_source_receipts && !has_quarantine,
                        "episode projection cannot initialize over orphaned projection state"
                    );
                    anyhow::ensure!(
                        !matches!(start_policy, EpisodeProjectionStartPolicy::Beginning)
                            || !has_existing_episode_data,
                        "Beginning episode projection requires an empty legacy episode store"
                    );
                    let created = EpisodeProjectionControl {
                        projection_version: EPISODE_PROJECTION_VERSION,
                        start_policy: start_policy.clone(),
                        last_source_row_id: start_policy.source_row_id(),
                        last_source_event_id: None,
                    };
                    let encoded = serde_json::to_vec(&created)?;
                    state.insert(EPISODE_PROJECTION_CONTROL_KEY, encoded.as_slice())?;
                    created
                }
            };
            validate_projection_control(&control)?;

            for agent in agents {
                let key = projection_frontier_key(agent.subject);
                let existing_frontier =
                    state.get(key.as_str())?.map(|guard| guard.value().to_vec());
                match existing_frontier {
                    Some(encoded) => {
                        let existing: EpisodeProjectionFrontier = serde_json::from_slice(&encoded)?;
                        validate_projection_frontier(&existing, agent.subject, &control)?;
                        anyhow::ensure!(
                            existing.subject == agent.subject
                                && existing.agent_name == agent.agent_name
                                && existing.projection_version == EPISODE_PROJECTION_VERSION
                                && existing.start_policy == *start_policy,
                            "episode projection frontier contract mismatch for {}",
                            agent.subject.storage_key()
                        );
                    }
                    None => {
                        let frontier = EpisodeProjectionFrontier {
                            subject: agent.subject,
                            agent_name: agent.agent_name.clone(),
                            projection_version: EPISODE_PROJECTION_VERSION,
                            start_policy: start_policy.clone(),
                            last_source_row_id: control.last_source_row_id,
                            last_source_event_id: control.last_source_event_id.clone(),
                            last_request_digest: None,
                            applied_count: 0,
                        };
                        let encoded = serde_json::to_vec(&frontier)?;
                        state.insert(key.as_str(), encoded.as_slice())?;
                    }
                }
            }
        }
        write_txn.commit()?;
        Ok(control)
    }

    /// Add a newly registered agent to the existing projection contract.
    pub fn initialize_episode_projection_agent(
        &self,
        agent: &EpisodeProjectionAgent,
    ) -> anyhow::Result<EpisodeProjectionFrontier> {
        agent.subject.validate()?;
        validate_projection_key_part(&agent.agent_name, "agent name")?;
        let control = self
            .load_episode_projection_control()?
            .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;
        self.initialize_episode_projection(&control.start_policy, std::slice::from_ref(agent))?;
        self.load_episode_projection_frontier(agent.subject)?
            .ok_or_else(|| anyhow::anyhow!("episode projection frontier was not initialized"))
    }

    /// Load the store-owned source cursor.
    pub fn load_episode_projection_control(
        &self,
    ) -> anyhow::Result<Option<EpisodeProjectionControl>> {
        load_json_value(
            &self.db,
            EPISODE_PROJECTION_STATE,
            EPISODE_PROJECTION_CONTROL_KEY,
        )
    }

    /// Load one agent's durable projection frontier.
    pub fn load_episode_projection_frontier(
        &self,
        subject: EpisodeProjectionSubject,
    ) -> anyhow::Result<Option<EpisodeProjectionFrontier>> {
        subject.validate()?;
        load_json_value(
            &self.db,
            EPISODE_PROJECTION_STATE,
            &projection_frontier_key(subject),
        )
    }

    /// List all per-agent frontiers for readiness and recovery readback.
    pub fn list_episode_projection_frontiers(
        &self,
    ) -> anyhow::Result<Vec<EpisodeProjectionFrontier>> {
        list_projection_frontiers_from(&self.db)
    }

    /// Load the permanent idempotency receipt for one agent/source event.
    pub fn load_episode_source_receipt(
        &self,
        subject: EpisodeProjectionSubject,
        source_event_id: &str,
    ) -> anyhow::Result<Option<EpisodeSourceReceipt>> {
        subject.validate()?;
        validate_projection_key_part(source_event_id, "source event id")?;
        load_json_value(
            &self.db,
            EPISODE_SOURCE_RECEIPTS,
            &source_receipt_key(subject, source_event_id),
        )
    }

    /// Atomically append an episode and commit its permanent source receipt,
    /// per-agent frontier, and global source cursor.
    pub fn commit_episode_projection(
        &self,
        input: &EpisodeProjectionWrite,
    ) -> anyhow::Result<EpisodeProjectionApplyOutcome> {
        validate_projection_write(input)?;
        let receipt_key = source_receipt_key(input.subject, &input.source_event_id);
        let identity_key = episode_identity_key(input.subject, input.episode.id);

        let write_txn = self.db.begin_write()?;
        let mut state = write_txn.open_table(EPISODE_PROJECTION_STATE)?;
        let mut receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS)?;
        let mut episodes = write_txn.open_table(EPISODES)?;

        let mut control: EpisodeProjectionControl =
            table_json_value(&state, EPISODE_PROJECTION_CONTROL_KEY)?
                .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;
        validate_projection_control(&control)?;
        let frontier_key = projection_frontier_key(input.subject);
        let mut frontier: EpisodeProjectionFrontier = table_json_value(&state, &frontier_key)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "episode projection frontier is not initialized for {} ({})",
                    input.subject.storage_key(),
                    input.agent_name,
                )
            })?;
        validate_projection_frontier(&frontier, input.subject, &control)?;
        anyhow::ensure!(
            frontier.subject == input.subject
                && frontier.agent_name == input.agent_name
                && frontier.projection_version == input.projection_version,
            "episode projection frontier contract mismatch for {}",
            input.subject.storage_key()
        );
        let mut persisted_receipts = Vec::new();
        for entry in receipts.iter()? {
            let (key, value) = entry?;
            persisted_receipts.push(PersistedEpisodeReceipt {
                key: key.value().to_string(),
                encoded: value.value().to_vec(),
                receipt: serde_json::from_slice(value.value())?,
            });
        }
        let retained_before: Vec<Episode> = match episodes.get(input.agent_name.as_str())? {
            Some(value) => serde_json::from_slice(value.value())?,
            None => Vec::new(),
        };
        validate_subject_integrity(
            input.subject,
            &frontier,
            &persisted_receipts,
            &retained_before,
        )?;

        if let Some(existing) = table_json_value::<EpisodeSourceReceipt>(&receipts, &receipt_key)? {
            ensure_exact_receipt_replay(&existing, input)?;
            if input.source_row_id > control.last_source_row_id {
                ensure_expected_frontier(&control, input.expected_global_frontier)?;
                control.last_source_row_id = input.source_row_id;
                control.last_source_event_id = Some(input.source_event_id.clone());
                insert_json(&mut state, EPISODE_PROJECTION_CONTROL_KEY, &control)?;
            }
            drop(episodes);
            drop(receipts);
            drop(state);
            write_txn.commit()?;
            return Ok(EpisodeProjectionApplyOutcome::Duplicate {
                receipt: existing,
                control,
                frontier,
            });
        }

        ensure_expected_frontier(&control, input.expected_global_frontier)?;
        anyhow::ensure!(
            input.source_row_id > control.last_source_row_id,
            "out-of-order episode source row {} is not after global frontier {}",
            input.source_row_id,
            control.last_source_row_id
        );
        anyhow::ensure!(
            input.source_row_id > frontier.last_source_row_id,
            "out-of-order episode source row {} is not after agent frontier {}",
            input.source_row_id,
            frontier.last_source_row_id
        );
        anyhow::ensure!(
            table_json_value::<EpisodeSourceReceipt>(&receipts, &identity_key)?.is_none(),
            "stable episode id collision for {}:{}",
            input.subject.storage_key(),
            input.episode.id
        );

        let mut retained: Vec<Episode> = match episodes.get(input.agent_name.as_str())? {
            Some(guard) => serde_json::from_slice(guard.value())?,
            None => Vec::new(),
        };
        retained.push(input.episode.clone());
        if retained.len() > MAX_EPISODES_PER_AGENT {
            let excess = retained.len() - MAX_EPISODES_PER_AGENT;
            retained.drain(..excess);
        }
        let encoded_episodes = serde_json::to_vec(&retained)?;
        episodes.insert(input.agent_name.as_str(), encoded_episodes.as_slice())?;

        #[cfg(test)]
        if FAIL_AFTER_EPISODE_WRITE.swap(false, Ordering::SeqCst) {
            anyhow::bail!("injected episode projection failure after episode write");
        }

        let receipt = EpisodeSourceReceipt {
            subject: input.subject,
            agent_name: input.agent_name.clone(),
            source_event_id: input.source_event_id.clone(),
            source_row_id: input.source_row_id,
            projection_version: input.projection_version,
            request_digest: input.request_digest.clone(),
            episode_id: input.episode.id,
        };
        insert_json(&mut receipts, &receipt_key, &receipt)?;
        insert_json(&mut receipts, &identity_key, &receipt)?;

        frontier.last_source_row_id = input.source_row_id;
        frontier.last_source_event_id = Some(input.source_event_id.clone());
        frontier.last_request_digest = Some(input.request_digest.clone());
        frontier.applied_count = frontier.applied_count.saturating_add(1);
        control.last_source_row_id = input.source_row_id;
        control.last_source_event_id = Some(input.source_event_id.clone());
        insert_json(&mut state, &frontier_key, &frontier)?;
        insert_json(&mut state, EPISODE_PROJECTION_CONTROL_KEY, &control)?;

        let mut persisted_receipts = Vec::new();
        for entry in receipts.iter()? {
            let (key, value) = entry?;
            persisted_receipts.push(PersistedEpisodeReceipt {
                key: key.value().to_string(),
                encoded: value.value().to_vec(),
                receipt: serde_json::from_slice(value.value())?,
            });
        }
        validate_subject_integrity(
            input.subject,
            &frontier,
            &persisted_receipts,
            &retained,
        )?;

        drop(episodes);
        drop(receipts);
        drop(state);
        write_txn.commit()?;
        Ok(EpisodeProjectionApplyOutcome::Applied {
            receipt,
            control,
            frontier,
        })
    }

    /// Advance the store-owned cursor for a source event that is explicitly
    /// irrelevant to the episode projection.
    pub fn advance_episode_projection(
        &self,
        source: &EpisodeProjectionAdvance,
    ) -> anyhow::Result<EpisodeProjectionControl> {
        validate_projection_source(
            &source.source_event_id,
            source.source_row_id,
            source.projection_version,
            &source.request_digest,
        )?;
        let write_txn = self.db.begin_write()?;
        let mut state = write_txn.open_table(EPISODE_PROJECTION_STATE)?;
        let mut control: EpisodeProjectionControl =
            table_json_value(&state, EPISODE_PROJECTION_CONTROL_KEY)?
                .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;

        if source.source_row_id > control.last_source_row_id {
            anyhow::ensure!(
                control.last_source_row_id == source.expected_global_frontier,
                "episode projection frontier conflict: expected {}, current {}",
                source.expected_global_frontier,
                control.last_source_row_id
            );
            control.last_source_row_id = source.source_row_id;
            control.last_source_event_id = Some(source.source_event_id.clone());
            insert_json(&mut state, EPISODE_PROJECTION_CONTROL_KEY, &control)?;
        }
        drop(state);
        write_txn.commit()?;
        Ok(control)
    }

    /// Durably quarantine a relevant source event and advance the global scan
    /// cursor without advancing the affected per-agent frontier.
    pub fn quarantine_episode_projection(
        &self,
        record: &EpisodeProjectionQuarantine,
        expected_global_frontier: i64,
    ) -> anyhow::Result<EpisodeProjectionControl> {
        validate_projection_quarantine(record)?;
        let mut canonical_record = record.clone();
        let write_txn = self.db.begin_write()?;
        let mut state = write_txn.open_table(EPISODE_PROJECTION_STATE)?;
        let mut control: EpisodeProjectionControl =
            table_json_value(&state, EPISODE_PROJECTION_CONTROL_KEY)?
                .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;
        validate_projection_control(&control)?;

        if let Some(subject @ EpisodeProjectionSubject::Agent { .. }) = record.affected_subject {
            let frontier: Option<EpisodeProjectionFrontier> =
                table_json_value(&state, &projection_frontier_key(subject))?;
            canonical_record.affected_subject = match frontier {
                Some(frontier)
                    if validate_projection_frontier(&frontier, subject, &control).is_ok() =>
                {
                    Some(subject)
                }
                Some(_) | None => None,
            };
        }

        let mut quarantine = write_txn.open_table(EPISODE_QUARANTINE)?;
        let key = quarantine_key(
            canonical_record.source_row_id,
            &canonical_record.source_event_id,
        );
        if let Some(existing) = table_json_value::<EpisodeProjectionQuarantine>(&quarantine, &key)?
        {
            anyhow::ensure!(
                existing == canonical_record,
                "episode quarantine replay conflict"
            );
            anyhow::ensure!(
                control.last_source_row_id >= canonical_record.source_row_id,
                "episode quarantine exists beyond the durable cursor"
            );
        } else {
            anyhow::ensure!(
                control.last_source_row_id == expected_global_frontier,
                "episode projection quarantine frontier conflict: expected {}, current {}",
                expected_global_frontier,
                control.last_source_row_id
            );
            anyhow::ensure!(
                canonical_record.source_row_id > control.last_source_row_id,
                "quarantined source row must be after the durable frontier"
            );
            insert_json(&mut quarantine, &key, &canonical_record)?;
            control.last_source_row_id = canonical_record.source_row_id;
            control.last_source_event_id = Some(canonical_record.source_event_id.clone());
            insert_json(&mut state, EPISODE_PROJECTION_CONTROL_KEY, &control)?;
        }
        drop(quarantine);
        drop(state);
        write_txn.commit()?;
        Ok(control)
    }

    /// List durable projection quarantines in source order.
    pub fn list_episode_projection_quarantine(
        &self,
    ) -> anyhow::Result<Vec<EpisodeProjectionQuarantine>> {
        list_quarantine_from(&self.db)
    }

    /// Read projection readiness for one stable subject without mutating state.
    pub fn load_episode_projection_readiness(
        &self,
        subject: EpisodeProjectionSubject,
    ) -> anyhow::Result<EpisodeProjectionReadiness> {
        load_projection_readiness_from(&self.db, subject)
    }

    // === NARRATIVES ===

    /// Store narrative state for an agent.
    pub fn store_narrative(&self, agent: &str, state: &NarrativeState) -> anyhow::Result<()> {
        let json = serde_json::to_vec(state)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(NARRATIVES)?;
            table.insert(agent, json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load narrative state for an agent.
    pub fn load_narrative(&self, agent: &str) -> anyhow::Result<Option<NarrativeState>> {
        load_narrative_from(&self.db, agent)
    }

    // === FACTS ===

    /// Store a fact by key.
    pub fn store_fact(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FACTS)?;
            table.insert(key, value.as_bytes())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load a fact by key.
    pub fn load_fact(&self, key: &str) -> anyhow::Result<Option<String>> {
        load_fact_from(&self.db, key)
    }

    /// Delete a fact by key. Returns true if it existed.
    pub fn delete_fact(&self, key: &str) -> anyhow::Result<bool> {
        let write_txn = self.db.begin_write()?;
        let existed;
        {
            let mut table = write_txn.open_table(FACTS)?;
            existed = table.remove(key)?.is_some();
        }
        write_txn.commit()?;
        Ok(existed)
    }

    // === CACHE STATE ===

    /// Store cache state (hot/cold) for an agent.
    pub fn store_cache_state(&self, agent: &str, is_hot: bool) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CACHE_STATE)?;
            table.insert(agent, &[is_hot as u8][..])?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load cache state for an agent. None if never stored.
    pub fn load_cache_state(&self, agent: &str) -> anyhow::Result<Option<bool>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CACHE_STATE)?;
        match table.get(agent)? {
            Some(guard) => {
                let bytes: &[u8] = guard.value();
                Ok(Some(bytes.first().copied().unwrap_or(0) != 0))
            }
            None => Ok(None),
        }
    }

    // === GOALS (GOLF Framework) ===

    /// Store goals for an agent (overwrites existing).
    pub fn store_goals(&self, agent: &str, goals: &[Goal]) -> anyhow::Result<()> {
        let json = serde_json::to_vec(goals)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(GOALS)?;
            table.insert(agent, json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load goals for an agent. Returns empty vec if none stored.
    pub fn load_goals(&self, agent: &str) -> anyhow::Result<Vec<Goal>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(GOALS)?;
        match table.get(agent)? {
            Some(guard) => {
                let bytes: &[u8] = guard.value();
                let goals: Vec<Goal> = serde_json::from_slice(bytes)?;
                Ok(goals)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Append goals to an agent's existing list.
    pub fn append_goals(&self, agent: &str, new: &[Goal]) -> anyhow::Result<()> {
        let mut existing = self.load_goals(agent)?;
        existing.extend_from_slice(new);
        self.store_goals(agent, &existing)
    }

    /// Update progress for a specific goal (by id) of an agent.
    ///
    /// Returns `true` if the goal was found and updated.
    pub fn update_goal_progress(
        &self,
        agent: &str,
        goal_id: u64,
        progress: f64,
        tick: u64,
    ) -> anyhow::Result<bool> {
        let mut goals = self.load_goals(agent)?;
        let mut found = false;
        for goal in &mut goals {
            if goal.id == goal_id {
                goal.update_progress(progress, tick);
                found = true;
                break;
            }
        }
        if found {
            self.store_goals(agent, &goals)?;
        }
        Ok(found)
    }

    /// List all agents that have stored goals.
    pub fn list_agents_with_goals(&self) -> anyhow::Result<Vec<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(GOALS)?;
        let mut agents = Vec::new();
        let iter = table.iter()?;
        for entry in iter {
            let (key, _): (redb::AccessGuard<'_, &str>, redb::AccessGuard<'_, &[u8]>) = entry?;
            agents.push(key.value().to_string());
        }
        Ok(agents)
    }

    // === ARCHIVE (consolidated episode preservation) ===

    /// Store archived episodes for an agent (overwrites existing).
    pub fn store_archive(&self, agent: &str, eps: &[Episode]) -> anyhow::Result<()> {
        let json = serde_json::to_vec(eps)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ARCHIVE)?;
            table.insert(agent, json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load archived episodes for an agent. Returns empty vec if none stored.
    pub fn load_archive(&self, agent: &str) -> anyhow::Result<Vec<Episode>> {
        load_archive_from(&self.db, agent)
    }

    /// Append episodes to an agent's archive. Caps at 1000 episodes per agent.
    pub fn append_archive(&self, agent: &str, new: &[Episode]) -> anyhow::Result<()> {
        let mut existing = self.load_archive(agent)?;
        existing.extend_from_slice(new);
        // Cap at 1000 episodes — drop oldest if exceeding
        if existing.len() > MAX_EPISODES_PER_AGENT {
            let excess = existing.len() - MAX_EPISODES_PER_AGENT;
            existing.drain(..excess);
        }
        self.store_archive(agent, &existing)
    }

    /// List all agents that have archived episodes.
    pub fn list_agents_with_archive(&self) -> anyhow::Result<Vec<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ARCHIVE)?;
        let mut agents = Vec::new();
        let iter = table.iter()?;
        for entry in iter {
            let (key, _): (redb::AccessGuard<'_, &str>, redb::AccessGuard<'_, &[u8]>) = entry?;
            agents.push(key.value().to_string());
        }
        Ok(agents)
    }

    // === UTILITY ===

    /// List all agents that have stored episodes.
    pub fn list_agents_with_episodes(&self) -> anyhow::Result<Vec<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(EPISODES)?;
        let mut agents = Vec::new();
        let iter = table.iter()?;
        for entry in iter {
            let (key, _): (redb::AccessGuard<'_, &str>, redb::AccessGuard<'_, &[u8]>) = entry?;
            agents.push(key.value().to_string());
        }
        Ok(agents)
    }
}

impl ReadOnlyHippocampusStore {
    /// Open an existing hippocampus store in redb read-only mode.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let db = ReadOnlyDatabase::open(path).map_err(|e| {
            anyhow::anyhow!("Failed to open hippocampus.redb read-only at {path}: {e}")
        })?;
        Ok(Self { db })
    }

    /// Load episodes for an agent. Returns empty vec if none stored.
    pub fn load_episodes(&self, agent: &str) -> anyhow::Result<Vec<Episode>> {
        load_episodes_from(&self.db, agent)
    }

    /// Load narrative state for an agent.
    pub fn load_narrative(&self, agent: &str) -> anyhow::Result<Option<NarrativeState>> {
        load_narrative_from(&self.db, agent)
    }

    /// Load a fact by key.
    pub fn load_fact(&self, key: &str) -> anyhow::Result<Option<String>> {
        load_fact_from(&self.db, key)
    }

    /// Load archived episodes for an agent. Returns empty vec if none stored.
    pub fn load_archive(&self, agent: &str) -> anyhow::Result<Vec<Episode>> {
        load_archive_from(&self.db, agent)
    }

    /// Load the store-owned episode projection cursor.
    pub fn load_episode_projection_control(
        &self,
    ) -> anyhow::Result<Option<EpisodeProjectionControl>> {
        load_json_value(
            &self.db,
            EPISODE_PROJECTION_STATE,
            EPISODE_PROJECTION_CONTROL_KEY,
        )
    }

    /// Load one agent's durable episode projection frontier.
    pub fn load_episode_projection_frontier(
        &self,
        subject: EpisodeProjectionSubject,
    ) -> anyhow::Result<Option<EpisodeProjectionFrontier>> {
        subject.validate()?;
        load_json_value(
            &self.db,
            EPISODE_PROJECTION_STATE,
            &projection_frontier_key(subject),
        )
    }

    /// List all per-agent episode projection frontiers.
    pub fn list_episode_projection_frontiers(
        &self,
    ) -> anyhow::Result<Vec<EpisodeProjectionFrontier>> {
        list_projection_frontiers_from(&self.db)
    }

    /// Load one permanent source receipt without opening a write transaction.
    pub fn load_episode_source_receipt(
        &self,
        subject: EpisodeProjectionSubject,
        source_event_id: &str,
    ) -> anyhow::Result<Option<EpisodeSourceReceipt>> {
        subject.validate()?;
        validate_projection_key_part(source_event_id, "source event id")?;
        load_json_value(
            &self.db,
            EPISODE_SOURCE_RECEIPTS,
            &source_receipt_key(subject, source_event_id),
        )
    }

    /// List durable episode projection quarantines.
    pub fn list_episode_projection_quarantine(
        &self,
    ) -> anyhow::Result<Vec<EpisodeProjectionQuarantine>> {
        list_quarantine_from(&self.db)
    }

    /// Read projection readiness for one stable subject.
    pub fn load_episode_projection_readiness(
        &self,
        subject: EpisodeProjectionSubject,
    ) -> anyhow::Result<EpisodeProjectionReadiness> {
        load_projection_readiness_from(&self.db, subject)
    }
}

fn load_episodes_from<D: ReadableDatabase>(db: &D, agent: &str) -> anyhow::Result<Vec<Episode>> {
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(EPISODES)?;
    match table.get(agent)? {
        Some(guard) => {
            let bytes: &[u8] = guard.value();
            let episodes: Vec<Episode> = serde_json::from_slice(bytes)?;
            Ok(episodes)
        }
        None => Ok(Vec::new()),
    }
}

fn load_narrative_from<D: ReadableDatabase>(
    db: &D,
    agent: &str,
) -> anyhow::Result<Option<NarrativeState>> {
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(NARRATIVES)?;
    match table.get(agent)? {
        Some(guard) => {
            let bytes: &[u8] = guard.value();
            let state: NarrativeState = serde_json::from_slice(bytes)?;
            Ok(Some(state))
        }
        None => Ok(None),
    }
}

fn load_fact_from<D: ReadableDatabase>(db: &D, key: &str) -> anyhow::Result<Option<String>> {
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(FACTS)?;
    match table.get(key)? {
        Some(guard) => {
            let bytes: &[u8] = guard.value();
            let value = std::str::from_utf8(bytes)?;
            Ok(Some(value.to_string()))
        }
        None => Ok(None),
    }
}

fn load_archive_from<D: ReadableDatabase>(db: &D, agent: &str) -> anyhow::Result<Vec<Episode>> {
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(ARCHIVE)?;
    match table.get(agent)? {
        Some(guard) => {
            let bytes: &[u8] = guard.value();
            let episodes: Vec<Episode> = serde_json::from_slice(bytes)?;
            Ok(episodes)
        }
        None => Ok(Vec::new()),
    }
}

fn load_json_value<D, T>(
    db: &D,
    definition: TableDefinition<&str, &[u8]>,
    key: &str,
) -> anyhow::Result<Option<T>>
where
    D: ReadableDatabase,
    T: serde::de::DeserializeOwned,
{
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(definition)?;
    match table.get(key)? {
        Some(guard) => Ok(Some(serde_json::from_slice(guard.value())?)),
        None => Ok(None),
    }
}

fn list_projection_frontiers_from<D: ReadableDatabase>(
    db: &D,
) -> anyhow::Result<Vec<EpisodeProjectionFrontier>> {
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(EPISODE_PROJECTION_STATE)?;
    let prefix = format!("frontier{KEY_SEPARATOR}");
    let mut frontiers: Vec<EpisodeProjectionFrontier> = Vec::new();
    for entry in table.iter()? {
        let (key, value) = entry?;
        if key.value().starts_with(&prefix) {
            frontiers.push(serde_json::from_slice(value.value())?);
        }
    }
    frontiers.sort_by_key(|frontier| frontier.subject.storage_key());
    Ok(frontiers)
}

fn list_quarantine_from<D: ReadableDatabase>(
    db: &D,
) -> anyhow::Result<Vec<EpisodeProjectionQuarantine>> {
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(EPISODE_QUARANTINE)?;
    let mut records = Vec::new();
    for entry in table.iter()? {
        let (_, value) = entry?;
        records.push(serde_json::from_slice(value.value())?);
    }
    records.sort_by_key(|record: &EpisodeProjectionQuarantine| record.source_row_id);
    Ok(records)
}

fn load_projection_readiness_from<D: ReadableDatabase>(
    db: &D,
    subject: EpisodeProjectionSubject,
) -> anyhow::Result<EpisodeProjectionReadiness> {
    subject.validate()?;
    let read_txn = db.begin_read()?;
    let state = read_txn.open_table(EPISODE_PROJECTION_STATE)?;
    let receipts = read_txn.open_table(EPISODE_SOURCE_RECEIPTS)?;
    let episodes = read_txn.open_table(EPISODES)?;
    let quarantine = read_txn.open_table(EPISODE_QUARANTINE)?;
    let control: Option<EpisodeProjectionControl> = match state.get(EPISODE_PROJECTION_CONTROL_KEY)?
    {
        Some(value) => Some(serde_json::from_slice(value.value())?),
        None => None,
    };
    if let Some(control) = &control {
        validate_projection_control(control)?;
    }
    let frontier_prefix = format!("frontier{KEY_SEPARATOR}");
    let mut frontier = None;
    for entry in state.iter()? {
        let (key, value) = entry?;
        if key.value() == EPISODE_PROJECTION_CONTROL_KEY {
            continue;
        }
        anyhow::ensure!(
            key.value().starts_with(&frontier_prefix),
            "unknown episode projection state key"
        );
        let persisted: EpisodeProjectionFrontier = serde_json::from_slice(value.value())?;
        let expected_key = projection_frontier_key(persisted.subject);
        anyhow::ensure!(
            key.value() == expected_key.as_str(),
            "episode projection frontier key/value subject mismatch"
        );
        let control = control
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("orphaned episode projection frontier"))?;
        validate_projection_frontier(&persisted, persisted.subject, control)?;
        if persisted.subject == subject {
            frontier = Some(persisted);
        }
    }
    let mut persisted_receipts = Vec::new();
    for entry in receipts.iter()? {
        let (key, value) = entry?;
        persisted_receipts.push(PersistedEpisodeReceipt {
            key: key.value().to_string(),
            encoded: value.value().to_vec(),
            receipt: serde_json::from_slice(value.value())?,
        });
    }
    match &frontier {
        Some(frontier) => {
            let retained: Vec<Episode> = match episodes.get(frontier.agent_name.as_str())? {
                Some(value) => serde_json::from_slice(value.value())?,
                None => Vec::new(),
            };
            validate_subject_integrity(subject, frontier, &persisted_receipts, &retained)?;
        }
        None => {
            anyhow::ensure!(
                !persisted_receipts
                    .iter()
                    .any(|entry| receipt_entry_targets_subject(entry, subject)),
                "episode receipts exist without a subject frontier"
            );
        }
    }
    let mut blockers = Vec::new();
    if control.is_none() {
        blockers.push(EpisodeProjectionReadinessBlock::ProjectionUninitialized);
    }
    if frontier.is_none() {
        blockers.push(EpisodeProjectionReadinessBlock::FrontierMissing);
    }
    for entry in quarantine.iter()? {
        let (key, value) = entry?;
        let record: EpisodeProjectionQuarantine = serde_json::from_slice(value.value())?;
        validate_projection_quarantine(&record)?;
        anyhow::ensure!(
            key.value() == quarantine_key(record.source_row_id, &record.source_event_id),
            "episode projection quarantine key/value mismatch"
        );
        let control = control
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("orphaned episode projection quarantine"))?;
        anyhow::ensure!(
            record.source_row_id > control.start_policy.source_row_id()
                && record.source_row_id <= control.last_source_row_id,
            "episode projection quarantine cursor is outside the control contract"
        );
        match record.affected_subject {
            Some(quarantine_subject @ EpisodeProjectionSubject::Agent { .. }) => {
                let quarantine_frontier: EpisodeProjectionFrontier = state
                    .get(projection_frontier_key(quarantine_subject).as_str())?
                    .map(|value| serde_json::from_slice(value.value()))
                    .transpose()?
                    .ok_or_else(|| {
                        anyhow::anyhow!("agent quarantine has no durable subject frontier")
                    })?;
                validate_projection_frontier(
                    &quarantine_frontier,
                    quarantine_subject,
                    control,
                )?;
                if quarantine_subject == subject {
                    blockers.push(EpisodeProjectionReadinessBlock::SubjectQuarantine {
                        quarantine: record,
                    });
                }
            }
            Some(EpisodeProjectionSubject::Building) | None => {
                blockers.push(EpisodeProjectionReadinessBlock::GlobalQuarantine {
                    quarantine: record,
                });
            }
        }
    }
    Ok(EpisodeProjectionReadiness {
        subject,
        frontier,
        blockers,
    })
}

fn table_json_value<T>(table: &redb::Table<'_, &str, &[u8]>, key: &str) -> anyhow::Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    match table.get(key)? {
        Some(guard) => Ok(Some(serde_json::from_slice(guard.value())?)),
        None => Ok(None),
    }
}

fn insert_json<T>(
    table: &mut redb::Table<'_, &str, &[u8]>,
    key: &str,
    value: &T,
) -> anyhow::Result<()>
where
    T: serde::Serialize,
{
    let encoded = serde_json::to_vec(value)?;
    table.insert(key, encoded.as_slice())?;
    Ok(())
}

fn projection_frontier_key(subject: EpisodeProjectionSubject) -> String {
    format!("frontier{KEY_SEPARATOR}{}", subject.storage_key())
}

fn source_receipt_key(
    subject: EpisodeProjectionSubject,
    source_event_id: &str,
) -> String {
    format!(
        "source{KEY_SEPARATOR}{}{KEY_SEPARATOR}{source_event_id}",
        subject.storage_key()
    )
}

fn episode_identity_key(subject: EpisodeProjectionSubject, episode_id: u64) -> String {
    format!(
        "episode{KEY_SEPARATOR}{}{KEY_SEPARATOR}{episode_id:016x}",
        subject.storage_key()
    )
}

fn quarantine_key(source_row_id: i64, source_event_id: &str) -> String {
    format!("{source_row_id:020}{KEY_SEPARATOR}{source_event_id}")
}

fn validate_projection_key_part(value: &str, name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty(),
        "episode projection {name} must not be empty"
    );
    anyhow::ensure!(
        !value.contains(KEY_SEPARATOR),
        "episode projection {name} contains a reserved separator"
    );
    anyhow::ensure!(
        value.len() <= 512,
        "episode projection {name} exceeds 512 bytes"
    );
    Ok(())
}

fn validate_projection_source(
    source_event_id: &str,
    source_row_id: i64,
    projection_version: u32,
    request_digest: &str,
) -> anyhow::Result<()> {
    validate_projection_key_part(source_event_id, "source event id")?;
    anyhow::ensure!(
        source_row_id > 0,
        "episode projection source row must be positive"
    );
    anyhow::ensure!(
        projection_version == EPISODE_PROJECTION_VERSION,
        "unsupported episode projection version {projection_version}"
    );
    anyhow::ensure!(
        is_sha256_hex(request_digest),
        "episode projection request digest must be SHA-256 hex"
    );
    Ok(())
}

fn validate_projection_write(input: &EpisodeProjectionWrite) -> anyhow::Result<()> {
    input.subject.validate()?;
    validate_projection_key_part(&input.agent_name, "agent name")?;
    validate_projection_source(
        &input.source_event_id,
        input.source_row_id,
        input.projection_version,
        &input.request_digest,
    )?;
    anyhow::ensure!(
        input.expected_global_frontier >= 0,
        "expected global frontier must be non-negative"
    );
    anyhow::ensure!(
        input.episode.agent_name == input.agent_name,
        "episode agent does not match projection agent"
    );
    anyhow::ensure!(input.episode.id != 0, "stable episode id must be non-zero");
    Ok(())
}

fn validate_projection_control(control: &EpisodeProjectionControl) -> anyhow::Result<()> {
    anyhow::ensure!(
        control.projection_version == EPISODE_PROJECTION_VERSION,
        "unsupported episode projection control version {}",
        control.projection_version
    );
    control.start_policy.validate()?;
    anyhow::ensure!(
        control.last_source_row_id >= control.start_policy.source_row_id(),
        "episode projection control cursor is before its start policy"
    );
    if let Some(event_id) = &control.last_source_event_id {
        validate_projection_key_part(event_id, "control source event id")?;
    }
    Ok(())
}

fn validate_projection_frontier(
    frontier: &EpisodeProjectionFrontier,
    expected_subject: EpisodeProjectionSubject,
    control: &EpisodeProjectionControl,
) -> anyhow::Result<()> {
    expected_subject.validate()?;
    frontier.subject.validate()?;
    validate_projection_key_part(&frontier.agent_name, "agent name")?;
    anyhow::ensure!(
        frontier.subject == expected_subject,
        "episode projection frontier key/value subject mismatch"
    );
    anyhow::ensure!(
        frontier.projection_version == control.projection_version,
        "episode projection frontier/control version mismatch"
    );
    anyhow::ensure!(
        frontier.start_policy == control.start_policy,
        "episode projection frontier/control policy mismatch"
    );
    anyhow::ensure!(
        frontier.last_source_row_id >= frontier.start_policy.source_row_id(),
        "episode projection frontier cursor is before its start policy"
    );
    anyhow::ensure!(
        frontier.last_source_row_id <= control.last_source_row_id,
        "episode projection frontier cursor is ahead of global control"
    );
    if let Some(event_id) = &frontier.last_source_event_id {
        validate_projection_key_part(event_id, "frontier source event id")?;
    }
    if let Some(digest) = &frontier.last_request_digest {
        anyhow::ensure!(
            is_sha256_hex(digest),
            "episode projection frontier request digest must be SHA-256 hex"
        );
    }
    Ok(())
}

fn validate_subject_integrity(
    subject: EpisodeProjectionSubject,
    frontier: &EpisodeProjectionFrontier,
    receipt_entries: &[PersistedEpisodeReceipt],
    retained_episodes: &[Episode],
) -> anyhow::Result<()> {
    let relevant: Vec<&PersistedEpisodeReceipt> = receipt_entries
        .iter()
        .filter(|entry| receipt_entry_targets_subject(entry, subject))
        .collect();
    let by_key: HashMap<&str, &PersistedEpisodeReceipt> = relevant
        .iter()
        .map(|entry| (entry.key.as_str(), *entry))
        .collect();
    anyhow::ensure!(
        by_key.len() == relevant.len(),
        "duplicate episode receipt key"
    );

    let mut source_receipts = Vec::new();
    let mut source_rows = HashSet::new();
    let mut source_events = HashSet::new();
    let mut episode_ids = HashSet::new();
    for entry in &relevant {
        let receipt = &entry.receipt;
        receipt.subject.validate()?;
        validate_projection_key_part(&receipt.agent_name, "receipt agent name")?;
        validate_projection_source(
            &receipt.source_event_id,
            receipt.source_row_id,
            receipt.projection_version,
            &receipt.request_digest,
        )?;
        anyhow::ensure!(
            receipt.subject == subject
                && receipt.agent_name == frontier.agent_name
                && receipt.projection_version == frontier.projection_version,
            "episode receipt subject/frontier contract mismatch"
        );
        anyhow::ensure!(
            receipt.source_row_id > frontier.start_policy.source_row_id()
                && receipt.source_row_id <= frontier.last_source_row_id,
            "episode receipt row is outside the subject frontier contract"
        );
        anyhow::ensure!(receipt.episode_id != 0, "episode receipt ID must be non-zero");
        let expected_source = source_receipt_key(subject, &receipt.source_event_id);
        let expected_identity = episode_identity_key(subject, receipt.episode_id);
        anyhow::ensure!(
            entry.key == expected_source || entry.key == expected_identity,
            "episode receipt key/value binding mismatch"
        );
        let source_entry = by_key
            .get(expected_source.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing episode source receipt counterpart"))?;
        let identity_entry = by_key
            .get(expected_identity.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing episode identity receipt counterpart"))?;
        anyhow::ensure!(
            source_entry.encoded == identity_entry.encoded
                && source_entry.receipt == identity_entry.receipt
                && entry.encoded == source_entry.encoded,
            "episode receipt counterparts do not match exactly"
        );
        if entry.key == expected_source {
            anyhow::ensure!(
                source_rows.insert(receipt.source_row_id),
                "duplicate episode receipt source row"
            );
            anyhow::ensure!(
                source_events.insert(receipt.source_event_id.as_str()),
                "duplicate episode receipt source event"
            );
            anyhow::ensure!(
                episode_ids.insert(receipt.episode_id),
                "duplicate stable episode receipt ID"
            );
            source_receipts.push(receipt);
        }
    }
    anyhow::ensure!(
        relevant.len() == source_receipts.len().saturating_mul(2),
        "episode receipt index cardinality mismatch"
    );
    anyhow::ensure!(
        frontier.applied_count == source_receipts.len() as u64,
        "episode frontier applied count does not match source receipts"
    );

    if let Some(last_receipt) = source_receipts
        .into_iter()
        .max_by_key(|receipt| receipt.source_row_id)
    {
        anyhow::ensure!(
            frontier.last_source_row_id == last_receipt.source_row_id
                && frontier.last_source_event_id.as_deref()
                    == Some(last_receipt.source_event_id.as_str())
                && frontier.last_request_digest.as_deref()
                    == Some(last_receipt.request_digest.as_str()),
            "episode frontier does not match its maximal source receipt"
        );
        let retained_matches = retained_episodes
            .iter()
            .filter(|episode| {
                episode.id == last_receipt.episode_id
                    && episode.agent_name == frontier.agent_name
            })
            .count();
        anyhow::ensure!(
            retained_matches == 1,
            "maximal source receipt episode is missing or duplicated in retained bucket"
        );
    } else {
        anyhow::ensure!(
            frontier.applied_count == 0 && frontier.last_request_digest.is_none(),
            "zero-receipt frontier contains contradictory applied material"
        );
    }
    Ok(())
}

fn validate_projection_quarantine(record: &EpisodeProjectionQuarantine) -> anyhow::Result<()> {
    if let Some(subject) = record.affected_subject {
        subject.validate()?;
    }
    validate_projection_key_part(&record.event_type, "event type")?;
    validate_projection_source(
        &record.source_event_id,
        record.source_row_id,
        record.projection_version,
        &record.request_digest,
    )?;
    anyhow::ensure!(
        is_sha256_hex(&record.diagnostic_digest),
        "episode projection quarantine diagnostic digest must be SHA-256 hex"
    );
    Ok(())
}

fn ensure_expected_frontier(
    control: &EpisodeProjectionControl,
    expected: i64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        control.last_source_row_id == expected,
        "episode projection frontier conflict: expected {expected}, current {}",
        control.last_source_row_id
    );
    Ok(())
}

fn ensure_exact_receipt_replay(
    existing: &EpisodeSourceReceipt,
    input: &EpisodeProjectionWrite,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        existing.subject == input.subject
            && existing.agent_name == input.agent_name
            && existing.source_event_id == input.source_event_id
            && existing.projection_version == input.projection_version
            && existing.request_digest == input.request_digest
            && existing.episode_id == input.episode.id,
        "episode source receipt replay conflict"
    );
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Persistent FactStore implementation backed by HippocampusStore.
pub struct RedbFactStore<'a> {
    store: &'a HippocampusStore,
}

impl<'a> RedbFactStore<'a> {
    pub fn new(store: &'a HippocampusStore) -> Self {
        Self { store }
    }
}

impl FactStore for RedbFactStore<'_> {
    fn get_fact(&self, key: &str) -> anyhow::Result<Option<String>> {
        self.store.load_fact(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    static PROJECTION_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn temp_store() -> (HippocampusStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-hippocampus.redb");
        let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
        (store, dir)
    }

    fn make_episode(id: u64, summary: &str) -> Episode {
        Episode {
            id,
            agent_name: "Thomas".to_string(),
            summary: summary.to_string(),
            relevance: 0.8,
            emotion: 0.7,
            repetitions: 1,
            hours_ago: 1.0,
            participants: vec!["Lisa".to_string()],
            tags: vec!["meeting".to_string()],
        }
    }

    fn projection_subject() -> EpisodeProjectionSubject {
        EpisodeProjectionSubject::Agent {
            agent_id: AgentId(1),
        }
    }

    fn projection_agent() -> EpisodeProjectionAgent {
        EpisodeProjectionAgent {
            subject: projection_subject(),
            agent_name: "Thomas".to_string(),
        }
    }

    fn overwrite_projection_control(
        store: &HippocampusStore,
        control: &EpisodeProjectionControl,
    ) {
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut state = write_txn.open_table(EPISODE_PROJECTION_STATE).unwrap();
            insert_json(&mut state, EPISODE_PROJECTION_CONTROL_KEY, control).unwrap();
        }
        write_txn.commit().unwrap();
    }

    fn overwrite_projection_frontier(
        store: &HippocampusStore,
        key_subject: EpisodeProjectionSubject,
        frontier: &EpisodeProjectionFrontier,
    ) {
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut state = write_txn.open_table(EPISODE_PROJECTION_STATE).unwrap();
            insert_json(
                &mut state,
                &projection_frontier_key(key_subject),
                frontier,
            )
            .unwrap();
        }
        write_txn.commit().unwrap();
    }

    fn committed_projection_store() -> (
        HippocampusStore,
        tempfile::TempDir,
        EpisodeProjectionWrite,
    ) {
        let (store, dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let input = projection_write(1, "integrity-event", 17, 0);
        store.commit_episode_projection(&input).unwrap();
        (store, dir, input)
    }

    fn receipt_table_snapshot(store: &HippocampusStore) -> Vec<(String, Vec<u8>)> {
        let read_txn = store.db.begin_read().unwrap();
        let table = read_txn.open_table(EPISODE_SOURCE_RECEIPTS).unwrap();
        let mut snapshot = Vec::new();
        for entry in table.iter().unwrap() {
            let (key, value) = entry.unwrap();
            snapshot.push((key.value().to_string(), value.value().to_vec()));
        }
        snapshot.sort_by(|a, b| a.0.cmp(&b.0));
        snapshot
    }

    fn episode_bucket_snapshot(store: &HippocampusStore, agent_name: &str) -> Option<Vec<u8>> {
        let read_txn = store.db.begin_read().unwrap();
        let table = read_txn.open_table(EPISODES).unwrap();
        let snapshot = table
            .get(agent_name)
            .unwrap()
            .map(|value| value.value().to_vec());
        snapshot
    }

    fn assert_integrity_retry_is_no_write(
        store: &HippocampusStore,
        input: &EpisodeProjectionWrite,
    ) {
        let control_before = store.load_episode_projection_control().unwrap();
        let frontier_before = store
            .load_episode_projection_frontier(input.subject)
            .unwrap();
        let receipts_before = receipt_table_snapshot(store);
        let episodes_before = episode_bucket_snapshot(store, &input.agent_name);

        assert!(store
            .load_episode_projection_readiness(input.subject)
            .is_err());
        assert!(store.commit_episode_projection(input).is_err());

        assert_eq!(
            store.load_episode_projection_control().unwrap(),
            control_before
        );
        assert_eq!(
            store
                .load_episode_projection_frontier(input.subject)
                .unwrap(),
            frontier_before
        );
        assert_eq!(receipt_table_snapshot(store), receipts_before);
        assert_eq!(
            episode_bucket_snapshot(store, &input.agent_name),
            episodes_before
        );
    }

    fn projection_write(
        row: i64,
        source_event_id: &str,
        episode_id: u64,
        expected_global_frontier: i64,
    ) -> EpisodeProjectionWrite {
        EpisodeProjectionWrite {
            subject: projection_subject(),
            agent_name: "Thomas".to_string(),
            source_event_id: source_event_id.to_string(),
            source_row_id: row,
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: format!("{row:064x}"),
            expected_global_frontier,
            episode: make_episode(episode_id, source_event_id),
        }
    }

    #[test]
    fn episode_projection_start_policy_is_durable_and_immutable() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        let policy = EpisodeProjectionStartPolicy::RecoveryCut {
            source_row_id: 41,
            proof_digest: "ab".repeat(32),
        };

        let control = store
            .initialize_episode_projection(&policy, &[projection_agent()])
            .unwrap();
        assert_eq!(control.last_source_row_id, 41);
        assert_eq!(control.start_policy, policy);
        let frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        assert_eq!(frontier.last_source_row_id, 41);

        let error = store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::ExplicitPosition { source_row_id: 41 },
                &[projection_agent()],
            )
            .unwrap_err();
        assert!(error.to_string().contains("already fixed"));
    }

    #[test]
    fn episode_projection_initialization_rejects_every_orphan_state_atomically() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();

        let (frontier_store, _frontier_dir) = temp_store();
        let orphan_frontier = EpisodeProjectionFrontier {
            subject: projection_subject(),
            agent_name: "Thomas".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            start_policy: EpisodeProjectionStartPolicy::Beginning,
            last_source_row_id: 0,
            last_source_event_id: None,
            last_request_digest: None,
            applied_count: 0,
        };
        let write_txn = frontier_store.db.begin_write().unwrap();
        {
            let mut state = write_txn.open_table(EPISODE_PROJECTION_STATE).unwrap();
            insert_json(
                &mut state,
                &projection_frontier_key(projection_subject()),
                &orphan_frontier,
            )
            .unwrap();
        }
        write_txn.commit().unwrap();
        let error = frontier_store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap_err();
        assert!(error.to_string().contains("orphaned"));
        assert!(frontier_store
            .load_episode_projection_control()
            .unwrap()
            .is_none());
        assert_eq!(
            frontier_store
                .list_episode_projection_frontiers()
                .unwrap(),
            vec![orphan_frontier]
        );

        let (receipt_store, _receipt_dir) = temp_store();
        let orphan_receipt = EpisodeSourceReceipt {
            subject: projection_subject(),
            agent_name: "Thomas".to_string(),
            source_event_id: "orphan-receipt".to_string(),
            source_row_id: 1,
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "ab".repeat(32),
            episode_id: 17,
        };
        let write_txn = receipt_store.db.begin_write().unwrap();
        {
            let mut receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS).unwrap();
            insert_json(
                &mut receipts,
                &source_receipt_key(projection_subject(), "orphan-receipt"),
                &orphan_receipt,
            )
            .unwrap();
        }
        write_txn.commit().unwrap();
        let error = receipt_store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::ExplicitPosition { source_row_id: 1 },
                &[projection_agent()],
            )
            .unwrap_err();
        assert!(error.to_string().contains("orphaned"));
        assert!(receipt_store
            .load_episode_projection_control()
            .unwrap()
            .is_none());
        assert_eq!(
            receipt_store
                .load_episode_source_receipt(projection_subject(), "orphan-receipt")
                .unwrap(),
            Some(orphan_receipt)
        );

        let (quarantine_store, _quarantine_dir) = temp_store();
        let orphan_quarantine = EpisodeProjectionQuarantine {
            affected_subject: None,
            source_event_id: "orphan-quarantine".to_string(),
            source_row_id: 1,
            event_type: "agent_action_received".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "bc".repeat(32),
            reason: EpisodeProjectionQuarantineReason::UnknownAgent,
            diagnostic_digest: "cd".repeat(32),
        };
        let write_txn = quarantine_store.db.begin_write().unwrap();
        {
            let mut quarantine = write_txn.open_table(EPISODE_QUARANTINE).unwrap();
            insert_json(
                &mut quarantine,
                &quarantine_key(1, "orphan-quarantine"),
                &orphan_quarantine,
            )
            .unwrap();
        }
        write_txn.commit().unwrap();
        let error = quarantine_store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::RecoveryCut {
                    source_row_id: 1,
                    proof_digest: "de".repeat(32),
                },
                &[projection_agent()],
            )
            .unwrap_err();
        assert!(error.to_string().contains("orphaned"));
        assert!(quarantine_store
            .load_episode_projection_control()
            .unwrap()
            .is_none());
        assert_eq!(
            quarantine_store
                .list_episode_projection_quarantine()
                .unwrap(),
            vec![orphan_quarantine]
        );
    }

    #[test]
    fn episode_projection_apply_and_exact_retry_are_idempotent() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let input = projection_write(1, "event-1", 17, 0);

        assert!(matches!(
            store.commit_episode_projection(&input).unwrap(),
            EpisodeProjectionApplyOutcome::Applied { .. }
        ));
        assert!(matches!(
            store.commit_episode_projection(&input).unwrap(),
            EpisodeProjectionApplyOutcome::Duplicate { .. }
        ));

        let episodes = store.load_episodes("Thomas").unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].id, 17);
        let receipt = store
            .load_episode_source_receipt(projection_subject(), "event-1")
            .unwrap()
            .unwrap();
        assert_eq!(receipt.episode_id, 17);
        let frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        assert_eq!(frontier.last_source_row_id, 1);
        assert_eq!(frontier.applied_count, 1);
    }

    #[test]
    fn episode_projection_integrity_rejects_missing_source_receipt_without_writes() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, dir, input) = committed_projection_store();
        let path = dir.path().join("test-hippocampus.redb");
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS).unwrap();
            receipts
                .remove(source_receipt_key(input.subject, &input.source_event_id).as_str())
                .unwrap();
        }
        write_txn.commit().unwrap();
        drop(store);
        let readonly = HippocampusStore::open_readonly(path.to_str().unwrap()).unwrap();
        assert!(readonly
            .load_episode_projection_readiness(input.subject)
            .is_err());
        drop(readonly);
        let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
        assert_integrity_retry_is_no_write(&store, &input);
    }

    #[test]
    fn episode_projection_integrity_rejects_missing_identity_receipt_without_writes() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir, input) = committed_projection_store();
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS).unwrap();
            receipts
                .remove(episode_identity_key(input.subject, input.episode.id).as_str())
                .unwrap();
        }
        write_txn.commit().unwrap();
        assert_integrity_retry_is_no_write(&store, &input);
    }

    #[test]
    fn episode_projection_integrity_rejects_corrupt_source_receipt_without_writes() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir, input) = committed_projection_store();
        let mut corrupt = store
            .load_episode_source_receipt(input.subject, &input.source_event_id)
            .unwrap()
            .unwrap();
        corrupt.request_digest = "ff".repeat(32);
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS).unwrap();
            insert_json(
                &mut receipts,
                &source_receipt_key(input.subject, &input.source_event_id),
                &corrupt,
            )
            .unwrap();
        }
        write_txn.commit().unwrap();
        assert_integrity_retry_is_no_write(&store, &input);
    }

    #[test]
    fn episode_projection_integrity_rejects_corrupt_identity_receipt_without_writes() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir, input) = committed_projection_store();
        let mut corrupt = store
            .load_episode_source_receipt(input.subject, &input.source_event_id)
            .unwrap()
            .unwrap();
        corrupt.request_digest = "ff".repeat(32);
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS).unwrap();
            insert_json(
                &mut receipts,
                &episode_identity_key(input.subject, input.episode.id),
                &corrupt,
            )
            .unwrap();
        }
        write_txn.commit().unwrap();
        assert_integrity_retry_is_no_write(&store, &input);
    }

    #[test]
    fn episode_projection_integrity_rejects_frontier_last_receipt_mismatch_without_writes() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir, input) = committed_projection_store();
        let mut frontier = store
            .load_episode_projection_frontier(input.subject)
            .unwrap()
            .unwrap();
        frontier.last_source_event_id = Some("different-event".to_string());
        overwrite_projection_frontier(&store, input.subject, &frontier);
        assert_integrity_retry_is_no_write(&store, &input);
    }

    #[test]
    fn episode_projection_integrity_rejects_missing_retained_episode_without_writes() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir, input) = committed_projection_store();
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut episodes = write_txn.open_table(EPISODES).unwrap();
            episodes.remove(input.agent_name.as_str()).unwrap();
        }
        write_txn.commit().unwrap();
        assert_integrity_retry_is_no_write(&store, &input);
    }

    #[test]
    fn duplicate_source_event_at_later_row_advances_without_second_episode() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let original = projection_write(1, "event-1", 17, 0);
        store.commit_episode_projection(&original).unwrap();

        let mut duplicate = original.clone();
        duplicate.source_row_id = 3;
        duplicate.expected_global_frontier = 1;
        let outcome = store.commit_episode_projection(&duplicate).unwrap();

        assert!(matches!(
            outcome,
            EpisodeProjectionApplyOutcome::Duplicate { .. }
        ));
        assert_eq!(store.load_episodes("Thomas").unwrap().len(), 1);
        let control = store.load_episode_projection_control().unwrap().unwrap();
        let frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        assert_eq!(control.last_source_row_id, 3);
        assert_eq!(frontier.last_source_row_id, 1);
        assert_eq!(frontier.applied_count, 1);
        assert_eq!(
            store
                .load_episode_source_receipt(projection_subject(), "event-1")
                .unwrap()
                .unwrap()
                .source_row_id,
            1
        );

        let mut conflicting = duplicate;
        conflicting.source_row_id = 4;
        conflicting.expected_global_frontier = 3;
        conflicting.request_digest = "ff".repeat(32);
        let error = store.commit_episode_projection(&conflicting).unwrap_err();
        assert!(error.to_string().contains("receipt replay conflict"));
        assert_eq!(
            store
                .load_episode_projection_control()
                .unwrap()
                .unwrap()
                .last_source_row_id,
            3
        );
    }

    #[test]
    fn newly_registered_agent_starts_at_committed_global_cursor() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        store
            .advance_episode_projection(&EpisodeProjectionAdvance {
                source_event_id: "irrelevant-5".to_string(),
                source_row_id: 5,
                projection_version: EPISODE_PROJECTION_VERSION,
                request_digest: "ab".repeat(32),
                expected_global_frontier: 0,
            })
            .unwrap();

        let kevin = EpisodeProjectionAgent {
            subject: EpisodeProjectionSubject::Agent {
                agent_id: AgentId(2),
            },
            agent_name: "Kevin".to_string(),
        };
        let frontier = store.initialize_episode_projection_agent(&kevin).unwrap();
        assert_eq!(frontier.subject, kevin.subject);
        assert_eq!(frontier.last_source_row_id, 5);
        assert_eq!(
            frontier.last_source_event_id.as_deref(),
            Some("irrelevant-5")
        );
        assert_eq!(frontier.applied_count, 0);
    }

    #[test]
    fn episode_projection_rejects_subject_and_bucket_aliases_atomically() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let control_before = store.load_episode_projection_control().unwrap();
        let frontiers_before = store.list_episode_projection_frontiers().unwrap();

        let duplicate_bucket = EpisodeProjectionAgent {
            subject: EpisodeProjectionSubject::Agent {
                agent_id: AgentId(2),
            },
            agent_name: "Thomas".to_string(),
        };
        let error = store
            .initialize_episode_projection_agent(&duplicate_bucket)
            .unwrap_err();
        assert!(error.to_string().contains("already bound"));

        let renamed_subject = EpisodeProjectionAgent {
            subject: projection_subject(),
            agent_name: "Renamed Thomas".to_string(),
        };
        let error = store
            .initialize_episode_projection_agent(&renamed_subject)
            .unwrap_err();
        assert!(error.to_string().contains("immutable"));

        assert_eq!(
            store.load_episode_projection_control().unwrap(),
            control_before
        );
        assert_eq!(
            store.list_episode_projection_frontiers().unwrap(),
            frontiers_before
        );
        assert!(store
            .load_episode_projection_frontier(duplicate_bucket.subject)
            .unwrap()
            .is_none());
    }

    #[test]
    fn episode_projection_write_failure_rolls_back_all_records() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, dir) = temp_store();
        let path = dir.path().join("test-hippocampus.redb");
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        FAIL_AFTER_EPISODE_WRITE.store(true, Ordering::SeqCst);

        let input = projection_write(1, "event-fail", 18, 0);
        let error = store.commit_episode_projection(&input).unwrap_err();
        assert!(error.to_string().contains("injected"));
        drop(store);

        let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
        assert!(store.load_episodes("Thomas").unwrap().is_empty());
        assert!(store
            .load_episode_source_receipt(projection_subject(), "event-fail")
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .load_episode_projection_control()
                .unwrap()
                .unwrap()
                .last_source_row_id,
            0
        );
        assert_eq!(
            store
                .load_episode_projection_frontier(projection_subject())
                .unwrap()
                .unwrap()
                .last_source_row_id,
            0
        );

        assert!(matches!(
            store.commit_episode_projection(&input).unwrap(),
            EpisodeProjectionApplyOutcome::Applied { .. }
        ));
        assert_eq!(store.load_episodes("Thomas").unwrap().len(), 1);
        assert!(store
            .load_episode_source_receipt(projection_subject(), "event-fail")
            .unwrap()
            .is_some());
        let control = store.load_episode_projection_control().unwrap().unwrap();
        let frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        assert_eq!(control.last_source_row_id, 1);
        assert_eq!(frontier.last_source_row_id, 1);
        assert_eq!(frontier.applied_count, 1);
    }

    #[test]
    fn episode_projection_rejects_reordered_and_stale_writers() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        store
            .commit_episode_projection(&projection_write(2, "event-2", 19, 0))
            .unwrap();

        let reordered = store
            .commit_episode_projection(&projection_write(1, "event-1", 20, 2))
            .unwrap_err();
        assert!(reordered.to_string().contains("out-of-order"));
        let stale = store
            .commit_episode_projection(&projection_write(3, "event-3", 21, 0))
            .unwrap_err();
        assert!(stale.to_string().contains("frontier conflict"));
        assert_eq!(store.load_episodes("Thomas").unwrap().len(), 1);
    }

    #[test]
    fn episode_projection_quarantine_advances_only_global_cursor() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let quarantine = EpisodeProjectionQuarantine {
            affected_subject: Some(projection_subject()),
            source_event_id: "poison-1".to_string(),
            source_row_id: 1,
            event_type: "agent_action_received".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "cd".repeat(32),
            reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
            diagnostic_digest: "de".repeat(32),
        };

        let control = store.quarantine_episode_projection(&quarantine, 0).unwrap();
        assert_eq!(control.last_source_row_id, 1);
        assert_eq!(
            store
                .load_episode_projection_frontier(projection_subject())
                .unwrap()
                .unwrap()
                .last_source_row_id,
            0
        );
        assert_eq!(
            store.list_episode_projection_quarantine().unwrap(),
            vec![quarantine.clone()]
        );
        assert_eq!(
            store
                .quarantine_episode_projection(&quarantine, 0)
                .unwrap()
                .last_source_row_id,
            1
        );
    }

    #[test]
    fn episode_projection_readiness_scopes_agent_and_global_quarantines() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        let kevin = EpisodeProjectionAgent {
            subject: EpisodeProjectionSubject::Agent {
                agent_id: AgentId(2),
            },
            agent_name: "Kevin".to_string(),
        };
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent(), kevin.clone()],
            )
            .unwrap();
        assert!(store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
        assert!(store
            .load_episode_projection_readiness(kevin.subject)
            .unwrap()
            .is_ready());

        let agent_quarantine = EpisodeProjectionQuarantine {
            affected_subject: Some(projection_subject()),
            source_event_id: "agent-poison".to_string(),
            source_row_id: 1,
            event_type: "agent_action_received".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "ab".repeat(32),
            reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
            diagnostic_digest: "de".repeat(32),
        };
        store
            .quarantine_episode_projection(&agent_quarantine, 0)
            .unwrap();
        let thomas_readiness = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap();
        assert!(!thomas_readiness.is_ready());
        assert!(thomas_readiness.blockers.iter().any(|block| matches!(
            block,
            EpisodeProjectionReadinessBlock::SubjectQuarantine { .. }
        )));
        assert!(store
            .load_episode_projection_readiness(kevin.subject)
            .unwrap()
            .is_ready());

        let unresolved = EpisodeProjectionQuarantine {
            affected_subject: None,
            source_event_id: "unresolved-poison".to_string(),
            source_row_id: 2,
            event_type: "agent_action_received".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "bc".repeat(32),
            reason: EpisodeProjectionQuarantineReason::UnknownAgent,
            diagnostic_digest: "ef".repeat(32),
        };
        store
            .quarantine_episode_projection(&unresolved, 1)
            .unwrap();
        let building = EpisodeProjectionQuarantine {
            affected_subject: Some(EpisodeProjectionSubject::Building),
            source_event_id: "building-poison".to_string(),
            source_row_id: 3,
            event_type: "chaos_triggered".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "cd".repeat(32),
            reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
            diagnostic_digest: "fa".repeat(32),
        };
        store
            .quarantine_episode_projection(&building, 2)
            .unwrap();

        for subject in [projection_subject(), kevin.subject] {
            let readiness = store.load_episode_projection_readiness(subject).unwrap();
            assert!(!readiness.is_ready());
            assert_eq!(
                readiness
                    .blockers
                    .iter()
                    .filter(|block| matches!(
                        block,
                        EpisodeProjectionReadinessBlock::GlobalQuarantine { .. }
                    ))
                    .count(),
                2
            );
        }
        for quarantine in [agent_quarantine, unresolved, building] {
            let encoded = serde_json::to_value(&quarantine).unwrap();
            let fields = encoded.as_object().unwrap();
            assert!(!fields.contains_key("payload"));
            assert!(!fields.contains_key("diagnostic"));
            assert!(fields.contains_key("diagnostic_digest"));
        }
    }

    #[test]
    fn episode_projection_readiness_rejects_tampered_contracts_after_reopen() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();

        let (store, dir) = temp_store();
        let path = dir.path().join("test-hippocampus.redb");
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let mut wrong_subject = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        wrong_subject.subject = EpisodeProjectionSubject::Agent {
            agent_id: AgentId(2),
        };
        overwrite_projection_frontier(&store, projection_subject(), &wrong_subject);
        drop(store);
        let readonly = HippocampusStore::open_readonly(path.to_str().unwrap()).unwrap();
        let error = readonly
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("key/value subject mismatch"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let mut control = store.load_episode_projection_control().unwrap().unwrap();
        control.projection_version += 1;
        overwrite_projection_control(&store, &control);
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("control version"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let mut control = store.load_episode_projection_control().unwrap().unwrap();
        control.start_policy = EpisodeProjectionStartPolicy::RecoveryCut {
            source_row_id: 0,
            proof_digest: "invalid".to_string(),
        };
        overwrite_projection_control(&store, &control);
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("SHA-256 proof digest"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::RecoveryCut {
                    source_row_id: 41,
                    proof_digest: "ab".repeat(32),
                },
                &[projection_agent()],
            )
            .unwrap();
        let mut control = store.load_episode_projection_control().unwrap().unwrap();
        control.last_source_row_id = 40;
        overwrite_projection_control(&store, &control);
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("control cursor"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let mut frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        frontier.projection_version += 1;
        overwrite_projection_frontier(&store, projection_subject(), &frontier);
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("version mismatch"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let mut frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        frontier.start_policy = EpisodeProjectionStartPolicy::ExplicitPosition {
            source_row_id: 0,
        };
        overwrite_projection_frontier(&store, projection_subject(), &frontier);
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("policy mismatch"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::RecoveryCut {
                    source_row_id: 41,
                    proof_digest: "ab".repeat(32),
                },
                &[projection_agent()],
            )
            .unwrap();
        let mut frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        frontier.last_source_row_id = 40;
        overwrite_projection_frontier(&store, projection_subject(), &frontier);
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("before its start policy"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let mut frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        frontier.last_source_row_id = 1;
        overwrite_projection_frontier(&store, projection_subject(), &frontier);
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("ahead of global control"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let mut frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        frontier.applied_count = 1;
        frontier.last_request_digest = Some("ab".repeat(32));
        overwrite_projection_frontier(&store, projection_subject(), &frontier);
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("applied count"));
    }

    #[test]
    fn episode_projection_readiness_rejects_tampered_quarantine_records() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        store
            .advance_episode_projection(&EpisodeProjectionAdvance {
                source_event_id: "row-1".to_string(),
                source_row_id: 1,
                projection_version: EPISODE_PROJECTION_VERSION,
                request_digest: "ab".repeat(32),
                expected_global_frontier: 0,
            })
            .unwrap();
        let orphan_subject_quarantine = EpisodeProjectionQuarantine {
            affected_subject: Some(EpisodeProjectionSubject::Agent {
                agent_id: AgentId(2),
            }),
            source_event_id: "orphan-subject".to_string(),
            source_row_id: 1,
            event_type: "agent_action_received".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "bc".repeat(32),
            reason: EpisodeProjectionQuarantineReason::UnknownAgent,
            diagnostic_digest: "cd".repeat(32),
        };
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut quarantine = write_txn.open_table(EPISODE_QUARANTINE).unwrap();
            insert_json(
                &mut quarantine,
                &quarantine_key(1, "orphan-subject"),
                &orphan_subject_quarantine,
            )
            .unwrap();
        }
        write_txn.commit().unwrap();
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("no durable subject frontier"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        store
            .advance_episode_projection(&EpisodeProjectionAdvance {
                source_event_id: "row-1".to_string(),
                source_row_id: 1,
                projection_version: EPISODE_PROJECTION_VERSION,
                request_digest: "ab".repeat(32),
                expected_global_frontier: 0,
            })
            .unwrap();
        let invalid_quarantine = EpisodeProjectionQuarantine {
            affected_subject: None,
            source_event_id: "invalid-digest".to_string(),
            source_row_id: 1,
            event_type: "agent_action_received".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "not-a-digest".to_string(),
            reason: EpisodeProjectionQuarantineReason::UnknownAgent,
            diagnostic_digest: "cd".repeat(32),
        };
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut quarantine = write_txn.open_table(EPISODE_QUARANTINE).unwrap();
            insert_json(
                &mut quarantine,
                &quarantine_key(1, "invalid-digest"),
                &invalid_quarantine,
            )
            .unwrap();
        }
        write_txn.commit().unwrap();
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("request digest"));
    }

    #[test]
    fn episode_projection_atomic_path_enforces_retention_bound() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let existing: Vec<Episode> = (1..=MAX_EPISODES_PER_AGENT as u64)
            .map(|id| make_episode(id, &format!("episode-{id}")))
            .collect();
        store.store_episodes("Thomas", &existing).unwrap();
        store
            .commit_episode_projection(&projection_write(1, "event-new", 1001, 0))
            .unwrap();

        let retained = store.load_episodes("Thomas").unwrap();
        assert_eq!(retained.len(), MAX_EPISODES_PER_AGENT);
        assert_eq!(retained.first().unwrap().id, 2);
        assert_eq!(retained.last().unwrap().id, 1001);
    }

    #[test]
    fn concurrent_same_agent_writers_commit_one_episode() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let store = Arc::new(store);
        let input = projection_write(1, "event-concurrent", 22, 0);
        let mut threads = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            let input = input.clone();
            threads.push(std::thread::spawn(move || {
                store.commit_episode_projection(&input).unwrap()
            }));
        }
        let outcomes: Vec<EpisodeProjectionApplyOutcome> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();

        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, EpisodeProjectionApplyOutcome::Applied { .. }))
                .count(),
            1
        );
        assert_eq!(store.load_episodes("Thomas").unwrap().len(), 1);
        assert_eq!(
            store
                .load_episode_projection_frontier(projection_subject())
                .unwrap()
                .unwrap()
                .applied_count,
            1
        );
    }

    #[test]
    fn test_episode_store_load_roundtrip() {
        let (store, _dir) = temp_store();
        let episodes = vec![
            make_episode(1, "Wichtiges Meeting"),
            make_episode(2, "Kundengespraech"),
        ];

        store.store_episodes("Thomas", &episodes).unwrap();
        let loaded = store.load_episodes("Thomas").unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].summary, "Wichtiges Meeting");
        assert_eq!(loaded[1].summary, "Kundengespraech");
        assert_eq!(loaded[0].participants, vec!["Lisa"]);
    }

    #[test]
    fn test_episode_append() {
        let (store, _dir) = temp_store();
        store
            .store_episodes("Thomas", &[make_episode(1, "Erstes")])
            .unwrap();
        store
            .append_episodes("Thomas", &[make_episode(2, "Zweites")])
            .unwrap();

        let loaded = store.load_episodes("Thomas").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].summary, "Erstes");
        assert_eq!(loaded[1].summary, "Zweites");
    }

    #[test]
    fn test_live_episodes_cap_at_1000() {
        let (store, _dir) = temp_store();
        let many: Vec<Episode> = (0..1100)
            .map(|i| make_episode(i, &format!("Episode {i}")))
            .collect();

        store.append_episodes("Thomas", &many).unwrap();
        let loaded = store.load_episodes("Thomas").unwrap();
        assert_eq!(loaded.len(), 1000);
        assert_eq!(loaded[0].summary, "Episode 100");
        assert_eq!(loaded[999].summary, "Episode 1099");
    }

    #[test]
    fn test_episode_clear() {
        let (store, _dir) = temp_store();
        store
            .store_episodes("Thomas", &[make_episode(1, "Test")])
            .unwrap();
        store.clear_episodes("Thomas").unwrap();

        let loaded = store.load_episodes("Thomas").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_load_nonexistent_episodes() {
        let (store, _dir) = temp_store();
        let loaded = store.load_episodes("Nobody").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_fact_store_crud() {
        let (store, _dir) = temp_store();

        // Store
        store
            .store_fact("facts/projects/aurora", "Projekt Aurora: Webseite Redesign")
            .unwrap();

        // Load
        let fact = store.load_fact("facts/projects/aurora").unwrap();
        assert_eq!(fact.unwrap(), "Projekt Aurora: Webseite Redesign");

        // Load nonexistent
        assert!(store.load_fact("nonexistent").unwrap().is_none());

        // Delete
        assert!(store.delete_fact("facts/projects/aurora").unwrap());
        assert!(store.load_fact("facts/projects/aurora").unwrap().is_none());
        assert!(!store.delete_fact("facts/projects/aurora").unwrap());
    }

    #[test]
    fn test_redb_fact_store_trait() {
        let (store, _dir) = temp_store();
        store
            .store_fact("facts/hr/vacation", "30 Tage pro Jahr")
            .unwrap();

        let fact_store = RedbFactStore::new(&store);
        let result = fact_store.get_fact("facts/hr/vacation").unwrap();
        assert_eq!(result.unwrap(), "30 Tage pro Jahr");

        assert!(fact_store.get_fact("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_narrative_persistence() {
        let (store, _dir) = temp_store();
        let state = NarrativeState {
            agent_name: "Thomas".to_string(),
            summary: "- Wichtiges Meeting (Score: 0.56)".to_string(),
            episode_count: 3,
        };

        store.store_narrative("Thomas", &state).unwrap();
        let loaded = store.load_narrative("Thomas").unwrap().unwrap();

        assert_eq!(loaded.agent_name, "Thomas");
        assert!(loaded.summary.contains("Wichtiges Meeting"));
        assert_eq!(loaded.episode_count, 3);

        // Nonexistent
        assert!(store.load_narrative("Nobody").unwrap().is_none());
    }

    #[test]
    fn test_cache_state_persistence() {
        let (store, _dir) = temp_store();

        store.store_cache_state("Thomas", true).unwrap();
        assert_eq!(store.load_cache_state("Thomas").unwrap(), Some(true));

        store.store_cache_state("Thomas", false).unwrap();
        assert_eq!(store.load_cache_state("Thomas").unwrap(), Some(false));

        assert!(store.load_cache_state("Nobody").unwrap().is_none());
    }

    #[test]
    fn test_list_agents_with_episodes() {
        let (store, _dir) = temp_store();
        store
            .store_episodes("Thomas", &[make_episode(1, "A")])
            .unwrap();
        store
            .store_episodes("Lisa", &[make_episode(2, "B")])
            .unwrap();
        store
            .store_episodes("Andreas", &[make_episode(3, "C")])
            .unwrap();

        let mut agents = store.list_agents_with_episodes().unwrap();
        agents.sort();
        assert_eq!(agents, vec!["Andreas", "Lisa", "Thomas"]);
    }

    #[test]
    fn test_data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persist-test.redb");
        let path_str = path.to_str().unwrap();

        // Write data
        {
            let store = HippocampusStore::open(path_str).unwrap();
            store
                .store_episodes("Thomas", &[make_episode(1, "Survivor")])
                .unwrap();
            store.store_fact("facts/test", "Persistent Value").unwrap();
            store
                .store_narrative(
                    "Thomas",
                    &NarrativeState {
                        agent_name: "Thomas".to_string(),
                        summary: "Survived".to_string(),
                        episode_count: 1,
                    },
                )
                .unwrap();
            store.store_cache_state("Thomas", true).unwrap();
        } // store dropped here

        // Reopen and verify
        {
            let store = HippocampusStore::open(path_str).unwrap();
            let episodes = store.load_episodes("Thomas").unwrap();
            assert_eq!(episodes.len(), 1);
            assert_eq!(episodes[0].summary, "Survivor");

            let fact = store.load_fact("facts/test").unwrap().unwrap();
            assert_eq!(fact, "Persistent Value");

            let narrative = store.load_narrative("Thomas").unwrap().unwrap();
            assert_eq!(narrative.summary, "Survived");

            assert_eq!(store.load_cache_state("Thomas").unwrap(), Some(true));
        }
    }

    // === GOLF Tests ===

    use crate::golf::{GoalStatus, GoalType};

    fn make_goal(id: u64, agent: &str, goal_type: GoalType) -> Goal {
        Goal::new(id, agent, goal_type, "Test goal", 0, None)
    }

    #[test]
    fn test_golf_store_load_roundtrip() {
        let (store, _dir) = temp_store();
        let goals = vec![
            make_goal(1, "Thomas", GoalType::Career),
            make_goal(2, "Thomas", GoalType::Project),
        ];

        store.store_goals("Thomas", &goals).unwrap();
        let loaded = store.load_goals("Thomas").unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, 1);
        assert_eq!(loaded[0].goal_type, GoalType::Career);
        assert_eq!(loaded[1].id, 2);
        assert_eq!(loaded[1].goal_type, GoalType::Project);
    }

    #[test]
    fn test_golf_append() {
        let (store, _dir) = temp_store();
        store
            .store_goals("Thomas", &[make_goal(1, "Thomas", GoalType::Career)])
            .unwrap();
        store
            .append_goals("Thomas", &[make_goal(2, "Thomas", GoalType::Skill)])
            .unwrap();

        let loaded = store.load_goals("Thomas").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].goal_type, GoalType::Career);
        assert_eq!(loaded[1].goal_type, GoalType::Skill);
    }

    #[test]
    fn test_golf_load_nonexistent() {
        let (store, _dir) = temp_store();
        let loaded = store.load_goals("Nobody").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_golf_update_progress() {
        let (store, _dir) = temp_store();
        store
            .store_goals(
                "Thomas",
                &[
                    make_goal(1, "Thomas", GoalType::Career),
                    make_goal(2, "Thomas", GoalType::Project),
                ],
            )
            .unwrap();

        // Update goal 2 progress
        let updated = store.update_goal_progress("Thomas", 2, 0.75, 500).unwrap();
        assert!(updated);

        let loaded = store.load_goals("Thomas").unwrap();
        assert_eq!(loaded[0].progress, 0.0); // goal 1 unchanged
        assert_eq!(loaded[1].progress, 0.75); // goal 2 updated
        assert_eq!(loaded[1].last_updated_tick, 500);
    }

    #[test]
    fn test_golf_update_progress_auto_complete() {
        let (store, _dir) = temp_store();
        store
            .store_goals("Lisa", &[make_goal(1, "Lisa", GoalType::Skill)])
            .unwrap();

        store.update_goal_progress("Lisa", 1, 1.0, 1000).unwrap();

        let loaded = store.load_goals("Lisa").unwrap();
        assert_eq!(loaded[0].progress, 1.0);
        assert_eq!(loaded[0].status, GoalStatus::Completed);
    }

    #[test]
    fn test_golf_update_progress_nonexistent_goal() {
        let (store, _dir) = temp_store();
        store
            .store_goals("Thomas", &[make_goal(1, "Thomas", GoalType::Career)])
            .unwrap();

        let updated = store.update_goal_progress("Thomas", 99, 0.5, 100).unwrap();
        assert!(!updated);
    }

    #[test]
    fn test_golf_list_agents_with_goals() {
        let (store, _dir) = temp_store();
        store
            .store_goals("Thomas", &[make_goal(1, "Thomas", GoalType::Career)])
            .unwrap();
        store
            .store_goals("Lisa", &[make_goal(2, "Lisa", GoalType::Skill)])
            .unwrap();

        let mut agents = store.list_agents_with_goals().unwrap();
        agents.sort();
        assert_eq!(agents, vec!["Lisa", "Thomas"]);
    }

    #[test]
    fn test_golf_data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("golf-persist.redb");
        let path_str = path.to_str().unwrap();

        // Write
        {
            let store = HippocampusStore::open(path_str).unwrap();
            let mut goal = make_goal(1, "Thomas", GoalType::Career);
            goal.update_progress(0.42, 100);
            store.store_goals("Thomas", &[goal]).unwrap();
        }

        // Reopen and verify
        {
            let store = HippocampusStore::open(path_str).unwrap();
            let loaded = store.load_goals("Thomas").unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].progress, 0.42);
            assert_eq!(loaded[0].goal_type, GoalType::Career);
            assert_eq!(loaded[0].last_updated_tick, 100);
        }
    }

    #[test]
    fn test_golf_integrity_no_empty_agent() {
        // Goal struct requires agent_name — empty string is technically valid
        // but we test that the struct enforces non-optional agent_name
        let goal = make_goal(1, "Thomas", GoalType::Career);
        assert!(!goal.agent_name.is_empty());
    }

    // === ARCHIVE Tests ===

    #[test]
    fn test_archive_store_load_roundtrip() {
        let (store, _dir) = temp_store();
        let episodes = vec![
            make_episode(1, "Konsolidiert A"),
            make_episode(2, "Konsolidiert B"),
        ];

        store.store_archive("Thomas", &episodes).unwrap();
        let loaded = store.load_archive("Thomas").unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].summary, "Konsolidiert A");
        assert_eq!(loaded[1].summary, "Konsolidiert B");
    }

    #[test]
    fn test_archive_append() {
        let (store, _dir) = temp_store();
        store
            .store_archive("Thomas", &[make_episode(1, "Erste Konsolidierung")])
            .unwrap();
        store
            .append_archive("Thomas", &[make_episode(2, "Zweite Konsolidierung")])
            .unwrap();

        let loaded = store.load_archive("Thomas").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].summary, "Erste Konsolidierung");
        assert_eq!(loaded[1].summary, "Zweite Konsolidierung");
    }

    #[test]
    fn test_archive_load_nonexistent() {
        let (store, _dir) = temp_store();
        let loaded = store.load_archive("Nobody").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_archive_caps_at_1000() {
        let (store, _dir) = temp_store();
        let many: Vec<Episode> = (0..1100)
            .map(|i| make_episode(i, &format!("Episode {i}")))
            .collect();

        store.append_archive("Thomas", &many).unwrap();
        let loaded = store.load_archive("Thomas").unwrap();
        assert_eq!(loaded.len(), 1000);
        // Oldest should be pruned, newest kept
        assert_eq!(loaded[0].summary, "Episode 100");
        assert_eq!(loaded[999].summary, "Episode 1099");
    }

    #[test]
    fn test_archive_data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive-persist.redb");
        let path_str = path.to_str().unwrap();

        {
            let store = HippocampusStore::open(path_str).unwrap();
            store
                .store_archive("Thomas", &[make_episode(1, "Archived")])
                .unwrap();
        }

        {
            let store = HippocampusStore::open(path_str).unwrap();
            let loaded = store.load_archive("Thomas").unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].summary, "Archived");
        }
    }

    #[test]
    fn test_archive_list_agents() {
        let (store, _dir) = temp_store();
        store
            .store_archive("Thomas", &[make_episode(1, "A")])
            .unwrap();
        store
            .store_archive("Lisa", &[make_episode(2, "B")])
            .unwrap();

        let mut agents = store.list_agents_with_archive().unwrap();
        agents.sort();
        assert_eq!(agents, vec!["Lisa", "Thomas"]);
    }

    #[test]
    fn test_readonly_store_loads_existing_memory_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readonly-hippocampus.redb");
        let path_str = path.to_str().unwrap();

        {
            let store = HippocampusStore::open(path_str).unwrap();
            store
                .initialize_episode_projection(
                    &EpisodeProjectionStartPolicy::Beginning,
                    &[projection_agent()],
                )
                .unwrap();
            store
                .store_episodes("Thomas", &[make_episode(1, "Read episode")])
                .unwrap();
            store
                .store_narrative(
                    "Thomas",
                    &NarrativeState {
                        agent_name: "Thomas".to_string(),
                        summary: "Read narrative".to_string(),
                        episode_count: 1,
                    },
                )
                .unwrap();
            store.store_fact("facts/projects/aurora", "Aurora").unwrap();
            store
                .store_archive("Thomas", &[make_episode(2, "Archived read")])
                .unwrap();
            store
                .commit_episode_projection(&projection_write(1, "readonly-event", 3, 0))
                .unwrap();
            store
                .quarantine_episode_projection(
                    &EpisodeProjectionQuarantine {
                        affected_subject: None,
                        source_event_id: "readonly-poison".to_string(),
                        source_row_id: 2,
                        event_type: "agent_action_received".to_string(),
                        projection_version: EPISODE_PROJECTION_VERSION,
                        request_digest: "cd".repeat(32),
                        reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
                        diagnostic_digest: "de".repeat(32),
                    },
                    1,
                )
                .unwrap();
        }

        let readonly = HippocampusStore::open_readonly(path_str).unwrap();
        assert_eq!(readonly.load_episodes("Thomas").unwrap().len(), 2);
        assert_eq!(
            readonly.load_narrative("Thomas").unwrap().unwrap().summary,
            "Read narrative"
        );
        assert_eq!(
            readonly
                .load_fact("facts/projects/aurora")
                .unwrap()
                .as_deref(),
            Some("Aurora")
        );
        assert_eq!(readonly.load_archive("Thomas").unwrap().len(), 1);
        let control = readonly
            .load_episode_projection_control()
            .unwrap()
            .unwrap();
        assert_eq!(control.last_source_row_id, 2);
        let frontier = readonly
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        assert_eq!(frontier.subject, projection_subject());
        assert_eq!(frontier.last_source_row_id, 1);
        let receipt = readonly
            .load_episode_source_receipt(projection_subject(), "readonly-event")
            .unwrap()
            .unwrap();
        assert_eq!(receipt.subject, projection_subject());
        assert_eq!(receipt.episode_id, 3);
        let quarantines = readonly.list_episode_projection_quarantine().unwrap();
        assert_eq!(quarantines.len(), 1);
        assert_eq!(quarantines[0].source_event_id, "readonly-poison");
        assert_eq!(
            quarantines[0].reason,
            EpisodeProjectionQuarantineReason::MalformedRelevantPayload
        );
        let readiness = readonly
            .load_episode_projection_readiness(projection_subject())
            .unwrap();
        assert!(!readiness.is_ready());
        assert!(readiness.blockers.iter().any(|block| matches!(
            block,
            EpisodeProjectionReadinessBlock::GlobalQuarantine { .. }
        )));
    }
}
