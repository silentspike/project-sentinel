//! Private CoW namespace and ordered, shared per-inode write handles.
//!
//! Lock order: manager state, metadata transaction, content-plane internals.
//! Retention precedes namespace adoption in separate databases. Historical roots
//! remain retained until snapshot-aware cleanup can prove release is safe.

use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::artifact::{ArtifactPlane, WorkspaceContentRef, WorkspacePatch};
use crate::cas::CasStore;
use crate::metadata::{FileKind, InodeData, MetadataStore, WorkspaceInode};
use crate::SHARED_BASE_LAYER_ID;
use sentinel_common::{FsMetadataDump, OwnerRegistry, OwnerWriteGuard};

const MAX_DIRTY_FILE: usize = 8 * 1024 * 1024;
const MAX_DIRTY_GLOBAL: usize = 64 * 1024 * 1024;
const MAX_PATCHES: usize = 4096;
const MAX_HANDLES: usize = 16384;
const MAX_FILE_SIZE: u64 = 1 << 40;
const MAX_LEGACY_SIZE: u64 = 64 * 1024 * 1024;
const PATCH_CHARGE: usize = std::mem::size_of::<WorkspacePatch>();

fn errno(code: i32) -> anyhow::Error { std::io::Error::from_raw_os_error(code).into() }
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(errno(22));
    }
    if name.len() > 255 { return Err(errno(36)); }
    Ok(())
}
fn is_whiteout(data: &InodeData) -> bool {
    data.size == u64::MAX && data.hash == [0; 32] && data.kind == FileKind::Regular
}
type InodeKey = (String, u64);
type InvalidationHook = Arc<dyn Fn(&str, u64) + Send + Sync>;
struct OpenInode {
    record: WorkspaceInode,
    published_size: u64,
    base_limit: u64,
    patches: Vec<WorkspacePatch>,
    dirty_bytes: usize,
    dirty: bool,
    dirty_guard: Option<OwnerWriteGuard>,
    opens: usize,
}
struct Handle { key: InodeKey, writable: bool, append: bool }
#[derive(Default)]
struct State {
    next_handle: u64,
    handles: HashMap<u64, Handle>,
    inodes: HashMap<InodeKey, OpenInode>,
    dirty_bytes: usize,
    invalidations: Vec<InodeKey>,
}

impl State {
    fn invalidate(&mut self, agent: &str, inode: u64) {
        let key = (agent.to_string(), inode);
        // Each operation affects at most a few inodes; never build an unbounded queue.
        if self.invalidations.len() < 64 && !self.invalidations.contains(&key) {
            self.invalidations.push(key);
        }
    }
}

struct StateGuard<'a> {
    state: Option<MutexGuard<'a, State>>,
    hook: Option<InvalidationHook>,
}

impl Deref for StateGuard<'_> {
    type Target = State;
    fn deref(&self) -> &State { self.state.as_deref().expect("live state guard") }
}

