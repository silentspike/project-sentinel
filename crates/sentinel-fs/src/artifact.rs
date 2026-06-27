//! Artifact Plane data model: 6 redb tables for content-defined chunked storage.
//!
//! Tables:
//! - `FS_OBJECTS`: ObjectId -> ObjectMetadata (size, mime, created_at, chunk_count)
//! - `FS_MANIFESTS`: ObjectId -> JSON-serialized `Vec<[u8;16]>` (ordered chunk list)
//! - `FS_CHUNKS`: `[u8;16]` (BLAKE3-128) -> zstd-compressed chunk data
//! - `FS_CHUNK_REFCOUNT`: `[u8;16]` -> u32 (how many manifests reference this chunk)
//! - `FS_OBJECT_REFS`: &str (name) -> u64 (ObjectId, named references)
//! - `FS_INGEST_SESSIONS`: session_id (u64) -> JSON-serialized IngestSessionState

use redb::{
    Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition,
    WriteTransaction,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crate::block_resolver::ChunkResolve;
use crate::chunk_cache::{ChunkCache, DEFAULT_CACHE_BYTES};
use crate::commit_scheduler::{CommitScheduler, SchedulerConfig};
use crate::segment::{ChunkLocation, SegmentStore};

/// Durability level for ArtifactPlane write transactions.
///
/// Controls the fsync behavior on commit:
/// - `Immediate` (default): every commit fsyncs to disk — safe against VM crashes.
/// - `Eventual`: commits skip fsync — significantly faster on VMs but data written
///   since the last `Immediate` commit may be lost on crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityLevel {
    /// Every commit is durable on disk (fsync). Default.
    Immediate,
    /// Commits skip fsync. Much faster on VMs, but not crash-safe.
    Eventual,
}

// --- Table Definitions ---

/// Object metadata: ObjectId -> JSON-serialized ObjectMetadata.
pub const FS_OBJECTS: TableDefinition<u64, &[u8]> = TableDefinition::new("fs_objects");

/// Manifests: ObjectId -> JSON-serialized list of chunk hashes (ordered).
pub const FS_MANIFESTS: TableDefinition<u64, &[u8]> = TableDefinition::new("fs_manifests");

/// Chunk index: BLAKE3-128 fingerprint -> ChunkLocation (segment_id + offset + len).
/// Actual compressed data is stored in segment pack files, not in redb.
pub const FS_CHUNKS: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new("fs_chunks");

/// Chunk reference counts: BLAKE3-128 fingerprint -> refcount.
pub const FS_CHUNK_REFCOUNT: TableDefinition<&[u8; 16], u32> =
    TableDefinition::new("fs_chunk_refcount");

/// Trash queue for GC grace period: chunk_hash → trashed_at_ms (Unix epoch).
/// Chunks mit Refcount 0 werden hier zwischengespeichert statt sofort geloescht.
pub const FS_TRASH_QUEUE: TableDefinition<&[u8; 16], u64> = TableDefinition::new("fs_trash_queue");

/// Named object references: name -> ObjectId.
pub const FS_OBJECT_REFS: TableDefinition<&str, u64> = TableDefinition::new("fs_object_refs");

/// Ingest session tracking: session_id -> JSON-serialized IngestSessionState.
/// Shows active downloads as .part files in the FUSE layer.
pub const FS_INGEST_SESSIONS: TableDefinition<u64, &[u8]> =
    TableDefinition::new("fs_ingest_sessions");

// --- Types ---

/// State of an active ingest session, stored in FS_INGEST_SESSIONS.
/// Visible as `.part` files in the FUSE layer while streaming is in progress.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestSessionState {
    /// MIME type hint for the in-progress object.
    pub mime: String,
    /// Filename hint (shown as `{name}.part` in FUSE).
    pub name: String,
    /// Bytes received so far.
    pub bytes_received: u64,
    /// Unix epoch seconds when the session started.
    pub started_at: u64,
}

