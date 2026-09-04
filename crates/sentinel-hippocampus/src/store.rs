//! Persistent storage for hippocampus memory data via redb.
//!
//! Separate database file (`hippocampus.redb`) from the main StateStore.
//! 10 tables: episodes, narratives, facts, cache_state, goals, archive, and the
//! episode projection control, receipt, quarantine, and generation tables.

use redb::{Database, ReadOnlyDatabase, ReadableDatabase, ReadableTable, TableDefinition};
use sentinel_common::types::AgentId;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::sync::atomic::{AtomicU8, Ordering};

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
const EPISODE_PROJECTION_GENERATIONS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("episode_projection_generations");

const MAX_EPISODES_PER_AGENT: usize = 1000;
/// Maximum number of live source episodes retained by one projection subject.
pub const EPISODE_PROJECTION_MAX_LIVE_EPISODES_PER_SUBJECT: usize = MAX_EPISODES_PER_AGENT;
/// Canonical v1 simulation-tick duration used by durable episode effects.
pub const EPISODE_PROJECTION_TICK_DURATION_MILLIS: u64 = 1000;
const EPISODE_PROJECTION_CONTROL_KEY: &str = "episode-producer-v1";
const EPISODE_PROJECTION_CUTOVER_KEY: &str = "episode-producer-cutover-v1";
const EPISODE_PROJECTION_GENERATION_CONTROL_KEY: &str = "generation-control-v1";
const KEY_SEPARATOR: char = '\u{1f}';

#[cfg(test)]
static EPISODE_PROJECTION_FAULT_STAGE: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
thread_local! {
    static GENERATION_SNAPSHOT_CAPTURES: Cell<usize> = const { Cell::new(0) };
    static GENERATION_RECORD_DEEP_VALIDATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
enum EpisodeProjectionFaultStage {
    AfterEpisode = 1,
    AfterSourceReceipt = 2,
    AfterIdentityReceipt = 3,
    AfterFrontier = 4,
    AfterControl = 5,
    AfterQuarantineRemoval = 6,
    BeforeCommit = 7,
}

#[cfg(test)]
fn inject_episode_projection_fault(stage: EpisodeProjectionFaultStage) -> anyhow::Result<()> {
    if EPISODE_PROJECTION_FAULT_STAGE
        .compare_exchange(stage as u8, 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        anyhow::bail!("injected episode projection fault at {stage:?}");
    }
    Ok(())
}

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

/// Persisted proof for the one authorized legacy-to-projection cutover.
///
/// The daemon validates the exact legacy-store and EventStore cut digests and
/// authenticates `authorization_digest` before this receipt can be committed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionCutoverReceipt {
    pub projection_version: u32,
    pub source_row_id: i64,
    pub legacy_state_digest: String,
    pub source_cut_digest: String,
    pub authorization_digest: String,
}

impl EpisodeProjectionStartPolicy {
    /// Fixed first excluded EventStore row for this projection lineage.
    pub fn source_row_id(&self) -> i64 {
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
    /// Monotonic maximum source tick observed since the fixed start cut.
    /// This makes effect age independent of batch partitioning and restarts.
    #[serde(default)]
    pub effect_reference_tick: u64,
}

/// Stable authority subject for an episode projection. Display names are not
/// projection identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    pub effect_reference_tick: u64,
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
    pub effect_reference_tick: u64,
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
    BlockedByEarlierQuarantine,
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
    pub effect_reference_tick: u64,
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
    GenerationTransition {
        generation_id: String,
        phase: EpisodeProjectionGenerationPhase,
    },
}

/// Read-only readiness material for later orchestrator wiring.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionReadiness {
    pub subject: EpisodeProjectionSubject,
    pub frontier: Option<EpisodeProjectionFrontier>,
    pub blockers: Vec<EpisodeProjectionReadinessBlock>,
}

/// CAS material supplied when an operator retries one immutable source row.
#[derive(Debug, Clone)]
pub struct EpisodeProjectionResolution {
    pub quarantine: EpisodeProjectionQuarantine,
    pub write: EpisodeProjectionWrite,
}

/// Typed admission decision for a source row before projection work starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpisodeProjectionAdmission {
    Allowed,
    SubjectBlocked(EpisodeProjectionQuarantine),
    GloballyBlocked(EpisodeProjectionQuarantine),
}

/// Immutable identity and source cut for one episode projection generation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionGenerationDescriptor {
    pub generation_id: String,
    pub parent_generation_id: Option<String>,
    pub projection_version: u32,
    pub source_cut: EpisodeProjectionSourceCutCoverage,
    /// CAS over archive material owned by consolidation, not by this projection.
    pub archive_snapshot_digest: String,
}

/// Intrinsic classification of one immutable EventStore row. Execution-only
/// blocking is not a source classification and must be resolved before a
/// complete candidate can activate.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EpisodeProjectionSourceClassification {
    Irrelevant,
    Episode {
        subject: EpisodeProjectionSubject,
    },
    Quarantined {
        affected_subject: Option<EpisodeProjectionSubject>,
        reason: EpisodeProjectionQuarantineReason,
    },
}

/// Payload-free canonical evidence for one EventStore row in a source cut.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionSourceCoverageEntry {
    pub source_row_id: i64,
    pub source_event_id: String,
    pub source_tick: u64,
    /// Prefix maximum of `source_tick` from the fixed generation start cut.
    pub effect_reference_tick: u64,
    pub request_digest: String,
    pub classification: EpisodeProjectionSourceClassification,
}

/// Versioned authoritative EventStore coverage for one generation candidate.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionSourceCutCoverage {
    pub schema_version: u32,
    pub replay_clock_version: u32,
    pub reference_tick: u64,
    pub tick_duration_millis: u64,
    pub from_exclusive_source_row_id: i64,
    pub through_source_row_id: i64,
    pub event_count: u64,
    pub episode_count: u64,
    pub irrelevant_count: u64,
    pub quarantine_count: u64,
    pub coverage_digest: String,
}

/// Independently computed EventStore evidence supplied by the production
/// owner to every generation mutation. Candidate-provided digests alone are
/// never authoritative.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionSourceCutEvidence {
    pub coverage: EpisodeProjectionSourceCutCoverage,
    pub entries: Vec<EpisodeProjectionSourceCoverageEntry>,
}

/// One subject's complete candidate material and deterministic coverage proof.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionGenerationSubject {
    pub agent: EpisodeProjectionAgent,
    pub frontier: EpisodeProjectionFrontier,
    pub receipts: Vec<EpisodeSourceReceipt>,
    pub live_episodes: Vec<Episode>,
    pub archived_episodes: Vec<Episode>,
    pub coverage_digest: String,
}

/// Complete candidate snapshot staged before validation and CAS activation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionGenerationCandidate {
    pub descriptor: EpisodeProjectionGenerationDescriptor,
    pub control: EpisodeProjectionControl,
    pub subjects: Vec<EpisodeProjectionGenerationSubject>,
    pub quarantines: Vec<EpisodeProjectionQuarantine>,
    pub source_coverage: Vec<EpisodeProjectionSourceCoverageEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeProjectionGenerationPhase {
    Building,
    Validated,
    Active,
    Retained,
}

