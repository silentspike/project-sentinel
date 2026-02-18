//! Filesystem metadata store backed by redb.
//!
//! Three tables with agent-scoped composite keys:
//! - `FS_INODES`: `(agent_id, inode)` -> serialized `InodeData`
//! - `FS_DIRENTS`: `(agent_id, parent_inode, name)` -> child inode
//! - `CAS_REFCOUNT`: `sha256_hash` -> reference count (u32)

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
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
        serde_json::to_vec(self).map_err(|e| anyhow::anyhow!("InodeData serialize: {e}"))
    }

    fn deserialize(data: &[u8]) -> anyhow::Result<Self> {
        serde_json::from_slice(data).map_err(|e| anyhow::anyhow!("InodeData deserialize: {e}"))
    }
}

// --- MetadataStore ---

/// Filesystem metadata backed by a single redb database.
pub struct MetadataStore {
    db: Database,
}

impl MetadataStore {
    /// Open or create the metadata store at the given path.
    #[instrument(level = "debug", fields(path = %path.as_ref().display()))]
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let db = Database::create(path.as_ref())
            .map_err(|e| anyhow::anyhow!("MetadataStore open: {e}"))?;

        // Initialize all tables
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(FS_INODES)?;
            write_txn.open_table(FS_DIRENTS)?;
            write_txn.open_table(CAS_REFCOUNT)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
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
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FS_INODES)?;
            table.insert((agent_id, inode), serialized.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Remove an inode. Returns the old data if it existed.
    pub fn remove_inode(
        &self,
        agent_id: &str,
        inode: u64,
    ) -> anyhow::Result<Option<InodeData>> {
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
    pub fn list_dirents(
        &self,
        agent_id: &str,
        parent: u64,
    ) -> anyhow::Result<Vec<(String, u64)>> {
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
        let write_txn = self.db.begin_write()?;
        let new_count = {
            let mut table = write_txn.open_table(CAS_REFCOUNT)?;
            let current = table.get(hash)?.map(|g| g.value()).unwrap_or(0);
            let new = current + 1;
            table.insert(hash, new)?;
            new
        };
        write_txn.commit()?;
        Ok(new_count)
    }

    /// Decrement reference count. Returns the new count (clamped to 0).
    pub fn dec_refcount(&self, hash: &[u8; 32]) -> anyhow::Result<u32> {
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
        {
            let mut inodes = write_txn.open_table(FS_INODES)?;
            inodes.insert((agent_id, inode), serialized.as_slice())?;

            let mut dirents = write_txn.open_table(FS_DIRENTS)?;
            dirents.insert((agent_id, parent_inode, name), inode)?;

            if data.kind == FileKind::Regular {
                let mut refs = write_txn.open_table(CAS_REFCOUNT)?;
                let current = refs.get(&data.hash)?.map(|g| g.value()).unwrap_or(0);
                refs.insert(&data.hash, current + 1)?;
            }
        }
        write_txn.commit()?;
        Ok(())
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
        let write_txn = self.db.begin_write()?;
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
                    let current = match refs.get(&inode_data.hash)? {
                        Some(g) => g.value(),
                        None => 0,
                    };
                    let new = current.saturating_sub(1);
                    if new == 0 {
                        refs.remove(&inode_data.hash)?;
                    } else {
                        refs.insert(&inode_data.hash, new)?;
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
        let write_txn = self.db.begin_write()?;
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
    }

    #[test]
    fn shared_refcount_across_agents() {
        let (store, _dir) = temp_meta();
        let hash = [0xEE; 32];

        let data = InodeData::regular(hash, 100, 0o644);
        store.create_file("AGENT-01", 1, "shared.txt", 2, &data).unwrap();
        store.create_file("AGENT-02", 1, "shared.txt", 2, &data).unwrap();

        // Both agents reference the same hash
        assert_eq!(store.get_refcount(&hash).unwrap(), 2);

        // Remove one — refcount drops to 1
        store.remove_file("AGENT-01", 1, "shared.txt", 2).unwrap();
        assert_eq!(store.get_refcount(&hash).unwrap(), 1);
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