impl IngestSessionState {
    pub fn new(mime: impl Into<String>, name: impl Into<String>) -> Self {
        let started_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            mime: mime.into(),
            name: name.into(),
            bytes_received: 0,
            started_at,
        }
    }

    pub fn serialize(&self) -> anyhow::Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| anyhow::anyhow!("IngestSessionState serialize: {e}"))
    }

    pub fn deserialize(data: &[u8]) -> anyhow::Result<Self> {
        serde_json::from_slice(data)
            .map_err(|e| anyhow::anyhow!("IngestSessionState deserialize: {e}"))
    }
}

/// Metadata stored per object in FS_OBJECTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMetadata {
    /// Original (uncompressed) size in bytes.
    pub size: u64,
    /// MIME type hint (e.g. "application/octet-stream", "text/html").
    pub mime: String,
    /// Unix epoch seconds of creation.
    pub created_at: u64,
    /// Number of chunks in the manifest.
    pub chunk_count: u32,
    /// SHA-256 digest of the original (uncompressed) source data.
    /// Enables post-hoc integrity verification without chunk recomputation.
    #[serde(default)]
    pub sha256: [u8; 32],
}

impl ObjectMetadata {
    pub fn new(size: u64, mime: impl Into<String>, chunk_count: u32, sha256: [u8; 32]) -> Self {
        let created_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            size,
            mime: mime.into(),
            created_at,
            chunk_count,
            sha256,
        }
    }

    pub fn serialize(&self) -> anyhow::Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| anyhow::anyhow!("ObjectMetadata serialize: {e}"))
    }

    pub fn deserialize(data: &[u8]) -> anyhow::Result<Self> {
        serde_json::from_slice(data).map_err(|e| anyhow::anyhow!("ObjectMetadata deserialize: {e}"))
    }
}

/// A chunk fingerprint (BLAKE3-128, 16 bytes).
pub type ChunkHash = [u8; 16];

// --- ArtifactPlane ---

/// The Artifact Plane: wraps a redb Database + SegmentStore for chunk storage.
pub struct ArtifactPlane {
    pub(crate) db: Database,
    durability: DurabilityLevel,
    /// Segment pack storage for chunk data (append-only files).
    /// Wrapped in Mutex for interior mutability (SegmentStore::append needs &mut).
    pub(crate) segments: Mutex<SegmentStore>,
    /// L1 RAM cache for decompressed chunk data (avoids redundant segment reads + zstd).
    cache: Mutex<ChunkCache>,
    /// Adaptive commit scheduler: smooths write spikes under I/O pressure.
    scheduler: Mutex<CommitScheduler>,
    /// #498 4c (V9): set in cluster mode; on a chunk read miss the plane resolves the
    /// chunk by hash (pull + verify + durable store), then retries. Unset = unchanged.
    chunk_resolver: OnceLock<Arc<dyn ChunkResolve>>,
}