impl DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut State { self.state.as_deref_mut().expect("live state guard") }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        let pending = self.state.as_deref_mut()
            .map(|state| std::mem::take(&mut state.invalidations)).unwrap_or_default();
        drop(self.state.take());
        if let Some(hook) = &self.hook {
            for (agent, inode) in pending { hook(&agent, inode); }
        }
    }
}
pub struct LayerManager {
    cas: CasStore,
    meta: MetadataStore,
    plane: Option<Arc<ArtifactPlane>>,
    state: Mutex<State>,
    invalidation_hook: Mutex<Option<InvalidationHook>>,
}
/// Legacy CAS statistics; chunk-backed storage is accounted by ArtifactPlane.
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
    pub fn new(cas: CasStore, meta: MetadataStore) -> Self {
        Self { cas, meta, plane: None, state: Mutex::new(State::default()), invalidation_hook: Mutex::new(None) }
    }
    pub fn with_artifact_plane(cas: CasStore, meta: MetadataStore, plane: Arc<ArtifactPlane>) -> Self {
        Self { cas, meta, plane: Some(plane), state: Mutex::new(State::default()), invalidation_hook: Mutex::new(None) }
    }
    pub fn cas(&self) -> &CasStore { &self.cas }
    pub fn meta(&self) -> &MetadataStore { &self.meta }
    pub fn artifact_plane(&self) -> Option<Arc<ArtifactPlane>> { self.plane.clone() }
    /// Retained historical/orphan publications need snapshot-aware reconciliation.
    pub fn workspace_retention_cleanup_pending(&self) -> bool {
        self.plane.is_some() || self.meta.workspace_has_retired_inodes().unwrap_or(true)
    }
    /// The callback must perform only bounded, non-blocking enqueue work. Kernel
    /// invalidation belongs on the adapter's observer thread, never this callback.
    pub fn set_invalidation_hook(&self, hook: Option<InvalidationHook>) -> anyhow::Result<()> {
        *self.invalidation_hook.lock().map_err(|_| errno(5))? = hook;
        Ok(())
    }
    fn lock(&self) -> anyhow::Result<StateGuard<'_>> {
        let hook = self.invalidation_hook.lock().map_err(|_| errno(5))?.clone();
        let state = self.state.lock().map_err(|_| errno(5))?;
        Ok(StateGuard { state: Some(state), hook })
    }
    pub fn storage_stats(&self) -> anyhow::Result<LayerStorageStats> {
        if self.plane.is_some() {
            return Err(errno(95).context("Chunk-backed workspace accounting must use ArtifactPlane, not legacy CAS dedup ratios"));
        }
        let cas = self.cas.stats()?;
        let meta = self.meta.storage_stats()?;
        let savings = meta.logical_regular_file_bytes.saturating_sub(cas.total_bytes_on_disk);
        Ok(LayerStorageStats {
            cas_blob_count: cas.blob_count, cas_bytes_on_disk: cas.total_bytes_on_disk,
            regular_file_count: meta.regular_file_count,
            logical_regular_file_bytes: meta.logical_regular_file_bytes,
            dedup_savings_bytes: savings,
            dedup_ratio_percent: if meta.logical_regular_file_bytes == 0 { 0.0 }
                else { savings as f64 * 100.0 / meta.logical_regular_file_bytes as f64 },
            unreadable_inode_rows: meta.unreadable_inode_rows,
        })
    }
    pub fn init_base_root(&self) -> anyhow::Result<()> {
        self.meta.bootstrap_shared_base_root_node_local()?;
        Ok(())
    }
    pub fn ensure_agent_root(&self, agent: &str) -> anyhow::Result<()> {
        let _state = self.lock()?;
        self.ensure_root(agent)
    }
    fn ensure_root(&self, agent: &str) -> anyhow::Result<()> {
        if self.meta.get_inode(agent, 1)?.is_none() {
            let root = self.meta.get_inode(SHARED_BASE_LAYER_ID, 1)?
                .unwrap_or_else(|| InodeData::directory(0o755));
            self.meta.set_inode(agent, 1, &root)?;
        }
        Ok(())
    }
    fn record(&self, state: &State, agent: &str, inode: u64) -> anyhow::Result<Option<WorkspaceInode>> {
        if inode == 0 { return Ok(None); }
        if let Some(open) = state.inodes.get(&(agent.to_string(), inode)) {
            return Ok((open.record.data.nlinks > 0).then(|| open.record.clone()));
        }
        if let Some(record) = self.meta.workspace_inode(agent, inode)? {
            if !is_whiteout(&record.data) && record.data.nlinks > 0 && inode != 1
                && agent != SHARED_BASE_LAYER_ID && !record.inherited
                && self.meta.workspace_inode(SHARED_BASE_LAYER_ID, inode)?.is_some() {
                // Old per-layer counters could alias unrelated base and private rows.
                // Fail closed: a migration must assign disjoint identities before use.
                return Err(errno(116));
            }
            return Ok((!is_whiteout(&record.data) && record.data.nlinks > 0).then_some(record));
        }
        Ok(self.meta.workspace_inode(SHARED_BASE_LAYER_ID, inode)?
            .filter(|record| !is_whiteout(&record.data) && record.data.nlinks > 0).map(|mut record| {
            record.inherited = true;
            record
        }))
    }
    fn required(&self, state: &State, agent: &str, inode: u64) -> anyhow::Result<WorkspaceInode> {
        self.record(state, agent, inode)?.ok_or_else(|| errno(2))
    }
    pub fn lookup_inode(&self, agent: &str, inode: u64) -> anyhow::Result<Option<InodeData>> {
        Ok(self.record(&*self.lock()?, agent, inode)?.map(|r| r.data))
    }
    fn dirent(&self, agent: &str, parent: u64, name: &str) -> anyhow::Result<Option<u64>> {
        if let Some(inode) = self.meta.get_dirent(agent, parent, name)? {
            if inode == 0 { return Ok(None); }
            if self.meta.get_inode(agent, inode)?.as_ref().is_some_and(is_whiteout) { return Ok(None); }
            return Ok(Some(inode));
        }
        Ok(self.meta.get_dirent(SHARED_BASE_LAYER_ID, parent, name)?.filter(|inode| *inode != 0))
    }
    pub fn lookup_dirent(&self, agent: &str, parent: u64, name: &str) -> anyhow::Result<Option<u64>> {
        validate_name(name)?;
        let state = self.lock()?;
        self.parent(&state, agent, parent, false)?;
        self.dirent(agent, parent, name)
    }
    fn parent(&self, state: &State, agent: &str, inode: u64, write: bool) -> anyhow::Result<WorkspaceInode> {
        let record = self.required(state, agent, inode)?;
        if record.data.kind != FileKind::Directory { return Err(errno(20)); }
        // Caller credentials are checked in the kernel adapter; mode intent is enforced here.
        if write && record.data.mode & 0o222 == 0 { return Err(errno(13)); }
        if record.data.mode & 0o111 == 0 { return Err(errno(13)); }
        Ok(record)
    }
    fn entries(&self, state: &State, agent: &str, parent: u64) -> anyhow::Result<Vec<(String, u64, FileKind)>> {
        self.parent(state, agent, parent, false)?;
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        for layer in [agent, SHARED_BASE_LAYER_ID] {
            for (name, inode) in self.meta.list_dirents(layer, parent)? {
                if !seen.insert(name.clone()) || inode == 0 { continue; }
                if let Some(record) = self.record(state, agent, inode)? {
                    entries.push((name, inode, record.data.kind));
                }
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(entries)
    }
    pub fn readdir(&self, agent: &str, parent: u64) -> anyhow::Result<Vec<(String, u64, FileKind)>> {
        self.entries(&*self.lock()?, agent, parent)
    }
    fn adoption(&self, state: &mut State, agent: &str, guard: &OwnerWriteGuard,
        rows: &[(u64, WorkspaceInode)], names: &[(u64, String, Option<u64>)], durable: bool) -> anyhow::Result<()> {
        let mut persisted = rows.to_vec();
        for (inode, record) in &mut persisted {
            if let Some(open) = state.inodes.get(&(agent.to_string(), *inode)) {
                if let Some(dirty_guard) = &open.dirty_guard {
                    OwnerRegistry::global().validate(dirty_guard)?;
                }
                if open.dirty && record.generation == open.record.generation
                    && record.content == open.record.content {
                    record.data.size = open.published_size;
                }
            }
        }
        self.meta.commit_namespace(agent, guard, &persisted, names, durable)?;
        for (inode, record) in rows {
            state.invalidate(agent, *inode);
            if let Some(open) = state.inodes.get_mut(&(agent.to_string(), *inode)) {
                open.record = record.clone();
            }
        }
        for (parent, _, _) in names { state.invalidate(agent, *parent); }
        Ok(())
    }
    fn touch_parent(rows: &mut Vec<(u64, WorkspaceInode)>, inode: u64, mut record: WorkspaceInode, delta: i32) {
        record.data.nlinks = record.data.nlinks.saturating_add_signed(delta);
        record.data.mtime = now();
        record.data.ctime = record.data.mtime;
        rows.push((inode, record));
    }
    fn retain(&self, agent: &str, inode: u64, content: WorkspaceContentRef) -> anyhow::Result<()> {
        let plane = self.plane.as_ref().ok_or_else(|| errno(5))?;
        // Object identity prevents replacing roots required by historical snapshots.
        let root = format!("workspace:{}:{}:{}:{}", agent.len(), agent, inode, content.object_id);
        plane.retain_workspace(&root, content)
    }
    fn publish(&self, agent: &str, inode: u64, guard: &OwnerWriteGuard,
        base: Option<WorkspaceContentRef>, size: u64, patches: &[WorkspacePatch]) -> anyhow::Result<WorkspaceContentRef> {
        OwnerRegistry::global().validate(guard)?;
        let plane = self.plane.as_ref().ok_or_else(|| errno(5))?;
        let content = plane.publish_workspace(base, size, patches)?;
        OwnerRegistry::global().validate(guard)?;
        self.retain(agent, inode, content)?;
        OwnerRegistry::global().validate(guard)?;
        Ok(content)
    }
    fn create(&self, state: &mut State, agent: &str, parent: u64, name: &str,
        mut record: WorkspaceInode, bytes: Option<&[u8]>) -> anyhow::Result<u64> {
        validate_name(name)?;
        let guard = self.meta.namespace_guard(agent)?;
        self.ensure_root(agent)?;
        let parent_record = self.parent(state, agent, parent, true)?;
        if self.dirent(agent, parent, name)?.is_some() { return Err(errno(17)); }
        let inode = self.meta.workspace_next_inode(agent)?;
        if let Some(bytes) = bytes {
            if self.plane.is_some() {
                let content = self.publish(agent, inode, &guard, None, bytes.len() as u64,
                    &[WorkspacePatch { offset: 0, data: bytes.to_vec() }])?;
                record.data.hash = content.sha256;
                record.content = Some(content);
                record.generation = 1;
            } else {
                OwnerRegistry::global().validate(&guard)?;
                record.data.hash = self.cas.store(bytes)?.0;
            }
        }
        let delta = if record.data.kind == FileKind::Directory { 1 } else { 0 };
        let mut rows = vec![(inode, record)];
        Self::touch_parent(&mut rows, parent, parent_record, delta);
        self.adoption(state, agent, &guard, &rows, &[(parent, name.to_string(), Some(inode))], false)?;
        Ok(inode)
    }
    pub fn populate_base_file(&self, parent: u64, name: &str, bytes: &[u8], mode: u32) -> anyhow::Result<u64> {
        self.write_file(SHARED_BASE_LAYER_ID, parent, name, bytes, mode)
    }
    pub fn populate_base_dir(&self, parent: u64, name: &str, mode: u32) -> anyhow::Result<u64> {
        self.mkdir(SHARED_BASE_LAYER_ID, parent, name, mode)
    }
    pub fn write_file(&self, agent: &str, parent: u64, name: &str, bytes: &[u8], mode: u32) -> anyhow::Result<u64> {
        if bytes.len() > MAX_DIRTY_FILE { return Err(errno(27)); }
        validate_name(name)?;
        let mut state = self.lock()?;
        self.meta.validate_layer_write_authority(agent)?;
        self.ensure_root(agent)?;
        self.parent(&state, agent, parent, true)?;
        if let Some(inode) = self.dirent(agent, parent, name)? {
            if self.required(&state, agent, inode)?.data.mode & 0o222 == 0 { return Err(errno(13)); }
            self.open_state(&mut state, agent, inode)?;
            let old = state.inodes.get(&(agent.to_string(), inode)).ok_or_else(|| errno(5))?;
            let old_dirty = old.dirty_bytes + old.patches.len() * PATCH_CHARGE;
            if bytes.len() + PATCH_CHARGE > MAX_DIRTY_GLOBAL.saturating_sub(state.dirty_bytes - old_dirty) {
                return Err(errno(28));
            }
            self.truncate_state(&mut state, agent, inode, 0)?;
            self.write_state(&mut state, agent, inode, 0, bytes)?;
            self.flush(&mut state, agent, inode, false)?;
            self.drop_idle(&mut state, agent, inode);
            return Ok(inode);
        }
        self.create(&mut state, agent, parent, name,
            WorkspaceInode::legacy(InodeData::regular([0; 32], bytes.len() as u64, mode & 0o7777)), Some(bytes))
    }
    pub fn mkdir(&self, agent: &str, parent: u64, name: &str, mode: u32) -> anyhow::Result<u64> {
        self.create(&mut *self.lock()?, agent, parent, name,
            WorkspaceInode::legacy(InodeData::directory(mode & 0o7777)), None)
    }
    pub fn symlink(&self, agent: &str, parent: u64, name: &str, target: &str) -> anyhow::Result<u64> {
        if target.is_empty() || target.contains('\0') { return Err(errno(22)); }
        if target.len() > 4096 { return Err(errno(36)); }
        let mut data = InodeData::regular([0; 32], target.len() as u64, 0o777);
        data.kind = FileKind::Symlink;
        data.symlink_target = target.to_string();
        self.create(&mut *self.lock()?, agent, parent, name, WorkspaceInode::legacy(data), None)
    }
    pub fn link(&self, agent: &str, inode: u64, parent: u64, name: &str) -> anyhow::Result<u64> {
        validate_name(name)?;
        let mut state = self.lock()?;
        let guard = self.meta.namespace_guard(agent)?;
        let mut record = self.required(&state, agent, inode)?;
        if record.data.kind == FileKind::Directory { return Err(errno(1)); }
        let parent_record = self.parent(&state, agent, parent, true)?;
        if self.dirent(agent, parent, name)?.is_some() { return Err(errno(17)); }
        record.data.nlinks = record.data.nlinks.checked_add(1).ok_or_else(|| errno(31))?;
        record.data.ctime = now();
        let mut rows = vec![(inode, record)];
        Self::touch_parent(&mut rows, parent, parent_record, 0);
        self.adoption(&mut state, agent, &guard, &rows, &[(parent, name.to_string(), Some(inode))], false)?;
        Ok(inode)
    }
    fn remove(&self, state: &mut State, agent: &str, parent: u64, name: &str,
        expected: Option<u64>, directory: bool) -> anyhow::Result<()> {
        validate_name(name)?;
        let guard = self.meta.namespace_guard(agent)?;
        let parent_record = self.parent(state, agent, parent, true)?;
        let inode = self.dirent(agent, parent, name)?.ok_or_else(|| errno(2))?;
        if expected.is_some_and(|e| e != inode) { return Err(errno(2)); }
        let mut record = self.required(state, agent, inode)?;
        if directory {
            if record.data.kind != FileKind::Directory { return Err(errno(20)); }
            if !self.entries(state, agent, inode)?.is_empty() { return Err(errno(39)); }
            record.data.nlinks = 0;
        } else {
            if record.data.kind == FileKind::Directory { return Err(errno(21)); }
            record.data.nlinks = record.data.nlinks.saturating_sub(1);
        }
        record.data.ctime = now();
        let mut rows = vec![(inode, record)];
        Self::touch_parent(&mut rows, parent, parent_record, if directory { -1 } else { 0 });
        self.adoption(state, agent, &guard, &rows, &[(parent, name.to_string(), Some(0))], false)
    }
    pub fn unlink(&self, agent: &str, parent: u64, name: &str, inode: u64) -> anyhow::Result<()> {
        self.remove(&mut *self.lock()?, agent, parent, name, Some(inode), false)
    }
    pub fn rmdir(&self, agent: &str, parent: u64, name: &str) -> anyhow::Result<()> {
        self.remove(&mut *self.lock()?, agent, parent, name, None, true)
    }
    pub fn rename(&self, agent: &str, old_parent: u64, old_name: &str,
        new_parent: u64, new_name: &str, flags: u32) -> anyhow::Result<()> {
        validate_name(old_name)?;
        validate_name(new_name)?;
        // RENAME_NOREPLACE only; exchange/whiteout need a separately agreed contract.
        if flags & !1 != 0 { return Err(errno(22)); }
        let mut state = self.lock()?;
        let guard = self.meta.namespace_guard(agent)?;
        let old_dir = self.parent(&state, agent, old_parent, true)?;
        let new_dir = self.parent(&state, agent, new_parent, true)?;
        let inode = self.dirent(agent, old_parent, old_name)?.ok_or_else(|| errno(2))?;
        if old_parent == new_parent && old_name == new_name { return Ok(()); }
        let mut record = self.required(&state, agent, inode)?;
        let destination = self.dirent(agent, new_parent, new_name)?;
        if flags == 1 && destination.is_some() { return Err(errno(17)); }
        if destination == Some(inode) { return Ok(()); }
        if record.data.kind == FileKind::Directory {
            let mut pending = vec![inode];
            let mut seen = HashSet::new();
            while let Some(dir) = pending.pop() {
                if seen.len() >= 65536 || pending.len() >= 65536 { return Err(errno(12)); }
                if dir == new_parent { return Err(errno(22)); }
                if !seen.insert(dir) { return Err(errno(5)); }
                for (_, child, kind) in self.entries(&state, agent, dir)? {
                    if kind == FileKind::Directory { pending.push(child); }
                }
            }
        }
        let mut rows = Vec::new();
        let mut replaced_directory = false;
        if let Some(target) = destination {
            let mut victim = self.required(&state, agent, target)?;
            if record.data.kind == FileKind::Directory && victim.data.kind != FileKind::Directory { return Err(errno(20)); }
            if record.data.kind != FileKind::Directory && victim.data.kind == FileKind::Directory { return Err(errno(21)); }
            replaced_directory = victim.data.kind == FileKind::Directory;
            if replaced_directory && !self.entries(&state, agent, target)?.is_empty() { return Err(errno(39)); }
            victim.data.nlinks = if replaced_directory { 0 } else { victim.data.nlinks.saturating_sub(1) };
            victim.data.ctime = now();
            rows.push((target, victim));
        }
        record.data.ctime = now();
        let moving_directory = record.data.kind == FileKind::Directory;
        rows.push((inode, record));
        if old_parent == new_parent {
            Self::touch_parent(&mut rows, old_parent, old_dir, if replaced_directory { -1 } else { 0 });
        } else {
            Self::touch_parent(&mut rows, old_parent, old_dir, if moving_directory { -1 } else { 0 });
            Self::touch_parent(&mut rows, new_parent, new_dir,
                i32::from(moving_directory) - i32::from(replaced_directory));
        }
        self.adoption(&mut state, agent, &guard, &rows, &[
            (old_parent, old_name.to_string(), Some(0)), (new_parent, new_name.to_string(), Some(inode))], false)
    }
    fn open_state(&self, state: &mut State, agent: &str, inode: u64) -> anyhow::Result<()> {
        let key = (agent.to_string(), inode);
        if state.inodes.contains_key(&key) { return Ok(()); }
        if state.inodes.len() >= MAX_HANDLES { return Err(errno(24)); }
        let record = self.required(state, agent, inode)?;
        if record.data.kind != FileKind::Regular {
            return Err(errno(if record.data.kind == FileKind::Directory { 21 } else { 40 }));
        }
        let size = record.data.size;
        state.inodes.insert(key, OpenInode { record, published_size: size, base_limit: size,
            patches: Vec::new(), dirty_bytes: 0, dirty: false, dirty_guard: None, opens: 0 });
        Ok(())
    }
    pub fn open_file(&self, agent: &str, inode: u64, writable: bool, append: bool, truncate: bool) -> anyhow::Result<u64> {
        if truncate && !writable { return Err(errno(13)); }
        let mut state = self.lock()?;
        if writable { self.meta.validate_layer_write_authority(agent)?; }
        if state.handles.len() >= MAX_HANDLES { return Err(errno(24)); }
        let data = self.required(&state, agent, inode)?.data;
        if data.mode & (if writable { 0o222 } else { 0o444 }) == 0 { return Err(errno(13)); }
        let handle = state.next_handle.checked_add(1).ok_or_else(|| errno(24))?;
        self.open_state(&mut state, agent, inode)?;
        if truncate { self.truncate_state(&mut state, agent, inode, 0)?; }
        let key = (agent.to_string(), inode);
        state.inodes.get_mut(&key).ok_or_else(|| errno(5))?.opens += 1;
        state.next_handle = handle;
        state.handles.insert(handle, Handle { key, writable, append });
        Ok(handle)
    }
    fn handle<'a>(state: &'a State, agent: &str, handle: u64) -> anyhow::Result<&'a Handle> {
        let h = state.handles.get(&handle).ok_or_else(|| errno(9))?;
        if h.key.0 != agent { return Err(errno(9)); }
        Ok(h)
    }
    pub fn getattr_handle(&self, agent: &str, handle: u64) -> anyhow::Result<InodeData> {
        let state = self.lock()?;
        let h = Self::handle(&state, agent, handle)?;
        Ok(state.inodes.get(&h.key).ok_or_else(|| errno(9))?.record.data.clone())
    }
    fn read_record(&self, record: &WorkspaceInode, offset: u64, length: usize) -> anyhow::Result<Vec<u8>> {
        let length = length.min(record.data.size.saturating_sub(offset).min(usize::MAX as u64) as usize);
        if length == 0 { return Ok(Vec::new()); }
        if length > MAX_DIRTY_FILE { return Err(errno(22)); }
        if let Some(content) = record.content {
            return self.plane.as_ref().ok_or_else(|| errno(5))?.read_workspace_range(content, offset, length);
        }
        // Explicit legacy reads/migration, never fallback after a content-plane error.
        if record.data.size > MAX_LEGACY_SIZE { return Err(errno(27)); }
        let bytes = self.cas.read(&record.data.hash)?;
        if CasStore::hash(&bytes) != record.data.hash { return Err(errno(5)); }
        let start = usize::try_from(offset).map_err(|_| errno(27))?;
        let end = start.checked_add(length).ok_or_else(|| errno(27))?;
        Ok(bytes.get(start..end).ok_or_else(|| errno(5))?.to_vec())
    }
    fn read_open(&self, open: &OpenInode, offset: u64, length: usize) -> anyhow::Result<Vec<u8>> {
        let length = length.min(open.record.data.size.saturating_sub(offset).min(usize::MAX as u64) as usize);
        if length > MAX_DIRTY_FILE { return Err(errno(22)); }
        let mut result = vec![0; length];
        let base_length = length.min(open.base_limit.saturating_sub(offset).min(usize::MAX as u64) as usize);
        if base_length > 0 {
            let mut base = open.record.clone();
            base.data.size = open.base_limit;
            let bytes = self.read_record(&base, offset, base_length)?;
            if bytes.len() != base_length { return Err(errno(5)); }
            result[..base_length].copy_from_slice(&bytes);
        }
        let end = offset.checked_add(length as u64).ok_or_else(|| errno(27))?;
        for patch in &open.patches {
            let start = offset.max(patch.offset);
            let stop = end.min(patch.offset + patch.data.len() as u64);
            if start < stop {
                result[(start - offset) as usize..(stop - offset) as usize]
                    .copy_from_slice(&patch.data[(start - patch.offset) as usize..(stop - patch.offset) as usize]);
            }
        }
        Ok(result)
    }
    pub fn read_handle(&self, agent: &str, handle: u64, offset: u64, length: usize) -> anyhow::Result<Vec<u8>> {
        let state = self.lock()?;
        let h = Self::handle(&state, agent, handle)?;
        self.read_open(state.inodes.get(&h.key).ok_or_else(|| errno(9))?, offset, length)
    }
    pub fn read_file_range(&self, agent: &str, inode: u64, offset: u64, length: usize) -> anyhow::Result<Vec<u8>> {
        let state = self.lock()?;
        if let Some(open) = state.inodes.get(&(agent.to_string(), inode)) {
            if open.record.data.nlinks == 0 { return Err(errno(2)); }
            return self.read_open(open, offset, length);
        }
        let record = self.required(&state, agent, inode)?;
        if record.data.kind != FileKind::Regular { return Err(errno(21)); }
        self.read_record(&record, offset, length)
    }
    pub fn read_file(&self, agent: &str, inode: u64) -> anyhow::Result<Vec<u8>> {
        let data = self.lookup_inode(agent, inode)?.ok_or_else(|| errno(2))?;
        if data.kind != FileKind::Regular { return Err(errno(21)); }
        let size = data.size;
        if size > MAX_LEGACY_SIZE { return Err(errno(27)); }
        let mut bytes = Vec::with_capacity(size as usize);
        while bytes.len() < size as usize {
            let chunk = self.read_file_range(agent, inode, bytes.len() as u64,
                MAX_DIRTY_FILE.min(size as usize - bytes.len()))?;
            if chunk.is_empty() { return Err(errno(5)); }
            bytes.extend(chunk);
        }
        Ok(bytes)
    }
    fn write_state(&self, state: &mut State, agent: &str, inode: u64, offset: u64, bytes: &[u8]) -> anyhow::Result<usize> {
        let guard = self.meta.namespace_guard(agent)?;
        if bytes.is_empty() { return Ok(0); }
        let end = offset.checked_add(bytes.len() as u64).ok_or_else(|| errno(27))?;
        let open = state.inodes.get_mut(&(agent.to_string(), inode)).ok_or_else(|| errno(9))?;
        if let Some(dirty_guard) = &open.dirty_guard { OwnerRegistry::global().validate(dirty_guard)?; }
        if end > MAX_FILE_SIZE || (self.plane.is_none() && end > MAX_LEGACY_SIZE) { return Err(errno(27)); }
        if bytes.len() > MAX_DIRTY_FILE.saturating_sub(open.dirty_bytes)
            || bytes.len().saturating_add(PATCH_CHARGE) > MAX_DIRTY_GLOBAL.saturating_sub(state.dirty_bytes)
            || open.patches.len() >= MAX_PATCHES { return Err(errno(28)); }
        open.patches.push(WorkspacePatch { offset, data: bytes.to_vec() });
        open.dirty_bytes += bytes.len();
        state.dirty_bytes += bytes.len() + PATCH_CHARGE;
        open.record.data.size = open.record.data.size.max(end);
        open.record.data.mtime = now();
        open.record.data.ctime = open.record.data.mtime;
        open.dirty = true;
        open.dirty_guard = Some(guard);
        state.invalidate(agent, inode);
        Ok(bytes.len())
    }
    pub fn write_handle(&self, agent: &str, handle: u64, offset: u64, bytes: &[u8]) -> anyhow::Result<usize> {
        let mut state = self.lock()?;
        let h = Self::handle(&state, agent, handle)?;
        if !h.writable { return Err(errno(9)); }
        let inode = h.key.1;
        let offset = if h.append { state.inodes.get(&h.key).ok_or_else(|| errno(9))?.record.data.size } else { offset };
        self.write_state(&mut state, agent, inode, offset, bytes)
    }
    fn truncate_state(&self, state: &mut State, agent: &str, inode: u64, size: u64) -> anyhow::Result<()> {
        let guard = self.meta.namespace_guard(agent)?;
        if size > MAX_FILE_SIZE || (self.plane.is_none() && size > MAX_LEGACY_SIZE) { return Err(errno(27)); }
        let open = state.inodes.get_mut(&(agent.to_string(), inode)).ok_or_else(|| errno(9))?;
        if let Some(dirty_guard) = &open.dirty_guard { OwnerRegistry::global().validate(dirty_guard)?; }
        open.base_limit = open.base_limit.min(size);
        let old_patch_count = open.patches.len();
        for patch in &mut open.patches {
            let keep = size.saturating_sub(patch.offset).min(patch.data.len() as u64) as usize;
            let removed = patch.data.len() - keep;
            patch.data.truncate(keep);
            patch.data.shrink_to_fit();
            open.dirty_bytes -= removed;
            state.dirty_bytes -= removed;
        }
        open.patches.retain(|p| !p.data.is_empty());
        state.dirty_bytes -= (old_patch_count - open.patches.len()) * PATCH_CHARGE;
        open.patches.shrink_to_fit();
        open.record.data.size = size;
        open.record.data.mtime = now();
        open.record.data.ctime = open.record.data.mtime;
        open.dirty = true;
        open.dirty_guard = Some(guard);
        state.invalidate(agent, inode);
        Ok(())
    }
    fn flush(&self, state: &mut State, agent: &str, inode: u64, durable: bool) -> anyhow::Result<()> {
        let key = (agent.to_string(), inode);
        let open = state.inodes.get(&key).ok_or_else(|| errno(9))?;
        let guard = match &open.dirty_guard {
            Some(guard) => { OwnerRegistry::global().validate(guard)?; guard.clone() }
            None => self.meta.namespace_guard(agent)?,
        };
        let mut record = open.record.clone();
        if open.dirty {
            if self.plane.is_some() {
                let mut base = record.content;
                // Shrink then extension must not resurrect the old tail.
                if let Some(content) = base {
                    if open.base_limit < content.size {
                        base = Some(self.publish(agent, inode, &guard, base, open.base_limit, &[])?);
                    }
                } else if open.base_limit > 0 {
                    if open.base_limit > MAX_LEGACY_SIZE { return Err(errno(27)); }
                    let mut legacy = record.clone();
                    legacy.data.size = open.published_size;
                    let mut offset = 0;
                    while offset < open.base_limit {
                        let length = (open.base_limit - offset).min(MAX_DIRTY_FILE as u64) as usize;
                        let bytes = self.read_record(&legacy, offset, length)?;
                        base = Some(self.publish(agent, inode, &guard, base, offset + length as u64,
                            &[WorkspacePatch { offset, data: bytes }])?);
                        offset += length as u64;
                    }
                }
                let content = self.publish(agent, inode, &guard, base, record.data.size, &open.patches)?;
                record.content = Some(content);
                record.data.hash = content.sha256;
            } else {
                let mut bytes = Vec::with_capacity(record.data.size as usize);
                while bytes.len() < record.data.size as usize {
                    bytes.extend(self.read_open(open, bytes.len() as u64,
                        MAX_DIRTY_FILE.min(record.data.size as usize - bytes.len()))?);
                }
                OwnerRegistry::global().validate(&guard)?;
                record.data.hash = self.cas.store(&bytes)?.0;
            }
            record.generation = record.generation.checked_add(1).ok_or_else(|| errno(75))?;
        }
        if durable && record.content.is_none() {
            let hash = crate::cas::hex_encode(&record.data.hash);
            let directory = self.cas.cas_dir().join(&hash[..2]);
            std::fs::File::open(directory.join(&hash[2..]))?.sync_all()?;
            std::fs::File::open(&directory)?.sync_all()?;
            std::fs::File::open(self.cas.cas_dir())?.sync_all()?;
        }
        self.adoption(state, agent, &guard, &[(inode, record.clone())], &[], durable)?;
        let open = state.inodes.get_mut(&key).ok_or_else(|| errno(9))?;
        state.dirty_bytes -= open.dirty_bytes + open.patches.len() * PATCH_CHARGE;
        open.dirty_bytes = 0;
        open.patches.clear();
        open.patches.shrink_to_fit();
        open.base_limit = record.data.size;
        open.published_size = record.data.size;
        open.dirty = false;
        open.dirty_guard = None;
        Ok(())
    }
    fn drop_idle(&self, state: &mut State, agent: &str, inode: u64) {
        let key = (agent.to_string(), inode);
        if state.inodes.get(&key).is_some_and(|o| o.opens == 0 && !o.dirty) {
            state.inodes.remove(&key);
        }
    }
    pub fn sync_handle(&self, agent: &str, handle: u64) -> anyhow::Result<()> {
        let mut state = self.lock()?;
        let inode = Self::handle(&state, agent, handle)?.key.1;
        self.flush(&mut state, agent, inode, true)
    }
    pub fn release_handle(&self, agent: &str, handle: u64) -> anyhow::Result<()> {
        let mut state = self.lock()?;
        let inode = Self::handle(&state, agent, handle)?.key.1;
        // Failure leaves the handle and dirty state available for retry.
        self.flush(&mut state, agent, inode, false)?;
        state.handles.remove(&handle);
        state.inodes.get_mut(&(agent.to_string(), inode)).ok_or_else(|| errno(9))?.opens -= 1;
        self.drop_idle(&mut state, agent, inode);
        Ok(())
    }
    pub fn truncate_file(&self, agent: &str, inode: u64, size: u64) -> anyhow::Result<()> {
        let mut state = self.lock()?;
        if self.required(&state, agent, inode)?.data.mode & 0o222 == 0 { return Err(errno(13)); }
        self.open_state(&mut state, agent, inode)?;
        self.truncate_state(&mut state, agent, inode, size)?;
        self.flush(&mut state, agent, inode, false)?;
        self.drop_idle(&mut state, agent, inode);
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn set_file_attributes(&self, agent: &str, inode: u64, mode: Option<u32>,
        uid: Option<u32>, gid: Option<u32>, atime: Option<u64>, mtime: Option<u64>) -> anyhow::Result<()> {
        let mut state = self.lock()?;
        let guard = self.meta.namespace_guard(agent)?;
        let mut record = self.required(&state, agent, inode)?;
        if let Some(mode) = mode { record.data.mode = mode & 0o7777; }
        if let Some(uid) = uid { record.data.uid = uid; }
        if let Some(gid) = gid { record.data.gid = gid; }
        if let Some(atime) = atime { record.data.atime = atime; }
        if let Some(mtime) = mtime { record.data.mtime = mtime; }
        record.data.ctime = now();
        self.adoption(&mut state, agent, &guard, &[(inode, record)], &[], false)
    }
    pub fn sync_directory(&self, agent: &str, inode: u64) -> anyhow::Result<()> {
        let mut state = self.lock()?;
        let guard = self.meta.namespace_guard(agent)?;
        let record = self.parent(&state, agent, inode, false)?;
        // Immediate commit is a barrier for earlier transactions in this metadata DB.
        self.adoption(&mut state, agent, &guard, &[(inode, record)], &[], true)
    }

    /// Atomic O_CREAT lookup/create, without O_TRUNC semantics.
    pub fn create_file(&self, agent: &str, parent: u64, name: &str, mode: u32, exclusive: bool) -> anyhow::Result<u64> {
        self.create_file_owned(agent, parent, name, mode, exclusive, 0, 0)
    }

    /// Assign ownership inside initial namespace adoption, never chown an existing file.
    #[allow(clippy::too_many_arguments)]
    pub fn create_file_owned(&self, agent: &str, parent: u64, name: &str, mode: u32,
        exclusive: bool, uid: u32, gid: u32) -> anyhow::Result<u64> {
        validate_name(name)?;
        let mut state = self.lock()?;
        self.meta.validate_layer_write_authority(agent)?;
        self.ensure_root(agent)?;
        self.parent(&state, agent, parent, false)?;
        if let Some(inode) = self.dirent(agent, parent, name)? {
            if exclusive { return Err(errno(17)); }
            let record = self.required(&state, agent, inode)?;
            if record.data.kind != FileKind::Regular {
                return Err(errno(if record.data.kind == FileKind::Directory { 21 } else { 40 }));
            }
            return Ok(inode);
        }
        let mut data = InodeData::regular([0; 32], 0, mode & 0o7777);
        data.uid = uid;
        data.gid = gid;
        self.create(&mut state, agent, parent, name, WorkspaceInode::legacy(data), Some(&[]))
    }

    pub fn parent_inode(&self, agent: &str, inode: u64) -> anyhow::Result<u64> {
        let state = self.lock()?;
        self.parent(&state, agent, inode, false)?;
        if inode == 1 { return Ok(1); }
        for (parent, name) in self.meta.workspace_parent_candidates(agent, inode)? {
            if self.record(&state, agent, parent)?.is_some()
                && self.dirent(agent, parent, &name)? == Some(inode) {
                return Ok(parent);
            }
        }
        Err(errno(2))
    }

    /// Snapshot preparation flushes only bounded dirty state, not whole file images.
    /// The caller must quiesce writers through the subsequent metadata dump.
    pub fn sync_all_workspaces(&self) -> anyhow::Result<()> {
        let mut state = self.lock()?;
        self.sync_workspaces_locked(&mut state)
    }

    fn sync_workspaces_locked(&self, state: &mut State) -> anyhow::Result<()> {
        let guard = self.meta.namespace_guard(SHARED_BASE_LAYER_ID)?;
        let keys: Vec<_> = state.inodes.iter().filter(|(_, open)| open.dirty)
            .map(|(key, _)| key.clone()).collect();
        for (agent, inode) in keys { self.flush(state, &agent, inode, true)?; }
        self.meta.commit_namespace(SHARED_BASE_LAYER_ID, &guard, &[], &[], true)
    }

    /// Atomic manager-level snapshot cut: no namespace/handle mutation can enter
    /// between dirty publication and the metadata dump. No cross-redb atomicity.
    pub fn snapshot_metadata(&self) -> anyhow::Result<FsMetadataDump> {
        let mut state = self.lock()?;
        self.sync_workspaces_locked(&mut state)?;
        self.meta.dump_all_tables()
    }

    /// Fail closed rather than letting live FDs refer into a restored namespace.
    /// Content/snapshot roots must be restored by the caller before this adoption.
    pub fn restore_metadata(&self, dump: &FsMetadataDump) -> anyhow::Result<()> {
        let mut state = self.lock()?;
        if !state.handles.is_empty() || state.inodes.values().any(|open| open.dirty) {
            return Err(errno(16));
        }
        let guard = self.meta.namespace_guard(SHARED_BASE_LAYER_ID)?;
        let previous = self.meta.dump_all_tables()?;
        self.meta.restore_workspace_tables(dump, &guard)?;
        let next_handle = state.next_handle;
        *state = State { next_handle, ..State::default() };
        let hook = state.hook.clone();
        drop(state);
        // Stream existing snapshot vectors instead of growing a notification queue.
        // Duplicates are coalesced by the adapter's bounded observer queue.
        if let Some(hook) = hook {
            for (agent, inode, _) in previous.inodes.iter().chain(&dump.inodes) {
                if *inode != 0 { hook(agent, *inode); }
            }
        }
        Ok(())
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
