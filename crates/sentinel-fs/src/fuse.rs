//! FUSE filesystem handler — single mount for all agents.
//!
//! Mount point: `/cas-root/` with top-level directories per agent (`/cas-root/AGENT-01/`).
//! Path-based Agent-ID extraction: first path component maps to an agent.
//! Gated behind the `fuse-tests` feature (requires libfuse + kernel FUSE support).

#[cfg(feature = "fuse-tests")]
mod inner {
    use crate::cas::CasStore;
    use crate::layer::LayerManager;
    use crate::metadata::{FileKind, InodeData, MetadataStore};
    use fuser::{
        Config, Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo,
        LockOwner, MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
        Request,
    };
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::{Duration, UNIX_EPOCH};
    use tracing::warn;

    const TTL: Duration = Duration::from_secs(1);

    /// Virtual inode space: we map (agent_id, real_inode) to a global FUSE inode.
    /// FUSE inode 1 = root of the mount (contains agent dirs).
    /// Agent-specific inodes start at offset = agent_index * INODE_SPACE_PER_AGENT + 2.
    const INODE_SPACE_PER_AGENT: u64 = 1_000_000;

    /// FUSE handler for the CAS-backed agent filesystem.
    pub struct SentinelFuse {
        layer: LayerManager,
        /// Map of known agent IDs to their inode offset base.
        agents: Mutex<AgentRegistry>,
    }

    struct AgentRegistry {
        /// Agent ID -> offset base for inode mapping.
        map: HashMap<String, u64>,
        next_offset: u64,
    }

    impl AgentRegistry {
        fn new() -> Self {
            Self {
                map: HashMap::new(),
                next_offset: 2, // 1 = root
            }
        }

        fn get_or_insert(&mut self, agent_id: &str) -> u64 {
            if let Some(&base) = self.map.get(agent_id) {
                return base;
            }
            let base = self.next_offset;
            self.map.insert(agent_id.to_string(), base);
            self.next_offset += INODE_SPACE_PER_AGENT;
            base
        }

        fn find_agent(&self, fuse_inode: u64) -> Option<(&str, u64)> {
            for (agent_id, &base) in &self.map {
                if fuse_inode >= base && fuse_inode < base + INODE_SPACE_PER_AGENT {
                    let real_inode = fuse_inode - base + 1; // +1 because base maps to inode 1
                    return Some((agent_id, real_inode));
                }
            }
            None
        }
    }

    impl SentinelFuse {
        /// Create a new FUSE handler.
        pub fn new(layer: LayerManager) -> Self {
            Self {
                layer,
                agents: Mutex::new(AgentRegistry::new()),
            }
        }

        /// Mount the filesystem. Blocks until unmounted.
        pub fn mount(self, mountpoint: &Path) -> anyhow::Result<()> {
            let mut config = Config::default();
            config.mount_options = vec![
                MountOption::RW,
                MountOption::FSName("sentinel-fs".to_string()),
                MountOption::AutoUnmount,
                MountOption::CUSTOM("allow_other".to_string()),
            ];
            fuser::mount2(self, mountpoint, &config)
                .map_err(|e| anyhow::anyhow!("FUSE mount failed: {e}"))
        }

        fn inode_to_attr(data: &InodeData, ino: u64) -> FileAttr {
            let kind = match data.kind {
                FileKind::Regular => FileType::RegularFile,
                FileKind::Directory => FileType::Directory,
                FileKind::Symlink => FileType::Symlink,
            };
            FileAttr {
                ino: INodeNo(ino),
                size: data.size,
                blocks: data.size.div_ceil(512),
                atime: UNIX_EPOCH + Duration::from_secs(data.atime),
                mtime: UNIX_EPOCH + Duration::from_secs(data.mtime),
                ctime: UNIX_EPOCH + Duration::from_secs(data.ctime),
                crtime: UNIX_EPOCH,
                kind,
                perm: data.mode as u16,
                nlink: data.nlinks,
                uid: data.uid,
                gid: data.gid,
                rdev: 0,
                blksize: 4096,
                flags: 0,
            }
        }

