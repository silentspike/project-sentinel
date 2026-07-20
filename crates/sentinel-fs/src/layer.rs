//! Layer manager: Base (readonly, shared) + Agent (rw, copy-on-write).
//!
//! Each agent sees the union of the shared base layer and its own agent layer.
//! Writes always go to the agent layer. Reads fall back to the base layer.
//! Deletes place a whiteout marker in the agent layer that hides the base entry.
//! Agent layers are created lazily on first write.

use std::collections::HashSet;
use std::sync::RwLock;

use crate::cas::CasStore;
use crate::metadata::{FileKind, InodeData, MetadataStore};
use crate::SHARED_BASE_LAYER_ID;
use tracing::instrument;

/// Whiteout marker: inode data with a zeroed hash and size=u64::MAX.
fn is_whiteout(data: &InodeData) -> bool {
    data.size == u64::MAX && data.hash == [0u8; 32] && data.kind == FileKind::Regular
}

fn whiteout_marker() -> InodeData {
    InodeData {
        kind: FileKind::Regular,
        hash: [0u8; 32],
        size: u64::MAX,
        mode: 0,
        uid: 0,
        gid: 0,
        nlinks: 0,
        mtime: 0,
        ctime: 0,
        atime: 0,
        symlink_target: String::new(),
    }
}

/// Layer manager that combines a shared base layer with per-agent CoW layers.
pub struct LayerManager {
    cas: CasStore,
    meta: MetadataStore,
    known_agent_roots: RwLock<HashSet<String>>,
}

/// Live storage stats for the layer manager.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerStorageStats {
    pub cas_blob_count: u64,
    pub cas_bytes_on_disk: u64,
    pub regular_file_count: u64,
    pub logical_regular_file_bytes: u64,
    pub dedup_savings_bytes: u64,
    pub dedup_ratio_percent: f64,
    pub unreadable_inode_rows: u64,
}

impl LayerManager {
    /// Create a new layer manager.
    pub fn new(cas: CasStore, meta: MetadataStore) -> Self {
        Self {
            cas,
            meta,
            known_agent_roots: RwLock::new(HashSet::new()),
        }
    }

    /// Access the CAS store.
    pub fn cas(&self) -> &CasStore {
        &self.cas
    }

    /// Access the metadata store.
    pub fn meta(&self) -> &MetadataStore {
        &self.meta
    }

    /// Aggregate CAS and metadata counters for live dedup verification.
    pub fn storage_stats(&self) -> anyhow::Result<LayerStorageStats> {
        let cas_stats = self.cas.stats()?;
        let metadata_stats = self.meta.storage_stats()?;
        let dedup_savings_bytes = metadata_stats
            .logical_regular_file_bytes
            .saturating_sub(cas_stats.total_bytes_on_disk);
        let dedup_ratio_percent = if metadata_stats.logical_regular_file_bytes == 0 {
            0.0
        } else {
            (dedup_savings_bytes as f64 * 100.0) / metadata_stats.logical_regular_file_bytes as f64
        };
        Ok(LayerStorageStats {
            cas_blob_count: cas_stats.blob_count,
            cas_bytes_on_disk: cas_stats.total_bytes_on_disk,
            regular_file_count: metadata_stats.regular_file_count,
            logical_regular_file_bytes: metadata_stats.logical_regular_file_bytes,
            dedup_savings_bytes,
            dedup_ratio_percent,
            unreadable_inode_rows: metadata_stats.unreadable_inode_rows,
        })
    }

    // === BASE LAYER OPERATIONS (populating shared content) ===

    /// Populate base layer: store a file's content in CAS and create inode + dirent.
    #[instrument(skip(self, content), level = "debug", fields(name, parent_inode, content_len = content.len()))]
    pub fn populate_base_file(
        &self,
        parent_inode: u64,
        name: &str,
        content: &[u8],
        mode: u32,
    ) -> anyhow::Result<u64> {
        self.meta
            .validate_layer_write_authority(SHARED_BASE_LAYER_ID)?;
        let (hash, _deduped) = self.cas.store(content)?;
        let inode = self.meta.next_inode(SHARED_BASE_LAYER_ID)?;
        let data = InodeData::regular(hash, content.len() as u64, mode);
        self.meta
            .create_file(SHARED_BASE_LAYER_ID, parent_inode, name, inode, &data)?;
        Ok(inode)
    }

