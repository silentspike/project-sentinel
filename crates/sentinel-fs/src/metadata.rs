//! Filesystem metadata store backed by redb.
//!
//! Three tables with agent-scoped composite keys:
//! - `FS_INODES`: `(agent_id, inode)` -> serialized `InodeData`
//! - `FS_DIRENTS`: `(agent_id, parent_inode, name)` -> child inode
//! - `CAS_REFCOUNT`: `sha256_hash` -> reference count (u32)

use crate::cas::{CasStore, ChunkGcStats};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction};
use sentinel_common::FsMetadataDump;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;
use tracing::instrument;

// --- Table Definitions ---

/// Inode metadata: `(agent_id, inode_number)` -> serialized InodeData.
const FS_INODES: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("fs_inodes");

/// Directory entries: `(agent_id, parent_inode, entry_name)` -> child inode.
const FS_DIRENTS: TableDefinition<(&str, u64, &str), u64> = TableDefinition::new("fs_dirents");

/// CAS blob reference counts: `sha256_hash` -> count.
const CAS_REFCOUNT: TableDefinition<&[u8; 32], u32> = TableDefinition::new("cas_refcount");

/// Grace-period Trash-Queue fuer null-ref CAS-Bloecke.
const FS_TRASH_QUEUE: TableDefinition<&[u8; 32], u64> = TableDefinition::new("fs_trash_queue");

/// #492: explicit snapshot blob pin manifest. `(snapshot_id, blob_hash)` -> pinned_at_ms.
/// A retained world snapshot pins every CAS blob its inode tree references, so Trash GC cannot
/// delete a blob that a retained snapshot still needs (key ordered by `snapshot_id` first so an
/// unpin is a prefix range over one snapshot). The pin is a pointer, never a blob copy (1:n).
const FS_SNAPSHOT_BLOB_REFS: TableDefinition<(&str, &[u8; 32]), u64> =
    TableDefinition::new("fs_snapshot_blob_refs");

const INODE_DATA_BINCODE_V1: &[u8; 4] = b"SFI1";

/// #492: the CAS blob hashes a snapshot's FS metadata actually references — the inode hashes of
/// regular files in the dump. NOT the `refcounts` aggregate (that is live-fs metadata state, not a
/// retention manifest, Codex objection round 3). Deduplicated. Used to build the pin manifest.
pub fn referenced_blob_hashes(dump: &FsMetadataDump) -> Vec<[u8; 32]> {
    let mut set: HashSet<[u8; 32]> = HashSet::new();
    for (_agent_id, _inode, bytes) in &dump.inodes {
        if let Ok(data) = InodeData::deserialize(bytes) {
            if data.kind == FileKind::Regular && data.size != u64::MAX && data.hash != [0u8; 32] {
                set.insert(data.hash);
            }
        }
    }
    set.into_iter().collect()
}

// --- Types ---

/// File type in the virtual filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
}

/// Inode metadata stored in redb.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InodeData {
    pub kind: FileKind,
    /// SHA-256 hash of content (only meaningful for Regular files).
    pub hash: [u8; 32],
    /// Original (uncompressed) size in bytes.
    pub size: u64,
    /// Unix permission bits.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// Number of hard links.
    pub nlinks: u32,
    /// Seconds since UNIX epoch.
    pub mtime: u64,
    pub ctime: u64,
    pub atime: u64,
    /// Symlink target (empty for non-symlinks).
    pub symlink_target: String,
}

impl InodeData {
    /// Create metadata for a new regular file.
    pub fn regular(hash: [u8; 32], size: u64, mode: u32) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            kind: FileKind::Regular,
            hash,
            size,
            mode,
            uid: 0,
            gid: 0,
            nlinks: 1,
            mtime: now,
            ctime: now,
            atime: now,
            symlink_target: String::new(),
        }
    }

    /// Create metadata for a new directory.
    pub fn directory(mode: u32) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            kind: FileKind::Directory,
            hash: [0u8; 32],
            size: 0,
            mode,
            uid: 0,
            gid: 0,
            nlinks: 2,
            mtime: now,
            ctime: now,
            atime: now,
            symlink_target: String::new(),
        }
    }

    fn serialize(&self) -> anyhow::Result<Vec<u8>> {
        let payload = bincode::serde::encode_to_vec(self, bincode::config::standard())
            .map_err(|e| anyhow::anyhow!("InodeData bincode serialize: {e}"))?;
        let mut bytes = Vec::with_capacity(INODE_DATA_BINCODE_V1.len() + payload.len());
        bytes.extend_from_slice(INODE_DATA_BINCODE_V1);
        bytes.extend_from_slice(&payload);
        Ok(bytes)
    }

    fn deserialize(data: &[u8]) -> anyhow::Result<Self> {
        if let Some(payload) = data.strip_prefix(INODE_DATA_BINCODE_V1) {
            let (decoded, _): (Self, usize) =
                bincode::serde::decode_from_slice(payload, bincode::config::standard())
                    .map_err(|e| anyhow::anyhow!("InodeData bincode deserialize: {e}"))?;
            return Ok(decoded);
        }

        serde_json::from_slice(data)
            .map_err(|e| anyhow::anyhow!("InodeData legacy json deserialize: {e}"))
    }
}

// --- MetadataStore ---

/// Filesystem metadata backed by a single redb database.
pub struct MetadataStore {
    db: Database,
    durability: MetadataDurability,
}

/// Durability level for metadata write transactions.
///
/// `Immediate` is the default and fsyncs every commit. `Eventual` skips fsync
/// for FUSE hot-path writes, trading crash durability for substantially lower
/// VM write latency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MetadataDurability {
    #[default]
    Immediate,
    Eventual,
}