        fn root_attr() -> FileAttr {
            FileAttr {
                ino: INodeNo(1),
                size: 0,
                blocks: 0,
                atime: UNIX_EPOCH,
                mtime: UNIX_EPOCH,
                ctime: UNIX_EPOCH,
                crtime: UNIX_EPOCH,
                kind: FileType::Directory,
                perm: 0o755,
                nlink: 2,
                uid: 0,
                gid: 0,
                rdev: 0,
                blksize: 4096,
                flags: 0,
            }
        }
    }

    impl Filesystem for SentinelFuse {
        fn getattr(
            &self,
            _req: &Request,
            ino: INodeNo,
            _fh: Option<FileHandle>,
            reply: ReplyAttr,
        ) {
            let ino_val: u64 = ino.into();
            if ino_val == 1 {
                reply.attr(&TTL, &Self::root_attr());
                return;
            }

            let agents = self.agents.lock().unwrap();
            if let Some((agent_id, real_inode)) = agents.find_agent(ino_val) {
                let agent_id = agent_id.to_string();
                drop(agents);
                match self.layer.lookup_inode(&agent_id, real_inode) {
                    Ok(Some(data)) => reply.attr(&TTL, &Self::inode_to_attr(&data, ino_val)),
                    Ok(None) => reply.error(Errno::ENOENT),
                    Err(e) => {
                        warn!("getattr error: {e}");
                        reply.error(Errno::EIO);
                    }
                }
            } else {
                reply.error(Errno::ENOENT);
            }
        }

        fn readdir(
            &self,
            _req: &Request,
            ino: INodeNo,
            _fh: FileHandle,
            offset: u64,
            mut reply: ReplyDirectory,
        ) {
            let ino_val: u64 = ino.into();

            if ino_val == 1 {
                // Root: list agent directories
                let agents = self.agents.lock().unwrap();
                let mut entries: Vec<(String, u64)> = agents
                    .map
                    .iter()
                    .map(|(name, &base)| (name.clone(), base))
                    .collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                drop(agents);

                if offset == 0 && reply.add(INodeNo(1), 1, FileType::Directory, ".") {
                    reply.ok();
                    return;
                }
                if offset <= 1 && reply.add(INodeNo(1), 2, FileType::Directory, "..") {
                    reply.ok();
                    return;
                }

                for (i, (name, base_ino)) in entries.iter().enumerate() {
                    let entry_offset = (i as u64) + 2;
                    if entry_offset < offset {
                        continue;
                    }
                    if reply.add(
                        INodeNo(*base_ino),
                        entry_offset + 1,
                        FileType::Directory,
                        name,
                    ) {
                        break;
                    }
                }
                reply.ok();
                return;
            }

            let agents = self.agents.lock().unwrap();
            if let Some((agent_id, real_inode)) = agents.find_agent(ino_val) {
                let agent_id = agent_id.to_string();
                let base_offset = agents.map.get(&agent_id).copied().unwrap_or(2);
                drop(agents);

                match self.layer.readdir(&agent_id, real_inode) {
                    Ok(entries) => {
                        if offset == 0
                            && reply.add(INodeNo(ino_val), 1, FileType::Directory, ".")
                        {
                            reply.ok();
                            return;
                        }
                        if offset <= 1
                            && reply.add(INodeNo(ino_val), 2, FileType::Directory, "..")
                        {
                            reply.ok();
                            return;
                        }
                        for (i, (name, child_inode, kind)) in entries.iter().enumerate() {
                            let entry_offset = (i as u64) + 2;
                            if entry_offset < offset {
                                continue;
                            }
                            let fuse_ino = base_offset + child_inode - 1;
                            let ft = match kind {
                                FileKind::Regular => FileType::RegularFile,
                                FileKind::Directory => FileType::Directory,
                                FileKind::Symlink => FileType::Symlink,
                            };
                            if reply.add(INodeNo(fuse_ino), entry_offset + 1, ft, name) {
                                break;
                            }
                        }
                        reply.ok();
                    }
                    Err(e) => {
                        warn!("readdir error: {e}");
                        reply.error(Errno::EIO);
                    }
                }
            } else {
                reply.error(Errno::ENOENT);
            }
        }

        fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
            let parent_val: u64 = parent.into();
            let name_str = match name.to_str() {
                Some(s) => s,
                None => {
                    reply.error(Errno::EINVAL);
                    return;
                }
            };

            if parent_val == 1 {
                // Looking up an agent directory
                let mut agents = self.agents.lock().unwrap();
                if name_str.starts_with("AGENT-") {
                    let base = agents.get_or_insert(name_str);
                    // Ensure agent root exists
                    drop(agents);
                    if let Err(e) = self.layer.ensure_agent_root(name_str) {
                        warn!("ensure_agent_root error: {e}");
                        reply.error(Errno::EIO);
                        return;
                    }
                    let attr = FileAttr {
                        ino: INodeNo(base),
                        size: 0,
                        blocks: 0,
                        atime: UNIX_EPOCH,
                        mtime: UNIX_EPOCH,
                        ctime: UNIX_EPOCH,
                        crtime: UNIX_EPOCH,
                        kind: FileType::Directory,
                        perm: 0o755,
                        nlink: 2,
                        uid: 0,
                        gid: 0,
                        rdev: 0,
                        blksize: 4096,
                        flags: 0,
                    };
                    reply.entry(&TTL, &attr, Generation(0));
                } else {
                    reply.error(Errno::ENOENT);
                }
                return;
            }

            let agents = self.agents.lock().unwrap();
            if let Some((agent_id, real_parent)) = agents.find_agent(parent_val) {
                let agent_id = agent_id.to_string();
                let base_offset = agents.map.get(&agent_id).copied().unwrap_or(2);
                drop(agents);

                match self.layer.lookup_dirent(&agent_id, real_parent, name_str) {
                    Ok(Some(child_inode)) => {
                        let fuse_ino = base_offset + child_inode - 1;
                        match self.layer.lookup_inode(&agent_id, child_inode) {
                            Ok(Some(data)) => {
                                reply.entry(
                                    &TTL,
                                    &Self::inode_to_attr(&data, fuse_ino),
                                    Generation(0),
                                );
                            }
                            Ok(None) => reply.error(Errno::ENOENT),
                            Err(e) => {
                                warn!("lookup inode error: {e}");
                                reply.error(Errno::EIO);
                            }
                        }
                    }
                    Ok(None) => reply.error(Errno::ENOENT),
                    Err(e) => {
                        warn!("lookup dirent error: {e}");
                        reply.error(Errno::EIO);
                    }
                }
            } else {
                reply.error(Errno::ENOENT);
            }
        }

        fn read(
            &self,
            _req: &Request,
            ino: INodeNo,
            _fh: FileHandle,
            offset: u64,
            size: u32,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            reply: ReplyData,
        ) {
            let ino_val: u64 = ino.into();
            let agents = self.agents.lock().unwrap();
            if let Some((agent_id, real_inode)) = agents.find_agent(ino_val) {
                let agent_id = agent_id.to_string();
                drop(agents);

                match self.layer.read_file(&agent_id, real_inode) {
                    Ok(data) => {
                        let start = offset as usize;
                        if start >= data.len() {
                            reply.data(&[]);
                        } else {
                            let end = (start + size as usize).min(data.len());
                            reply.data(&data[start..end]);
                        }
                    }
                    Err(e) => {
                        warn!("read error: {e}");
                        reply.error(Errno::EIO);
                    }
                }
            } else {
                reply.error(Errno::ENOENT);
            }
        }
    }

    /// Start the FUSE daemon on the given mountpoint with the given data directory.
    pub fn start_fuse(data_dir: &Path, mountpoint: &Path) -> anyhow::Result<()> {
        let cas = CasStore::open(data_dir)?;
        let meta_path = data_dir.join("metadata.redb");
        let meta = MetadataStore::open(&meta_path)?;
        let layer = LayerManager::new(cas, meta);
        layer.init_base_root()?;

        let fuse = SentinelFuse::new(layer);
        fuse.mount(mountpoint)
    }
}

#[cfg(feature = "fuse-tests")]
pub use inner::*;