    /// Populate base layer: create a directory.
    pub fn populate_base_dir(
        &self,
        parent_inode: u64,
        name: &str,
        mode: u32,
    ) -> anyhow::Result<u64> {
        self.meta
            .validate_layer_write_authority(SHARED_BASE_LAYER_ID)?;
        let inode = self.meta.next_inode(SHARED_BASE_LAYER_ID)?;
        let data = InodeData::directory(mode);
        self.meta
            .create_file(SHARED_BASE_LAYER_ID, parent_inode, name, inode, &data)?;
        Ok(inode)
    }

    /// Initialize the base layer root directory (inode 1).
    pub fn init_base_root(&self) -> anyhow::Result<()> {
        self.meta.bootstrap_shared_base_root_node_local()?;
        Ok(())
    }

    // === AGENT LAYER OPERATIONS ===

    /// Ensure agent root directory exists (lazy creation).
    pub fn ensure_agent_root(&self, agent_id: &str) -> anyhow::Result<()> {
        if self.agent_root_known(agent_id)? {
            return Ok(());
        }

        if self.meta.get_inode(agent_id, 1)?.is_none() {
            let root = InodeData::directory(0o755);
            self.meta.set_inode(agent_id, 1, &root)?;
        }
        self.mark_agent_root_known(agent_id)?;
        Ok(())
    }

    fn agent_root_known(&self, agent_id: &str) -> anyhow::Result<bool> {
        let roots = self
            .known_agent_roots
            .read()
            .map_err(|err| anyhow::anyhow!("Agent-Root-Cache read failed: {err}"))?;
        Ok(roots.contains(agent_id))
    }

    fn mark_agent_root_known(&self, agent_id: &str) -> anyhow::Result<()> {
        let mut roots = self
            .known_agent_roots
            .write()
            .map_err(|err| anyhow::anyhow!("Agent-Root-Cache write failed: {err}"))?;
        roots.insert(agent_id.to_string());
        Ok(())
    }

    /// Lookup an inode: agent layer first, then base layer fallback.
    /// Returns None if whiteout or not found in either layer.
    pub fn lookup_inode(&self, agent_id: &str, inode: u64) -> anyhow::Result<Option<InodeData>> {
        // Agent layer first
        if let Some(data) = self.meta.get_inode(agent_id, inode)? {
            if is_whiteout(&data) {
                return Ok(None);
            }
            return Ok(Some(data));
        }
        // Base layer fallback
        self.meta.get_inode(SHARED_BASE_LAYER_ID, inode)
    }

    /// Lookup a directory entry: agent layer first, then base layer.
    pub fn lookup_dirent(
        &self,
        agent_id: &str,
        parent: u64,
        name: &str,
    ) -> anyhow::Result<Option<u64>> {
        // Check agent layer
        if let Some(child) = self.meta.get_dirent(agent_id, parent, name)? {
            // Check for whiteout on the child inode
            if let Some(data) = self.meta.get_inode(agent_id, child)? {
                if is_whiteout(&data) {
                    return Ok(None);
                }
            }
            return Ok(Some(child));
        }
        // Base layer fallback
        self.meta.get_dirent(SHARED_BASE_LAYER_ID, parent, name)
    }

    /// Read file content by looking up the inode and reading from CAS.
    pub fn read_file(&self, agent_id: &str, inode: u64) -> anyhow::Result<Vec<u8>> {
        let data = self
            .lookup_inode(agent_id, inode)?
            .ok_or_else(|| anyhow::anyhow!("Inode {inode} not found for {agent_id}"))?;

        if data.kind != FileKind::Regular {
            return Err(anyhow::anyhow!("Inode {inode} is not a regular file"));
        }

        self.cas.read(&data.hash)
    }