/// Aggregated metadata statistics for storage and dedup verification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetadataStorageStats {
    pub regular_file_count: u64,
    pub logical_regular_file_bytes: u64,
    pub unreadable_inode_rows: u64,
}

impl MetadataStore {
    /// Open or create the metadata store at the given path.
    #[instrument(level = "debug", fields(path = %path.as_ref().display()))]
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::open_with_durability(path, MetadataDurability::Immediate)
    }

    /// Open or create the metadata store with a specific write durability.
    #[instrument(level = "debug", fields(path = %path.as_ref().display(), ?durability))]
    pub fn open_with_durability(
        path: impl AsRef<Path>,
        durability: MetadataDurability,
    ) -> anyhow::Result<Self> {
        let db = Database::create(path.as_ref())
            .map_err(|e| anyhow::anyhow!("MetadataStore open: {e}"))?;

        // Initialize all tables with immediate durability.
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(FS_INODES)?;
            write_txn.open_table(FS_DIRENTS)?;
            write_txn.open_table(CAS_REFCOUNT)?;
            write_txn.open_table(FS_TRASH_QUEUE)?;
            write_txn.open_table(FS_SNAPSHOT_BLOB_REFS)?;
        }
        write_txn.commit()?;

        Ok(Self { db, durability })
    }

    fn begin_write(&self) -> anyhow::Result<WriteTransaction> {
        let mut write_txn = self.db.begin_write()?;
        match self.durability {
            MetadataDurability::Immediate => {}
            MetadataDurability::Eventual => {
                write_txn.set_durability(redb::Durability::None)?;
            }
        }
        Ok(write_txn)
    }

    // === INODE OPERATIONS ===

    /// Get inode metadata for an agent.
    pub fn get_inode(&self, agent_id: &str, inode: u64) -> anyhow::Result<Option<InodeData>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FS_INODES)?;
        match table.get((agent_id, inode))? {
            Some(guard) => Ok(Some(InodeData::deserialize(guard.value())?)),
            None => Ok(None),
        }
    }

    /// Set inode metadata for an agent.
    pub fn set_inode(&self, agent_id: &str, inode: u64, data: &InodeData) -> anyhow::Result<()> {
        let serialized = data.serialize()?;
        let write_txn = self.begin_write()?;
        {
            let mut table = write_txn.open_table(FS_INODES)?;
            table.insert((agent_id, inode), serialized.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Remove an inode. Returns the old data if it existed.
    pub fn remove_inode(&self, agent_id: &str, inode: u64) -> anyhow::Result<Option<InodeData>> {
        let write_txn = self.begin_write()?;
        let old = {
            let mut table = write_txn.open_table(FS_INODES)?;
            let x = match table.remove((agent_id, inode))? {
                Some(guard) => Some(InodeData::deserialize(guard.value())?),
                None => None,
            };
            x
        };
        write_txn.commit()?;
        Ok(old)
    }

    // === DIRECTORY ENTRY OPERATIONS ===

    /// Look up a directory entry. Returns the child inode number.
    pub fn get_dirent(
        &self,
        agent_id: &str,
        parent: u64,
        name: &str,
    ) -> anyhow::Result<Option<u64>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FS_DIRENTS)?;
        Ok(table
            .get((agent_id, parent, name))?
            .map(|guard| guard.value()))
    }

    /// Insert or update a directory entry.
    pub fn set_dirent(
        &self,
        agent_id: &str,
        parent: u64,
        name: &str,
        child_inode: u64,
    ) -> anyhow::Result<()> {
        let write_txn = self.begin_write()?;
        {
            let mut table = write_txn.open_table(FS_DIRENTS)?;
            table.insert((agent_id, parent, name), child_inode)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Remove a directory entry. Returns the child inode if it existed.
    pub fn remove_dirent(
        &self,
        agent_id: &str,
        parent: u64,
        name: &str,
    ) -> anyhow::Result<Option<u64>> {
        let write_txn = self.begin_write()?;
        let old = {
            let mut table = write_txn.open_table(FS_DIRENTS)?;
            // Deliberate match instead of .map() — redb AccessGuard lifetime workaround
            #[allow(clippy::manual_map)]
            let x = match table.remove((agent_id, parent, name))? {
                Some(guard) => Some(guard.value()),
                None => None,
            };
            x
        };
        write_txn.commit()?;
        Ok(old)
    }

    /// List all entries in a directory for an agent.
    pub fn list_dirents(&self, agent_id: &str, parent: u64) -> anyhow::Result<Vec<(String, u64)>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FS_DIRENTS)?;

        let range_start = (agent_id, parent, "");
        let range_end = (agent_id, parent + 1, "");

        let mut entries = Vec::new();
        let range = table.range(range_start..range_end)?;
        for entry in range {
            let (key, value) = entry?;
            let (_, _, name) = key.value();
            entries.push((name.to_string(), value.value()));
        }
        Ok(entries)
    }

    // === REFCOUNT OPERATIONS ===

    /// Get the reference count for a CAS hash.
    pub fn get_refcount(&self, hash: &[u8; 32]) -> anyhow::Result<u32> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CAS_REFCOUNT)?;
        Ok(table.get(hash)?.map(|g| g.value()).unwrap_or(0))
    }

    /// Increment reference count. Returns the new count.
    pub fn inc_refcount(&self, hash: &[u8; 32]) -> anyhow::Result<u32> {
        let write_txn = self.begin_write()?;
        let new_count = {
            let mut table = write_txn.open_table(CAS_REFCOUNT)?;
            let current = table.get(hash)?.map(|g| g.value()).unwrap_or(0);
            let new = current + 1;
            table.insert(hash, new)?;
            if current == 0 {
                let mut trash = write_txn.open_table(FS_TRASH_QUEUE)?;
                trash.remove(hash)?;
            }
            new
        };
        write_txn.commit()?;
        Ok(new_count)
    }

    /// Decrement reference count. Returns the new count (clamped to 0).
    pub fn dec_refcount(&self, hash: &[u8; 32]) -> anyhow::Result<u32> {
        let write_txn = self.begin_write()?;
        let new_count = {
            let mut table = write_txn.open_table(CAS_REFCOUNT)?;
            let current = table.get(hash)?.map(|g| g.value()).unwrap_or(0);
            let new = current.saturating_sub(1);
            if new == 0 {
                table.remove(hash)?;
            } else {
                table.insert(hash, new)?;
            }
            new
        };
        write_txn.commit()?;
        Ok(new_count)
    }

    /// Collect all hashes with zero references (GC candidates).
    /// Since we remove zero-ref entries, this returns hashes that exist
    /// in CAS but have no refcount entry.
    pub fn zero_ref_hashes(&self) -> anyhow::Result<Vec<[u8; 32]>> {
        // Refcount entries with value 0 are removed on dec, so this
        // method is mainly useful when called with an external blob list.
        // For now, return empty — the layer manager will track GC externally.
        Ok(Vec::new())
    }

    /// Read the trash timestamp for a CAS hash.
    pub fn get_trash_timestamp(&self, hash: &[u8; 32]) -> anyhow::Result<Option<u64>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FS_TRASH_QUEUE)?;
        Ok(table.get(hash)?.map(|g| g.value()))
    }

    /// Set or clear the trash timestamp for a CAS hash.
    pub fn set_trash_timestamp(
        &self,
        hash: &[u8; 32],
        trashed_at_ms: Option<u64>,
    ) -> anyhow::Result<bool> {
        let write_txn = self.begin_write()?;
        let updated = {
            let mut table = write_txn.open_table(FS_TRASH_QUEUE)?;
            match trashed_at_ms {
                Some(value) => {
                    table.insert(hash, value)?;
                    true
                }
                None => table.remove(hash)?.is_some(),
            }
        };
        write_txn.commit()?;
        Ok(updated)
    }

    /// Remove a hash from the trash queue and re-establish a refcount if needed.
    pub fn restore_from_trash(&self, hash: &[u8; 32]) -> anyhow::Result<bool> {
        let write_txn = self.begin_write()?;
        let restored = {
            let mut trash = write_txn.open_table(FS_TRASH_QUEUE)?;
            if trash.remove(hash)?.is_none() {
                false
            } else {
                let mut refs = write_txn.open_table(CAS_REFCOUNT)?;
                let current = refs.get(hash)?.map(|g| g.value()).unwrap_or(0);
                if current == 0 {
                    refs.insert(hash, 1)?;
                }
                true
            }
        };
        write_txn.commit()?;
        Ok(restored)
    }

    // === #492 SNAPSHOT BLOB PINNING ===

    /// Pin every blob hash a world snapshot references (pointer manifest, no blob copy). Idempotent.
    pub fn pin_snapshot_blobs(&self, snapshot_id: &str, hashes: &[[u8; 32]]) -> anyhow::Result<()> {
        if hashes.is_empty() {
            return Ok(());
        }
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let write_txn = self.begin_write()?;
        {
            let mut table = write_txn.open_table(FS_SNAPSHOT_BLOB_REFS)?;
            for hash in hashes {
                table.insert((snapshot_id, hash), now_ms)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Release all pins of a snapshot (when it leaves the retention window). Returns count unpinned.
    pub fn unpin_snapshot_blobs(&self, snapshot_id: &str) -> anyhow::Result<u64> {
        let to_remove: Vec<[u8; 32]> = {
            let read_txn = self.db.begin_read()?;
            let table = read_txn.open_table(FS_SNAPSHOT_BLOB_REFS)?;
            let mut v = Vec::new();
            for entry in table.iter()? {
                let (key, _) = entry?;
                let (sid, hash) = key.value();
                if sid == snapshot_id {
                    v.push(*hash);
                }
            }
            v
        };
        if to_remove.is_empty() {
            return Ok(0);
        }
        let write_txn = self.begin_write()?;
        {
            let mut table = write_txn.open_table(FS_SNAPSHOT_BLOB_REFS)?;
            for hash in &to_remove {
                table.remove((snapshot_id, hash))?;
            }
        }
        write_txn.commit()?;
        Ok(to_remove.len() as u64)
    }

    /// All blob hashes currently pinned by any retained snapshot — a transient set for one GC pass.
    pub fn pinned_hashes(&self) -> anyhow::Result<HashSet<[u8; 32]>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FS_SNAPSHOT_BLOB_REFS)?;
        let mut set = HashSet::new();
        for entry in table.iter()? {
            let (key, _) = entry?;
            let (_sid, hash) = key.value();
            set.insert(*hash);
        }
        Ok(set)
    }

    /// Dump all sentinel-fs metadata tables for a world snapshot.
    pub fn dump_all_tables(&self) -> anyhow::Result<FsMetadataDump> {
        let read_txn = self.db.begin_read()?;
        let inodes = {
            let table = read_txn.open_table(FS_INODES)?;
            let mut rows = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                let (agent_id, inode) = key.value();
                rows.push((agent_id.to_string(), inode, value.value().to_vec()));
            }
            rows
        };
        let dirents = {
            let table = read_txn.open_table(FS_DIRENTS)?;
            let mut rows = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                let (agent_id, parent, name) = key.value();
                rows.push((
                    agent_id.to_string(),
                    parent,
                    name.to_string(),
                    value.value(),
                ));
            }
            rows
        };
        let refcounts = {
            let table = read_txn.open_table(CAS_REFCOUNT)?;
            let mut rows = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                rows.push((*key.value(), value.value()));
            }
            rows
        };
        let trash_queue = {
            let table = read_txn.open_table(FS_TRASH_QUEUE)?;
            let mut rows = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                rows.push((*key.value(), value.value()));
            }
            rows
        };
        Ok(FsMetadataDump {
            inodes,
            dirents,
            refcounts,
            trash_queue,
        })
    }

    /// Sum logical payload bytes for live storage/dedup verification.
    pub fn storage_stats(&self) -> anyhow::Result<MetadataStorageStats> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FS_INODES)?;
        let mut stats = MetadataStorageStats::default();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let data = match InodeData::deserialize(value.value()) {
                Ok(data) => data,
                Err(_) => {
                    stats.unreadable_inode_rows += 1;
                    continue;
                }
            };
            if data.kind == FileKind::Regular && data.size != u64::MAX {
                stats.regular_file_count += 1;
                stats.logical_regular_file_bytes =
                    stats.logical_regular_file_bytes.saturating_add(data.size);
            }
        }
        Ok(stats)
    }

    /// Restore all sentinel-fs metadata tables from a snapshot dump.
    pub fn restore_all_tables(&self, dump: &FsMetadataDump) -> anyhow::Result<()> {
        let current = self.dump_all_tables()?;
        let write_txn = self.begin_write()?;
        {
            let mut inodes = write_txn.open_table(FS_INODES)?;
            for (agent_id, inode, _) in &current.inodes {
                inodes.remove((agent_id.as_str(), *inode))?;
            }
            for (agent_id, inode, data) in &dump.inodes {
                inodes.insert((agent_id.as_str(), *inode), data.as_slice())?;
            }

            let mut dirents = write_txn.open_table(FS_DIRENTS)?;
            for (agent_id, parent, name, _) in &current.dirents {
                dirents.remove((agent_id.as_str(), *parent, name.as_str()))?;
            }
            for (agent_id, parent, name, child) in &dump.dirents {
                dirents.insert((agent_id.as_str(), *parent, name.as_str()), *child)?;
            }

            let mut refs = write_txn.open_table(CAS_REFCOUNT)?;
            for (hash, _) in &current.refcounts {
                refs.remove(hash)?;
            }
            for (hash, count) in &dump.refcounts {
                refs.insert(hash, *count)?;
            }

            let mut trash = write_txn.open_table(FS_TRASH_QUEUE)?;
            for (hash, _) in &current.trash_queue {
                trash.remove(hash)?;
            }
            for (hash, ts) in &dump.trash_queue {
                trash.insert(hash, *ts)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Delete expired trash blobs from CAS and remove their queue entries.
    pub fn gc_trash(
        &self,
        cas: &CasStore,
        grace_period_hours: u64,
    ) -> anyhow::Result<ChunkGcStats> {
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let cutoff_ms = now_ms.saturating_sub(grace_period_hours * 3600 * 1000);

        // #492: a blob pinned by any retained snapshot must not be GC'd even at refcount 0.
        // Read the pin set once per pass into a transient HashSet (O(1) membership per blob).
        let pinned = self.pinned_hashes()?;

        let expired_hashes = {
            let read_txn = self.db.begin_read()?;
            let trash = read_txn.open_table(FS_TRASH_QUEUE)?;
            let refs = read_txn.open_table(CAS_REFCOUNT)?;
            let mut hashes = Vec::new();
            for entry in trash.iter()? {
                let (key, value) = entry?;
                let hash = *key.value();
                let trashed_at_ms = value.value();
                if trashed_at_ms <= cutoff_ms
                    && refs.get(&hash)?.is_none()
                    && !pinned.contains(&hash)
                {
                    hashes.push(hash);
                }
            }
            hashes
        };

        if expired_hashes.is_empty() {
            return Ok(ChunkGcStats::default());
        }

        let gc_stats = cas.gc(&expired_hashes)?;

        let write_txn = self.begin_write()?;
        {
            let mut trash = write_txn.open_table(FS_TRASH_QUEUE)?;
            let refs = write_txn.open_table(CAS_REFCOUNT)?;
            for hash in &expired_hashes {
                if refs.get(hash)?.is_none() {
                    trash.remove(hash)?;
                }
            }
        }
        write_txn.commit()?;

        Ok(ChunkGcStats {
            removed: gc_stats.removed,
            freed_bytes: gc_stats.freed_bytes,
            freed_from_trash: gc_stats.removed,
            ..Default::default()
        })
    }

    // === BATCH / TRANSACTIONAL OPERATIONS ===

    /// Atomically create a file: set inode + dirent + inc refcount in one transaction.
    pub fn create_file(
        &self,
        agent_id: &str,
        parent_inode: u64,
        name: &str,
        inode: u64,
        data: &InodeData,
    ) -> anyhow::Result<()> {
        let serialized = data.serialize()?;
        let write_txn = self.begin_write()?;
        {
            let mut inodes = write_txn.open_table(FS_INODES)?;
            inodes.insert((agent_id, inode), serialized.as_slice())?;

            let mut dirents = write_txn.open_table(FS_DIRENTS)?;
            dirents.insert((agent_id, parent_inode, name), inode)?;

            if data.kind == FileKind::Regular {
                let mut refs = write_txn.open_table(CAS_REFCOUNT)?;
                let current = refs.get(&data.hash)?.map(|g| g.value()).unwrap_or(0);
                refs.insert(&data.hash, current + 1)?;
                if current == 0 {
                    let mut trash = write_txn.open_table(FS_TRASH_QUEUE)?;
                    trash.remove(&data.hash)?;
                }
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Atomically allocate an inode and create an entry in one transaction.
    ///
    /// This is the hot-path variant for agent writes: it avoids the old
    /// `next_inode()` transaction followed by a second `create_file()`
    /// transaction. When `ensure_root` is true, the agent root inode is also
    /// checked/created inside the same write transaction.
    pub fn create_file_allocating_inode(
        &self,
        agent_id: &str,
        parent_inode: u64,
        name: &str,
        data: &InodeData,
        ensure_root: bool,
    ) -> anyhow::Result<u64> {
        let serialized = data.serialize()?;

        let write_txn = self.begin_write()?;
        let inode = {
            let mut inodes = write_txn.open_table(FS_INODES)?;

            if ensure_root && inodes.get((agent_id, 1u64))?.is_none() {
                let root = InodeData::directory(0o755).serialize()?;
                inodes.insert((agent_id, 1u64), root.as_slice())?;
            }

            let current = inodes
                .get((agent_id, 0u64))?
                .map(|g| {
                    let bytes = g.value();
                    if bytes.len() == 8 {
                        u64::from_le_bytes(bytes.try_into().unwrap())
                    } else {
                        1
                    }
                })
                .unwrap_or(1);
            let inode = current + 1;
            inodes.insert((agent_id, 0u64), inode.to_le_bytes().as_slice())?;
            inodes.insert((agent_id, inode), serialized.as_slice())?;
            inode
        };

        {
            let mut dirents = write_txn.open_table(FS_DIRENTS)?;
            dirents.insert((agent_id, parent_inode, name), inode)?;
        }

        if data.kind == FileKind::Regular {
            let mut refs = write_txn.open_table(CAS_REFCOUNT)?;
            let current = refs.get(&data.hash)?.map(|g| g.value()).unwrap_or(0);
            refs.insert(&data.hash, current + 1)?;
            if current == 0 {
                let mut trash = write_txn.open_table(FS_TRASH_QUEUE)?;
                trash.remove(&data.hash)?;
            }
        }

        write_txn.commit()?;
        Ok(inode)
    }

    /// Atomically remove a file: remove inode + dirent + dec refcount in one transaction.
    /// Returns the removed InodeData if it existed.
    pub fn remove_file(
        &self,
        agent_id: &str,
        parent_inode: u64,
        name: &str,
        inode: u64,
    ) -> anyhow::Result<Option<InodeData>> {
        let write_txn = self.begin_write()?;
        let old_data = {
            let mut inodes = write_txn.open_table(FS_INODES)?;
            let old = match inodes.remove((agent_id, inode))? {
                Some(guard) => Some(InodeData::deserialize(guard.value())?),
                None => None,
            };
            drop(inodes);

            let mut dirents = write_txn.open_table(FS_DIRENTS)?;
            dirents.remove((agent_id, parent_inode, name))?;
            drop(dirents);

            if let Some(ref inode_data) = old {
                if inode_data.kind == FileKind::Regular {
                    let mut refs = write_txn.open_table(CAS_REFCOUNT)?;
                    let mut trash = write_txn.open_table(FS_TRASH_QUEUE)?;
                    let current = match refs.get(&inode_data.hash)? {
                        Some(g) => g.value(),
                        None => 0,
                    };
                    let new = current.saturating_sub(1);
                    if new == 0 {
                        refs.remove(&inode_data.hash)?;
                        trash.insert(
                            &inode_data.hash,
                            SystemTime::now()
                                .duration_since(SystemTime::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                        )?;
                    } else {
                        refs.insert(&inode_data.hash, new)?;
                        trash.remove(&inode_data.hash)?;
                    }
                }
            }

            old
        };
        write_txn.commit()?;
        Ok(old_data)
    }

    /// Allocate the next inode number for an agent.
    /// Uses a special inode 0 entry to track the counter.
    pub fn next_inode(&self, agent_id: &str) -> anyhow::Result<u64> {
        let write_txn = self.begin_write()?;
        let next = {
            let mut table = write_txn.open_table(FS_INODES)?;
            // Use inode 0 as the counter (never a real inode in FUSE — root is 1)
            let current = table
                .get((agent_id, 0u64))?
                .map(|g| {
                    let bytes = g.value();
                    if bytes.len() == 8 {
                        u64::from_le_bytes(bytes.try_into().unwrap())
                    } else {
                        1
                    }
                })
                .unwrap_or(1);
            let next = current + 1;
            table.insert((agent_id, 0u64), next.to_le_bytes().as_slice())?;
            next
        };
        write_txn.commit()?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_meta() -> (MetadataStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-meta.redb");
        let store = MetadataStore::open(&path).unwrap();
        (store, dir)
    }

    #[test]
    fn inode_crud() {
        let (store, _dir) = temp_meta();
        let agent = "AGENT-01";

        // Not found
        assert!(store.get_inode(agent, 1).unwrap().is_none());

        // Create
        let data = InodeData::regular([0xAA; 32], 1024, 0o644);
        store.set_inode(agent, 1, &data).unwrap();

        // Read back
        let read = store.get_inode(agent, 1).unwrap().unwrap();
        assert_eq!(read.size, 1024);
        assert_eq!(read.mode, 0o644);
        assert_eq!(read.hash, [0xAA; 32]);
        assert_eq!(read.kind, FileKind::Regular);

        // Update
        let mut updated = data.clone();
        updated.size = 2048;
        store.set_inode(agent, 1, &updated).unwrap();
        assert_eq!(store.get_inode(agent, 1).unwrap().unwrap().size, 2048);

        // Remove
        let old = store.remove_inode(agent, 1).unwrap().unwrap();
        assert_eq!(old.size, 2048);
        assert!(store.get_inode(agent, 1).unwrap().is_none());
    }

    #[test]
    fn inode_data_serializes_as_bincode_with_legacy_json_fallback() {
        let data = InodeData::regular([0xA1; 32], 1024, 0o644);

        let encoded = data.serialize().unwrap();
        assert!(encoded.starts_with(INODE_DATA_BINCODE_V1));
        let decoded = InodeData::deserialize(&encoded).unwrap();
        assert_eq!(decoded.kind, FileKind::Regular);
        assert_eq!(decoded.hash, [0xA1; 32]);
        assert_eq!(decoded.size, 1024);

        let legacy_json = serde_json::to_vec(&data).unwrap();
        let legacy_decoded = InodeData::deserialize(&legacy_json).unwrap();
        assert_eq!(legacy_decoded.kind, FileKind::Regular);
        assert_eq!(legacy_decoded.hash, [0xA1; 32]);
        assert_eq!(legacy_decoded.size, 1024);
    }

    #[test]
    fn agent_isolation() {
        let (store, _dir) = temp_meta();

        let data_a = InodeData::regular([0x01; 32], 100, 0o644);
        let data_b = InodeData::regular([0x02; 32], 200, 0o755);

        store.set_inode("AGENT-01", 1, &data_a).unwrap();
        store.set_inode("AGENT-02", 1, &data_b).unwrap();

        // Same inode number, different agents
        let a = store.get_inode("AGENT-01", 1).unwrap().unwrap();
        let b = store.get_inode("AGENT-02", 1).unwrap().unwrap();
        assert_eq!(a.size, 100);
        assert_eq!(b.size, 200);
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn dirent_crud() {
        let (store, _dir) = temp_meta();
        let agent = "AGENT-07";

        // Not found
        assert!(store.get_dirent(agent, 1, "hello.txt").unwrap().is_none());

        // Create
        store.set_dirent(agent, 1, "hello.txt", 2).unwrap();
        assert_eq!(store.get_dirent(agent, 1, "hello.txt").unwrap(), Some(2));

        // Overwrite
        store.set_dirent(agent, 1, "hello.txt", 3).unwrap();
        assert_eq!(store.get_dirent(agent, 1, "hello.txt").unwrap(), Some(3));

        // Remove
        let old = store.remove_dirent(agent, 1, "hello.txt").unwrap();
        assert_eq!(old, Some(3));
        assert!(store.get_dirent(agent, 1, "hello.txt").unwrap().is_none());
    }

    #[test]
    fn list_dirents_returns_children() {
        let (store, _dir) = temp_meta();
        let agent = "AGENT-01";

        store.set_dirent(agent, 1, "a.txt", 10).unwrap();
        store.set_dirent(agent, 1, "b.txt", 11).unwrap();
        store.set_dirent(agent, 1, "sub", 12).unwrap();

        // Different parent — should not appear
        store.set_dirent(agent, 2, "c.txt", 20).unwrap();

        let entries = store.list_dirents(agent, 1).unwrap();
        assert_eq!(entries.len(), 3);

        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
        assert!(names.contains(&"sub"));
    }

    #[test]
    fn refcount_inc_dec() {
        let (store, _dir) = temp_meta();
        let hash = [0xBB; 32];

        assert_eq!(store.get_refcount(&hash).unwrap(), 0);

        assert_eq!(store.inc_refcount(&hash).unwrap(), 1);
        assert_eq!(store.inc_refcount(&hash).unwrap(), 2);
        assert_eq!(store.inc_refcount(&hash).unwrap(), 3);

        assert_eq!(store.dec_refcount(&hash).unwrap(), 2);
        assert_eq!(store.dec_refcount(&hash).unwrap(), 1);
        assert_eq!(store.dec_refcount(&hash).unwrap(), 0);

        // Already zero, stays zero
        assert_eq!(store.dec_refcount(&hash).unwrap(), 0);
    }

    #[test]
    fn create_file_atomic() {
        let (store, _dir) = temp_meta();
        let agent = "AGENT-01";
        let hash = [0xCC; 32];
        let data = InodeData::regular(hash, 512, 0o644);

        store.create_file(agent, 1, "test.txt", 2, &data).unwrap();

        // Inode exists
        let inode = store.get_inode(agent, 2).unwrap().unwrap();
        assert_eq!(inode.size, 512);

        // Dirent exists
        assert_eq!(store.get_dirent(agent, 1, "test.txt").unwrap(), Some(2));

        // Refcount incremented
        assert_eq!(store.get_refcount(&hash).unwrap(), 1);
    }

    #[test]
    fn create_file_allocating_inode_is_single_path_counter_and_refcount() {
        let (store, _dir) = temp_meta();
        let agent = "AGENT-77";
        let hash = [0xC7; 32];
        let data = InodeData::regular(hash, 512, 0o644);

        let first = store
            .create_file_allocating_inode(agent, 1, "first.txt", &data, true)
            .unwrap();
        let second = store
            .create_file_allocating_inode(agent, 1, "second.txt", &data, true)
            .unwrap();

        assert_eq!(first, 2);
        assert_eq!(second, 3);
        assert!(store.get_inode(agent, 1).unwrap().is_some());
        assert_eq!(store.get_dirent(agent, 1, "first.txt").unwrap(), Some(2));
        assert_eq!(store.get_dirent(agent, 1, "second.txt").unwrap(), Some(3));
        assert_eq!(store.get_refcount(&hash).unwrap(), 2);
        assert_eq!(store.next_inode(agent).unwrap(), 4);
    }

    #[test]
    fn create_file_allocating_inode_removes_zero_ref_trash_entry() {
        let (store, _dir) = temp_meta();
        let agent = "AGENT-78";
        let hash = [0xC8; 32];
        let data = InodeData::regular(hash, 512, 0o644);

        let first = store
            .create_file_allocating_inode(agent, 1, "first.txt", &data, true)
            .unwrap();
        store.remove_file(agent, 1, "first.txt", first).unwrap();
        assert_eq!(store.get_refcount(&hash).unwrap(), 0);
        assert!(store.get_trash_timestamp(&hash).unwrap().is_some());

        let second = store
            .create_file_allocating_inode(agent, 1, "second.txt", &data, true)
            .unwrap();

        assert_eq!(second, 3);
        assert_eq!(store.get_refcount(&hash).unwrap(), 1);
        assert_eq!(store.get_trash_timestamp(&hash).unwrap(), None);
    }

    #[test]
    fn open_with_eventual_metadata_durability() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetadataStore::open_with_durability(
            dir.path().join("meta.redb"),
            MetadataDurability::Eventual,
        )
        .unwrap();

        assert_eq!(store.durability, MetadataDurability::Eventual);
        let data = InodeData::directory(0o755);
        store.set_inode("AGENT-01", 1, &data).unwrap();
        assert!(store.get_inode("AGENT-01", 1).unwrap().is_some());
    }

    #[test]
    fn remove_file_atomic() {
        let (store, _dir) = temp_meta();
        let agent = "AGENT-01";
        let hash = [0xDD; 32];
        let data = InodeData::regular(hash, 256, 0o644);

        store.create_file(agent, 1, "gone.txt", 3, &data).unwrap();
        assert_eq!(store.get_refcount(&hash).unwrap(), 1);

        let removed = store.remove_file(agent, 1, "gone.txt", 3).unwrap().unwrap();
        assert_eq!(removed.size, 256);

        // Everything cleaned up
        assert!(store.get_inode(agent, 3).unwrap().is_none());
        assert!(store.get_dirent(agent, 1, "gone.txt").unwrap().is_none());
        assert_eq!(store.get_refcount(&hash).unwrap(), 0);
        assert!(store.get_trash_timestamp(&hash).unwrap().is_some());
    }

    #[test]
    fn shared_refcount_across_agents() {
        let (store, _dir) = temp_meta();
        let hash = [0xEE; 32];

        let data = InodeData::regular(hash, 100, 0o644);
        store
            .create_file("AGENT-01", 1, "shared.txt", 2, &data)
            .unwrap();
        store
            .create_file("AGENT-02", 1, "shared.txt", 2, &data)
            .unwrap();

        // Both agents reference the same hash
        assert_eq!(store.get_refcount(&hash).unwrap(), 2);

        // Remove one — refcount drops to 1
        store.remove_file("AGENT-01", 1, "shared.txt", 2).unwrap();
        assert_eq!(store.get_refcount(&hash).unwrap(), 1);
        assert_eq!(store.get_trash_timestamp(&hash).unwrap(), None);
    }

    #[test]
    fn restore_from_trash_reinstates_refcount() {
        let (store, _dir) = temp_meta();
        let hash = [0xAB; 32];
        let data = InodeData::regular(hash, 64, 0o644);

        store
            .create_file("AGENT-01", 1, "trash.txt", 2, &data)
            .unwrap();
        store.remove_file("AGENT-01", 1, "trash.txt", 2).unwrap();
        assert_eq!(store.get_refcount(&hash).unwrap(), 0);
        assert!(store.get_trash_timestamp(&hash).unwrap().is_some());

        assert!(store.restore_from_trash(&hash).unwrap());
        assert_eq!(store.get_refcount(&hash).unwrap(), 1);
        assert_eq!(store.get_trash_timestamp(&hash).unwrap(), None);
    }

    #[test]
    fn dump_restore_roundtrip_includes_trash_queue() {
        let (store, dir) = temp_meta();
        let hash = [0xBC; 32];
        let data = InodeData::regular(hash, 128, 0o644);

        store.create_file("AGENT-01", 1, "f.txt", 2, &data).unwrap();
        store.remove_file("AGENT-01", 1, "f.txt", 2).unwrap();

        let dump = store.dump_all_tables().unwrap();
        let restored = MetadataStore::open(dir.path().join("restored.redb")).unwrap();
        restored.restore_all_tables(&dump).unwrap();

        assert!(restored.get_inode("AGENT-01", 2).unwrap().is_none());
        assert_eq!(restored.get_refcount(&hash).unwrap(), 0);
        assert!(restored.get_trash_timestamp(&hash).unwrap().is_some());
    }

    #[test]
    fn gc_trash_deletes_expired_blob() {
        let (store, dir) = temp_meta();
        let cas = CasStore::open(dir.path()).unwrap();
        let content = b"issue-264-gc-trash";
        let (hash, _) = cas.store(content).unwrap();
        let data = InodeData::regular(hash, content.len() as u64, 0o644);

        store
            .create_file("AGENT-01", 1, "gc.txt", 2, &data)
            .unwrap();
        store.remove_file("AGENT-01", 1, "gc.txt", 2).unwrap();
        assert!(cas.contains(&hash));

        store
            .set_trash_timestamp(
                &hash,
                Some(
                    SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64
                        - 25 * 3600 * 1000,
                ),
            )
            .unwrap();

        let stats = store.gc_trash(&cas, 24).unwrap();
        assert_eq!(stats.freed_from_trash, 1);
        assert!(!cas.contains(&hash));
        assert_eq!(store.get_trash_timestamp(&hash).unwrap(), None);
    }

    /// #492 helper: only regular-file inode hashes are referenced (not directories, not refcounts).
    #[test]
    fn referenced_blob_hashes_extracts_regular_inode_hashes() {
        let (store, _dir) = temp_meta();
        let h_reg = [11u8; 32];
        store
            .set_inode("AGENT-01", 2, &InodeData::regular(h_reg, 10, 0o644))
            .unwrap();
        store
            .set_inode("AGENT-01", 1, &InodeData::directory(0o755))
            .unwrap();
        let dump = store.dump_all_tables().unwrap();
        let refs = referenced_blob_hashes(&dump);
        assert_eq!(
            refs,
            vec![h_reg],
            "only the regular file's hash, no directory"
        );
    }

    /// #492 AC-2/AC-3: a snapshot pin blocks Trash GC; unpinning makes the blob GC-eligible again.
    #[test]
    fn snapshot_pin_blocks_gc_then_unpin_frees() {
        let (store, dir) = temp_meta();
        let cas = CasStore::open(dir.path()).unwrap();
        let content = b"issue-492-pinned-blob";
        let (hash, _) = cas.store(content).unwrap();
        let data = InodeData::regular(hash, content.len() as u64, 0o644);
        store.create_file("AGENT-01", 1, "f.txt", 2, &data).unwrap();
        store.remove_file("AGENT-01", 1, "f.txt", 2).unwrap(); // refcount -> 0, into trash
        let old = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
            - 25 * 3600 * 1000;
        store.set_trash_timestamp(&hash, Some(old)).unwrap();

        // AC-2: pinned by a retained snapshot → GC must NOT delete it despite refcount 0 + expired.
        store.pin_snapshot_blobs("snap-A", &[hash]).unwrap();
        let stats = store.gc_trash(&cas, 24).unwrap();
        assert_eq!(stats.freed_from_trash, 0, "pinned blob must survive GC");
        assert!(cas.contains(&hash), "pinned blob still on disk");

        // AC-3: after the snapshot is removed (unpin), the blob becomes GC-eligible and is freed.
        let n = store.unpin_snapshot_blobs("snap-A").unwrap();
        assert_eq!(n, 1);
        let stats = store.gc_trash(&cas, 24).unwrap();
        assert_eq!(stats.freed_from_trash, 1, "unpinned blob now GC-eligible");
        assert!(!cas.contains(&hash));
    }

    /// #492: pin set survives partial unpin (a blob pinned by two snapshots stays until both go).
    #[test]
    fn pin_survives_until_last_snapshot_unpinned() {
        let (store, _dir) = temp_meta();
        let hash = [22u8; 32];
        store.pin_snapshot_blobs("snap-A", &[hash]).unwrap();
        store.pin_snapshot_blobs("snap-B", &[hash]).unwrap();
        assert!(store.pinned_hashes().unwrap().contains(&hash));
        store.unpin_snapshot_blobs("snap-A").unwrap();
        assert!(
            store.pinned_hashes().unwrap().contains(&hash),
            "still pinned by snap-B"
        );
        store.unpin_snapshot_blobs("snap-B").unwrap();
        assert!(!store.pinned_hashes().unwrap().contains(&hash));
    }

    /// #492 AC-4: a V1 snapshot (no FS metadata → no pins) drives no delete; unpinning a snapshot
    /// that never pinned is a safe no-op and does not touch other snapshots' pins.
    #[test]
    fn v1_snapshot_no_pins_unpin_is_noop() {
        let (store, _dir) = temp_meta();
        let hash = [33u8; 32];
        store.pin_snapshot_blobs("snap-A", &[hash]).unwrap();
        // V1 snapshot id never pinned anything (fs_metadata: None at create time).
        let n = store.unpin_snapshot_blobs("v1-snapshot").unwrap();
        assert_eq!(n, 0, "no pins to release for a V1 snapshot");
        assert!(
            store.pinned_hashes().unwrap().contains(&hash),
            "other snapshot's pin untouched"
        );
    }

    #[test]
    fn next_inode_sequential() {
        let (store, _dir) = temp_meta();
        let agent = "AGENT-01";

        let i1 = store.next_inode(agent).unwrap();
        let i2 = store.next_inode(agent).unwrap();
        let i3 = store.next_inode(agent).unwrap();

        assert_eq!(i1, 2); // starts at 2 (root=1)
        assert_eq!(i2, 3);
        assert_eq!(i3, 4);

        // Different agent has its own counter
        let j1 = store.next_inode("AGENT-02").unwrap();
        assert_eq!(j1, 2);
    }

    #[test]
    fn directory_inode() {
        let (store, _dir) = temp_meta();
        let agent = "AGENT-01";
        let dir_data = InodeData::directory(0o755);

        store.set_inode(agent, 1, &dir_data).unwrap();
        let read = store.get_inode(agent, 1).unwrap().unwrap();
        assert_eq!(read.kind, FileKind::Directory);
        assert_eq!(read.mode, 0o755);
        assert_eq!(read.nlinks, 2);
    }

    #[test]
    fn db_size_reasonable() {
        let (store, dir) = temp_meta();
        store
            .create_file(
                "AGENT-01",
                1,
                "f.txt",
                2,
                &InodeData::regular([0; 32], 10, 0o644),
            )
            .unwrap();
        let path = dir.path().join("test-meta.redb");
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size < 2_097_152, "DB should be <2MB, was {size}");
    }
}