/// Redacted generation lifecycle readback for readiness and operators.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionGenerationInfo {
    pub descriptor: EpisodeProjectionGenerationDescriptor,
    pub phase: EpisodeProjectionGenerationPhase,
    pub candidate_digest: String,
    pub snapshot_source_cut: EpisodeProjectionSourceCutCoverage,
    pub snapshot_archive_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeProjectionGenerationStatus {
    pub active_generation_id: String,
    pub activation_epoch: u64,
    pub generations: Vec<EpisodeProjectionGenerationInfo>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct EpisodeProjectionGenerationRecord {
    descriptor: EpisodeProjectionGenerationDescriptor,
    phase: EpisodeProjectionGenerationPhase,
    candidate_digest: String,
    snapshot: EpisodeProjectionGenerationSnapshot,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct EpisodeProjectionGenerationSnapshot {
    control: EpisodeProjectionControl,
    subjects: Vec<EpisodeProjectionGenerationSubject>,
    quarantines: Vec<EpisodeProjectionQuarantine>,
    source_cut: EpisodeProjectionSourceCutCoverage,
    source_coverage: Vec<EpisodeProjectionSourceCoverageEntry>,
    archive_snapshot_digest: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct EpisodeProjectionGenerationControl {
    active_generation_id: String,
    activation_epoch: u64,
    #[serde(default)]
    record_seals: Vec<EpisodeProjectionGenerationRecordSeal>,
    #[serde(default)]
    seal_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct EpisodeProjectionGenerationRecordSeal {
    generation_id: String,
    phase: EpisodeProjectionGenerationPhase,
    candidate_digest: String,
    encoded_record_digest: String,
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
    let source_prefix = format!(
        "source{KEY_SEPARATOR}{}{KEY_SEPARATOR}",
        subject.storage_key()
    );
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
    pub effect_reference_tick: u64,
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
            write_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
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

    /// Canonical bytes covering every non-projection Hippocampus table.
    ///
    /// The daemon hashes this material while authorizing a one-time legacy
    /// cutover. Table names, keys, and values are length-delimited and iterated
    /// in redb key order; projection-owned tables are deliberately excluded.
    pub fn episode_projection_legacy_state_material(&self) -> anyhow::Result<Vec<u8>> {
        let read_txn = self.db.begin_read()?;
        let mut material = b"sentinel-episode-legacy-state-v1\0".to_vec();
        append_table_material(&read_txn, EPISODES, "episodes", &mut material)?;
        append_table_material(&read_txn, NARRATIVES, "narratives", &mut material)?;
        append_table_material(&read_txn, FACTS, "facts", &mut material)?;
        append_table_material(&read_txn, CACHE_STATE, "cache_state", &mut material)?;
        append_table_material(&read_txn, GOALS, "goals", &mut material)?;
        append_table_material(&read_txn, ARCHIVE, "archive", &mut material)?;
        Ok(material)
    }

    // === EPISODE PROJECTION ===

    /// Initialize the store-owned cursor and durable per-agent frontiers.
    /// Existing state must use the exact same start policy.
    pub fn initialize_episode_projection(
        &self,
        start_policy: &EpisodeProjectionStartPolicy,
        agents: &[EpisodeProjectionAgent],
    ) -> anyhow::Result<EpisodeProjectionControl> {
        self.initialize_episode_projection_inner(start_policy, agents, None)
    }

    /// Initialize from a daemon-authenticated legacy cutover receipt.
    pub fn initialize_episode_projection_cutover(
        &self,
        receipt: &EpisodeProjectionCutoverReceipt,
        agents: &[EpisodeProjectionAgent],
    ) -> anyhow::Result<EpisodeProjectionControl> {
        validate_cutover_receipt(receipt)?;
        let start_policy = EpisodeProjectionStartPolicy::RecoveryCut {
            source_row_id: receipt.source_row_id,
            proof_digest: receipt.authorization_digest.clone(),
        };
        self.initialize_episode_projection_inner(&start_policy, agents, Some(receipt))
    }

    fn initialize_episode_projection_inner(
        &self,
        start_policy: &EpisodeProjectionStartPolicy,
        agents: &[EpisodeProjectionAgent],
        cutover_receipt: Option<&EpisodeProjectionCutoverReceipt>,
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

        let projection_already_initialized = self.load_episode_projection_control()?.is_some();
        let write_txn = self.db.begin_write()?;
        let has_existing_episode_data = !projection_already_initialized && {
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
        let has_generation_state = {
            let generations = write_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
            let mut has_data = false;
            if let Some(entry) = generations.iter()?.next() {
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
                if matches!(
                    key.value(),
                    EPISODE_PROJECTION_CONTROL_KEY | EPISODE_PROJECTION_CUTOVER_KEY
                ) {
                    continue;
                }
                anyhow::ensure!(
                    key.value().starts_with(&frontier_prefix),
                    "unknown episode projection state key"
                );
                let frontier: EpisodeProjectionFrontier = serde_json::from_slice(value.value())?;
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
                    validate_persisted_cutover(&state, &existing)?;
                    existing
                }
                None => {
                    anyhow::ensure!(
                        state_entry_count == 0
                            && !has_source_receipts
                            && !has_quarantine
                            && !has_generation_state,
                        "episode projection cannot initialize over orphaned projection state"
                    );
                    match (start_policy, cutover_receipt) {
                        (EpisodeProjectionStartPolicy::Beginning, None) => anyhow::ensure!(
                            !has_existing_episode_data,
                            "Beginning episode projection requires an empty legacy episode store"
                        ),
                        (EpisodeProjectionStartPolicy::RecoveryCut { .. }, Some(receipt)) => {
                            validate_cutover_binding(start_policy, receipt)?;
                            insert_json(&mut state, EPISODE_PROJECTION_CUTOVER_KEY, receipt)?;
                        }
                        (EpisodeProjectionStartPolicy::ExplicitPosition { .. }, _) => {
                            anyhow::bail!(
                                "explicit episode projection positions require an authenticated cutover contract"
                            );
                        }
                        _ => anyhow::bail!(
                            "episode projection recovery cut requires an authenticated cutover receipt"
                        ),
                    }
                    let created = EpisodeProjectionControl {
                        projection_version: EPISODE_PROJECTION_VERSION,
                        start_policy: start_policy.clone(),
                        last_source_row_id: start_policy.source_row_id(),
                        last_source_event_id: None,
                        effect_reference_tick: 0,
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
        self.ensure_episode_projection_generation(&control)?;
        Ok(control)
    }

    /// Load the persisted one-time cutover receipt, when this store was
    /// initialized over legacy memory.
    pub fn load_episode_projection_cutover_receipt(
        &self,
    ) -> anyhow::Result<Option<EpisodeProjectionCutoverReceipt>> {
        load_json_value(
            &self.db,
            EPISODE_PROJECTION_STATE,
            EPISODE_PROJECTION_CUTOVER_KEY,
        )
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

    /// Validate staged subject/name bindings without mutating projection state.
    pub fn validate_episode_projection_agents(
        &self,
        agents: &[EpisodeProjectionAgent],
    ) -> anyhow::Result<()> {
        let control = self
            .load_episode_projection_control()?
            .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;
        validate_projection_control(&control)?;

        let mut staged_subjects = HashSet::new();
        let mut staged_names = HashSet::new();
        for agent in agents {
            agent.subject.validate()?;
            validate_projection_key_part(&agent.agent_name, "agent name")?;
            anyhow::ensure!(
                staged_subjects.insert(agent.subject),
                "duplicate episode projection subject {}",
                agent.subject.storage_key()
            );
            anyhow::ensure!(
                staged_names.insert(agent.agent_name.as_str()),
                "duplicate episode projection agent name {}",
                agent.agent_name
            );
        }

        let read_txn = self.db.begin_read()?;
        let state = read_txn.open_table(EPISODE_PROJECTION_STATE)?;
        let frontier_prefix = format!("frontier{KEY_SEPARATOR}");
        let mut persisted_subjects = HashSet::new();
        let mut persisted_names = HashSet::new();
        for entry in state.iter()? {
            let (key, value) = entry?;
            if matches!(
                key.value(),
                EPISODE_PROJECTION_CONTROL_KEY | EPISODE_PROJECTION_CUTOVER_KEY
            ) {
                continue;
            }
            anyhow::ensure!(
                key.value().starts_with(&frontier_prefix),
                "unknown episode projection state key"
            );
            let frontier: EpisodeProjectionFrontier = serde_json::from_slice(value.value())?;
            anyhow::ensure!(
                key.value() == projection_frontier_key(frontier.subject).as_str(),
                "episode projection frontier key/value subject mismatch"
            );
            validate_projection_frontier(&frontier, frontier.subject, &control)?;
            anyhow::ensure!(
                persisted_subjects.insert(frontier.subject),
                "duplicate persisted episode projection subject {}",
                frontier.subject.storage_key()
            );
            anyhow::ensure!(
                persisted_names.insert(frontier.agent_name.clone()),
                "duplicate persisted episode bucket name {}",
                frontier.agent_name
            );
            for agent in agents {
                anyhow::ensure!(
                    frontier.subject != agent.subject || frontier.agent_name == agent.agent_name,
                    "episode bucket name is immutable for subject {}",
                    agent.subject.storage_key()
                );
                anyhow::ensure!(
                    frontier.agent_name != agent.agent_name || frontier.subject == agent.subject,
                    "episode bucket name {} is already bound to subject {}",
                    agent.agent_name,
                    frontier.subject.storage_key()
                );
            }
        }
        Ok(())
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

    /// Load and validate every frontier used by runtime admission in one read.
    pub fn list_episode_projection_frontiers_for_admission(
        &self,
    ) -> anyhow::Result<Vec<EpisodeProjectionFrontier>> {
        list_projection_frontiers_for_admission_from(&self.db)
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
        self.commit_episode_projection_inner(input, None)
    }

    /// Resolve one quarantined immutable source row by CAS-binding the exact
    /// durable quarantine record to the normal episode transaction.
    pub fn resolve_episode_projection(
        &self,
        resolution: &EpisodeProjectionResolution,
    ) -> anyhow::Result<EpisodeProjectionApplyOutcome> {
        self.commit_episode_projection_inner(&resolution.write, Some(&resolution.quarantine))
    }

    fn commit_episode_projection_inner(
        &self,
        input: &EpisodeProjectionWrite,
        resolution: Option<&EpisodeProjectionQuarantine>,
    ) -> anyhow::Result<EpisodeProjectionApplyOutcome> {
        validate_projection_write(input)?;
        let receipt_key = source_receipt_key(input.subject, &input.source_event_id);
        let identity_key = episode_identity_key(input.subject, input.episode.id);

        let write_txn = self.db.begin_write()?;
        let mut state = write_txn.open_table(EPISODE_PROJECTION_STATE)?;
        let mut receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS)?;
        let mut episodes = write_txn.open_table(EPISODES)?;
        let archive = write_txn.open_table(ARCHIVE)?;
        let mut quarantine = write_txn.open_table(EPISODE_QUARANTINE)?;

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
        let retained_before: Vec<Episode> = match episodes.get(input.agent_name.as_str())? {
            Some(value) => serde_json::from_slice(value.value())?,
            None => Vec::new(),
        };
        let archived_before: Vec<Episode> = match archive.get(input.agent_name.as_str())? {
            Some(value) => serde_json::from_slice(value.value())?,
            None => Vec::new(),
        };
        validate_frontier_tip_integrity(
            input.subject,
            &frontier,
            &receipts,
            &retained_before,
            &archived_before,
        )?;

        if let Some(existing) = table_json_value::<EpisodeSourceReceipt>(&receipts, &receipt_key)? {
            ensure_exact_receipt_replay(&existing, input)?;
            validate_receipt_pair(&receipts, &existing)?;
            if let Some(record) = resolution {
                validate_resolution_admission(
                    &quarantine,
                    record,
                    input,
                    control.last_source_row_id,
                )?;
                let key = quarantine_key(record.source_row_id, &record.source_event_id);
                quarantine.remove(key.as_str())?;
            }
            drop(quarantine);
            drop(archive);
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

        match resolution {
            Some(record) => validate_resolution_admission(
                &quarantine,
                record,
                input,
                control.last_source_row_id,
            )?,
            None => {
                ensure_projection_admitted(&quarantine, Some(input.subject), input.source_row_id)?;
                ensure_expected_frontier(&control, input.expected_global_frontier)?;
                anyhow::ensure!(
                    input.source_row_id > control.last_source_row_id,
                    "out-of-order episode source row {} is not after global frontier {}",
                    input.source_row_id,
                    control.last_source_row_id
                );
            }
        }
        anyhow::ensure!(
            input.source_row_id > frontier.last_source_row_id,
            "out-of-order episode source row {} is not after agent frontier {}",
            input.source_row_id,
            frontier.last_source_row_id
        );
        if resolution.is_none() {
            anyhow::ensure!(
                input.effect_reference_tick >= control.effect_reference_tick,
                "episode projection effect clock regressed"
            );
        }
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
        inject_episode_projection_fault(EpisodeProjectionFaultStage::AfterEpisode)?;

        let receipt = EpisodeSourceReceipt {
            subject: input.subject,
            agent_name: input.agent_name.clone(),
            source_event_id: input.source_event_id.clone(),
            source_row_id: input.source_row_id,
            projection_version: input.projection_version,
            request_digest: input.request_digest.clone(),
            episode_id: input.episode.id,
            effect_reference_tick: input.effect_reference_tick,
        };
        insert_json(&mut receipts, &receipt_key, &receipt)?;
        #[cfg(test)]
        inject_episode_projection_fault(EpisodeProjectionFaultStage::AfterSourceReceipt)?;
        insert_json(&mut receipts, &identity_key, &receipt)?;
        #[cfg(test)]
        inject_episode_projection_fault(EpisodeProjectionFaultStage::AfterIdentityReceipt)?;

        frontier.last_source_row_id = input.source_row_id;
        frontier.last_source_event_id = Some(input.source_event_id.clone());
        frontier.last_request_digest = Some(input.request_digest.clone());
        frontier.applied_count = frontier.applied_count.saturating_add(1);
        if input.source_row_id > control.last_source_row_id {
            control.last_source_row_id = input.source_row_id;
            control.last_source_event_id = Some(input.source_event_id.clone());
        }
        control.effect_reference_tick = control
            .effect_reference_tick
            .max(input.effect_reference_tick);
        insert_json(&mut state, &frontier_key, &frontier)?;
        #[cfg(test)]
        inject_episode_projection_fault(EpisodeProjectionFaultStage::AfterFrontier)?;
        insert_json(&mut state, EPISODE_PROJECTION_CONTROL_KEY, &control)?;
        #[cfg(test)]
        inject_episode_projection_fault(EpisodeProjectionFaultStage::AfterControl)?;

        if let Some(record) = resolution {
            quarantine
                .remove(quarantine_key(record.source_row_id, &record.source_event_id).as_str())?;
            #[cfg(test)]
            inject_episode_projection_fault(EpisodeProjectionFaultStage::AfterQuarantineRemoval)?;
        }
        validate_receipt_pair(&receipts, &receipt)?;
        validate_frontier_tip_integrity(
            input.subject,
            &frontier,
            &receipts,
            &retained,
            &archived_before,
        )?;
        #[cfg(test)]
        inject_episode_projection_fault(EpisodeProjectionFaultStage::BeforeCommit)?;

        drop(quarantine);
        drop(archive);
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
        let quarantine = write_txn.open_table(EPISODE_QUARANTINE)?;
        let mut control: EpisodeProjectionControl =
            table_json_value(&state, EPISODE_PROJECTION_CONTROL_KEY)?
                .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;

        if source.source_row_id > control.last_source_row_id {
            ensure_projection_admitted(&quarantine, None, source.source_row_id)?;
            anyhow::ensure!(
                control.last_source_row_id == source.expected_global_frontier,
                "episode projection frontier conflict: expected {}, current {}",
                source.expected_global_frontier,
                control.last_source_row_id
            );
            anyhow::ensure!(
                source.effect_reference_tick >= control.effect_reference_tick,
                "episode projection effect clock regressed"
            );
            control.last_source_row_id = source.source_row_id;
            control.last_source_event_id = Some(source.source_event_id.clone());
            control.effect_reference_tick = source.effect_reference_tick;
            insert_json(&mut state, EPISODE_PROJECTION_CONTROL_KEY, &control)?;
        }
        drop(quarantine);
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
            match projection_admission_from_table(
                &quarantine,
                canonical_record.affected_subject,
                canonical_record.source_row_id,
            )? {
                EpisodeProjectionAdmission::Allowed => anyhow::ensure!(
                    canonical_record.reason
                        != EpisodeProjectionQuarantineReason::BlockedByEarlierQuarantine,
                    "blocked quarantine reason requires an earlier subject quarantine"
                ),
                EpisodeProjectionAdmission::SubjectBlocked(_) => anyhow::ensure!(
                    canonical_record.reason
                        == EpisodeProjectionQuarantineReason::BlockedByEarlierQuarantine,
                    "later subject work must be retained as a blocked quarantine"
                ),
                EpisodeProjectionAdmission::GloballyBlocked(_) => {
                    anyhow::bail!("global episode projection quarantine blocks later source rows")
                }
            }
            anyhow::ensure!(
                control.last_source_row_id == expected_global_frontier,
                "episode projection quarantine frontier conflict: expected {}, current {}",
                expected_global_frontier,
                control.last_source_row_id
            );
            anyhow::ensure!(
                canonical_record.effect_reference_tick >= control.effect_reference_tick,
                "episode projection effect clock regressed"
            );
            anyhow::ensure!(
                canonical_record.source_row_id > control.last_source_row_id,
                "quarantined source row must be after the durable frontier"
            );
            insert_json(&mut quarantine, &key, &canonical_record)?;
            control.last_source_row_id = canonical_record.source_row_id;
            control.last_source_event_id = Some(canonical_record.source_event_id.clone());
            control.effect_reference_tick = canonical_record.effect_reference_tick;
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

    /// Load one quarantine by immutable source identity.
    pub fn load_episode_projection_quarantine(
        &self,
        source_row_id: i64,
        source_event_id: &str,
    ) -> anyhow::Result<Option<EpisodeProjectionQuarantine>> {
        validate_projection_key_part(source_event_id, "source event id")?;
        load_json_value(
            &self.db,
            EPISODE_QUARANTINE,
            &quarantine_key(source_row_id, source_event_id),
        )
    }

    /// Check whether a later source row is fenced by unresolved poison work.
    pub fn episode_projection_admission(
        &self,
        subject: Option<EpisodeProjectionSubject>,
        source_row_id: i64,
    ) -> anyhow::Result<EpisodeProjectionAdmission> {
        if let Some(subject) = subject {
            subject.validate()?;
        }
        anyhow::ensure!(source_row_id > 0, "source row must be positive");
        let records = list_quarantine_from(&self.db)?;
        projection_admission_from_records(&records, subject, source_row_id)
    }

    /// Read projection readiness for one stable subject without mutating state.
    pub fn load_episode_projection_readiness(
        &self,
        subject: EpisodeProjectionSubject,
    ) -> anyhow::Result<EpisodeProjectionReadiness> {
        load_projection_readiness_from(&self.db, subject)
    }

    /// Stage one immutable, complete candidate while keeping the active
    /// generation untouched. Readiness closes until validation and activation
    /// complete or the candidate is explicitly discarded.
    pub fn begin_episode_projection_generation(
        &self,
        candidate: &EpisodeProjectionGenerationCandidate,
        expected_active_generation_id: &str,
        expected_source_cut: &EpisodeProjectionSourceCutEvidence,
    ) -> anyhow::Result<String> {
        validate_source_cut_evidence(expected_source_cut)?;
        validate_generation_descriptor(&candidate.descriptor)?;
        anyhow::ensure!(
            candidate.descriptor.source_cut == expected_source_cut.coverage
                && candidate.source_coverage == expected_source_cut.entries,
            "episode projection generation does not match authoritative source coverage"
        );
        anyhow::ensure!(
            candidate.descriptor.parent_generation_id.as_deref()
                == Some(expected_active_generation_id),
            "episode projection generation parent mismatch"
        );
        let snapshot = generation_snapshot(candidate);
        validate_generation_snapshot(
            &candidate.descriptor,
            &snapshot,
            Some(expected_active_generation_id),
            true,
        )?;
        let candidate_subjects: Vec<_> = snapshot
            .subjects
            .iter()
            .map(|subject| (subject.agent.subject, subject.agent.agent_name.as_str()))
            .collect();
        let candidate_digest = episode_projection_candidate_digest(&snapshot)?;
        let write_txn = self.db.begin_write()?;
        let state = write_txn.open_table(EPISODE_PROJECTION_STATE)?;
        let receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS)?;
        let episodes = write_txn.open_table(EPISODES)?;
        let archive = write_txn.open_table(ARCHIVE)?;
        let quarantine = write_txn.open_table(EPISODE_QUARANTINE)?;
        let current = capture_generation_snapshot_from_tables(
            &state,
            &receipts,
            &episodes,
            &archive,
            &quarantine,
        )?;
        ensure_no_receiptless_live_episodes(&current)?;
        let current_subjects: Vec<_> = current
            .subjects
            .iter()
            .map(|subject| (subject.agent.subject, subject.agent.agent_name.as_str()))
            .collect();
        anyhow::ensure!(
            candidate_subjects == current_subjects,
            "episode projection generation must cover every active subject exactly"
        );
        let current_archive_digest =
            archive_snapshot_digest_from_table(&archive, &snapshot.subjects)?;
        anyhow::ensure!(
            constant_time_bytes_eq(
                current_archive_digest.as_bytes(),
                snapshot.archive_snapshot_digest.as_bytes(),
            ),
            "episode projection archive changed before generation staging"
        );
        let mut generations = write_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
        let control = load_generation_control(&generations)?;
        anyhow::ensure!(
            control.active_generation_id == expected_active_generation_id,
            "episode projection generation activation CAS conflict"
        );
        let key = generation_record_key(&candidate.descriptor.generation_id);
        anyhow::ensure!(
            table_json_value::<EpisodeProjectionGenerationRecord>(&generations, &key)?.is_none(),
            "episode projection generation already exists"
        );
        ensure_no_open_generation_transition(&generations)?;
        let record = EpisodeProjectionGenerationRecord {
            descriptor: candidate.descriptor.clone(),
            phase: EpisodeProjectionGenerationPhase::Building,
            candidate_digest: candidate_digest.clone(),
            snapshot,
        };
        insert_json(&mut generations, &key, &record)?;
        seal_generation_control(&mut generations, control)?;
        drop(generations);
        drop(quarantine);
        drop(archive);
        drop(episodes);
        drop(receipts);
        drop(state);
        write_txn.commit()?;
        Ok(candidate_digest)
    }

    /// Atomically discard one open candidate after binding every active-head
    /// and candidate identity field. Projection state and the active record are
    /// not modified.
    pub fn discard_episode_projection_generation(
        &self,
        generation_id: &str,
        expected_active_generation_id: &str,
        expected_candidate_digest: &str,
    ) -> anyhow::Result<EpisodeProjectionGenerationStatus> {
        validate_generation_id(generation_id)?;
        validate_generation_id(expected_active_generation_id)?;
        anyhow::ensure!(
            is_lower_sha256_hex(expected_candidate_digest),
            "episode projection candidate digest must be SHA-256 hex"
        );
        let write_txn = self.db.begin_write()?;
        let mut generations = write_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
        let control = load_generation_control(&generations)?;
        anyhow::ensure!(
            control.active_generation_id == expected_active_generation_id,
            "episode projection generation discard active-head CAS conflict"
        );
        let key = generation_record_key(generation_id);
        let record: EpisodeProjectionGenerationRecord = table_json_value(&generations, &key)?
            .ok_or_else(|| anyhow::anyhow!("episode projection generation candidate not found"))?;
        anyhow::ensure!(
            matches!(
                record.phase,
                EpisodeProjectionGenerationPhase::Building
                    | EpisodeProjectionGenerationPhase::Validated
            ),
            "only an open episode projection generation can be discarded"
        );
        anyhow::ensure!(
            record.descriptor.parent_generation_id.as_deref()
                == Some(expected_active_generation_id),
            "episode projection generation discard parent CAS conflict"
        );
        anyhow::ensure!(
            constant_time_bytes_eq(
                record.candidate_digest.as_bytes(),
                expected_candidate_digest.as_bytes(),
            ),
            "episode projection generation discard candidate CAS conflict"
        );
        generations.remove(key.as_str())?;
        seal_generation_control(&mut generations, control)?;
        drop(generations);
        write_txn.commit()?;
        self.load_episode_projection_generation_status()
    }

    /// Validate complete subject coverage and deterministic per-agent digests
    /// before a candidate can be activated.
    pub fn validate_episode_projection_generation(
        &self,
        generation_id: &str,
        expected_active_generation_id: &str,
        expected_source_cut: &EpisodeProjectionSourceCutEvidence,
    ) -> anyhow::Result<String> {
        validate_source_cut_evidence(expected_source_cut)?;
        validate_generation_id(generation_id)?;
        let write_txn = self.db.begin_write()?;
        let archive = write_txn.open_table(ARCHIVE)?;
        let mut generations = write_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
        let control = load_generation_control(&generations)?;
        anyhow::ensure!(
            control.active_generation_id == expected_active_generation_id,
            "episode projection generation validation CAS conflict"
        );
        let key = generation_record_key(generation_id);
        let mut record: EpisodeProjectionGenerationRecord = table_json_value(&generations, &key)?
            .ok_or_else(|| {
            anyhow::anyhow!("episode projection generation candidate not found")
        })?;
        anyhow::ensure!(
            record.descriptor.source_cut == expected_source_cut.coverage
                && record.snapshot.source_coverage == expected_source_cut.entries,
            "episode projection generation validation source-cut conflict"
        );
        anyhow::ensure!(
            record.phase == EpisodeProjectionGenerationPhase::Building,
            "episode projection generation is not building"
        );
        validate_generation_snapshot(
            &record.descriptor,
            &record.snapshot,
            Some(expected_active_generation_id),
            true,
        )?;
        let current_archive_digest =
            archive_snapshot_digest_from_table(&archive, &record.snapshot.subjects)?;
        anyhow::ensure!(
            constant_time_bytes_eq(
                current_archive_digest.as_bytes(),
                record.snapshot.archive_snapshot_digest.as_bytes(),
            ),
            "episode projection archive changed before generation validation"
        );
        let digest = episode_projection_candidate_digest(&record.snapshot)?;
        anyhow::ensure!(
            constant_time_bytes_eq(digest.as_bytes(), record.candidate_digest.as_bytes()),
            "episode projection generation candidate digest mismatch"
        );
        record.phase = EpisodeProjectionGenerationPhase::Validated;
        insert_json(&mut generations, &key, &record)?;
        seal_generation_control(&mut generations, control)?;
        drop(generations);
        drop(archive);
        write_txn.commit()?;
        Ok(digest)
    }

    /// Atomically activate one validated candidate and retain the complete old
    /// generation snapshot for a CAS-bound rollback.
    pub fn activate_episode_projection_generation(
        &self,
        generation_id: &str,
        expected_active_generation_id: &str,
        expected_candidate_digest: &str,
        expected_source_cut: &EpisodeProjectionSourceCutEvidence,
        active_source_cut: &EpisodeProjectionSourceCutEvidence,
    ) -> anyhow::Result<EpisodeProjectionGenerationStatus> {
        validate_source_cut_evidence(expected_source_cut)?;
        validate_source_cut_evidence(active_source_cut)?;
        self.activate_generation(
            generation_id,
            expected_active_generation_id,
            expected_candidate_digest,
            expected_source_cut,
            active_source_cut,
            EpisodeProjectionGenerationPhase::Validated,
        )
    }

    /// Roll back to one retained generation without discarding the generation
    /// that was active immediately before the rollback.
    pub fn rollback_episode_projection_generation(
        &self,
        generation_id: &str,
        expected_active_generation_id: &str,
        expected_candidate_digest: &str,
        expected_source_cut: &EpisodeProjectionSourceCutEvidence,
        active_source_cut: &EpisodeProjectionSourceCutEvidence,
    ) -> anyhow::Result<EpisodeProjectionGenerationStatus> {
        validate_source_cut_evidence(expected_source_cut)?;
        validate_source_cut_evidence(active_source_cut)?;
        self.activate_generation(
            generation_id,
            expected_active_generation_id,
            expected_candidate_digest,
            expected_source_cut,
            active_source_cut,
            EpisodeProjectionGenerationPhase::Retained,
        )
    }

    pub fn load_episode_projection_generation_status(
        &self,
    ) -> anyhow::Result<EpisodeProjectionGenerationStatus> {
        load_generation_status_from(&self.db)
    }

    /// Validate the compact generation seal and report only open transitions.
    /// Full generation material remains available through the operator status
    /// path, but runtime admission must not deserialize it once per agent.
    pub fn load_episode_projection_generation_readiness_blocks(
        &self,
    ) -> anyhow::Result<Vec<EpisodeProjectionReadinessBlock>> {
        let read_txn = self.db.begin_read()?;
        let generations = read_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
        generation_readiness_blocks(&generations)
    }

    fn ensure_episode_projection_generation(
        &self,
        projection_control: &EpisodeProjectionControl,
    ) -> anyhow::Result<()> {
        let needs_seal_migration = {
            let read_txn = self.db.begin_read()?;
            let generations = read_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
            if let Some(control) = table_json_value::<EpisodeProjectionGenerationControl>(
                &generations,
                EPISODE_PROJECTION_GENERATION_CONTROL_KEY,
            )? {
                if !control.record_seals.is_empty() || control.seal_digest.is_some() {
                    anyhow::ensure!(
                        !control.record_seals.is_empty() && control.seal_digest.is_some(),
                        "episode projection generation control seal is incomplete"
                    );
                    generation_readiness_blocks_from_seal(&generations, &control)?;
                    return Ok(());
                }
                true
            } else {
                anyhow::ensure!(
                    generations.iter()?.next().is_none(),
                    "orphaned episode projection generation records"
                );
                false
            }
        };

        if needs_seal_migration {
            let write_txn = self.db.begin_write()?;
            let mut generations = write_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
            let control = load_generation_control(&generations)?;
            if !control.record_seals.is_empty() || control.seal_digest.is_some() {
                anyhow::ensure!(
                    !control.record_seals.is_empty() && control.seal_digest.is_some(),
                    "episode projection generation control seal is incomplete"
                );
                generation_readiness_blocks_from_seal(&generations, &control)?;
            } else {
                seal_generation_control(&mut generations, control)?;
            }
            drop(generations);
            write_txn.commit()?;
            return Ok(());
        }

        let mut snapshot = capture_generation_snapshot_from(&self.db)?;
        let source_cut = episode_projection_source_cut_coverage(
            projection_control.start_policy.source_row_id(),
            projection_control.start_policy.source_row_id(),
            EPISODE_PROJECTION_TICK_DURATION_MILLIS,
            &[],
        )?;
        snapshot.source_cut = source_cut.clone();
        snapshot.source_coverage = Vec::new();
        let generation_id = episode_projection_generation_id(
            None,
            projection_control.projection_version,
            &source_cut,
            &snapshot.archive_snapshot_digest,
        );
        let descriptor = EpisodeProjectionGenerationDescriptor {
            generation_id: generation_id.clone(),
            parent_generation_id: None,
            projection_version: projection_control.projection_version,
            source_cut,
            archive_snapshot_digest: snapshot.archive_snapshot_digest.clone(),
        };
        let digest = episode_projection_candidate_digest(&snapshot)?;

        let write_txn = self.db.begin_write()?;
        let mut generations = write_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
        if let Some(control) = table_json_value::<EpisodeProjectionGenerationControl>(
            &generations,
            EPISODE_PROJECTION_GENERATION_CONTROL_KEY,
        )? {
            if !control.record_seals.is_empty() || control.seal_digest.is_some() {
                anyhow::ensure!(
                    !control.record_seals.is_empty() && control.seal_digest.is_some(),
                    "episode projection generation control seal is incomplete"
                );
                generation_readiness_blocks_from_seal(&generations, &control)?;
            } else {
                seal_generation_control(&mut generations, control)?;
            }
            drop(generations);
            write_txn.commit()?;
            return Ok(());
        }
        anyhow::ensure!(
            generations.iter()?.next().is_none(),
            "orphaned episode projection generation records"
        );
        validate_generation_descriptor(&descriptor)?;
        validate_generation_snapshot(&descriptor, &snapshot, None, true)?;
        let record = EpisodeProjectionGenerationRecord {
            descriptor,
            phase: EpisodeProjectionGenerationPhase::Active,
            candidate_digest: digest,
            snapshot,
        };
        insert_json(
            &mut generations,
            &generation_record_key(&generation_id),
            &record,
        )?;
        seal_generation_control(
            &mut generations,
            EpisodeProjectionGenerationControl {
                active_generation_id: generation_id,
                activation_epoch: 0,
                record_seals: Vec::new(),
                seal_digest: None,
            },
        )?;
        drop(generations);
        write_txn.commit()?;
        Ok(())
    }

    fn activate_generation(
        &self,
        generation_id: &str,
        expected_active_generation_id: &str,
        expected_candidate_digest: &str,
        expected_source_cut: &EpisodeProjectionSourceCutEvidence,
        active_source_cut: &EpisodeProjectionSourceCutEvidence,
        required_phase: EpisodeProjectionGenerationPhase,
    ) -> anyhow::Result<EpisodeProjectionGenerationStatus> {
        validate_generation_id(generation_id)?;
        anyhow::ensure!(
            is_lower_sha256_hex(expected_candidate_digest),
            "episode projection candidate digest must be SHA-256 hex"
        );
        let write_txn = self.db.begin_write()?;
        let mut state = write_txn.open_table(EPISODE_PROJECTION_STATE)?;
        let mut receipts = write_txn.open_table(EPISODE_SOURCE_RECEIPTS)?;
        let mut episodes = write_txn.open_table(EPISODES)?;
        let archive = write_txn.open_table(ARCHIVE)?;
        let mut quarantine = write_txn.open_table(EPISODE_QUARANTINE)?;
        let mut generations = write_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
        let mut generation_control = load_generation_control(&generations)?;
        anyhow::ensure!(
            generation_control.active_generation_id == expected_active_generation_id,
            "episode projection generation activation CAS conflict"
        );
        let target_key = generation_record_key(generation_id);
        let mut target: EpisodeProjectionGenerationRecord =
            table_json_value(&generations, &target_key)?
                .ok_or_else(|| anyhow::anyhow!("episode projection generation target not found"))?;
        anyhow::ensure!(
            target.phase == required_phase,
            "episode projection generation target phase mismatch"
        );
        let target_source_cut = if required_phase == EpisodeProjectionGenerationPhase::Retained {
            &target.snapshot.source_cut
        } else {
            &target.descriptor.source_cut
        };
        anyhow::ensure!(
            target_source_cut == &expected_source_cut.coverage
                && target.snapshot.source_coverage == expected_source_cut.entries,
            "episode projection generation activation source-cut conflict"
        );
        anyhow::ensure!(
            constant_time_bytes_eq(
                target.candidate_digest.as_bytes(),
                expected_candidate_digest.as_bytes(),
            ),
            "episode projection generation candidate CAS conflict"
        );
        validate_generation_snapshot(
            &target.descriptor,
            &target.snapshot,
            target.descriptor.parent_generation_id.as_deref(),
            required_phase != EpisodeProjectionGenerationPhase::Retained,
        )?;
        let current_archive_digest =
            archive_snapshot_digest_from_table(&archive, &target.snapshot.subjects)?;
        anyhow::ensure!(
            constant_time_bytes_eq(
                current_archive_digest.as_bytes(),
                target.snapshot.archive_snapshot_digest.as_bytes(),
            ),
            "episode projection archive changed before generation activation"
        );

        let active_key = generation_record_key(expected_active_generation_id);
        let mut active: EpisodeProjectionGenerationRecord =
            table_json_value(&generations, &active_key)?.ok_or_else(|| {
                anyhow::anyhow!("active episode projection generation record missing")
            })?;
        anyhow::ensure!(
            active.phase == EpisodeProjectionGenerationPhase::Active,
            "episode projection active generation phase mismatch"
        );
        active.snapshot = capture_generation_snapshot_from_tables(
            &state,
            &receipts,
            &episodes,
            &archive,
            &quarantine,
        )?;
        active.snapshot.source_cut = active_source_cut.coverage.clone();
        active.snapshot.source_coverage = active_source_cut.entries.clone();
        validate_generation_snapshot(
            &active.descriptor,
            &active.snapshot,
            active.descriptor.parent_generation_id.as_deref(),
            false,
        )?;
        active.candidate_digest = episode_projection_candidate_digest(&active.snapshot)?;
        active.phase = EpisodeProjectionGenerationPhase::Retained;
        apply_generation_snapshot_to_tables(
            &target.snapshot,
            &mut state,
            &mut receipts,
            &mut episodes,
            &mut quarantine,
        )?;
        target.phase = EpisodeProjectionGenerationPhase::Active;
        insert_json(&mut generations, &active_key, &active)?;
        insert_json(&mut generations, &target_key, &target)?;
        generation_control.active_generation_id = generation_id.to_string();
        generation_control.activation_epoch = generation_control.activation_epoch.saturating_add(1);
        seal_generation_control(&mut generations, generation_control)?;

        drop(generations);
        drop(quarantine);
        drop(archive);
        drop(episodes);
        drop(receipts);
        drop(state);
        write_txn.commit()?;
        self.load_episode_projection_generation_status()
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

    /// Atomically move the exact live bucket into the bounded archive.
    ///
    /// This is the projection-aware consolidation boundary: a crash can leave
    /// the bucket either wholly live or wholly archived, never duplicated or
    /// absent across separate commits.
    pub fn archive_and_clear_episodes(
        &self,
        agent: &str,
        expected_live: &[Episode],
    ) -> anyhow::Result<()> {
        validate_projection_key_part(agent, "agent name")?;
        let write_txn = self.db.begin_write()?;
        let mut episodes = write_txn.open_table(EPISODES)?;
        let mut archive = write_txn.open_table(ARCHIVE)?;
        let current: Vec<Episode> = match episodes.get(agent)? {
            Some(value) => serde_json::from_slice(value.value())?,
            None => Vec::new(),
        };
        anyhow::ensure!(
            serde_json::to_vec(&current)? == serde_json::to_vec(expected_live)?,
            "episode bucket changed during consolidation"
        );
        let mut archived: Vec<Episode> = match archive.get(agent)? {
            Some(value) => serde_json::from_slice(value.value())?,
            None => Vec::new(),
        };
        archived.extend_from_slice(&current);
        if archived.len() > MAX_EPISODES_PER_AGENT {
            let excess = archived.len() - MAX_EPISODES_PER_AGENT;
            archived.drain(..excess);
        }
        let encoded_archive = serde_json::to_vec(&archived)?;
        archive.insert(agent, encoded_archive.as_slice())?;
        episodes.remove(agent)?;
        drop(archive);
        drop(episodes);
        write_txn.commit()?;
        Ok(())
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

fn append_table_material(
    read_txn: &redb::ReadTransaction,
    definition: TableDefinition<&str, &[u8]>,
    table_name: &str,
    material: &mut Vec<u8>,
) -> anyhow::Result<()> {
    append_length_delimited(material, table_name.as_bytes());
    let table = read_txn.open_table(definition)?;
    for entry in table.iter()? {
        let (key, value) = entry?;
        append_length_delimited(material, key.value().as_bytes());
        append_length_delimited(material, value.value());
    }
    material.extend_from_slice(&0_u64.to_be_bytes());
    Ok(())
}

fn append_length_delimited(material: &mut Vec<u8>, value: &[u8]) {
    material.extend_from_slice(&(value.len() as u64).to_be_bytes());
    material.extend_from_slice(value);
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

fn list_projection_frontiers_for_admission_from<D: ReadableDatabase>(
    db: &D,
) -> anyhow::Result<Vec<EpisodeProjectionFrontier>> {
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(EPISODE_PROJECTION_STATE)?;
    let control: EpisodeProjectionControl = table
        .get(EPISODE_PROJECTION_CONTROL_KEY)?
        .map(|value| serde_json::from_slice(value.value()))
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("episode projection is uninitialized"))?;
    validate_projection_control(&control)?;
    let cutover = table
        .get(EPISODE_PROJECTION_CUTOVER_KEY)?
        .map(|value| serde_json::from_slice(value.value()))
        .transpose()?;
    validate_persisted_cutover_value(&control, cutover)?;

    let prefix = format!("frontier{KEY_SEPARATOR}");
    let mut frontiers = Vec::new();
    for entry in table.iter()? {
        let (key, value) = entry?;
        if matches!(
            key.value(),
            EPISODE_PROJECTION_CONTROL_KEY | EPISODE_PROJECTION_CUTOVER_KEY
        ) {
            continue;
        }
        anyhow::ensure!(
            key.value().starts_with(&prefix),
            "unknown episode projection state key"
        );
        let frontier: EpisodeProjectionFrontier = serde_json::from_slice(value.value())?;
        anyhow::ensure!(
            key.value() == projection_frontier_key(frontier.subject),
            "episode projection frontier key/value subject mismatch"
        );
        validate_projection_frontier(&frontier, frontier.subject, &control)?;
        frontiers.push(frontier);
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
    let archive = read_txn.open_table(ARCHIVE)?;
    let quarantine = read_txn.open_table(EPISODE_QUARANTINE)?;
    let generations = read_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
    let control: Option<EpisodeProjectionControl> =
        match state.get(EPISODE_PROJECTION_CONTROL_KEY)? {
            Some(value) => Some(serde_json::from_slice(value.value())?),
            None => None,
        };
    if let Some(control) = &control {
        validate_projection_control(control)?;
        let cutover = state
            .get(EPISODE_PROJECTION_CUTOVER_KEY)?
            .map(|value| serde_json::from_slice(value.value()))
            .transpose()?;
        validate_persisted_cutover_value(control, cutover)?;
    } else {
        anyhow::ensure!(
            state.get(EPISODE_PROJECTION_CUTOVER_KEY)?.is_none(),
            "orphaned episode projection cutover receipt"
        );
    }
    let frontier_prefix = format!("frontier{KEY_SEPARATOR}");
    let mut frontier = None;
    for entry in state.iter()? {
        let (key, value) = entry?;
        if matches!(
            key.value(),
            EPISODE_PROJECTION_CONTROL_KEY | EPISODE_PROJECTION_CUTOVER_KEY
        ) {
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
            let archived: Vec<Episode> = match archive.get(frontier.agent_name.as_str())? {
                Some(value) => serde_json::from_slice(value.value())?,
                None => Vec::new(),
            };
            validate_subject_integrity(
                subject,
                frontier,
                &persisted_receipts,
                &retained,
                &archived,
            )?;
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
    if control.is_some() {
        blockers.extend(generation_readiness_blocks(&generations)?);
    } else {
        anyhow::ensure!(
            generations.iter()?.next().is_none(),
            "orphaned episode projection generation state"
        );
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
                validate_projection_frontier(&quarantine_frontier, quarantine_subject, control)?;
                if quarantine_subject == subject {
                    blockers.push(EpisodeProjectionReadinessBlock::SubjectQuarantine {
                        quarantine: record,
                    });
                }
            }
            Some(EpisodeProjectionSubject::Building) | None => {
                blockers
                    .push(EpisodeProjectionReadinessBlock::GlobalQuarantine { quarantine: record });
            }
        }
    }
    Ok(EpisodeProjectionReadiness {
        subject,
        frontier,
        blockers,
    })
}

fn table_json_value<T>(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    key: &str,
) -> anyhow::Result<Option<T>>
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

fn generation_record_key(generation_id: &str) -> String {
    format!("generation{KEY_SEPARATOR}{generation_id}")
}

fn validate_generation_id(generation_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_lower_sha256_hex(generation_id),
        "episode projection generation ID must be 64 lowercase hex characters"
    );
    Ok(())
}

fn projection_sha256(domain: &[u8], canonical_value: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update([0]);
    digest.update((canonical_value.len() as u64).to_be_bytes());
    digest.update(canonical_value);
    format!("{:x}", digest.finalize())
}

/// Derive the immutable generation ID from its parent, source cut, and archive CAS.
pub fn episode_projection_generation_id(
    parent_generation_id: Option<&str>,
    projection_version: u32,
    source_cut: &EpisodeProjectionSourceCutCoverage,
    archive_snapshot_digest: &str,
) -> String {
    // Canonical v1 order: parent presence+bytes, projection version, complete
    // source-cut contract, and consolidation-owned archive snapshot digest.
    // Every variable field is length-delimited and domain-separated.
    let mut material = Vec::new();
    match parent_generation_id {
        Some(parent) => {
            material.push(1);
            append_length_delimited(&mut material, parent.as_bytes());
        }
        None => material.push(0),
    }
    material.extend_from_slice(&projection_version.to_be_bytes());
    append_length_delimited(
        &mut material,
        &serde_json::to_vec(source_cut).expect("typed source cut is serializable"),
    );
    append_length_delimited(&mut material, archive_snapshot_digest.as_bytes());
    projection_sha256(b"sentinel-episode-projection-generation-v1", &material)
}

fn validate_generation_descriptor(
    descriptor: &EpisodeProjectionGenerationDescriptor,
) -> anyhow::Result<()> {
    validate_generation_id(&descriptor.generation_id)?;
    if let Some(parent) = descriptor.parent_generation_id.as_deref() {
        validate_generation_id(parent)?;
        anyhow::ensure!(
            parent != descriptor.generation_id,
            "episode projection generation cannot parent itself"
        );
    }
    anyhow::ensure!(
        descriptor.projection_version == EPISODE_PROJECTION_VERSION,
        "unsupported episode projection generation version {}",
        descriptor.projection_version
    );
    anyhow::ensure!(
        descriptor.source_cut.schema_version == 1,
        "unsupported episode projection source-cut schema {}",
        descriptor.source_cut.schema_version
    );
    validate_source_cut_shape(&descriptor.source_cut)?;
    anyhow::ensure!(
        is_lower_sha256_hex(&descriptor.archive_snapshot_digest),
        "episode projection archive snapshot digest must be lowercase SHA-256 hex"
    );
    anyhow::ensure!(
        descriptor.generation_id
            == episode_projection_generation_id(
                descriptor.parent_generation_id.as_deref(),
                descriptor.projection_version,
                &descriptor.source_cut,
                &descriptor.archive_snapshot_digest,
            ),
        "episode projection generation identity does not bind its source/archive cut"
    );
    Ok(())
}

fn generation_snapshot(
    candidate: &EpisodeProjectionGenerationCandidate,
) -> EpisodeProjectionGenerationSnapshot {
    EpisodeProjectionGenerationSnapshot {
        control: candidate.control.clone(),
        subjects: candidate.subjects.clone(),
        quarantines: candidate.quarantines.clone(),
        source_cut: candidate.descriptor.source_cut.clone(),
        source_coverage: candidate.source_coverage.clone(),
        archive_snapshot_digest: candidate.descriptor.archive_snapshot_digest.clone(),
    }
}

fn episode_projection_candidate_digest(
    snapshot: &EpisodeProjectionGenerationSnapshot,
) -> anyhow::Result<String> {
    let encoded = serde_json::to_vec(snapshot)?;
    // Snapshot structs have a fixed serde field order. Validation requires
    // subjects, receipts, and quarantines to be unique and sorted before this
    // v1 JSON encoding is accepted as canonical.
    Ok(projection_sha256(
        b"sentinel-episode-projection-candidate-v1",
        &encoded,
    ))
}

/// Compute one subject's deterministic candidate-coverage digest.
pub fn episode_projection_subject_coverage_digest(
    agent: &EpisodeProjectionAgent,
    frontier: &EpisodeProjectionFrontier,
    receipts: &[EpisodeSourceReceipt],
    live_episodes: &[Episode],
    archived_episodes: &[Episode],
) -> anyhow::Result<String> {
    let encoded =
        serde_json::to_vec(&(agent, frontier, receipts, live_episodes, archived_episodes))?;
    // The typed v1 tuple fixes field order. Receipt order is source order;
    // episode vectors preserve their authoritative bucket order.
    Ok(projection_sha256(
        b"sentinel-episode-projection-subject-coverage-v1",
        &encoded,
    ))
}

/// Seal consolidation-owned archive material covered by a generation.
/// Generation activation compares this digest but never writes ARCHIVE.
pub fn episode_projection_archive_snapshot_digest(
    subjects: &[EpisodeProjectionGenerationSubject],
) -> anyhow::Result<String> {
    let archive_material: Vec<_> = subjects
        .iter()
        .map(|subject| (&subject.agent, &subject.archived_episodes))
        .collect();
    Ok(projection_sha256(
        b"sentinel-episode-projection-archive-snapshot-v1",
        &serde_json::to_vec(&archive_material)?,
    ))
}

/// Seal a complete, payload-free EventStore row classification into the
/// source-cut contract used by generation CAS operations.
pub fn episode_projection_source_cut_coverage(
    from_exclusive_source_row_id: i64,
    through_source_row_id: i64,
    tick_duration_millis: u64,
    entries: &[EpisodeProjectionSourceCoverageEntry],
) -> anyhow::Result<EpisodeProjectionSourceCutCoverage> {
    anyhow::ensure!(
        from_exclusive_source_row_id >= 0 && through_source_row_id >= from_exclusive_source_row_id,
        "episode projection source-cut range is invalid"
    );
    anyhow::ensure!(
        tick_duration_millis > 0,
        "episode projection tick duration must be positive"
    );
    let mut previous_row: Option<i64> = None;
    let mut episode_count = 0_u64;
    let mut irrelevant_count = 0_u64;
    let mut quarantine_count = 0_u64;
    let mut reference_tick = 0_u64;
    for entry in entries {
        validate_projection_source(
            &entry.source_event_id,
            entry.source_row_id,
            EPISODE_PROJECTION_VERSION,
            &entry.request_digest,
        )?;
        anyhow::ensure!(
            entry.source_row_id > from_exclusive_source_row_id
                && entry.source_row_id <= through_source_row_id,
            "episode projection source coverage row is outside its cut"
        );
        if let Some(row) = previous_row {
            anyhow::ensure!(
                row < entry.source_row_id,
                "episode projection source coverage must be unique and sorted"
            );
        }
        previous_row = Some(entry.source_row_id);
        reference_tick = reference_tick.max(entry.source_tick);
        anyhow::ensure!(
            entry.effect_reference_tick == reference_tick,
            "episode projection source effect clock is not the canonical prefix maximum"
        );
        match &entry.classification {
            EpisodeProjectionSourceClassification::Irrelevant => irrelevant_count += 1,
            EpisodeProjectionSourceClassification::Episode { subject } => {
                subject.validate()?;
                episode_count += 1;
            }
            EpisodeProjectionSourceClassification::Quarantined {
                affected_subject, ..
            } => {
                if let Some(subject) = affected_subject {
                    subject.validate()?;
                }
                quarantine_count += 1;
            }
        }
    }
    if through_source_row_id > from_exclusive_source_row_id {
        anyhow::ensure!(
            entries
                .last()
                .is_some_and(|entry| entry.source_row_id == through_source_row_id),
            "episode projection source coverage does not reach its declared cut"
        );
    } else {
        anyhow::ensure!(entries.is_empty(), "empty source cut contains rows");
    }
    // Canonical v1 input binds the replay-clock version/duration, fixed range,
    // and typed strictly row-sorted entry vector. Entries contain no payload
    // or diagnostics. Each entry binds the deterministic prefix maximum used
    // for its effect; the final prefix value is the cut `reference_tick`.
    let encoded = serde_json::to_vec(&(
        1_u32,
        tick_duration_millis,
        from_exclusive_source_row_id,
        through_source_row_id,
        entries,
    ))?;
    Ok(EpisodeProjectionSourceCutCoverage {
        schema_version: 1,
        replay_clock_version: 1,
        reference_tick,
        tick_duration_millis,
        from_exclusive_source_row_id,
        through_source_row_id,
        event_count: entries.len() as u64,
        episode_count,
        irrelevant_count,
        quarantine_count,
        coverage_digest: projection_sha256(
            b"sentinel-episode-projection-source-coverage-v1",
            &encoded,
        ),
    })
}

fn validate_source_cut_shape(cut: &EpisodeProjectionSourceCutCoverage) -> anyhow::Result<()> {
    anyhow::ensure!(cut.schema_version == 1, "unsupported source-cut schema");
    anyhow::ensure!(
        cut.replay_clock_version == 1 && cut.tick_duration_millis > 0,
        "unsupported episode projection replay clock"
    );
    anyhow::ensure!(
        cut.from_exclusive_source_row_id >= 0
            && cut.through_source_row_id >= cut.from_exclusive_source_row_id,
        "episode projection source-cut range is invalid"
    );
    anyhow::ensure!(
        cut.event_count
            == cut
                .episode_count
                .saturating_add(cut.irrelevant_count)
                .saturating_add(cut.quarantine_count),
        "episode projection source-cut cardinalities do not add up"
    );
    anyhow::ensure!(
        is_lower_sha256_hex(&cut.coverage_digest),
        "episode projection source-cut digest must be lowercase SHA-256 hex"
    );
    Ok(())
}

fn validate_source_cut_entries(
    expected: &EpisodeProjectionSourceCutCoverage,
    entries: &[EpisodeProjectionSourceCoverageEntry],
) -> anyhow::Result<()> {
    validate_source_cut_shape(expected)?;
    let computed = episode_projection_source_cut_coverage(
        expected.from_exclusive_source_row_id,
        expected.through_source_row_id,
        expected.tick_duration_millis,
        entries,
    )?;
    anyhow::ensure!(
        &computed == expected,
        "episode projection candidate source coverage does not match the authoritative cut"
    );
    Ok(())
}

fn validate_source_cut_evidence(
    evidence: &EpisodeProjectionSourceCutEvidence,
) -> anyhow::Result<()> {
    validate_source_cut_entries(&evidence.coverage, &evidence.entries)
}

fn permits_receiptless_legacy_root(
    descriptor: &EpisodeProjectionGenerationDescriptor,
    snapshot: &EpisodeProjectionGenerationSnapshot,
) -> bool {
    let EpisodeProjectionStartPolicy::RecoveryCut { source_row_id, .. } =
        &snapshot.control.start_policy
    else {
        return false;
    };
    descriptor.parent_generation_id.is_none()
        && descriptor.source_cut.from_exclusive_source_row_id == *source_row_id
        && descriptor.source_cut.through_source_row_id == *source_row_id
        && descriptor.source_cut.event_count == 0
        && descriptor.source_cut.episode_count == 0
        && descriptor.source_cut.irrelevant_count == 0
        && descriptor.source_cut.quarantine_count == 0
        && snapshot.control.last_source_row_id == *source_row_id
        && snapshot.control.last_source_event_id.is_none()
        && snapshot.control.effect_reference_tick == 0
        && snapshot.source_coverage.is_empty()
}

fn ensure_no_receiptless_live_episodes(
    snapshot: &EpisodeProjectionGenerationSnapshot,
) -> anyhow::Result<()> {
    for subject in &snapshot.subjects {
        let receipt_episode_ids: HashSet<u64> = subject
            .receipts
            .iter()
            .map(|receipt| receipt.episode_id)
            .collect();
        anyhow::ensure!(
            subject
                .live_episodes
                .iter()
                .all(|episode| receipt_episode_ids.contains(&episode.id)),
            "active episode projection contains receiptless live legacy memory; consolidate it before staging"
        );
    }
    Ok(())
}

fn validate_generation_snapshot(
    descriptor: &EpisodeProjectionGenerationDescriptor,
    snapshot: &EpisodeProjectionGenerationSnapshot,
    expected_parent: Option<&str>,
    require_exact_source_cut: bool,
) -> anyhow::Result<()> {
    validate_generation_descriptor(descriptor)?;
    anyhow::ensure!(
        descriptor.parent_generation_id.as_deref() == expected_parent,
        "episode projection generation parent mismatch"
    );
    validate_projection_control(&snapshot.control)?;
    let archive_snapshot_digest = episode_projection_archive_snapshot_digest(&snapshot.subjects)?;
    anyhow::ensure!(
        constant_time_bytes_eq(
            archive_snapshot_digest.as_bytes(),
            snapshot.archive_snapshot_digest.as_bytes(),
        ),
        "episode projection generation archive snapshot digest mismatch"
    );
    if require_exact_source_cut {
        anyhow::ensure!(
            constant_time_bytes_eq(
                snapshot.archive_snapshot_digest.as_bytes(),
                descriptor.archive_snapshot_digest.as_bytes(),
            ),
            "episode projection generation archive snapshot changed from its staged cut"
        );
    }
    anyhow::ensure!(
        snapshot.control.projection_version == descriptor.projection_version,
        "episode projection generation control version mismatch"
    );
    anyhow::ensure!(
        snapshot.source_cut.from_exclusive_source_row_id
            == snapshot.control.start_policy.source_row_id(),
        "episode projection generation coverage does not begin at the fixed start policy"
    );
    if require_exact_source_cut {
        anyhow::ensure!(
            snapshot.source_cut == descriptor.source_cut
                && snapshot.control.last_source_row_id
                    == descriptor.source_cut.through_source_row_id
                && snapshot.control.effect_reference_tick == descriptor.source_cut.reference_tick,
            "episode projection generation control does not match its source cut"
        );
    } else {
        anyhow::ensure!(
            snapshot.source_cut.from_exclusive_source_row_id
                == descriptor.source_cut.from_exclusive_source_row_id
                && snapshot.source_cut.through_source_row_id == snapshot.control.last_source_row_id
                && snapshot.source_cut.reference_tick == snapshot.control.effect_reference_tick
                && snapshot.control.last_source_row_id
                    >= descriptor.source_cut.through_source_row_id,
            "retained episode projection generation precedes its immutable source cut"
        );
    }

    let mut previous_subject_key: Option<String> = None;
    let mut subjects = HashSet::new();
    let mut names = HashSet::new();
    for candidate in &snapshot.subjects {
        candidate.agent.subject.validate()?;
        validate_projection_key_part(&candidate.agent.agent_name, "generation agent name")?;
        let subject_key = candidate.agent.subject.storage_key();
        anyhow::ensure!(
            previous_subject_key
                .as_deref()
                .is_none_or(|previous| previous < subject_key.as_str()),
            "episode projection generation subjects must be unique and sorted"
        );
        previous_subject_key = Some(subject_key);
        anyhow::ensure!(
            subjects.insert(candidate.agent.subject),
            "duplicate episode projection generation subject"
        );
        anyhow::ensure!(
            names.insert(candidate.agent.agent_name.as_str()),
            "duplicate episode projection generation storage locator"
        );
        anyhow::ensure!(
            candidate.frontier.subject == candidate.agent.subject
                && candidate.frontier.agent_name == candidate.agent.agent_name,
            "episode projection generation subject/frontier mismatch"
        );
        validate_projection_frontier(
            &candidate.frontier,
            candidate.agent.subject,
            &snapshot.control,
        )?;
        let mut previous_receipt: Option<(i64, &str)> = None;
        let mut entries = Vec::with_capacity(candidate.receipts.len().saturating_mul(2));
        for receipt in &candidate.receipts {
            if let Some((row, event_id)) = previous_receipt {
                anyhow::ensure!(
                    (row, event_id) < (receipt.source_row_id, receipt.source_event_id.as_str()),
                    "episode projection generation receipts must be unique and sorted"
                );
            }
            previous_receipt = Some((receipt.source_row_id, receipt.source_event_id.as_str()));
            let encoded = serde_json::to_vec(receipt)?;
            entries.push(PersistedEpisodeReceipt {
                key: source_receipt_key(receipt.subject, &receipt.source_event_id),
                encoded: encoded.clone(),
                receipt: receipt.clone(),
            });
            entries.push(PersistedEpisodeReceipt {
                key: episode_identity_key(receipt.subject, receipt.episode_id),
                encoded,
                receipt: receipt.clone(),
            });
        }
        validate_subject_integrity(
            candidate.agent.subject,
            &candidate.frontier,
            &entries,
            &candidate.live_episodes,
            &candidate.archived_episodes,
        )?;
        anyhow::ensure!(
            candidate.live_episodes.len() <= MAX_EPISODES_PER_AGENT
                && candidate.archived_episodes.len() <= MAX_EPISODES_PER_AGENT,
            "episode projection generation exceeds bounded retention"
        );
        let receipt_episode_ids: HashSet<u64> = candidate
            .receipts
            .iter()
            .map(|receipt| receipt.episode_id)
            .collect();
        let mut receipt_backed_episode_ids = HashSet::new();
        for episode in candidate
            .live_episodes
            .iter()
            .chain(candidate.archived_episodes.iter())
        {
            anyhow::ensure!(
                episode.agent_name == candidate.agent.agent_name,
                "episode projection generation episode/storage locator mismatch"
            );
            // Legacy buckets predate stable projection identities and may reuse
            // an ID. Their ordered payload bytes remain covered by the cutover
            // and generation digests. Receipt-backed IDs must stay unique.
            if receipt_episode_ids.contains(&episode.id) {
                anyhow::ensure!(
                    receipt_backed_episode_ids.insert(episode.id),
                    "duplicate receipt-backed episode ID in projection generation subject"
                );
            }
        }
        if !permits_receiptless_legacy_root(descriptor, snapshot) {
            for episode in &candidate.live_episodes {
                anyhow::ensure!(
                    receipt_episode_ids.contains(&episode.id),
                    "live generation episode has no authoritative source receipt"
                );
            }
        }
        let digest = episode_projection_subject_coverage_digest(
            &candidate.agent,
            &candidate.frontier,
            &candidate.receipts,
            &candidate.live_episodes,
            &candidate.archived_episodes,
        )?;
        anyhow::ensure!(
            constant_time_bytes_eq(digest.as_bytes(), candidate.coverage_digest.as_bytes()),
            "episode projection generation subject coverage digest mismatch"
        );
    }

    let mut previous_quarantine: Option<(i64, &str)> = None;
    for record in &snapshot.quarantines {
        validate_projection_quarantine(record)?;
        if let Some((row, event_id)) = previous_quarantine {
            anyhow::ensure!(
                (row, event_id) < (record.source_row_id, record.source_event_id.as_str()),
                "episode projection generation quarantines must be unique and sorted"
            );
        }
        previous_quarantine = Some((record.source_row_id, record.source_event_id.as_str()));
        anyhow::ensure!(
            record.source_row_id > snapshot.control.start_policy.source_row_id()
                && record.source_row_id <= snapshot.control.last_source_row_id,
            "episode projection generation quarantine is outside the source cut"
        );
        if let Some(subject @ EpisodeProjectionSubject::Agent { .. }) = record.affected_subject {
            anyhow::ensure!(
                subjects.contains(&subject),
                "episode projection generation quarantine references a missing subject"
            );
        }
    }
    validate_source_cut_entries(&snapshot.source_cut, &snapshot.source_coverage)?;
    let coverage_by_row: HashMap<i64, &EpisodeProjectionSourceCoverageEntry> = snapshot
        .source_coverage
        .iter()
        .map(|entry| (entry.source_row_id, entry))
        .collect();
    anyhow::ensure!(
        coverage_by_row.len() == snapshot.source_coverage.len(),
        "duplicate source row in episode projection coverage"
    );
    let mut matched_episodes = HashSet::new();
    for subject in &snapshot.subjects {
        for receipt in &subject.receipts {
            let entry = coverage_by_row
                .get(&receipt.source_row_id)
                .ok_or_else(|| anyhow::anyhow!("episode receipt is absent from source coverage"))?;
            anyhow::ensure!(
                entry.source_event_id == receipt.source_event_id
                    && entry.request_digest == receipt.request_digest
                    && entry.effect_reference_tick == receipt.effect_reference_tick
                    && matches!(
                        &entry.classification,
                        EpisodeProjectionSourceClassification::Episode { subject }
                            if *subject == receipt.subject
                    ),
                "episode receipt/source classification mismatch"
            );
            matched_episodes.insert(receipt.source_row_id);
        }
    }
    let mut matched_quarantines = HashSet::new();
    for record in &snapshot.quarantines {
        let entry = coverage_by_row
            .get(&record.source_row_id)
            .ok_or_else(|| anyhow::anyhow!("episode quarantine is absent from source coverage"))?;
        anyhow::ensure!(
            entry.source_event_id == record.source_event_id
                && entry.request_digest == record.request_digest
                && entry.effect_reference_tick == record.effect_reference_tick
                && matches!(
                    &entry.classification,
                    EpisodeProjectionSourceClassification::Quarantined {
                        affected_subject,
                        reason,
                    } if *affected_subject == record.affected_subject && reason == &record.reason
                ),
            "episode quarantine/source classification mismatch"
        );
        matched_quarantines.insert(record.source_row_id);
    }
    for entry in &snapshot.source_coverage {
        match &entry.classification {
            EpisodeProjectionSourceClassification::Irrelevant => anyhow::ensure!(
                !matched_episodes.contains(&entry.source_row_id)
                    && !matched_quarantines.contains(&entry.source_row_id),
                "irrelevant source row has projection material"
            ),
            EpisodeProjectionSourceClassification::Episode { .. } => anyhow::ensure!(
                matched_episodes.contains(&entry.source_row_id),
                "episode-classified source row has no receipt"
            ),
            EpisodeProjectionSourceClassification::Quarantined { .. } => anyhow::ensure!(
                matched_quarantines.contains(&entry.source_row_id),
                "quarantine-classified source row has no quarantine"
            ),
        }
    }
    Ok(())
}

fn constant_time_bytes_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn capture_generation_snapshot_from<D: ReadableDatabase>(
    db: &D,
) -> anyhow::Result<EpisodeProjectionGenerationSnapshot> {
    #[cfg(test)]
    GENERATION_SNAPSHOT_CAPTURES.with(|count| count.set(count.get() + 1));

    let read_txn = db.begin_read()?;
    let state = read_txn.open_table(EPISODE_PROJECTION_STATE)?;
    let receipts = read_txn.open_table(EPISODE_SOURCE_RECEIPTS)?;
    let episodes = read_txn.open_table(EPISODES)?;
    let archive = read_txn.open_table(ARCHIVE)?;
    let quarantine = read_txn.open_table(EPISODE_QUARANTINE)?;
    capture_generation_snapshot_from_tables(&state, &receipts, &episodes, &archive, &quarantine)
}

fn capture_generation_snapshot_from_tables<S, R, E, A, Q>(
    state: &S,
    receipts: &R,
    episodes: &E,
    archive: &A,
    quarantine: &Q,
) -> anyhow::Result<EpisodeProjectionGenerationSnapshot>
where
    S: ReadableTable<&'static str, &'static [u8]>,
    R: ReadableTable<&'static str, &'static [u8]>,
    E: ReadableTable<&'static str, &'static [u8]>,
    A: ReadableTable<&'static str, &'static [u8]>,
    Q: ReadableTable<&'static str, &'static [u8]>,
{
    let control: EpisodeProjectionControl = state
        .get(EPISODE_PROJECTION_CONTROL_KEY)?
        .map(|value| serde_json::from_slice(value.value()))
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;
    validate_projection_control(&control)?;

    let frontier_prefix = format!("frontier{KEY_SEPARATOR}");
    let source_prefix = format!("source{KEY_SEPARATOR}");
    let mut subjects = Vec::new();
    for entry in state.iter()? {
        let (key, value) = entry?;
        if !key.value().starts_with(&frontier_prefix) {
            continue;
        }
        let frontier: EpisodeProjectionFrontier = serde_json::from_slice(value.value())?;
        validate_projection_frontier(&frontier, frontier.subject, &control)?;
        anyhow::ensure!(
            key.value() == projection_frontier_key(frontier.subject),
            "episode projection generation frontier key/value mismatch"
        );
        let mut subject_receipts = Vec::new();
        for receipt_entry in receipts.iter()? {
            let (receipt_key, receipt_value) = receipt_entry?;
            if !receipt_key.value().starts_with(&source_prefix) {
                continue;
            }
            let receipt: EpisodeSourceReceipt = serde_json::from_slice(receipt_value.value())?;
            if receipt.subject == frontier.subject {
                anyhow::ensure!(
                    receipt_key.value()
                        == source_receipt_key(receipt.subject, &receipt.source_event_id),
                    "episode projection generation source receipt key/value mismatch"
                );
                subject_receipts.push(receipt);
            }
        }
        subject_receipts.sort_by(|left, right| {
            (left.source_row_id, left.source_event_id.as_str())
                .cmp(&(right.source_row_id, right.source_event_id.as_str()))
        });
        let live_episodes: Vec<Episode> = episodes
            .get(frontier.agent_name.as_str())?
            .map(|value| serde_json::from_slice(value.value()))
            .transpose()?
            .unwrap_or_default();
        let archived_episodes: Vec<Episode> = archive
            .get(frontier.agent_name.as_str())?
            .map(|value| serde_json::from_slice(value.value()))
            .transpose()?
            .unwrap_or_default();
        let agent = EpisodeProjectionAgent {
            subject: frontier.subject,
            agent_name: frontier.agent_name.clone(),
        };
        let coverage_digest = episode_projection_subject_coverage_digest(
            &agent,
            &frontier,
            &subject_receipts,
            &live_episodes,
            &archived_episodes,
        )?;
        subjects.push(EpisodeProjectionGenerationSubject {
            agent,
            frontier,
            receipts: subject_receipts,
            live_episodes,
            archived_episodes,
            coverage_digest,
        });
    }
    subjects.sort_by_key(|subject| subject.agent.subject.storage_key());

    let mut quarantines = Vec::new();
    for entry in quarantine.iter()? {
        let (key, value) = entry?;
        let record: EpisodeProjectionQuarantine = serde_json::from_slice(value.value())?;
        anyhow::ensure!(
            key.value() == quarantine_key(record.source_row_id, &record.source_event_id),
            "episode projection generation quarantine key/value mismatch"
        );
        quarantines.push(record);
    }
    quarantines.sort_by(|left, right| {
        (left.source_row_id, left.source_event_id.as_str())
            .cmp(&(right.source_row_id, right.source_event_id.as_str()))
    });
    let source_cut = episode_projection_source_cut_coverage(
        control.start_policy.source_row_id(),
        control.start_policy.source_row_id(),
        EPISODE_PROJECTION_TICK_DURATION_MILLIS,
        &[],
    )?;
    let archive_snapshot_digest = episode_projection_archive_snapshot_digest(&subjects)?;
    Ok(EpisodeProjectionGenerationSnapshot {
        control,
        subjects,
        quarantines,
        source_cut,
        source_coverage: Vec::new(),
        archive_snapshot_digest,
    })
}

fn archive_snapshot_digest_from_table<A>(
    archive: &A,
    subjects: &[EpisodeProjectionGenerationSubject],
) -> anyhow::Result<String>
where
    A: ReadableTable<&'static str, &'static [u8]>,
{
    let mut current = subjects.to_vec();
    for subject in &mut current {
        subject.archived_episodes = archive
            .get(subject.agent.agent_name.as_str())?
            .map(|value| serde_json::from_slice(value.value()))
            .transpose()?
            .unwrap_or_default();
    }
    episode_projection_archive_snapshot_digest(&current)
}

fn apply_generation_snapshot_to_tables(
    snapshot: &EpisodeProjectionGenerationSnapshot,
    state: &mut redb::Table<'_, &str, &[u8]>,
    receipts: &mut redb::Table<'_, &str, &[u8]>,
    episodes: &mut redb::Table<'_, &str, &[u8]>,
    quarantine: &mut redb::Table<'_, &str, &[u8]>,
) -> anyhow::Result<()> {
    let frontier_prefix = format!("frontier{KEY_SEPARATOR}");
    let mut state_keys = Vec::new();
    let mut old_names = HashSet::new();
    for entry in state.iter()? {
        let (key, value) = entry?;
        if key.value().starts_with(&frontier_prefix) {
            let frontier: EpisodeProjectionFrontier = serde_json::from_slice(value.value())?;
            old_names.insert(frontier.agent_name);
            state_keys.push(key.value().to_string());
        }
    }
    for key in state_keys {
        state.remove(key.as_str())?;
    }
    let receipt_keys: Vec<String> = receipts
        .iter()?
        .map(|entry| entry.map(|(key, _)| key.value().to_string()))
        .collect::<Result<_, _>>()?;
    for key in receipt_keys {
        receipts.remove(key.as_str())?;
    }
    let quarantine_keys: Vec<String> = quarantine
        .iter()?
        .map(|entry| entry.map(|(key, _)| key.value().to_string()))
        .collect::<Result<_, _>>()?;
    for key in quarantine_keys {
        quarantine.remove(key.as_str())?;
    }

    for subject in &snapshot.subjects {
        old_names.insert(subject.agent.agent_name.clone());
    }
    for name in old_names {
        episodes.remove(name.as_str())?;
    }

    insert_json(state, EPISODE_PROJECTION_CONTROL_KEY, &snapshot.control)?;
    for subject in &snapshot.subjects {
        insert_json(
            state,
            &projection_frontier_key(subject.agent.subject),
            &subject.frontier,
        )?;
        if !subject.live_episodes.is_empty() {
            insert_json(episodes, &subject.agent.agent_name, &subject.live_episodes)?;
        }
        for receipt in &subject.receipts {
            insert_json(
                receipts,
                &source_receipt_key(receipt.subject, &receipt.source_event_id),
                receipt,
            )?;
            insert_json(
                receipts,
                &episode_identity_key(receipt.subject, receipt.episode_id),
                receipt,
            )?;
        }
    }
    for record in &snapshot.quarantines {
        insert_json(
            quarantine,
            &quarantine_key(record.source_row_id, &record.source_event_id),
            record,
        )?;
    }
    Ok(())
}

fn load_generation_control(
    generations: &redb::Table<'_, &str, &[u8]>,
) -> anyhow::Result<EpisodeProjectionGenerationControl> {
    table_json_value(generations, EPISODE_PROJECTION_GENERATION_CONTROL_KEY)?
        .ok_or_else(|| anyhow::anyhow!("episode projection generation control is missing"))
}

fn ensure_no_open_generation_transition(
    generations: &redb::Table<'_, &str, &[u8]>,
) -> anyhow::Result<()> {
    for entry in generations.iter()? {
        let (key, value) = entry?;
        if key.value() == EPISODE_PROJECTION_GENERATION_CONTROL_KEY {
            continue;
        }
        let record: EpisodeProjectionGenerationRecord = serde_json::from_slice(value.value())?;
        anyhow::ensure!(
            !matches!(
                record.phase,
                EpisodeProjectionGenerationPhase::Building
                    | EpisodeProjectionGenerationPhase::Validated
            ),
            "another episode projection generation transition is already open"
        );
    }
    Ok(())
}

fn validated_generation_record_seals(
    generations: &impl ReadableTable<&'static str, &'static [u8]>,
    control: &EpisodeProjectionGenerationControl,
) -> anyhow::Result<Vec<EpisodeProjectionGenerationRecordSeal>> {
    validate_generation_id(&control.active_generation_id)?;
    let mut active_count = 0_usize;
    let mut open_transition_count = 0_usize;
    let mut seals = Vec::new();
    for entry in generations.iter()? {
        let (key, value) = entry?;
        if key.value() == EPISODE_PROJECTION_GENERATION_CONTROL_KEY {
            continue;
        }
        let record: EpisodeProjectionGenerationRecord = serde_json::from_slice(value.value())?;
        #[cfg(test)]
        GENERATION_RECORD_DEEP_VALIDATIONS.with(|count| count.set(count.get() + 1));
        validate_generation_descriptor(&record.descriptor)?;
        validate_generation_snapshot(
            &record.descriptor,
            &record.snapshot,
            record.descriptor.parent_generation_id.as_deref(),
            matches!(
                record.phase,
                EpisodeProjectionGenerationPhase::Building
                    | EpisodeProjectionGenerationPhase::Validated
            ),
        )?;
        anyhow::ensure!(
            key.value() == generation_record_key(&record.descriptor.generation_id),
            "episode projection generation key/value mismatch"
        );
        let digest = episode_projection_candidate_digest(&record.snapshot)?;
        anyhow::ensure!(
            constant_time_bytes_eq(digest.as_bytes(), record.candidate_digest.as_bytes()),
            "episode projection generation candidate digest mismatch"
        );
        if record.phase == EpisodeProjectionGenerationPhase::Active {
            active_count += 1;
            anyhow::ensure!(
                record.descriptor.generation_id == control.active_generation_id,
                "episode projection active generation/control mismatch"
            );
        }
        if matches!(
            record.phase,
            EpisodeProjectionGenerationPhase::Building
                | EpisodeProjectionGenerationPhase::Validated
        ) {
            open_transition_count += 1;
        }
        seals.push(EpisodeProjectionGenerationRecordSeal {
            generation_id: record.descriptor.generation_id,
            phase: record.phase,
            candidate_digest: record.candidate_digest,
            encoded_record_digest: projection_sha256(
                b"sentinel-episode-projection-generation-record-v1",
                value.value(),
            ),
        });
    }
    anyhow::ensure!(
        active_count == 1,
        "episode projection must have exactly one active generation"
    );
    anyhow::ensure!(
        open_transition_count <= 1,
        "episode projection has multiple open generation transitions"
    );
    seals.sort_by(|left, right| left.generation_id.cmp(&right.generation_id));
    Ok(seals)
}

fn generation_control_seal_digest(
    control: &EpisodeProjectionGenerationControl,
    record_seals: &[EpisodeProjectionGenerationRecordSeal],
) -> anyhow::Result<String> {
    let material = serde_json::to_vec(&(
        1_u16,
        control.active_generation_id.as_str(),
        control.activation_epoch,
        record_seals,
    ))?;
    Ok(projection_sha256(
        b"sentinel-episode-projection-generation-control-v1",
        &material,
    ))
}

fn seal_generation_control(
    generations: &mut redb::Table<'_, &str, &[u8]>,
    mut control: EpisodeProjectionGenerationControl,
) -> anyhow::Result<EpisodeProjectionGenerationControl> {
    control.record_seals = validated_generation_record_seals(generations, &control)?;
    control.seal_digest = Some(generation_control_seal_digest(
        &control,
        &control.record_seals,
    )?);
    insert_json(
        generations,
        EPISODE_PROJECTION_GENERATION_CONTROL_KEY,
        &control,
    )?;
    Ok(control)
}

fn generation_readiness_blocks_from_seal<T>(
    generations: &T,
    control: &EpisodeProjectionGenerationControl,
) -> anyhow::Result<Vec<EpisodeProjectionReadinessBlock>>
where
    T: ReadableTable<&'static str, &'static [u8]>,
{
    validate_generation_id(&control.active_generation_id)?;
    anyhow::ensure!(
        !control.record_seals.is_empty() && control.seal_digest.is_some(),
        "episode projection generation control is not sealed"
    );
    let expected_control_digest = generation_control_seal_digest(control, &control.record_seals)?;
    let seal_digest = control
        .seal_digest
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("episode projection generation control seal is missing"))?;
    anyhow::ensure!(
        constant_time_bytes_eq(expected_control_digest.as_bytes(), seal_digest.as_bytes(),),
        "episode projection generation control seal mismatch"
    );

    let mut previous_generation_id: Option<&str> = None;
    let mut active_count = 0_usize;
    let mut open_transition_count = 0_usize;
    let mut expected_keys = HashSet::new();
    let mut blockers = Vec::new();
    for seal in &control.record_seals {
        validate_generation_id(&seal.generation_id)?;
        anyhow::ensure!(
            previous_generation_id.is_none_or(|previous| previous < seal.generation_id.as_str()),
            "episode projection generation seals must be unique and sorted"
        );
        previous_generation_id = Some(&seal.generation_id);
        anyhow::ensure!(
            is_lower_sha256_hex(&seal.candidate_digest)
                && is_lower_sha256_hex(&seal.encoded_record_digest),
            "episode projection generation record seal is invalid"
        );
        let key = generation_record_key(&seal.generation_id);
        anyhow::ensure!(
            expected_keys.insert(key.clone()),
            "duplicate episode projection generation seal"
        );
        let value = generations
            .get(key.as_str())?
            .ok_or_else(|| anyhow::anyhow!("sealed episode projection generation is missing"))?;
        let encoded_digest = projection_sha256(
            b"sentinel-episode-projection-generation-record-v1",
            value.value(),
        );
        anyhow::ensure!(
            constant_time_bytes_eq(
                encoded_digest.as_bytes(),
                seal.encoded_record_digest.as_bytes(),
            ),
            "episode projection generation encoded record seal mismatch"
        );
        match seal.phase {
            EpisodeProjectionGenerationPhase::Active => {
                active_count += 1;
                anyhow::ensure!(
                    seal.generation_id == control.active_generation_id,
                    "episode projection active generation/control seal mismatch"
                );
            }
            phase @ (EpisodeProjectionGenerationPhase::Building
            | EpisodeProjectionGenerationPhase::Validated) => {
                open_transition_count += 1;
                blockers.push(EpisodeProjectionReadinessBlock::GenerationTransition {
                    generation_id: seal.generation_id.clone(),
                    phase,
                });
            }
            EpisodeProjectionGenerationPhase::Retained => {}
        }
    }
    anyhow::ensure!(
        active_count == 1,
        "episode projection generation seal must have exactly one active generation"
    );
    anyhow::ensure!(
        open_transition_count <= 1,
        "episode projection generation seal has multiple open transitions"
    );

    let mut observed_keys = HashSet::new();
    for entry in generations.iter()? {
        let (key, _) = entry?;
        if key.value() != EPISODE_PROJECTION_GENERATION_CONTROL_KEY {
            observed_keys.insert(key.value().to_string());
        }
    }
    anyhow::ensure!(
        observed_keys == expected_keys,
        "episode projection generation seal inventory mismatch"
    );
    Ok(blockers)
}

fn load_generation_status_from<D: ReadableDatabase>(
    db: &D,
) -> anyhow::Result<EpisodeProjectionGenerationStatus> {
    let read_txn = db.begin_read()?;
    let generations = read_txn.open_table(EPISODE_PROJECTION_GENERATIONS)?;
    let control: EpisodeProjectionGenerationControl = generations
        .get(EPISODE_PROJECTION_GENERATION_CONTROL_KEY)?
        .map(|value| serde_json::from_slice(value.value()))
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("episode projection generation control is missing"))?;
    validate_generation_id(&control.active_generation_id)?;
    let mut infos = Vec::new();
    let mut active_count = 0_usize;
    for entry in generations.iter()? {
        let (key, value) = entry?;
        if key.value() == EPISODE_PROJECTION_GENERATION_CONTROL_KEY {
            continue;
        }
        let record: EpisodeProjectionGenerationRecord = serde_json::from_slice(value.value())?;
        validate_generation_descriptor(&record.descriptor)?;
        validate_generation_snapshot(
            &record.descriptor,
            &record.snapshot,
            record.descriptor.parent_generation_id.as_deref(),
            matches!(
                record.phase,
                EpisodeProjectionGenerationPhase::Building
                    | EpisodeProjectionGenerationPhase::Validated
            ),
        )?;
        anyhow::ensure!(
            key.value() == generation_record_key(&record.descriptor.generation_id),
            "episode projection generation key/value mismatch"
        );
        let digest = episode_projection_candidate_digest(&record.snapshot)?;
        anyhow::ensure!(
            constant_time_bytes_eq(digest.as_bytes(), record.candidate_digest.as_bytes()),
            "episode projection generation candidate digest mismatch"
        );
        if record.phase == EpisodeProjectionGenerationPhase::Active {
            active_count += 1;
            anyhow::ensure!(
                record.descriptor.generation_id == control.active_generation_id,
                "episode projection active generation/control mismatch"
            );
        }
        infos.push(EpisodeProjectionGenerationInfo {
            descriptor: record.descriptor,
            phase: record.phase,
            candidate_digest: record.candidate_digest,
            snapshot_source_cut: record.snapshot.source_cut,
            snapshot_archive_digest: record.snapshot.archive_snapshot_digest,
        });
    }
    anyhow::ensure!(
        active_count == 1,
        "episode projection must have exactly one active generation"
    );
    infos.sort_by(|left, right| {
        left.descriptor
            .generation_id
            .cmp(&right.descriptor.generation_id)
    });
    Ok(EpisodeProjectionGenerationStatus {
        active_generation_id: control.active_generation_id,
        activation_epoch: control.activation_epoch,
        generations: infos,
    })
}

fn generation_readiness_blocks<T>(
    generations: &T,
) -> anyhow::Result<Vec<EpisodeProjectionReadinessBlock>>
where
    T: ReadableTable<&'static str, &'static [u8]>,
{
    let control: EpisodeProjectionGenerationControl = generations
        .get(EPISODE_PROJECTION_GENERATION_CONTROL_KEY)?
        .map(|value| serde_json::from_slice(value.value()))
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("episode projection generation control is missing"))?;
    validate_generation_id(&control.active_generation_id)?;
    if !control.record_seals.is_empty() || control.seal_digest.is_some() {
        anyhow::ensure!(
            !control.record_seals.is_empty() && control.seal_digest.is_some(),
            "episode projection generation control seal is incomplete"
        );
        return generation_readiness_blocks_from_seal(generations, &control);
    }
    let mut active_count = 0_usize;
    let mut blockers = Vec::new();
    for entry in generations.iter()? {
        let (key, value) = entry?;
        if key.value() == EPISODE_PROJECTION_GENERATION_CONTROL_KEY {
            continue;
        }
        let record: EpisodeProjectionGenerationRecord = serde_json::from_slice(value.value())?;
        validate_generation_descriptor(&record.descriptor)?;
        validate_generation_snapshot(
            &record.descriptor,
            &record.snapshot,
            record.descriptor.parent_generation_id.as_deref(),
            matches!(
                record.phase,
                EpisodeProjectionGenerationPhase::Building
                    | EpisodeProjectionGenerationPhase::Validated
            ),
        )?;
        anyhow::ensure!(
            key.value() == generation_record_key(&record.descriptor.generation_id),
            "episode projection generation key/value mismatch"
        );
        let digest = episode_projection_candidate_digest(&record.snapshot)?;
        anyhow::ensure!(
            constant_time_bytes_eq(digest.as_bytes(), record.candidate_digest.as_bytes()),
            "episode projection generation candidate digest mismatch"
        );
        match record.phase {
            EpisodeProjectionGenerationPhase::Active => {
                active_count += 1;
                anyhow::ensure!(
                    record.descriptor.generation_id == control.active_generation_id,
                    "episode projection active generation/control mismatch"
                );
            }
            phase @ (EpisodeProjectionGenerationPhase::Building
            | EpisodeProjectionGenerationPhase::Validated) => {
                blockers.push(EpisodeProjectionReadinessBlock::GenerationTransition {
                    generation_id: record.descriptor.generation_id,
                    phase,
                });
            }
            EpisodeProjectionGenerationPhase::Retained => {}
        }
    }
    anyhow::ensure!(
        active_count == 1,
        "episode projection must have exactly one active generation"
    );
    Ok(blockers)
}

fn projection_frontier_key(subject: EpisodeProjectionSubject) -> String {
    format!("frontier{KEY_SEPARATOR}{}", subject.storage_key())
}

fn source_receipt_key(subject: EpisodeProjectionSubject, source_event_id: &str) -> String {
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

fn validate_cutover_receipt(receipt: &EpisodeProjectionCutoverReceipt) -> anyhow::Result<()> {
    anyhow::ensure!(
        receipt.projection_version == EPISODE_PROJECTION_VERSION,
        "unsupported episode projection cutover version {}",
        receipt.projection_version
    );
    anyhow::ensure!(
        receipt.source_row_id >= 0,
        "episode projection cutover row must be non-negative"
    );
    anyhow::ensure!(
        is_sha256_hex(&receipt.legacy_state_digest)
            && is_sha256_hex(&receipt.source_cut_digest)
            && is_sha256_hex(&receipt.authorization_digest),
        "episode projection cutover digests must be SHA-256 hex"
    );
    Ok(())
}

fn validate_cutover_binding(
    start_policy: &EpisodeProjectionStartPolicy,
    receipt: &EpisodeProjectionCutoverReceipt,
) -> anyhow::Result<()> {
    validate_cutover_receipt(receipt)?;
    match start_policy {
        EpisodeProjectionStartPolicy::RecoveryCut {
            source_row_id,
            proof_digest,
        } => anyhow::ensure!(
            *source_row_id == receipt.source_row_id
                && proof_digest == &receipt.authorization_digest,
            "episode projection cutover receipt does not bind the recovery policy"
        ),
        EpisodeProjectionStartPolicy::Beginning
        | EpisodeProjectionStartPolicy::ExplicitPosition { .. } => {
            anyhow::bail!("episode projection cutover receipt requires a recovery-cut policy")
        }
    }
    Ok(())
}

fn validate_persisted_cutover(
    state: &redb::Table<'_, &str, &[u8]>,
    control: &EpisodeProjectionControl,
) -> anyhow::Result<()> {
    let receipt: Option<EpisodeProjectionCutoverReceipt> =
        table_json_value(state, EPISODE_PROJECTION_CUTOVER_KEY)?;
    validate_persisted_cutover_value(control, receipt)
}

fn validate_persisted_cutover_value(
    control: &EpisodeProjectionControl,
    receipt: Option<EpisodeProjectionCutoverReceipt>,
) -> anyhow::Result<()> {
    match (&control.start_policy, receipt) {
        (EpisodeProjectionStartPolicy::Beginning, None) => Ok(()),
        (EpisodeProjectionStartPolicy::RecoveryCut { .. }, Some(receipt)) => {
            validate_cutover_binding(&control.start_policy, &receipt)
        }
        (EpisodeProjectionStartPolicy::ExplicitPosition { .. }, _) => {
            anyhow::bail!("unauthenticated explicit projection position is not supported")
        }
        _ => anyhow::bail!("episode projection cutover receipt/control mismatch"),
    }
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
    archived_episodes: &[Episode],
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
        anyhow::ensure!(
            receipt.episode_id != 0,
            "episode receipt ID must be non-zero"
        );
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
            .chain(archived_episodes.iter())
            .filter(|episode| {
                episode.id == last_receipt.episode_id && episode.agent_name == frontier.agent_name
            })
            .count();
        anyhow::ensure!(
            retained_matches == 1,
            "maximal source receipt episode is missing or duplicated in live/archive buckets"
        );
    } else {
        anyhow::ensure!(
            frontier.applied_count == 0 && frontier.last_request_digest.is_none(),
            "zero-receipt frontier contains contradictory applied material"
        );
    }
    Ok(())
}

fn validate_frontier_tip_integrity(
    subject: EpisodeProjectionSubject,
    frontier: &EpisodeProjectionFrontier,
    receipts: &redb::Table<'_, &str, &[u8]>,
    retained_episodes: &[Episode],
    archived_episodes: &[Episode],
) -> anyhow::Result<()> {
    if frontier.applied_count == 0 {
        anyhow::ensure!(
            frontier.last_request_digest.is_none(),
            "zero-receipt frontier contains contradictory applied material"
        );
        return Ok(());
    }
    let source_event_id = frontier
        .last_source_event_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("applied episode frontier lacks a source event"))?;
    let receipt: EpisodeSourceReceipt =
        table_json_value(receipts, &source_receipt_key(subject, source_event_id))?
            .ok_or_else(|| anyhow::anyhow!("episode frontier tip source receipt is missing"))?;
    validate_receipt_pair(receipts, &receipt)?;
    anyhow::ensure!(
        receipt.subject == subject
            && receipt.agent_name == frontier.agent_name
            && receipt.projection_version == frontier.projection_version
            && receipt.source_row_id == frontier.last_source_row_id
            && frontier.last_request_digest.as_deref() == Some(receipt.request_digest.as_str()),
        "episode frontier tip receipt does not match the frontier"
    );
    let retained_matches = retained_episodes
        .iter()
        .chain(archived_episodes.iter())
        .filter(|episode| {
            episode.id == receipt.episode_id && episode.agent_name == frontier.agent_name
        })
        .count();
    anyhow::ensure!(
        retained_matches == 1,
        "episode frontier tip is missing or duplicated across live/archive buckets"
    );
    Ok(())
}

fn validate_receipt_pair(
    receipts: &redb::Table<'_, &str, &[u8]>,
    receipt: &EpisodeSourceReceipt,
) -> anyhow::Result<()> {
    receipt.subject.validate()?;
    validate_projection_key_part(&receipt.agent_name, "receipt agent name")?;
    validate_projection_source(
        &receipt.source_event_id,
        receipt.source_row_id,
        receipt.projection_version,
        &receipt.request_digest,
    )?;
    anyhow::ensure!(
        receipt.episode_id != 0,
        "episode receipt ID must be non-zero"
    );
    let source_key = source_receipt_key(receipt.subject, &receipt.source_event_id);
    let identity_key = episode_identity_key(receipt.subject, receipt.episode_id);
    let source = receipts
        .get(source_key.as_str())?
        .ok_or_else(|| anyhow::anyhow!("missing episode source receipt counterpart"))?;
    let identity = receipts
        .get(identity_key.as_str())?
        .ok_or_else(|| anyhow::anyhow!("missing episode identity receipt counterpart"))?;
    anyhow::ensure!(
        source.value() == identity.value()
            && serde_json::from_slice::<EpisodeSourceReceipt>(source.value())? == *receipt,
        "episode receipt counterparts do not match exactly"
    );
    Ok(())
}

fn projection_admission_from_records<'a>(
    records: impl IntoIterator<Item = &'a EpisodeProjectionQuarantine>,
    subject: Option<EpisodeProjectionSubject>,
    source_row_id: i64,
) -> anyhow::Result<EpisodeProjectionAdmission> {
    let mut applicable: Vec<&EpisodeProjectionQuarantine> = Vec::new();
    for record in records {
        validate_projection_quarantine(record)?;
        if record.source_row_id >= source_row_id {
            continue;
        }
        let global = !matches!(
            record.affected_subject,
            Some(EpisodeProjectionSubject::Agent { .. })
        );
        if global || record.affected_subject == subject {
            applicable.push(record);
        }
    }
    let Some(blocker) = applicable
        .into_iter()
        .min_by_key(|record| record.source_row_id)
    else {
        return Ok(EpisodeProjectionAdmission::Allowed);
    };
    if matches!(
        blocker.affected_subject,
        Some(EpisodeProjectionSubject::Agent { .. })
    ) {
        Ok(EpisodeProjectionAdmission::SubjectBlocked(blocker.clone()))
    } else {
        Ok(EpisodeProjectionAdmission::GloballyBlocked(blocker.clone()))
    }
}

fn projection_admission_from_table(
    quarantine: &redb::Table<'_, &str, &[u8]>,
    subject: Option<EpisodeProjectionSubject>,
    source_row_id: i64,
) -> anyhow::Result<EpisodeProjectionAdmission> {
    let mut records = Vec::new();
    for entry in quarantine.iter()? {
        let (_, value) = entry?;
        records.push(serde_json::from_slice(value.value())?);
    }
    projection_admission_from_records(&records, subject, source_row_id)
}

fn ensure_projection_admitted(
    quarantine: &redb::Table<'_, &str, &[u8]>,
    subject: Option<EpisodeProjectionSubject>,
    source_row_id: i64,
) -> anyhow::Result<()> {
    match projection_admission_from_table(quarantine, subject, source_row_id)? {
        EpisodeProjectionAdmission::Allowed => Ok(()),
        EpisodeProjectionAdmission::SubjectBlocked(blocker) => anyhow::bail!(
            "episode projection subject is fenced by quarantine row {}",
            blocker.source_row_id
        ),
        EpisodeProjectionAdmission::GloballyBlocked(blocker) => anyhow::bail!(
            "episode projection is globally fenced by quarantine row {}",
            blocker.source_row_id
        ),
    }
}

fn validate_resolution_admission(
    quarantine: &redb::Table<'_, &str, &[u8]>,
    record: &EpisodeProjectionQuarantine,
    input: &EpisodeProjectionWrite,
    global_frontier: i64,
) -> anyhow::Result<()> {
    validate_projection_quarantine(record)?;
    anyhow::ensure!(
        record.source_event_id == input.source_event_id
            && record.source_row_id == input.source_row_id
            && record.projection_version == input.projection_version
            && record.request_digest == input.request_digest
            && record.effect_reference_tick == input.effect_reference_tick,
        "episode projection resolution does not bind the immutable source row"
    );
    if let Some(subject) = record.affected_subject {
        anyhow::ensure!(
            subject == input.subject || !matches!(subject, EpisodeProjectionSubject::Agent { .. }),
            "episode projection resolution subject rebinding rejected"
        );
    }
    anyhow::ensure!(
        global_frontier >= record.source_row_id,
        "episode projection resolution row is beyond the durable scan cursor"
    );
    let key = quarantine_key(record.source_row_id, &record.source_event_id);
    let persisted: EpisodeProjectionQuarantine = table_json_value(quarantine, &key)?
        .ok_or_else(|| anyhow::anyhow!("episode projection quarantine is already resolved"))?;
    anyhow::ensure!(
        persisted == *record,
        "episode projection resolution quarantine CAS conflict"
    );
    let target_is_global = !matches!(
        record.affected_subject,
        Some(EpisodeProjectionSubject::Agent { .. })
    );
    for entry in quarantine.iter()? {
        let (_, value) = entry?;
        let earlier: EpisodeProjectionQuarantine = serde_json::from_slice(value.value())?;
        if earlier.source_row_id >= record.source_row_id {
            continue;
        }
        let earlier_is_global = !matches!(
            earlier.affected_subject,
            Some(EpisodeProjectionSubject::Agent { .. })
        );
        anyhow::ensure!(
            !(target_is_global
                || earlier_is_global
                || earlier.affected_subject == record.affected_subject),
            "episode projection quarantine must resolve in source order"
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
            && existing.source_row_id == input.source_row_id
            && existing.projection_version == input.projection_version
            && existing.request_digest == input.request_digest
            && existing.episode_id == input.episode.id
            && existing.effect_reference_tick == input.effect_reference_tick,
        "episode source receipt replay conflict"
    );
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_lower_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
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

    fn cutover_receipt(source_row_id: i64) -> EpisodeProjectionCutoverReceipt {
        EpisodeProjectionCutoverReceipt {
            projection_version: EPISODE_PROJECTION_VERSION,
            source_row_id,
            legacy_state_digest: "bc".repeat(32),
            source_cut_digest: "cd".repeat(32),
            authorization_digest: "ab".repeat(32),
        }
    }

    #[test]
    fn existing_episode_projection_skips_generation_snapshot_rebuild() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        let agent = projection_agent();

        GENERATION_SNAPSHOT_CAPTURES.with(|count| count.set(0));
        GENERATION_RECORD_DEEP_VALIDATIONS.with(|count| count.set(0));
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                std::slice::from_ref(&agent),
            )
            .unwrap();
        assert_eq!(GENERATION_SNAPSHOT_CAPTURES.with(Cell::get), 1);
        assert_eq!(GENERATION_RECORD_DEEP_VALIDATIONS.with(Cell::get), 1);

        GENERATION_SNAPSHOT_CAPTURES.with(|count| count.set(0));
        GENERATION_RECORD_DEEP_VALIDATIONS.with(|count| count.set(0));
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                std::slice::from_ref(&agent),
            )
            .unwrap();
        assert_eq!(GENERATION_SNAPSHOT_CAPTURES.with(Cell::get), 0);
        assert_eq!(GENERATION_RECORD_DEEP_VALIDATIONS.with(Cell::get), 0);
    }

    #[test]
    fn legacy_generation_control_is_sealed_once_then_uses_bounded_validation() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        let agent = projection_agent();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                std::slice::from_ref(&agent),
            )
            .unwrap();

        let write_txn = store.db.begin_write().unwrap();
        {
            let mut generations = write_txn
                .open_table(EPISODE_PROJECTION_GENERATIONS)
                .unwrap();
            let mut control = load_generation_control(&generations).unwrap();
            control.record_seals.clear();
            control.seal_digest = None;
            insert_json(
                &mut generations,
                EPISODE_PROJECTION_GENERATION_CONTROL_KEY,
                &control,
            )
            .unwrap();
        }
        write_txn.commit().unwrap();

        GENERATION_RECORD_DEEP_VALIDATIONS.with(|count| count.set(0));
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                std::slice::from_ref(&agent),
            )
            .unwrap();
        assert_eq!(GENERATION_RECORD_DEEP_VALIDATIONS.with(Cell::get), 1);

        GENERATION_RECORD_DEEP_VALIDATIONS.with(|count| count.set(0));
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                std::slice::from_ref(&agent),
            )
            .unwrap();
        assert_eq!(GENERATION_RECORD_DEEP_VALIDATIONS.with(Cell::get), 0);
    }

    fn overwrite_projection_control(store: &HippocampusStore, control: &EpisodeProjectionControl) {
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
            insert_json(&mut state, &projection_frontier_key(key_subject), frontier).unwrap();
        }
        write_txn.commit().unwrap();
    }

    fn committed_projection_store() -> (HippocampusStore, tempfile::TempDir, EpisodeProjectionWrite)
    {
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
            effect_reference_tick: row as u64,
            episode: make_episode(episode_id, source_event_id),
        }
    }

    fn generation_candidate(
        store: &HippocampusStore,
        row: i64,
        source_event_id: &str,
        episode_id: u64,
    ) -> EpisodeProjectionGenerationCandidate {
        let status = store.load_episode_projection_generation_status().unwrap();
        let mut control = store.load_episode_projection_control().unwrap().unwrap();
        let mut frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        let request_digest = format!("{row:064x}");
        let receipt = EpisodeSourceReceipt {
            subject: projection_subject(),
            agent_name: "Thomas".to_string(),
            source_event_id: source_event_id.to_string(),
            source_row_id: row,
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: request_digest.clone(),
            episode_id,
            effect_reference_tick: row as u64,
        };
        control.last_source_row_id = row;
        control.last_source_event_id = Some(source_event_id.to_string());
        control.effect_reference_tick = row as u64;
        frontier.last_source_row_id = row;
        frontier.last_source_event_id = Some(source_event_id.to_string());
        frontier.last_request_digest = Some(request_digest);
        frontier.applied_count = 1;
        let agent = projection_agent();
        let receipts = vec![receipt];
        let source_coverage = vec![EpisodeProjectionSourceCoverageEntry {
            source_row_id: row,
            source_event_id: source_event_id.to_string(),
            source_tick: row as u64,
            effect_reference_tick: row as u64,
            request_digest: format!("{row:064x}"),
            classification: EpisodeProjectionSourceClassification::Episode {
                subject: projection_subject(),
            },
        }];
        let source_cut = episode_projection_source_cut_coverage(
            0,
            row,
            EPISODE_PROJECTION_TICK_DURATION_MILLIS,
            &source_coverage,
        )
        .unwrap();
        let live_episodes = vec![make_episode(episode_id, source_event_id)];
        let archived_episodes = store.load_archive("Thomas").unwrap();
        let coverage_digest = episode_projection_subject_coverage_digest(
            &agent,
            &frontier,
            &receipts,
            &live_episodes,
            &archived_episodes,
        )
        .unwrap();
        let subject = EpisodeProjectionGenerationSubject {
            agent,
            frontier,
            receipts,
            live_episodes,
            archived_episodes,
            coverage_digest,
        };
        let archive_snapshot_digest =
            episode_projection_archive_snapshot_digest(std::slice::from_ref(&subject)).unwrap();
        let descriptor = EpisodeProjectionGenerationDescriptor {
            generation_id: episode_projection_generation_id(
                Some(&status.active_generation_id),
                EPISODE_PROJECTION_VERSION,
                &source_cut,
                &archive_snapshot_digest,
            ),
            parent_generation_id: Some(status.active_generation_id),
            projection_version: EPISODE_PROJECTION_VERSION,
            source_cut,
            archive_snapshot_digest,
        };
        EpisodeProjectionGenerationCandidate {
            descriptor,
            control,
            subjects: vec![subject],
            quarantines: Vec::new(),
            source_coverage,
        }
    }

    fn generation_evidence(
        candidate: &EpisodeProjectionGenerationCandidate,
    ) -> EpisodeProjectionSourceCutEvidence {
        EpisodeProjectionSourceCutEvidence {
            coverage: candidate.descriptor.source_cut.clone(),
            entries: candidate.source_coverage.clone(),
        }
    }

    fn generation_candidate_from_current(
        store: &HippocampusStore,
        parent_generation_id: &str,
        evidence: &EpisodeProjectionSourceCutEvidence,
    ) -> EpisodeProjectionGenerationCandidate {
        let mut snapshot = capture_generation_snapshot_from(&store.db).unwrap();
        snapshot.source_cut = evidence.coverage.clone();
        snapshot.source_coverage = evidence.entries.clone();
        let descriptor = EpisodeProjectionGenerationDescriptor {
            generation_id: episode_projection_generation_id(
                Some(parent_generation_id),
                EPISODE_PROJECTION_VERSION,
                &evidence.coverage,
                &snapshot.archive_snapshot_digest,
            ),
            parent_generation_id: Some(parent_generation_id.to_string()),
            projection_version: EPISODE_PROJECTION_VERSION,
            source_cut: evidence.coverage.clone(),
            archive_snapshot_digest: snapshot.archive_snapshot_digest.clone(),
        };
        EpisodeProjectionGenerationCandidate {
            descriptor,
            control: snapshot.control,
            subjects: snapshot.subjects,
            quarantines: snapshot.quarantines,
            source_coverage: snapshot.source_coverage,
        }
    }

    #[test]
    fn episode_projection_generation_validates_activates_and_rolls_back() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let baseline = store.load_episode_projection_generation_status().unwrap();
        let candidate = generation_candidate(&store, 1, "generation-event", 501);
        let candidate_evidence = generation_evidence(&candidate);
        let baseline_evidence = EpisodeProjectionSourceCutEvidence {
            coverage: baseline
                .generations
                .iter()
                .find(|generation| generation.phase == EpisodeProjectionGenerationPhase::Active)
                .unwrap()
                .snapshot_source_cut
                .clone(),
            entries: Vec::new(),
        };
        let digest = store
            .begin_episode_projection_generation(
                &candidate,
                &baseline.active_generation_id,
                &candidate_evidence,
            )
            .unwrap();
        let readiness = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap();
        assert!(matches!(
            readiness.blockers.as_slice(),
            [EpisodeProjectionReadinessBlock::GenerationTransition {
                phase: EpisodeProjectionGenerationPhase::Building,
                ..
            }]
        ));
        assert_eq!(
            store
                .validate_episode_projection_generation(
                    &candidate.descriptor.generation_id,
                    &baseline.active_generation_id,
                    &candidate_evidence,
                )
                .unwrap(),
            digest
        );
        assert!(store
            .activate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &"ff".repeat(32),
                &digest,
                &candidate_evidence,
                &baseline_evidence,
            )
            .is_err());
        assert!(store
            .activate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &baseline.active_generation_id,
                &"ee".repeat(32),
                &candidate_evidence,
                &baseline_evidence,
            )
            .is_err());
        assert!(!store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
        let activated = store
            .activate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &baseline.active_generation_id,
                &digest,
                &candidate_evidence,
                &baseline_evidence,
            )
            .unwrap();
        assert_eq!(
            activated.active_generation_id,
            candidate.descriptor.generation_id
        );
        assert!(store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
        assert_eq!(store.load_episodes("Thomas").unwrap()[0].id, 501);

        let retained = activated
            .generations
            .iter()
            .find(|generation| generation.descriptor.generation_id == baseline.active_generation_id)
            .unwrap();
        assert_eq!(retained.phase, EpisodeProjectionGenerationPhase::Retained);
        let before_retained_discard = store.load_episode_projection_generation_status().unwrap();
        assert!(store
            .discard_episode_projection_generation(
                &baseline.active_generation_id,
                &candidate.descriptor.generation_id,
                &retained.candidate_digest,
            )
            .is_err());
        assert_eq!(
            store.load_episode_projection_generation_status().unwrap(),
            before_retained_discard
        );
        let rolled_back = store
            .rollback_episode_projection_generation(
                &baseline.active_generation_id,
                &candidate.descriptor.generation_id,
                &retained.candidate_digest,
                &baseline_evidence,
                &candidate_evidence,
            )
            .unwrap();
        assert_eq!(
            rolled_back.active_generation_id,
            baseline.active_generation_id
        );
        assert!(store.load_episodes("Thomas").unwrap().is_empty());
    }

    #[test]
    fn episode_projection_generation_discard_is_cas_bound_and_reopens_readiness() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let baseline = store.load_episode_projection_generation_status().unwrap();
        let active = baseline.active_generation_id.clone();
        let active_digest = baseline.generations[0].candidate_digest.clone();
        let control_before = store.load_episode_projection_control().unwrap();
        let frontier_before = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap();
        let episodes_before = store.load_episodes("Thomas").unwrap();
        let quarantine_before = store.list_episode_projection_quarantine().unwrap();
        let candidate = generation_candidate(&store, 1, "discard-candidate", 711);
        let evidence = generation_evidence(&candidate);
        let digest = store
            .begin_episode_projection_generation(&candidate, &active, &evidence)
            .unwrap();
        assert!(!store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());

        let wrong_source = EpisodeProjectionSourceCutEvidence {
            coverage: baseline.generations[0].snapshot_source_cut.clone(),
            entries: Vec::new(),
        };
        assert!(store
            .validate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &wrong_source,
            )
            .is_err());
        let archive_drift = make_episode(900, "concurrent archive drift");
        store
            .append_archive("Thomas", std::slice::from_ref(&archive_drift))
            .unwrap();
        assert!(store
            .validate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &evidence,
            )
            .is_err());
        assert!(!store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
        store.store_archive("Thomas", &[]).unwrap();
        store
            .validate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &evidence,
            )
            .unwrap();

        let staged = store.load_episode_projection_generation_status().unwrap();
        let assert_unchanged = || {
            assert_eq!(
                store.load_episode_projection_generation_status().unwrap(),
                staged
            );
            assert_eq!(
                store.load_episode_projection_control().unwrap(),
                control_before
            );
            assert_eq!(
                store
                    .load_episode_projection_frontier(projection_subject())
                    .unwrap(),
                frontier_before
            );
            assert_eq!(
                serde_json::to_vec(&store.load_episodes("Thomas").unwrap()).unwrap(),
                serde_json::to_vec(&episodes_before).unwrap()
            );
            assert_eq!(
                store.list_episode_projection_quarantine().unwrap(),
                quarantine_before
            );
        };
        assert!(store
            .discard_episode_projection_generation(&"11".repeat(32), &active, &digest)
            .is_err());
        assert_unchanged();
        assert!(store
            .discard_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &"22".repeat(32),
                &digest,
            )
            .is_err());
        assert_unchanged();
        assert!(store
            .discard_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &"33".repeat(32),
            )
            .is_err());
        assert_unchanged();
        assert!(store
            .discard_episode_projection_generation(&active, &active, &active_digest)
            .is_err());
        assert_unchanged();

        let discarded = store
            .discard_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &digest,
            )
            .unwrap();
        assert_eq!(discarded, baseline);
        assert!(store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
        assert_eq!(
            store.load_episode_projection_control().unwrap(),
            control_before
        );
        assert_eq!(
            store
                .load_episode_projection_frontier(projection_subject())
                .unwrap(),
            frontier_before
        );
        assert_eq!(
            serde_json::to_vec(&store.load_episodes("Thomas").unwrap()).unwrap(),
            serde_json::to_vec(&episodes_before).unwrap()
        );
        assert_eq!(
            store.list_episode_projection_quarantine().unwrap(),
            quarantine_before
        );
    }

    #[test]
    fn episode_projection_generation_rejects_incomplete_duplicate_and_corrupt_candidates() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let active = store
            .load_episode_projection_generation_status()
            .unwrap()
            .active_generation_id;

        let mut missing = generation_candidate(&store, 1, "missing", 601);
        let missing_evidence = generation_evidence(&missing);
        missing.subjects.clear();
        assert!(store
            .begin_episode_projection_generation(&missing, &active, &missing_evidence)
            .is_err());

        let mut duplicate = generation_candidate(&store, 1, "duplicate", 602);
        let duplicate_evidence = generation_evidence(&duplicate);
        let duplicate_episode = duplicate.subjects[0].live_episodes[0].clone();
        duplicate.subjects[0].live_episodes.push(duplicate_episode);
        duplicate.subjects[0].coverage_digest = episode_projection_subject_coverage_digest(
            &duplicate.subjects[0].agent,
            &duplicate.subjects[0].frontier,
            &duplicate.subjects[0].receipts,
            &duplicate.subjects[0].live_episodes,
            &duplicate.subjects[0].archived_episodes,
        )
        .unwrap();
        assert!(store
            .begin_episode_projection_generation(&duplicate, &active, &duplicate_evidence)
            .is_err());

        let mut corrupt = generation_candidate(&store, 1, "corrupt", 603);
        let corrupt_evidence = generation_evidence(&corrupt);
        corrupt.subjects[0].coverage_digest = "00".repeat(32);
        assert!(store
            .begin_episode_projection_generation(&corrupt, &active, &corrupt_evidence)
            .is_err());
        assert_eq!(
            store
                .load_episode_projection_generation_status()
                .unwrap()
                .generations
                .len(),
            1
        );
        assert!(store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
    }

    #[test]
    fn episode_projection_generation_rejects_omission_reclassification_reorder_and_stale_cut() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let active = store
            .load_episode_projection_generation_status()
            .unwrap()
            .active_generation_id;
        let omitted = generation_candidate(&store, 2, "authoritative", 605);
        let mut authoritative_entries = vec![EpisodeProjectionSourceCoverageEntry {
            source_row_id: 1,
            source_event_id: "irrelevant-before-episode".to_string(),
            source_tick: 1,
            effect_reference_tick: 1,
            request_digest: "01".repeat(32),
            classification: EpisodeProjectionSourceClassification::Irrelevant,
        }];
        authoritative_entries.extend(omitted.source_coverage.clone());
        let authoritative = EpisodeProjectionSourceCutEvidence {
            coverage: episode_projection_source_cut_coverage(
                0,
                2,
                EPISODE_PROJECTION_TICK_DURATION_MILLIS,
                &authoritative_entries,
            )
            .unwrap(),
            entries: authoritative_entries,
        };
        // The candidate is internally consistent through row 2, but silently
        // omits row 1. Only the independently computed source cut detects it.
        validate_generation_snapshot(
            &omitted.descriptor,
            &generation_snapshot(&omitted),
            Some(&active),
            true,
        )
        .unwrap();
        assert!(store
            .begin_episode_projection_generation(&omitted, &active, &authoritative)
            .is_err());

        let authoritative_candidate = generation_candidate(&store, 1, "reclassified", 606);
        let authoritative_reclassification = generation_evidence(&authoritative_candidate);
        let mut reclassified = authoritative_candidate.clone();
        reclassified.subjects[0].frontier.last_source_row_id = 0;
        reclassified.subjects[0].frontier.last_source_event_id = None;
        reclassified.subjects[0].frontier.last_request_digest = None;
        reclassified.subjects[0].frontier.applied_count = 0;
        reclassified.subjects[0].receipts.clear();
        reclassified.subjects[0].live_episodes.clear();
        reclassified.source_coverage[0].classification =
            EpisodeProjectionSourceClassification::Irrelevant;
        reclassified.descriptor.source_cut = episode_projection_source_cut_coverage(
            0,
            1,
            EPISODE_PROJECTION_TICK_DURATION_MILLIS,
            &reclassified.source_coverage,
        )
        .unwrap();
        reclassified.descriptor.generation_id = episode_projection_generation_id(
            Some(&active),
            EPISODE_PROJECTION_VERSION,
            &reclassified.descriptor.source_cut,
            &reclassified.descriptor.archive_snapshot_digest,
        );
        reclassified.subjects[0].coverage_digest = episode_projection_subject_coverage_digest(
            &reclassified.subjects[0].agent,
            &reclassified.subjects[0].frontier,
            &reclassified.subjects[0].receipts,
            &reclassified.subjects[0].live_episodes,
            &reclassified.subjects[0].archived_episodes,
        )
        .unwrap();
        assert!(store
            .begin_episode_projection_generation(
                &reclassified,
                &active,
                &authoritative_reclassification,
            )
            .is_err());

        let mut reordered = authoritative.entries.clone();
        let mut second = reordered[0].clone();
        second.source_row_id = 2;
        second.source_event_id = "second".to_string();
        reordered.push(second);
        reordered.reverse();
        assert!(episode_projection_source_cut_coverage(
            0,
            2,
            EPISODE_PROJECTION_TICK_DURATION_MILLIS,
            &reordered,
        )
        .is_err());

        let stale = EpisodeProjectionSourceCutEvidence {
            coverage: episode_projection_source_cut_coverage(
                0,
                0,
                EPISODE_PROJECTION_TICK_DURATION_MILLIS,
                &[],
            )
            .unwrap(),
            entries: Vec::new(),
        };
        assert!(store
            .begin_episode_projection_generation(&authoritative_candidate, &active, &stale,)
            .is_err());
    }

    #[test]
    fn episode_projection_generation_rollback_restores_an_advanced_active_head() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let baseline = store.load_episode_projection_generation_status().unwrap();
        let baseline_evidence = EpisodeProjectionSourceCutEvidence {
            coverage: baseline.generations[0].snapshot_source_cut.clone(),
            entries: Vec::new(),
        };
        let first = generation_candidate(&store, 1, "generation-one", 701);
        let first_evidence = generation_evidence(&first);
        let first_digest = store
            .begin_episode_projection_generation(
                &first,
                &baseline.active_generation_id,
                &first_evidence,
            )
            .unwrap();
        store
            .validate_episode_projection_generation(
                &first.descriptor.generation_id,
                &baseline.active_generation_id,
                &first_evidence,
            )
            .unwrap();
        store
            .activate_episode_projection_generation(
                &first.descriptor.generation_id,
                &baseline.active_generation_id,
                &first_digest,
                &first_evidence,
                &baseline_evidence,
            )
            .unwrap();

        let later = projection_write(2, "after-activation", 702, 1);
        store.commit_episode_projection(&later).unwrap();
        let mut advanced_entries = first_evidence.entries.clone();
        advanced_entries.push(EpisodeProjectionSourceCoverageEntry {
            source_row_id: 2,
            source_event_id: later.source_event_id.clone(),
            source_tick: 2,
            effect_reference_tick: 2,
            request_digest: later.request_digest.clone(),
            classification: EpisodeProjectionSourceClassification::Episode {
                subject: later.subject,
            },
        });
        let advanced_evidence = EpisodeProjectionSourceCutEvidence {
            coverage: episode_projection_source_cut_coverage(
                0,
                2,
                EPISODE_PROJECTION_TICK_DURATION_MILLIS,
                &advanced_entries,
            )
            .unwrap(),
            entries: advanced_entries,
        };
        let second = generation_candidate_from_current(
            &store,
            &first.descriptor.generation_id,
            &advanced_evidence,
        );
        let second_digest = store
            .begin_episode_projection_generation(
                &second,
                &first.descriptor.generation_id,
                &advanced_evidence,
            )
            .unwrap();
        store
            .validate_episode_projection_generation(
                &second.descriptor.generation_id,
                &first.descriptor.generation_id,
                &advanced_evidence,
            )
            .unwrap();
        let second_status = store
            .activate_episode_projection_generation(
                &second.descriptor.generation_id,
                &first.descriptor.generation_id,
                &second_digest,
                &advanced_evidence,
                &advanced_evidence,
            )
            .unwrap();
        let retained_first = second_status
            .generations
            .iter()
            .find(|generation| {
                generation.descriptor.generation_id == first.descriptor.generation_id
            })
            .unwrap();
        assert_eq!(retained_first.snapshot_source_cut.through_source_row_id, 2);
        let rolled_back = store
            .rollback_episode_projection_generation(
                &first.descriptor.generation_id,
                &second.descriptor.generation_id,
                &retained_first.candidate_digest,
                &advanced_evidence,
                &advanced_evidence,
            )
            .unwrap();
        assert_eq!(
            rolled_back.active_generation_id,
            first.descriptor.generation_id
        );
        assert!(store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
        assert_eq!(
            store
                .load_episodes("Thomas")
                .unwrap()
                .iter()
                .map(|episode| episode.id)
                .collect::<Vec<_>>(),
            vec![701, 702]
        );
    }

    #[test]
    fn episode_projection_generation_tamper_fails_readiness_and_activation() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let active = store
            .load_episode_projection_generation_status()
            .unwrap()
            .active_generation_id;
        let active_evidence = EpisodeProjectionSourceCutEvidence {
            coverage: store
                .load_episode_projection_generation_status()
                .unwrap()
                .generations
                .iter()
                .find(|generation| generation.phase == EpisodeProjectionGenerationPhase::Active)
                .unwrap()
                .snapshot_source_cut
                .clone(),
            entries: Vec::new(),
        };
        let candidate = generation_candidate(&store, 1, "tampered", 604);
        let candidate_evidence = generation_evidence(&candidate);
        let digest = store
            .begin_episode_projection_generation(&candidate, &active, &candidate_evidence)
            .unwrap();

        let write_txn = store.db.begin_write().unwrap();
        {
            let mut generations = write_txn
                .open_table(EPISODE_PROJECTION_GENERATIONS)
                .unwrap();
            let key = generation_record_key(&candidate.descriptor.generation_id);
            let mut record: EpisodeProjectionGenerationRecord =
                table_json_value(&generations, &key).unwrap().unwrap();
            record.snapshot.subjects[0].coverage_digest = "00".repeat(32);
            insert_json(&mut generations, &key, &record).unwrap();
        }
        write_txn.commit().unwrap();

        assert!(store
            .load_episode_projection_readiness(projection_subject())
            .is_err());
        assert!(store
            .validate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &candidate_evidence,
            )
            .is_err());
        assert!(store
            .activate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &digest,
                &candidate_evidence,
                &active_evidence,
            )
            .is_err());
        assert!(store.load_episodes("Thomas").unwrap().is_empty());
    }

    #[test]
    fn episode_projection_start_policy_is_durable_and_immutable() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        let policy = EpisodeProjectionStartPolicy::RecoveryCut {
            source_row_id: 41,
            proof_digest: "ab".repeat(32),
        };
        let cutover = cutover_receipt(41);

        let control = store
            .initialize_episode_projection_cutover(&cutover, &[projection_agent()])
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
    fn recovery_cut_root_seals_legacy_live_memory_and_requires_consolidation_before_stage() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, dir) = temp_store();
        let path = dir.path().join("test-hippocampus.redb");
        let legacy = make_episode(77, "legacy episode");
        store
            .store_episodes("Thomas", std::slice::from_ref(&legacy))
            .unwrap();
        store
            .initialize_episode_projection_cutover(&cutover_receipt(0), &[projection_agent()])
            .unwrap();
        let root = store.load_episode_projection_generation_status().unwrap();
        assert_eq!(root.generations.len(), 1);
        assert_eq!(root.generations[0].descriptor.parent_generation_id, None);
        assert_eq!(
            root.generations[0].phase,
            EpisodeProjectionGenerationPhase::Active
        );
        assert_eq!(
            serde_json::to_vec(&store.load_episodes("Thomas").unwrap()).unwrap(),
            serde_json::to_vec(std::slice::from_ref(&legacy)).unwrap()
        );
        drop(store);

        let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            store.load_episode_projection_generation_status().unwrap(),
            root
        );
        assert_eq!(
            serde_json::to_vec(&store.load_episodes("Thomas").unwrap()).unwrap(),
            serde_json::to_vec(std::slice::from_ref(&legacy)).unwrap()
        );
        let active = root.active_generation_id.clone();
        let evidence = EpisodeProjectionSourceCutEvidence {
            coverage: root.generations[0].snapshot_source_cut.clone(),
            entries: Vec::new(),
        };
        let mut without_legacy = generation_candidate_from_current(&store, &active, &evidence);
        without_legacy.subjects[0].live_episodes.clear();
        without_legacy.subjects[0].coverage_digest = episode_projection_subject_coverage_digest(
            &without_legacy.subjects[0].agent,
            &without_legacy.subjects[0].frontier,
            &without_legacy.subjects[0].receipts,
            &without_legacy.subjects[0].live_episodes,
            &without_legacy.subjects[0].archived_episodes,
        )
        .unwrap();
        let error = store
            .begin_episode_projection_generation(&without_legacy, &active, &evidence)
            .unwrap_err();
        assert!(error.to_string().contains("consolidate it before staging"));
        assert_eq!(
            store.load_episode_projection_generation_status().unwrap(),
            root
        );
        assert!(store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());

        store
            .archive_and_clear_episodes("Thomas", std::slice::from_ref(&legacy))
            .unwrap();
        assert!(store.load_episodes("Thomas").unwrap().is_empty());
        assert_eq!(
            serde_json::to_vec(&store.load_archive("Thomas").unwrap()).unwrap(),
            serde_json::to_vec(std::slice::from_ref(&legacy)).unwrap()
        );

        let mut receiptless_new = generation_candidate_from_current(&store, &active, &evidence);
        receiptless_new.subjects[0]
            .live_episodes
            .push(make_episode(78, "unreceipted new effect"));
        receiptless_new.subjects[0].coverage_digest = episode_projection_subject_coverage_digest(
            &receiptless_new.subjects[0].agent,
            &receiptless_new.subjects[0].frontier,
            &receiptless_new.subjects[0].receipts,
            &receiptless_new.subjects[0].live_episodes,
            &receiptless_new.subjects[0].archived_episodes,
        )
        .unwrap();
        assert!(store
            .begin_episode_projection_generation(&receiptless_new, &active, &evidence)
            .unwrap_err()
            .to_string()
            .contains("no authoritative source receipt"));
        assert_eq!(
            store.load_episode_projection_generation_status().unwrap(),
            root
        );

        let candidate = generation_candidate_from_current(&store, &active, &evidence);
        store
            .begin_episode_projection_generation(&candidate, &active, &evidence)
            .unwrap();
        let staged = store.load_episode_projection_generation_status().unwrap();
        assert_eq!(staged.generations.len(), 2);
        assert!(staged.generations.iter().any(|generation| {
            generation.descriptor.generation_id == candidate.descriptor.generation_id
                && generation.phase == EpisodeProjectionGenerationPhase::Building
        }));
        assert_eq!(
            serde_json::to_vec(&store.load_archive("Thomas").unwrap()).unwrap(),
            serde_json::to_vec(std::slice::from_ref(&legacy)).unwrap()
        );
    }

    #[test]
    fn recovery_cut_preserves_duplicate_legacy_ids_without_weakening_receipt_identity() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, dir) = temp_store();
        let path = dir.path().join("test-hippocampus.redb");
        let legacy = vec![
            make_episode(77, "first legacy episode"),
            make_episode(77, "second legacy episode"),
        ];
        store.store_episodes("Thomas", &legacy).unwrap();
        store
            .initialize_episode_projection_cutover(&cutover_receipt(0), &[projection_agent()])
            .unwrap();
        let root = store.load_episode_projection_generation_status().unwrap();
        drop(store);

        let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            store.load_episode_projection_generation_status().unwrap(),
            root
        );
        assert_eq!(
            serde_json::to_vec(&store.load_episodes("Thomas").unwrap()).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );
        store.archive_and_clear_episodes("Thomas", &legacy).unwrap();

        let active = root.active_generation_id;
        let evidence = EpisodeProjectionSourceCutEvidence {
            coverage: root.generations[0].snapshot_source_cut.clone(),
            entries: Vec::new(),
        };
        let candidate = generation_candidate_from_current(&store, &active, &evidence);
        store
            .begin_episode_projection_generation(&candidate, &active, &evidence)
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&store.load_archive("Thomas").unwrap()).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );
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
            frontier_store.list_episode_projection_frontiers().unwrap(),
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
            effect_reference_tick: 1,
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
            effect_reference_tick: 1,
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

        let (generation_store, _generation_dir) = temp_store();
        let write_txn = generation_store.db.begin_write().unwrap();
        {
            let mut generations = write_txn
                .open_table(EPISODE_PROJECTION_GENERATIONS)
                .unwrap();
            generations.insert("orphan", b"{}".as_slice()).unwrap();
        }
        write_txn.commit().unwrap();
        let error = generation_store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap_err();
        assert!(error.to_string().contains("orphaned"));
        assert!(generation_store
            .load_episode_projection_control()
            .unwrap()
            .is_none());
        let read_txn = generation_store.db.begin_read().unwrap();
        let generations = read_txn.open_table(EPISODE_PROJECTION_GENERATIONS).unwrap();
        assert!(generations.get("orphan").unwrap().is_some());
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
    fn duplicate_source_event_at_later_row_is_rejected_without_mutation() {
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
        let error = store.commit_episode_projection(&duplicate).unwrap_err();
        assert!(error.to_string().contains("receipt replay conflict"));
        assert_eq!(store.load_episodes("Thomas").unwrap().len(), 1);
        let control = store.load_episode_projection_control().unwrap().unwrap();
        let frontier = store
            .load_episode_projection_frontier(projection_subject())
            .unwrap()
            .unwrap();
        assert_eq!(control.last_source_row_id, 1);
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
        conflicting.expected_global_frontier = 1;
        conflicting.request_digest = "ff".repeat(32);
        let error = store.commit_episode_projection(&conflicting).unwrap_err();
        assert!(error.to_string().contains("receipt replay conflict"));
        assert_eq!(
            store
                .load_episode_projection_control()
                .unwrap()
                .unwrap()
                .last_source_row_id,
            1
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
                effect_reference_tick: 5,
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
        EPISODE_PROJECTION_FAULT_STAGE.store(
            EpisodeProjectionFaultStage::AfterEpisode as u8,
            Ordering::SeqCst,
        );

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
    fn episode_projection_fault_matrix_reopens_old_then_retries_fully_new() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        for stage in [
            EpisodeProjectionFaultStage::AfterEpisode,
            EpisodeProjectionFaultStage::AfterSourceReceipt,
            EpisodeProjectionFaultStage::AfterIdentityReceipt,
            EpisodeProjectionFaultStage::AfterFrontier,
            EpisodeProjectionFaultStage::AfterControl,
            EpisodeProjectionFaultStage::AfterQuarantineRemoval,
            EpisodeProjectionFaultStage::BeforeCommit,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("fault-{stage:?}.redb"));
            let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
            store
                .initialize_episode_projection(
                    &EpisodeProjectionStartPolicy::Beginning,
                    &[projection_agent()],
                )
                .unwrap();
            let quarantine = EpisodeProjectionQuarantine {
                affected_subject: Some(projection_subject()),
                source_event_id: "fault-event".to_string(),
                source_row_id: 1,
                event_type: "bio_action_performed".to_string(),
                projection_version: EPISODE_PROJECTION_VERSION,
                request_digest: "11".repeat(32),
                effect_reference_tick: 1,
                reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
                diagnostic_digest: "22".repeat(32),
            };
            store.quarantine_episode_projection(&quarantine, 0).unwrap();
            let mut write = projection_write(1, "fault-event", 91, 1);
            write.request_digest = quarantine.request_digest.clone();
            let resolution = EpisodeProjectionResolution {
                quarantine: quarantine.clone(),
                write,
            };

            EPISODE_PROJECTION_FAULT_STAGE.store(stage as u8, Ordering::SeqCst);
            let error = store.resolve_episode_projection(&resolution).unwrap_err();
            assert!(error.to_string().contains(&format!("{stage:?}")));
            assert_eq!(EPISODE_PROJECTION_FAULT_STAGE.load(Ordering::SeqCst), 0);
            drop(store);

            let reopened = HippocampusStore::open(path.to_str().unwrap()).unwrap();
            assert!(reopened.load_episodes("Thomas").unwrap().is_empty());
            assert!(receipt_table_snapshot(&reopened).is_empty());
            assert_eq!(
                reopened
                    .load_episode_projection_frontier(projection_subject())
                    .unwrap()
                    .unwrap()
                    .applied_count,
                0
            );
            assert_eq!(
                reopened
                    .load_episode_projection_control()
                    .unwrap()
                    .unwrap()
                    .last_source_row_id,
                1
            );
            assert_eq!(
                reopened.list_episode_projection_quarantine().unwrap(),
                vec![quarantine]
            );

            assert!(matches!(
                reopened.resolve_episode_projection(&resolution).unwrap(),
                EpisodeProjectionApplyOutcome::Applied { .. }
            ));
            drop(reopened);

            let verified = HippocampusStore::open(path.to_str().unwrap()).unwrap();
            assert_eq!(verified.load_episodes("Thomas").unwrap().len(), 1);
            assert_eq!(receipt_table_snapshot(&verified).len(), 2);
            let frontier = verified
                .load_episode_projection_frontier(projection_subject())
                .unwrap()
                .unwrap();
            assert_eq!(frontier.applied_count, 1);
            assert_eq!(frontier.last_source_row_id, 1);
            assert_eq!(
                verified
                    .load_episode_projection_control()
                    .unwrap()
                    .unwrap()
                    .last_source_row_id,
                1
            );
            assert!(verified
                .list_episode_projection_quarantine()
                .unwrap()
                .is_empty());
        }
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
            effect_reference_tick: 1,
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
    fn projection_survives_atomic_consolidation_next_event_and_restart() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, dir) = temp_store();
        let path = dir.path().join("test-hippocampus.redb");
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let first = projection_write(1, "event-before-consolidation", 31, 0);
        store.commit_episode_projection(&first).unwrap();

        let live_before = store.load_episodes("Thomas").unwrap();
        store
            .archive_and_clear_episodes("Thomas", &live_before)
            .unwrap();
        assert!(store.load_episodes("Thomas").unwrap().is_empty());
        assert_eq!(
            store
                .load_archive("Thomas")
                .unwrap()
                .iter()
                .map(|episode| episode.id)
                .collect::<Vec<_>>(),
            vec![31]
        );

        let second = projection_write(2, "event-after-consolidation", 32, 1);
        store.commit_episode_projection(&second).unwrap();
        assert!(store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
        assert_eq!(
            store.load_episodes("Thomas").unwrap()[0].id,
            second.episode.id
        );
        drop(store);

        let readonly = HippocampusStore::open_readonly(path.to_str().unwrap()).unwrap();
        assert!(readonly
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
        assert_eq!(readonly.load_archive("Thomas").unwrap()[0].id, 31);
        assert_eq!(
            readonly.load_episodes("Thomas").unwrap()[0].id,
            second.episode.id
        );
        drop(readonly);

        let reopened = HippocampusStore::open(path.to_str().unwrap()).unwrap();
        assert!(matches!(
            reopened.commit_episode_projection(&second).unwrap(),
            EpisodeProjectionApplyOutcome::Duplicate { .. }
        ));
        assert_eq!(reopened.load_archive("Thomas").unwrap().len(), 1);
        assert_eq!(reopened.load_episodes("Thomas").unwrap().len(), 1);
    }

    #[test]
    fn subject_quarantines_fence_and_resolve_in_source_order() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        let lisa_subject = EpisodeProjectionSubject::Agent {
            agent_id: AgentId(2),
        };
        let lisa_agent = EpisodeProjectionAgent {
            subject: lisa_subject,
            agent_name: "Lisa".to_string(),
        };
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent(), lisa_agent],
            )
            .unwrap();

        let first = EpisodeProjectionQuarantine {
            affected_subject: Some(projection_subject()),
            source_event_id: "poison-1".to_string(),
            source_row_id: 1,
            event_type: "bio_action_performed".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "11".repeat(32),
            effect_reference_tick: 1,
            reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
            diagnostic_digest: "aa".repeat(32),
        };
        store.quarantine_episode_projection(&first, 0).unwrap();
        let second = EpisodeProjectionQuarantine {
            affected_subject: Some(projection_subject()),
            source_event_id: "poison-2".to_string(),
            source_row_id: 2,
            event_type: "bio_action_performed".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "22".repeat(32),
            effect_reference_tick: 2,
            reason: EpisodeProjectionQuarantineReason::BlockedByEarlierQuarantine,
            diagnostic_digest: "bb".repeat(32),
        };
        store.quarantine_episode_projection(&second, 1).unwrap();

        let mut lisa_episode = make_episode(41, "lisa-unrelated");
        lisa_episode.agent_name = "Lisa".to_string();
        let lisa_write = EpisodeProjectionWrite {
            subject: lisa_subject,
            agent_name: "Lisa".to_string(),
            source_event_id: "lisa-event".to_string(),
            source_row_id: 3,
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "33".repeat(32),
            expected_global_frontier: 2,
            effect_reference_tick: 3,
            episode: lisa_episode,
        };
        store.commit_episode_projection(&lisa_write).unwrap();
        assert!(store
            .load_episode_projection_readiness(lisa_subject)
            .unwrap()
            .is_ready());
        assert!(!store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());

        let mut second_write = projection_write(2, "poison-2", 42, 3);
        second_write.request_digest = second.request_digest.clone();
        let error = store
            .resolve_episode_projection(&EpisodeProjectionResolution {
                quarantine: second.clone(),
                write: second_write.clone(),
            })
            .unwrap_err();
        assert!(error.to_string().contains("source order"));
        assert_eq!(store.list_episode_projection_quarantine().unwrap().len(), 2);
        assert!(store.load_episodes("Thomas").unwrap().is_empty());

        let mut first_write = projection_write(1, "poison-1", 43, 3);
        first_write.request_digest = first.request_digest.clone();
        assert!(matches!(
            store
                .resolve_episode_projection(&EpisodeProjectionResolution {
                    quarantine: first,
                    write: first_write,
                })
                .unwrap(),
            EpisodeProjectionApplyOutcome::Applied { .. }
        ));
        assert_eq!(
            store.list_episode_projection_quarantine().unwrap(),
            vec![second.clone()]
        );
        assert!(!store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());

        assert!(matches!(
            store
                .resolve_episode_projection(&EpisodeProjectionResolution {
                    quarantine: second,
                    write: second_write,
                })
                .unwrap(),
            EpisodeProjectionApplyOutcome::Applied { .. }
        ));
        assert!(store
            .list_episode_projection_quarantine()
            .unwrap()
            .is_empty());
        assert!(store
            .load_episode_projection_readiness(projection_subject())
            .unwrap()
            .is_ready());
        assert_eq!(store.load_episodes("Thomas").unwrap().len(), 2);
        assert_eq!(store.load_episodes("Lisa").unwrap().len(), 1);
    }

    #[test]
    fn global_quarantine_fences_every_later_transition() {
        let _guard = PROJECTION_TEST_LOCK.lock().unwrap();
        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection(
                &EpisodeProjectionStartPolicy::Beginning,
                &[projection_agent()],
            )
            .unwrap();
        let global = EpisodeProjectionQuarantine {
            affected_subject: None,
            source_event_id: "global-poison".to_string(),
            source_row_id: 1,
            event_type: "agent_action_received".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "44".repeat(32),
            effect_reference_tick: 1,
            reason: EpisodeProjectionQuarantineReason::UnknownAgent,
            diagnostic_digest: "cc".repeat(32),
        };
        store.quarantine_episode_projection(&global, 0).unwrap();
        let write_error = store
            .commit_episode_projection(&projection_write(2, "event-2", 44, 1))
            .unwrap_err();
        assert!(write_error.to_string().contains("globally fenced"));
        let advance_error = store
            .advance_episode_projection(&EpisodeProjectionAdvance {
                source_event_id: "irrelevant-2".to_string(),
                source_row_id: 2,
                projection_version: EPISODE_PROJECTION_VERSION,
                request_digest: "55".repeat(32),
                expected_global_frontier: 1,
                effect_reference_tick: 2,
            })
            .unwrap_err();
        assert!(advance_error.to_string().contains("globally fenced"));
        assert_eq!(
            store
                .load_episode_projection_control()
                .unwrap()
                .unwrap()
                .last_source_row_id,
            1
        );
        assert!(store.load_episodes("Thomas").unwrap().is_empty());
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
            effect_reference_tick: 1,
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
            effect_reference_tick: 2,
            reason: EpisodeProjectionQuarantineReason::UnknownAgent,
            diagnostic_digest: "ef".repeat(32),
        };
        store.quarantine_episode_projection(&unresolved, 1).unwrap();
        let building = EpisodeProjectionQuarantine {
            affected_subject: Some(EpisodeProjectionSubject::Building),
            source_event_id: "building-poison".to_string(),
            source_row_id: 3,
            event_type: "chaos_triggered".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "cd".repeat(32),
            effect_reference_tick: 3,
            reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
            diagnostic_digest: "fa".repeat(32),
        };
        for subject in [projection_subject(), kevin.subject] {
            let readiness = store.load_episode_projection_readiness(subject).unwrap();
            assert!(!readiness.is_ready());
            let global_blockers: Vec<_> = readiness
                .blockers
                .iter()
                .filter_map(|block| match block {
                    EpisodeProjectionReadinessBlock::GlobalQuarantine { quarantine } => {
                        Some(quarantine)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(global_blockers, vec![&unresolved]);
        }

        let error = store
            .quarantine_episode_projection(&building, 2)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("global episode projection quarantine blocks later source rows"));
        assert_eq!(
            store
                .load_episode_projection_control()
                .unwrap()
                .unwrap()
                .last_source_row_id,
            2
        );
        let persisted = store.list_episode_projection_quarantine().unwrap();
        assert_eq!(persisted, vec![agent_quarantine, unresolved]);
        for quarantine in persisted {
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
            .initialize_episode_projection_cutover(&cutover_receipt(41), &[projection_agent()])
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
        frontier.start_policy = EpisodeProjectionStartPolicy::ExplicitPosition { source_row_id: 0 };
        overwrite_projection_frontier(&store, projection_subject(), &frontier);
        let error = store
            .load_episode_projection_readiness(projection_subject())
            .unwrap_err();
        assert!(error.to_string().contains("policy mismatch"));

        let (store, _dir) = temp_store();
        store
            .initialize_episode_projection_cutover(&cutover_receipt(41), &[projection_agent()])
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
                effect_reference_tick: 1,
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
            effect_reference_tick: 1,
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
                effect_reference_tick: 1,
            })
            .unwrap();
        let invalid_quarantine = EpisodeProjectionQuarantine {
            affected_subject: None,
            source_event_id: "invalid-digest".to_string(),
            source_row_id: 1,
            event_type: "agent_action_received".to_string(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest: "not-a-digest".to_string(),
            effect_reference_tick: 1,
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
                        effect_reference_tick: 2,
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
        let control = readonly.load_episode_projection_control().unwrap().unwrap();
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