    /// Write a file in the agent layer (CoW: always writes to agent layer).
    #[instrument(skip(self, content), level = "debug", fields(agent_id, parent_inode, name, content_len = content.len()))]
    pub fn write_file(
        &self,
        agent_id: &str,
        parent_inode: u64,
        name: &str,
        content: &[u8],
        mode: u32,
    ) -> anyhow::Result<u64> {
        self.meta.validate_layer_write_authority(agent_id)?;
        let (hash, _deduped) = self.cas.store(content)?;
        let data = InodeData::regular(hash, content.len() as u64, mode);
        let ensure_root = !self.agent_root_known(agent_id)?;
        let inode = self.meta.create_file_allocating_inode(
            agent_id,
            parent_inode,
            name,
            &data,
            ensure_root,
        )?;
        if ensure_root {
            self.mark_agent_root_known(agent_id)?;
        }
        Ok(inode)
    }

    /// Create a directory in the agent layer.
    pub fn mkdir(
        &self,
        agent_id: &str,
        parent_inode: u64,
        name: &str,
        mode: u32,
    ) -> anyhow::Result<u64> {
        self.meta.validate_layer_write_authority(agent_id)?;
        let data = InodeData::directory(mode);
        let ensure_root = !self.agent_root_known(agent_id)?;
        let inode = self.meta.create_file_allocating_inode(
            agent_id,
            parent_inode,
            name,
            &data,
            ensure_root,
        )?;
        if ensure_root {
            self.mark_agent_root_known(agent_id)?;
        }
        Ok(inode)
    }

    /// Delete a file/directory in the agent layer.
    /// If the entry exists in the base layer, places a whiteout marker.
    pub fn unlink(
        &self,
        agent_id: &str,
        parent_inode: u64,
        name: &str,
        inode: u64,
    ) -> anyhow::Result<()> {
        self.ensure_agent_root(agent_id)?;

        // Remove from agent layer if present
        self.meta.remove_file(agent_id, parent_inode, name, inode)?;

        // If entry exists in base layer, place whiteout
        if self
            .meta
            .get_dirent(SHARED_BASE_LAYER_ID, parent_inode, name)?
            .is_some()
        {
            let wo = whiteout_marker();
            self.meta.set_inode(agent_id, inode, &wo)?;
            self.meta.set_dirent(agent_id, parent_inode, name, inode)?;
        }

        Ok(())
    }