impl ArtifactPlane {
    /// Open or create an ArtifactPlane database with `Immediate` durability.
    /// Segment packs are stored in a `segments/` subdirectory next to the redb file.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::open_with_durability(path, DurabilityLevel::Immediate)
    }

    /// Open or create an ArtifactPlane database with a specific durability level.
    pub fn open_with_durability(
        path: impl AsRef<Path>,
        durability: DurabilityLevel,
    ) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let db = Database::create(path).map_err(|e| anyhow::anyhow!("ArtifactPlane open: {e}"))?;

        // Segment packs dir: sibling to the redb file
        let seg_dir = path.with_extension("segments");
        let segments = SegmentStore::open(&seg_dir)?;

        // Initialize all 6 tables in one transaction (always Immediate for schema init).
        let wtxn = db.begin_write()?;
        {
            wtxn.open_table(FS_OBJECTS)?;
            wtxn.open_table(FS_MANIFESTS)?;
            wtxn.open_table(FS_CHUNKS)?;
            wtxn.open_table(FS_CHUNK_REFCOUNT)?;
            wtxn.open_table(FS_TRASH_QUEUE)?;
            wtxn.open_table(FS_OBJECT_REFS)?;
            wtxn.open_table(FS_INGEST_SESSIONS)?;
        }
        wtxn.commit()?;

        Ok(Self {
            db,
            durability,
            segments: Mutex::new(segments),
            cache: Mutex::new(ChunkCache::new(DEFAULT_CACHE_BYTES)),
            scheduler: Mutex::new(CommitScheduler::noop()),
            chunk_resolver: OnceLock::new(),
        })
    }

    /// Wire the #498 4c chunk resolver (V9). Called once in cluster mode; a chunk read
    /// miss then pulls the chunk from a peer (verify + durable store) and retries.
    pub fn set_chunk_resolver(&self, resolver: Arc<dyn ChunkResolve>) {
        let _ = self.chunk_resolver.set(resolver);
    }

    /// #498 4c: ask the wired resolver to make a missing chunk local. `false` if no
    /// resolver (single-node) — the read fails locally as before.
    fn resolve_missing_chunk(&self, hash: &ChunkHash) -> bool {
        self.chunk_resolver
            .get()
            .is_some_and(|r| r.ensure_chunk(hash))
    }

    /// Start a write transaction with the configured durability level.
    /// The commit scheduler may inject a short delay if IOPS budget is exceeded.
    pub(crate) fn begin_write(&self) -> anyhow::Result<WriteTransaction> {
        // Adaptive throttle: delay if commit rate exceeds IOPS budget
        if let Ok(mut sched) = self.scheduler.lock() {
            sched.pre_commit();
        }

        let mut wtxn = self.db.begin_write()?;
        match self.durability {
            DurabilityLevel::Immediate => {} // redb default
            DurabilityLevel::Eventual => {
                wtxn.set_durability(redb::Durability::None)?;
            }
        }
        Ok(wtxn)
    }

    /// Configure the adaptive commit scheduler for IOPS protection.
    /// By default, the scheduler is a noop (pass-through). Call this with
    /// a `SchedulerConfig` to enable rate-limited commits in production.
    pub fn set_scheduler(&self, config: SchedulerConfig) {
        if let Ok(mut sched) = self.scheduler.lock() {
            *sched = CommitScheduler::new(config);
        }
    }

    /// Get commit scheduler statistics.
    pub fn scheduler_stats(&self) -> crate::commit_scheduler::SchedulerStats {
        self.scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stats()
    }

    /// Allocate the next ObjectId (monotonic counter stored at key 0 in FS_OBJECTS using a
    /// separate counter key convention: we use u64::MAX as the counter slot).
    pub fn next_object_id(&self) -> anyhow::Result<u64> {
        let wtxn = self.begin_write()?;
        let id = {
            let mut table = wtxn.open_table(FS_OBJECTS)?;
            // Use key u64::MAX as counter slot (never a real ObjectId)
            let current = table
                .get(u64::MAX)?
                .map(|g| {
                    let bytes = g.value();
                    if bytes.len() == 8 {
                        u64::from_le_bytes(bytes.try_into().unwrap())
                    } else {
                        0
                    }
                })
                .unwrap_or(0);
            let next = current + 1;
            table.insert(u64::MAX, next.to_le_bytes().as_slice())?;
            next
        };
        wtxn.commit()?;
        Ok(id)
    }

    /// Get object metadata by ObjectId.
    pub fn get_object(&self, object_id: u64) -> anyhow::Result<Option<ObjectMetadata>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_OBJECTS)?;
        match table.get(object_id)? {
            Some(g) => Ok(Some(ObjectMetadata::deserialize(g.value())?)),
            None => Ok(None),
        }
    }

    /// Get ordered chunk hashes for an object.
    pub fn get_manifest(&self, object_id: u64) -> anyhow::Result<Option<Vec<ChunkHash>>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_MANIFESTS)?;
        match table.get(object_id)? {
            Some(g) => {
                let hashes: Vec<ChunkHash> = serde_json::from_slice(g.value())
                    .map_err(|e| anyhow::anyhow!("manifest deserialize: {e}"))?;
                Ok(Some(hashes))
            }
            None => Ok(None),
        }
    }

    /// Check if a chunk exists in the index.
    pub fn has_chunk(&self, hash: &ChunkHash) -> anyhow::Result<bool> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        Ok(table.get(hash)?.is_some())
    }

    /// Read raw (compressed) chunk bytes from the segment store (anchor B1). On a local
    /// miss with a #498 4c resolver wired (cluster mode), the chunk is pulled from a peer
    /// by hash + durably stored, then the read is retried once (V9). Without a resolver
    /// this is the unchanged local read.
    pub fn read_chunk_raw(&self, hash: &ChunkHash) -> anyhow::Result<Vec<u8>> {
        if let Some(bytes) = self.read_chunk_raw_local(hash)? {
            return Ok(bytes);
        }
        if self.resolve_missing_chunk(hash) {
            if let Some(bytes) = self.read_chunk_raw_local(hash)? {
                return Ok(bytes);
            }
        }
        anyhow::bail!("Chunk {} not found", crate::cas::hex_encode(hash))
    }

    /// Local-only read of a chunk's raw bytes; `None` if not in the index (no resolve).
    fn read_chunk_raw_local(&self, hash: &ChunkHash) -> anyhow::Result<Option<Vec<u8>>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        let Some(loc_bytes) = table.get(hash)? else {
            return Ok(None);
        };
        let loc = ChunkLocation::from_bytes(loc_bytes.value())?;
        let segments = self
            .segments
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        Ok(Some(segments.read(&loc)?))
    }

    /// Store a chunk **pulled from a peer** (#498 4c): verify its BLAKE3-128 identity
    /// (decompress → hash == `hash`), then durably store the raw (as-stored) bytes into
    /// the segment store + index (segment append + `FS_CHUNKS` + refcount, mirroring
    /// ingest). A corrupt / tampered chunk is **rejected, never stored** (AC-3 for the
    /// chunk plane). Idempotent (dedup if already held). The input is the raw form
    /// [`read_chunk_raw`](Self::read_chunk_raw) serves on the holder.
    pub fn store_pulled_chunk(&self, raw: &[u8], hash: &ChunkHash) -> anyhow::Result<()> {
        if self.has_chunk(hash)? {
            return Ok(()); // dedup — already durable
        }
        // Verify the identity the hash covers — the DECOMPRESSED chunk, not the wire bytes.
        let decompressed = crate::ingest::decompress_chunk(raw)
            .map_err(|e| anyhow::anyhow!("pulled chunk decompress failed: {e}"))?;
        let actual = crate::chunker::blake3_hash_128(&decompressed);
        if &actual != hash {
            anyhow::bail!(
                "pulled chunk digest mismatch: got {} want {} — rejected, not stored",
                crate::cas::hex_encode(&actual),
                crate::cas::hex_encode(hash)
            );
        }
        // Append to the segment store, then index + refcount in one redb txn (as ingest).
        let loc = {
            let mut segments = self
                .segments
                .lock()
                .map_err(|e| anyhow::anyhow!("segment lock: {e}"))?;
            segments.append(raw)?
        };
        let wtxn = self.begin_write()?;
        {
            let mut chunks = wtxn.open_table(FS_CHUNKS)?;
            let mut refcount = wtxn.open_table(FS_CHUNK_REFCOUNT)?;
            let loc_bytes = loc.to_bytes();
            chunks.insert(hash, loc_bytes.as_slice())?;
            let current = refcount.get(hash)?.map(|g| g.value()).unwrap_or(0);
            refcount.insert(hash, current + 1)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Read and decompress a chunk, using the L1 cache to avoid redundant I/O + zstd.
    ///
    /// Cache flow: check cache → (hit: return) | (miss: segment read → decompress → insert → return)
    pub fn read_chunk_decompressed(&self, hash: &ChunkHash) -> anyhow::Result<Vec<u8>> {
        // Fast path: cache hit
        {
            let mut cache = self
                .cache
                .lock()
                .map_err(|e| anyhow::anyhow!("cache lock: {e}"))?;
            if let Some(data) = cache.get(hash) {
                return Ok(data.to_vec());
            }
        }

        // Slow path: read from segment + decompress
        let compressed = self.read_chunk_raw(hash)?;
        let decompressed = crate::ingest::decompress_chunk(&compressed)?;

        // Insert into cache (subject to admission policy)
        {
            let mut cache = self
                .cache
                .lock()
                .map_err(|e| anyhow::anyhow!("cache lock: {e}"))?;
            cache.insert(*hash, decompressed.clone());
        }

        Ok(decompressed)
    }

    /// Batch read + decompress multiple chunks. Cache-first, then batch I/O for misses.
    ///
    /// Uses `SegmentStore::read_batch()` (io_uring when available) for cache misses,
    /// reducing syscall overhead when reading many chunks (e.g. full object reassembly).
    pub fn read_chunks_decompressed(&self, hashes: &[ChunkHash]) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut results: Vec<Option<Vec<u8>>> = vec![None; hashes.len()];

        // Phase 1: Check cache for each hash, collect miss indices
        let mut miss_indices: Vec<usize> = Vec::new();
        {
            let mut cache = self
                .cache
                .lock()
                .map_err(|e| anyhow::anyhow!("cache lock: {e}"))?;
            for (i, hash) in hashes.iter().enumerate() {
                if let Some(data) = cache.get(hash) {
                    results[i] = Some(data.to_vec());
                } else {
                    miss_indices.push(i);
                }
            }
        }

        if miss_indices.is_empty() {
            return Ok(results.into_iter().map(|r| r.unwrap()).collect());
        }

        // #498 4c (anchor B3 — a separate hook because this batch path bypasses B1):
        // resolve any cache-missing chunk that is also absent from the local index (pull
        // it from a peer + durably store), so the Phase-2 lookup finds it. Gated on a
        // wired resolver — single-node skips this entirely (unchanged).
        if self.chunk_resolver.get().is_some() {
            let absent: Vec<ChunkHash> = {
                let rtxn = self.db.begin_read()?;
                let table = rtxn.open_table(FS_CHUNKS)?;
                let mut absent = Vec::new();
                for &idx in &miss_indices {
                    if table.get(&hashes[idx])?.is_none() {
                        absent.push(hashes[idx]);
                    }
                }
                absent
            };
            for hash in &absent {
                self.resolve_missing_chunk(hash);
            }
        }

        // Phase 2: Look up ChunkLocations for all misses in one redb read txn
        let miss_locations: Vec<(usize, ChunkLocation)> = {
            let rtxn = self.db.begin_read()?;
            let table = rtxn.open_table(FS_CHUNKS)?;
            let mut locs = Vec::with_capacity(miss_indices.len());
            for &idx in &miss_indices {
                let hash = &hashes[idx];
                let loc_bytes = table.get(hash)?.ok_or_else(|| {
                    anyhow::anyhow!("Chunk {} not found", crate::cas::hex_encode(hash))
                })?;
                let loc = ChunkLocation::from_bytes(loc_bytes.value())?;
                locs.push((idx, loc));
            }
            locs
        };

        // Phase 3: Batch read from segment store
        let locations_only: Vec<_> = miss_locations.iter().map(|(_, loc)| *loc).collect();
        let compressed_results = {
            let segments = self
                .segments
                .lock()
                .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            segments.read_batch(&locations_only)
        };

        // Phase 4: Decompress and insert into cache
        {
            let mut cache = self
                .cache
                .lock()
                .map_err(|e| anyhow::anyhow!("cache lock: {e}"))?;
            for (batch_idx, compressed_result) in compressed_results.into_iter().enumerate() {
                let (result_idx, _) = miss_locations[batch_idx];
                let compressed = compressed_result?;
                let decompressed = crate::ingest::decompress_chunk(&compressed)?;
                cache.insert(hashes[result_idx], decompressed.clone());
                results[result_idx] = Some(decompressed);
            }
        }

        Ok(results.into_iter().map(|r| r.unwrap()).collect())
    }

    /// Get L1 cache statistics (for observability/benchmarks).
    pub fn cache_stats(&self) -> crate::chunk_cache::CacheStats {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).stats()
    }

    /// Get the ChunkLocation for an index entry (for direct segment reads).
    pub fn get_chunk_location(&self, hash: &ChunkHash) -> anyhow::Result<Option<ChunkLocation>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        match table.get(hash)? {
            Some(g) => Ok(Some(ChunkLocation::from_bytes(g.value())?)),
            None => Ok(None),
        }
    }

    /// Get chunk refcount.
    pub fn get_chunk_refcount(&self, hash: &ChunkHash) -> anyhow::Result<u32> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNK_REFCOUNT)?;
        Ok(table.get(hash)?.map(|g| g.value()).unwrap_or(0))
    }

    /// Get the trash-queue timestamp for a chunk.
    pub fn get_trash_timestamp(&self, hash: &ChunkHash) -> anyhow::Result<Option<u64>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_TRASH_QUEUE)?;
        Ok(table.get(hash)?.map(|g| g.value()))
    }

    /// Override the trash-queue timestamp for a chunk.
    pub fn set_trash_timestamp(
        &self,
        hash: &ChunkHash,
        trashed_at_ms: u64,
    ) -> anyhow::Result<bool> {
        let wtxn = self.begin_write()?;
        let updated = {
            let mut table = wtxn.open_table(FS_TRASH_QUEUE)?;
            if table.get(hash)?.is_some() {
                table.insert(hash, trashed_at_ms)?;
                true
            } else {
                false
            }
        };
        wtxn.commit()?;
        Ok(updated)
    }

    /// Resolve a named reference to an ObjectId.
    pub fn resolve_ref(&self, name: &str) -> anyhow::Result<Option<u64>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_OBJECT_REFS)?;
        Ok(table.get(name)?.map(|g| g.value()))
    }

    /// Set a named reference.
    pub fn set_ref(&self, name: &str, object_id: u64) -> anyhow::Result<()> {
        let wtxn = self.begin_write()?;
        {
            let mut table = wtxn.open_table(FS_OBJECT_REFS)?;
            table.insert(name, object_id)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Count total unique chunks in FS_CHUNKS.
    pub fn chunk_count(&self) -> anyhow::Result<u64> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        Ok(table.len()?)
    }

    // --- Ingest Session Tracking ---

    /// Register a new ingest session (shown as .part in FUSE).
    pub fn register_session(
        &self,
        session_id: u64,
        state: &IngestSessionState,
    ) -> anyhow::Result<()> {
        let wtxn = self.begin_write()?;
        {
            let mut table = wtxn.open_table(FS_INGEST_SESSIONS)?;
            table.insert(session_id, state.serialize()?.as_slice())?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Update bytes_received for an active session.
    pub fn update_session_progress(
        &self,
        session_id: u64,
        bytes_received: u64,
    ) -> anyhow::Result<()> {
        // Read current state first (separate scope to drop borrow)
        let updated = {
            let rtxn = self.db.begin_read()?;
            let table = rtxn.open_table(FS_INGEST_SESSIONS)?;
            match table.get(session_id)? {
                Some(g) => {
                    let mut state = IngestSessionState::deserialize(g.value())?;
                    state.bytes_received = bytes_received;
                    Some(state)
                }
                None => None,
            }
        };
        if let Some(state) = updated {
            let wtxn = self.begin_write()?;
            {
                let mut table = wtxn.open_table(FS_INGEST_SESSIONS)?;
                table.insert(session_id, state.serialize()?.as_slice())?;
            }
            wtxn.commit()?;
        }
        Ok(())
    }

    /// Remove a session (on commit or abort).
    pub fn remove_session(&self, session_id: u64) -> anyhow::Result<()> {
        let wtxn = self.begin_write()?;
        {
            let mut table = wtxn.open_table(FS_INGEST_SESSIONS)?;
            table.remove(session_id)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// List all active ingest sessions (for FUSE .part visibility).
    pub fn active_sessions(&self) -> anyhow::Result<Vec<(u64, IngestSessionState)>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_INGEST_SESSIONS)?;
        let mut sessions = Vec::new();
        for entry in table.iter()? {
            let (key, val) = entry?;
            let state = IngestSessionState::deserialize(val.value())?;
            sessions.push((key.value(), state));
        }
        Ok(sessions)
    }

    /// Get a specific session's state.
    pub fn get_session(&self, session_id: u64) -> anyhow::Result<Option<IngestSessionState>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_INGEST_SESSIONS)?;
        match table.get(session_id)? {
            Some(g) => Ok(Some(IngestSessionState::deserialize(g.value())?)),
            None => Ok(None),
        }
    }

    /// Get all chunk hashes with refcount == 0 (GC candidates).
    /// Since we remove zero-ref entries on decrement, this scans FS_CHUNKS for
    /// hashes not present in FS_CHUNK_REFCOUNT.
    pub fn zero_ref_chunks(&self) -> anyhow::Result<Vec<ChunkHash>> {
        let rtxn = self.db.begin_read()?;
        let chunks_table = rtxn.open_table(FS_CHUNKS)?;
        let refcount_table = rtxn.open_table(FS_CHUNK_REFCOUNT)?;

        let mut orphans = Vec::new();
        for entry in chunks_table.iter()? {
            let (key, _val) = entry?;
            let hash: ChunkHash = *key.value();
            if refcount_table.get(&hash)?.is_none() {
                orphans.push(hash);
            }
        }
        Ok(orphans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_plane() -> (ArtifactPlane, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let plane = ArtifactPlane::open(dir.path().join("artifact.redb")).unwrap();
        (plane, dir)
    }

    // ── #498 4c: durable pulled-chunk store + verify ──

    #[test]
    fn pulled_chunk_round_trips_through_verify_and_store() {
        // Holder ingests data -> a chunk; the puller receives its raw (as-stored) bytes
        // and stores them after verifying the BLAKE3-128 identity.
        let (holder, _h) = temp_plane();
        let data = b"distributed cas chunk pull-by-hash content";
        let id = crate::ingest::commit_ingest({
            let mut s = crate::ingest::begin_ingest(&holder, "text/plain");
            s.write(data);
            s
        })
        .unwrap();
        let hashes = holder.get_manifest(id).unwrap().unwrap();
        let hash = hashes[0];
        let raw = holder.read_chunk_raw(&hash).unwrap();

        let (puller, _p) = temp_plane();
        assert!(!puller.has_chunk(&hash).unwrap(), "puller starts without it");
        puller.store_pulled_chunk(&raw, &hash).unwrap();
        assert!(puller.has_chunk(&hash).unwrap(), "stored after verify");
        // The decompressed chunk on the puller matches the holder's.
        assert_eq!(
            puller.read_chunk_decompressed(&hash).unwrap(),
            holder.read_chunk_decompressed(&hash).unwrap()
        );
    }

    #[test]
    fn corrupt_pulled_chunk_is_rejected_and_not_stored() {
        let (holder, _h) = temp_plane();
        let data = b"chunk bytes that must not be tampered with on the wire";
        let id = crate::ingest::commit_ingest({
            let mut s = crate::ingest::begin_ingest(&holder, "text/plain");
            s.write(data);
            s
        })
        .unwrap();
        let hash = holder.get_manifest(id).unwrap().unwrap()[0];
        let mut raw = holder.read_chunk_raw(&hash).unwrap();
        raw[1] ^= 0xFF; // flip a content byte (index 0 is the encoding prefix)

        let (puller, _p) = temp_plane();
        let err = puller.store_pulled_chunk(&raw, &hash).unwrap_err();
        assert!(
            err.to_string().contains("digest mismatch"),
            "a corrupt chunk is rejected on the BLAKE3-128 digest"
        );
        assert!(
            !puller.has_chunk(&hash).unwrap(),
            "a rejected chunk is NEVER stored (AC-3)"
        );
    }

    // ── #498 4c: chunk anchors B1/B3 resolve a missing chunk via the wired resolver ──

    /// Ingest `data` into a fresh source plane; return its first chunk's hash + raw bytes.
    fn source_chunk(data: &[u8]) -> (ChunkHash, Vec<u8>, tempfile::TempDir) {
        let (src, dir) = temp_plane();
        let id = crate::ingest::commit_ingest({
            let mut s = crate::ingest::begin_ingest(&src, "text/plain");
            s.write(data);
            s
        })
        .unwrap();
        let hash = src.get_manifest(id).unwrap().unwrap()[0];
        let raw = src.read_chunk_raw(&hash).unwrap();
        (hash, raw, dir)
    }

    /// A resolver that "pulls" by storing a known raw chunk into the (Weak-referenced)
    /// plane — Weak avoids an Arc cycle, exactly as the daemon wiring does.
    struct PullChunk {
        plane: std::sync::Weak<ArtifactPlane>,
        hash: ChunkHash,
        raw: Vec<u8>,
    }
    impl ChunkResolve for PullChunk {
        fn ensure_chunk(&self, hash: &ChunkHash) -> bool {
            hash == &self.hash
                && self
                    .plane
                    .upgrade()
                    .is_some_and(|p| p.store_pulled_chunk(&self.raw, &self.hash).is_ok())
        }
    }

    fn target_plane() -> (Arc<ArtifactPlane>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let plane = Arc::new(ArtifactPlane::open(dir.path().join("target.redb")).unwrap());
        (plane, dir)
    }

    #[test]
    fn read_chunk_raw_b1_resolves_a_missing_chunk() {
        let (hash, raw, _sd) = source_chunk(b"B1: a chunk only a peer holds until resolved");
        let (plane, _td) = target_plane();
        plane.set_chunk_resolver(Arc::new(PullChunk {
            plane: Arc::downgrade(&plane),
            hash,
            raw: raw.clone(),
        }));
        assert!(!plane.has_chunk(&hash).unwrap(), "not local initially");
        assert_eq!(plane.read_chunk_raw(&hash).unwrap(), raw, "B1 resolves the miss");
        assert!(plane.has_chunk(&hash).unwrap(), "chunk is local after resolve");
    }

    #[test]
    fn read_chunks_decompressed_b3_resolves_a_missing_chunk() {
        let data = b"B3: a batch-read chunk only a peer holds until resolved";
        let (hash, raw, _sd) = source_chunk(data);
        let (plane, _td) = target_plane();
        plane.set_chunk_resolver(Arc::new(PullChunk {
            plane: Arc::downgrade(&plane),
            hash,
            raw,
        }));
        assert!(!plane.has_chunk(&hash).unwrap());
        // The batch path (B3, bypasses B1) resolves the missing chunk then reads it.
        let got = plane.read_chunks_decompressed(&[hash]).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], data, "B3 resolves + decompresses the missing chunk");
    }

    #[test]
    fn read_chunk_raw_without_resolver_fails_on_a_miss_unchanged() {
        let (plane, _td) = target_plane();
        let absent = crate::chunker::blake3_hash_128(b"never stored");
        assert!(plane.read_chunk_raw(&absent).is_err());
    }

    #[test]
    fn next_object_id_sequential() {
        let (plane, _dir) = temp_plane();
        assert_eq!(plane.next_object_id().unwrap(), 1);
        assert_eq!(plane.next_object_id().unwrap(), 2);
        assert_eq!(plane.next_object_id().unwrap(), 3);
    }

    #[test]
    fn object_metadata_roundtrip() {
        let sha256 = [0xABu8; 32];
        let meta = ObjectMetadata::new(1024, "text/plain", 16, sha256);
        let bytes = meta.serialize().unwrap();
        let decoded = ObjectMetadata::deserialize(&bytes).unwrap();
        assert_eq!(decoded.size, 1024);
        assert_eq!(decoded.mime, "text/plain");
        assert_eq!(decoded.chunk_count, 16);
        assert_eq!(decoded.sha256, sha256);
    }

    #[test]
    fn named_refs_roundtrip() {
        let (plane, _dir) = temp_plane();
        plane.set_ref("latest", 42).unwrap();
        assert_eq!(plane.resolve_ref("latest").unwrap(), Some(42));
        assert_eq!(plane.resolve_ref("unknown").unwrap(), None);
    }

    #[test]
    fn open_with_eventual_durability() {
        let dir = tempfile::tempdir().unwrap();
        let plane = ArtifactPlane::open_with_durability(
            dir.path().join("eventual.redb"),
            DurabilityLevel::Eventual,
        )
        .unwrap();
        assert_eq!(plane.durability, DurabilityLevel::Eventual);
        // Writes still work (just without fsync)
        let id = plane.next_object_id().unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn trash_queue_table_exists_on_open() {
        let (plane, _dir) = temp_plane();
        let missing = [0x11u8; 16];
        assert_eq!(plane.get_trash_timestamp(&missing).unwrap(), None);
    }
}