    /// List directory entries: union of agent + base, minus whiteouts.
    pub fn readdir(
        &self,
        agent_id: &str,
        parent_inode: u64,
    ) -> anyhow::Result<Vec<(String, u64, FileKind)>> {
        let mut result: Vec<(String, u64, FileKind)> = Vec::new();
        let mut whiteout_names: Vec<String> = Vec::new();
        let mut seen_names: Vec<String> = Vec::new();

        // Agent layer entries first
        let agent_entries = self.meta.list_dirents(agent_id, parent_inode)?;
        for (name, child_inode) in agent_entries {
            if let Some(data) = self.meta.get_inode(agent_id, child_inode)? {
                if is_whiteout(&data) {
                    whiteout_names.push(name);
                    continue;
                }
                seen_names.push(name.clone());
                result.push((name, child_inode, data.kind));
            }
        }

        // Base layer entries (only if not already in agent layer or whiteout)
        let base_entries = self.meta.list_dirents(SHARED_BASE_LAYER_ID, parent_inode)?;
        for (name, child_inode) in base_entries {
            if seen_names.contains(&name) || whiteout_names.contains(&name) {
                continue;
            }
            if let Some(data) = self.meta.get_inode(SHARED_BASE_LAYER_ID, child_inode)? {
                result.push((name, child_inode, data.kind));
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_layer() -> (LayerManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::open(dir.path()).unwrap();
        let meta_path = dir.path().join("meta.redb");
        let meta = MetadataStore::open(&meta_path).unwrap();
        let lm = LayerManager::new(cas, meta);
        lm.init_base_root().unwrap();
        (lm, dir)
    }

    #[test]
    fn base_layer_populate_and_read() {
        let (lm, _dir) = temp_layer();

        let inode = lm
            .populate_base_file(1, "readme.txt", b"Hello World", 0o644)
            .unwrap();

        // Read via base layer
        let content = lm.read_file(SHARED_BASE_LAYER_ID, inode).unwrap();
        assert_eq!(content, b"Hello World");

        // Agent sees base content
        let content = lm.read_file("AGENT-01", inode).unwrap();
        assert_eq!(content, b"Hello World");
    }

    #[test]
    fn agent_write_does_not_affect_base() {
        let (lm, _dir) = temp_layer();

        let base_inode = lm
            .populate_base_file(1, "shared.txt", b"base content", 0o644)
            .unwrap();

        // Agent writes a new file
        let agent_inode = lm
            .write_file("AGENT-01", 1, "agent.txt", b"agent content", 0o644)
            .unwrap();

        // Agent sees its file
        let content = lm.read_file("AGENT-01", agent_inode).unwrap();
        assert_eq!(content, b"agent content");

        // Base still has original content
        let base_content = lm.read_file(SHARED_BASE_LAYER_ID, base_inode).unwrap();
        assert_eq!(base_content, b"base content");

        // Other agent doesn't see AGENT-01's file
        assert!(lm
            .lookup_dirent("AGENT-02", 1, "agent.txt")
            .unwrap()
            .is_none());
    }

    #[test]
    fn agent_isolation() {
        let (lm, _dir) = temp_layer();

        lm.write_file("AGENT-01", 1, "secret.txt", b"agent-01 data", 0o600)
            .unwrap();
        lm.write_file("AGENT-02", 1, "secret.txt", b"agent-02 data", 0o600)
            .unwrap();

        let a1 = lm
            .lookup_dirent("AGENT-01", 1, "secret.txt")
            .unwrap()
            .unwrap();
        let a2 = lm
            .lookup_dirent("AGENT-02", 1, "secret.txt")
            .unwrap()
            .unwrap();

        assert_eq!(lm.read_file("AGENT-01", a1).unwrap(), b"agent-01 data");
        assert_eq!(lm.read_file("AGENT-02", a2).unwrap(), b"agent-02 data");
    }

    #[test]
    fn whiteout_hides_base_entry() {
        let (lm, _dir) = temp_layer();

        let base_inode = lm
            .populate_base_file(1, "deleteme.txt", b"to be deleted", 0o644)
            .unwrap();

        // Agent can see it before delete
        assert!(lm
            .lookup_dirent("AGENT-01", 1, "deleteme.txt")
            .unwrap()
            .is_some());

        // Agent deletes it
        lm.unlink("AGENT-01", 1, "deleteme.txt", base_inode)
            .unwrap();

        // Agent no longer sees it
        assert!(lm
            .lookup_dirent("AGENT-01", 1, "deleteme.txt")
            .unwrap()
            .is_none());

        // Other agent still sees it
        assert!(lm
            .lookup_dirent("AGENT-02", 1, "deleteme.txt")
            .unwrap()
            .is_some());

        // Base layer untouched
        assert!(lm
            .meta()
            .get_dirent(SHARED_BASE_LAYER_ID, 1, "deleteme.txt")
            .unwrap()
            .is_some());
    }

    #[test]
    fn readdir_merges_layers() {
        let (lm, _dir) = temp_layer();

        // Base has two files
        lm.populate_base_file(1, "base1.txt", b"b1", 0o644).unwrap();
        lm.populate_base_file(1, "base2.txt", b"b2", 0o644).unwrap();

        // Agent adds one, deletes one base file
        lm.write_file("AGENT-01", 1, "agent1.txt", b"a1", 0o644)
            .unwrap();

        let base2_inode = lm
            .meta()
            .get_dirent(SHARED_BASE_LAYER_ID, 1, "base2.txt")
            .unwrap()
            .unwrap();
        lm.unlink("AGENT-01", 1, "base2.txt", base2_inode).unwrap();

        let entries = lm.readdir("AGENT-01", 1).unwrap();
        let names: Vec<&str> = entries.iter().map(|(n, _, _)| n.as_str()).collect();

        assert!(names.contains(&"base1.txt"), "should see base1");
        assert!(names.contains(&"agent1.txt"), "should see agent file");
        assert!(!names.contains(&"base2.txt"), "base2 should be whiteout");
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn lazy_agent_root_creation() {
        let (lm, _dir) = temp_layer();

        // Agent root doesn't exist yet
        assert!(lm.meta().get_inode("AGENT-99", 1).unwrap().is_none());

        // Writing creates it lazily
        lm.write_file("AGENT-99", 1, "first.txt", b"data", 0o644)
            .unwrap();

        // Now agent root exists
        assert!(lm.meta().get_inode("AGENT-99", 1).unwrap().is_some());
    }

    #[test]
    fn write_file_allocates_sequential_inodes_in_single_metadata_path() {
        let (lm, _dir) = temp_layer();

        let first = lm
            .write_file("AGENT-88", 1, "first.txt", b"same", 0o644)
            .unwrap();
        let second = lm
            .write_file("AGENT-88", 1, "second.txt", b"same", 0o644)
            .unwrap();

        assert_eq!(first, 2);
        assert_eq!(second, 3);
        assert!(lm.meta().get_inode("AGENT-88", 1).unwrap().is_some());
        assert_eq!(
            lm.meta().get_dirent("AGENT-88", 1, "first.txt").unwrap(),
            Some(first)
        );
        assert_eq!(
            lm.meta().get_dirent("AGENT-88", 1, "second.txt").unwrap(),
            Some(second)
        );
        assert_eq!(lm.meta().get_refcount(&CasStore::hash(b"same")).unwrap(), 2);
    }

    #[test]
    fn dedup_across_agents() {
        let (lm, _dir) = temp_layer();
        let content = b"identical content for all agents";

        lm.write_file("AGENT-01", 1, "same.txt", content, 0o644)
            .unwrap();
        lm.write_file("AGENT-02", 1, "same.txt", content, 0o644)
            .unwrap();
        lm.write_file("AGENT-03", 1, "same.txt", content, 0o644)
            .unwrap();

        // Only one blob in CAS
        let stats = lm.cas().stats().unwrap();
        assert_eq!(stats.blob_count, 1, "identical content should be deduped");

        // Refcount should be 3
        let hash = CasStore::hash(content);
        assert_eq!(lm.meta().get_refcount(&hash).unwrap(), 3);
    }

    #[test]
    fn storage_stats_report_logical_bytes_and_dedup_savings() {
        let (lm, _dir) = temp_layer();
        let content = b"identical content for live stats";

        lm.write_file("AGENT-01", 1, "same-1.txt", content, 0o644)
            .unwrap();
        lm.write_file("AGENT-02", 1, "same-2.txt", content, 0o644)
            .unwrap();
        lm.write_file("AGENT-03", 1, "same-3.txt", content, 0o644)
            .unwrap();

        let stats = lm.storage_stats().unwrap();
        assert_eq!(stats.regular_file_count, 3);
        assert_eq!(stats.logical_regular_file_bytes, (content.len() * 3) as u64);
        assert_eq!(stats.cas_blob_count, 1);
        assert!(stats.dedup_savings_bytes > 0);
        assert!(stats.dedup_ratio_percent > 0.0);
    }

    #[test]
    fn mkdir_in_agent_layer() {
        let (lm, _dir) = temp_layer();

        let dir_inode = lm.mkdir("AGENT-01", 1, "subdir", 0o755).unwrap();
        let data = lm.lookup_inode("AGENT-01", dir_inode).unwrap().unwrap();
        assert_eq!(data.kind, FileKind::Directory);

        // Write file inside subdir
        let file_inode = lm
            .write_file("AGENT-01", dir_inode, "nested.txt", b"nested", 0o644)
            .unwrap();
        let content = lm.read_file("AGENT-01", file_inode).unwrap();
        assert_eq!(content, b"nested");
    }
}
