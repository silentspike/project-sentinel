//! POSIX adapter for private agent namespaces backed by shared immutable content.

#[cfg(feature = "fuse-tests")]
mod inner {
    use crate::artifact::ArtifactPlane;
    use crate::cas::CasStore;
    use crate::layer::LayerManager;
    use crate::metadata::{FileKind, InodeData, MetadataStore};
    use fuser::{
        BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags,
        Generation, INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr,
        ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs,
        ReplyWrite, Request, SessionACL, TimeOrNow, WriteFlags,
    };
    use std::collections::{HashMap, HashSet};
    use std::ffi::OsStr;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    // Direct tools can mutate the same namespace outside the kernel inode cache.
    const TTL: Duration = Duration::ZERO;
    const MAX_MAPPED_INODES: usize = 262_144;

    #[derive(Default)]
    struct AgentRegistry {
        forward: HashMap<(String, u64), u64>,
        reverse: HashMap<u64, (String, u64)>,
        roots: HashMap<String, u64>,
    }

    impl AgentRegistry {
        fn map(&mut self, agent: &str, inode: u64) -> anyhow::Result<u64> {
            let key = (agent.to_owned(), inode);
            if let Some(mapped) = self.forward.get(&key) {
                return Ok(*mapped);
            }
            if self.forward.len() >= MAX_MAPPED_INODES {
                return Err(std::io::Error::from_raw_os_error(libc::ENOSPC).into());
            }
            let mapped = self.forward.len() as u64 + 2;
            self.forward.insert(key.clone(), mapped);
            self.reverse.insert(mapped, key);
            if inode == 1 {
                self.roots.insert(agent.to_owned(), mapped);
            }
            Ok(mapped)
        }
    }

    pub struct SentinelFuse {
        layer: Arc<LayerManager>,
        agents: Arc<Mutex<AgentRegistry>>,
    }

    #[derive(Default)]
    struct Invalidations {
        pending: Mutex<HashSet<u64>>,
        ready: Condvar,
        stopped: AtomicBool,
    }

    fn io_error(code: i32) -> anyhow::Error {
        std::io::Error::from_raw_os_error(code).into()
    }

    fn errno(error: anyhow::Error) -> Errno {
        for cause in error.chain() {
            if let Some(code) = cause
                .downcast_ref::<std::io::Error>()
                .and_then(|e| e.raw_os_error())
            {
                return Errno::from_i32(code);
            }
        }
        tracing::warn!("agent filesystem operation failed: {error:#}");
        Errno::EIO
    }

    fn name(value: &OsStr) -> anyhow::Result<&str> {
        let value = value.to_str().ok_or_else(|| io_error(libc::EINVAL))?;
        if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\0']) {
            return Err(io_error(libc::EINVAL));
        }
        if value.len() > 255 {
            return Err(io_error(libc::ENAMETOOLONG));
        }
        Ok(value)
    }

    fn timestamp(value: TimeOrNow) -> u64 {
        let time = match value {
            TimeOrNow::SpecificTime(time) => time,
            TimeOrNow::Now => SystemTime::now(),
        };
        time.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    impl SentinelFuse {
        pub fn new(layer: Arc<LayerManager>) -> Self {
            Self {
                layer,
                agents: Arc::new(Mutex::new(AgentRegistry::default())),
            }
        }

        pub fn mount(self, mountpoint: &Path) -> anyhow::Result<()> {
            let mut config = Config::default();
            config.acl = SessionACL::All;
            config.mount_options = vec![
                MountOption::RW,
                MountOption::DefaultPermissions,
                MountOption::FSName("sentinel-fs".to_owned()),
                MountOption::AutoUnmount,
            ];
            let layer = Arc::clone(&self.layer);
            let agents = Arc::clone(&self.agents);
            let invalidations = Arc::new(Invalidations::default());
            let session = fuser::Session::new(self, mountpoint, &config)?;
            let notifier = session.notifier();
            let pending = Arc::clone(&invalidations);
            layer.set_invalidation_hook(Some(Arc::new(move |agent, inode| {
                let mapped = agents
                    .lock()
                    .ok()
                    .and_then(|registry| registry.forward.get(&(agent.to_owned(), inode)).copied());
                if let Some(mapped) = mapped {
                    if let Ok(mut queue) = pending.pending.lock() {
                        // Coalesce notifications: at most MAX_MAPPED_INODES pending keys.
                        queue.insert(mapped);
                        pending.ready.notify_one();
                    }
                }
            })))?;
            let pending = Arc::clone(&invalidations);
            let worker = match std::thread::Builder::new()
                .name("fs-cache-invalidation".into())
                .spawn(move || {
                    loop {
                        let Ok(mut queue) = pending.pending.lock() else {
                            return;
                        };
                        while queue.is_empty() && !pending.stopped.load(Ordering::Acquire) {
                            let Ok(next) = pending.ready.wait(queue) else {
                                return;
                            };
                            queue = next;
                        }
                        if pending.stopped.load(Ordering::Acquire) {
                            return;
                        }
                        let inodes: Vec<_> = queue.drain().collect();
                        drop(queue);
                        // Kernel invalidation can wait for a FUSE request. Never issue it
                        // inline from a mutation callback while that request is pending.
                        for inode in inodes {
                            if let Err(error) = notifier.inval_inode(INodeNo(inode), 0, 0) {
                                if error.raw_os_error() != Some(libc::ENOENT) {
                                    tracing::warn!(inode, %error, "FUSE cache invalidation failed");
                                }
                            }
                        }
                    }
                }) {
                Ok(worker) => worker,
                Err(error) => {
                    let _ = layer.set_invalidation_hook(None);
                    return Err(error.into());
                }
            };
            let result = session.spawn().and_then(|session| session.join());
            let clear_hook = layer.set_invalidation_hook(None);
            {
                let _queue = invalidations
                    .pending
                    .lock()
                    .map_err(|_| io_error(libc::EIO))?;
                invalidations.stopped.store(true, Ordering::Release);
                invalidations.ready.notify_all();
            }
            worker.join().map_err(|_| io_error(libc::EIO))?;
            clear_hook?;
            result.map_err(Into::into)
        }

        fn resolve(&self, inode: INodeNo) -> anyhow::Result<(String, u64)> {
            self.agents
                .lock()
                .map_err(|_| io_error(libc::EIO))?
                .reverse
                .get(&u64::from(inode))
                .cloned()
                .ok_or_else(|| io_error(libc::ENOENT))
        }

        fn map(&self, agent: &str, inode: u64) -> anyhow::Result<u64> {
            self.agents
                .lock()
                .map_err(|_| io_error(libc::EIO))?
                .map(agent, inode)
        }

        fn kind(kind: FileKind) -> FileType {
            match kind {
                FileKind::Regular => FileType::RegularFile,
                FileKind::Directory => FileType::Directory,
                FileKind::Symlink => FileType::Symlink,
            }
        }

        fn attr(data: &InodeData, inode: u64) -> FileAttr {
            FileAttr {
                ino: INodeNo(inode),
                size: data.size,
                blocks: data.size.div_ceil(512),
                atime: UNIX_EPOCH + Duration::from_secs(data.atime),
                mtime: UNIX_EPOCH + Duration::from_secs(data.mtime),
                ctime: UNIX_EPOCH + Duration::from_secs(data.ctime),
                crtime: UNIX_EPOCH,
                kind: Self::kind(data.kind),
                perm: (data.mode & 0o7777) as u16,
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

        fn inode_attr(&self, agent: &str, inode: u64) -> anyhow::Result<FileAttr> {
            let data = self
                .layer
                .lookup_inode(agent, inode)?
                .ok_or_else(|| io_error(libc::ENOENT))?;
            Ok(Self::attr(&data, self.map(agent, inode)?))
        }

        fn owned_entry(&self, req: &Request, agent: &str, inode: u64) -> anyhow::Result<FileAttr> {
            self.layer.set_file_attributes(
                agent,
                inode,
                None,
                Some(req.uid()),
                Some(req.gid()),
                None,
                None,
            )?;
            self.inode_attr(agent, inode)
        }

        fn directory_entries(
            &self,
            inode: INodeNo,
            handle: FileHandle,
        ) -> anyhow::Result<Vec<(u64, FileType, String)>> {
            let current = u64::from(inode);
            if current == 1 {
                if handle.0 != 0 {
                    return Err(io_error(libc::EBADF));
                }
                let agents = self.agents.lock().map_err(|_| io_error(libc::EIO))?;
                let mut children: Vec<_> = agents
                    .roots
                    .iter()
                    .map(|(agent, ino)| (*ino, FileType::Directory, agent.clone()))
                    .collect();
                children.sort_by(|a, b| a.2.cmp(&b.2));
                let mut entries = vec![
                    (1, FileType::Directory, ".".into()),
                    (1, FileType::Directory, "..".into()),
                ];
                entries.extend(children);
                return Ok(entries);
            }
            let (agent, real) = self.resolve(inode)?;
            let parent = if real == 1 {
                1
            } else {
                self.map(&agent, self.layer.parent_inode(&agent, real)?)?
            };
            let mut entries = vec![
                (current, FileType::Directory, ".".into()),
                (parent, FileType::Directory, "..".into()),
            ];
            let mut children = self.layer.readdir_handle(&agent, real, handle.0)?;
            children.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, child, kind) in children {
                entries.push((self.map(&agent, child)?, Self::kind(kind), name));
            }
            Ok(entries)
        }
    }

    impl Filesystem for SentinelFuse {
        fn lookup(&self, _req: &Request, parent: INodeNo, leaf: &OsStr, reply: ReplyEntry) {
            let result = (|| {
                let leaf = name(leaf)?;
                if u64::from(parent) == 1 {
                    let suffix = leaf
                        .strip_prefix("AGENT-")
                        .ok_or_else(|| io_error(libc::ENOENT))?;
                    if suffix.is_empty()
                        || suffix.len() > 10
                        || !suffix.bytes().all(|c| c.is_ascii_digit())
                    {
                        return Err(io_error(libc::ENOENT));
                    }
                    self.layer.ensure_agent_root(leaf)?;
                    return self.inode_attr(leaf, 1);
                }
                let (agent, parent) = self.resolve(parent)?;
                let child = self
                    .layer
                    .lookup_dirent(&agent, parent, leaf)?
                    .ok_or_else(|| io_error(libc::ENOENT))?;
                self.inode_attr(&agent, child)
            })();
            match result {
                Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn getattr(
            &self,
            _req: &Request,
            inode: INodeNo,
            handle: Option<FileHandle>,
            reply: ReplyAttr,
        ) {
            let result = (|| {
                if u64::from(inode) == 1 {
                    return Ok(Self::root_attr());
                }
                let (agent, real) = self.resolve(inode)?;
                match handle {
                    Some(handle) => Ok(Self::attr(
                        &self.layer.getattr_handle(&agent, handle.0)?,
                        u64::from(inode),
                    )),
                    None => self.inode_attr(&agent, real),
                }
            })();
            match result {
                Ok(attr) => reply.attr(&TTL, &attr),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn setattr(
            &self,
            _req: &Request,
            inode: INodeNo,
            mode: Option<u32>,
            uid: Option<u32>,
            gid: Option<u32>,
            size: Option<u64>,
            atime: Option<TimeOrNow>,
            mtime: Option<TimeOrNow>,
            _ctime: Option<SystemTime>,
            handle: Option<FileHandle>,
            _crtime: Option<SystemTime>,
            _chgtime: Option<SystemTime>,
            _bkuptime: Option<SystemTime>,
            flags: Option<BsdFileFlags>,
            reply: ReplyAttr,
        ) {
            let result = (|| {
                if flags.is_some_and(|f| !f.is_empty()) {
                    return Err(io_error(libc::EOPNOTSUPP));
                }
                let (agent, real) = self.resolve(inode)?;
                if let Some(handle) = handle {
                    self.layer.set_handle_attributes(
                        &agent,
                        real,
                        handle.0,
                        size,
                        mode,
                        uid,
                        gid,
                        atime.map(timestamp),
                        mtime.map(timestamp),
                    )?;
                    Ok(Self::attr(
                        &self.layer.getattr_handle(&agent, handle.0)?,
                        u64::from(inode),
                    ))
                } else {
                    if let Some(size) = size {
                        self.layer.truncate_file(&agent, real, size)?;
                    }
                    self.layer.set_file_attributes(
                        &agent,
                        real,
                        mode,
                        uid,
                        gid,
                        atime.map(timestamp),
                        mtime.map(timestamp),
                    )?;
                    self.inode_attr(&agent, real)
                }
            })();
            match result {
                Ok(attr) => reply.attr(&TTL, &attr),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn readdir(
            &self,
            _req: &Request,
            inode: INodeNo,
            handle: FileHandle,
            offset: u64,
            mut reply: ReplyDirectory,
        ) {
            match self.directory_entries(inode, handle) {
                Ok(entries) => {
                    for (index, (inode, kind, name)) in entries.iter().enumerate() {
                        if (index as u64) < offset {
                            continue;
                        }
                        if reply.add(INodeNo(*inode), index as u64 + 1, *kind, name) {
                            break;
                        }
                    }
                    reply.ok();
                }
                Err(e) => reply.error(errno(e)),
            }
        }

        fn opendir(&self, _req: &Request, inode: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
            let result = if u64::from(inode) == 1 {
                Ok(0)
            } else {
                self.resolve(inode)
                    .and_then(|(agent, inode)| self.layer.open_directory(&agent, inode))
            };
            match result {
                Ok(handle) => reply.opened(FileHandle(handle), FopenFlags::empty()),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn releasedir(
            &self,
            _req: &Request,
            inode: INodeNo,
            handle: FileHandle,
            _flags: OpenFlags,
            reply: ReplyEmpty,
        ) {
            let result = if u64::from(inode) == 1 && handle.0 == 0 {
                Ok(())
            } else {
                self.resolve(inode).and_then(|(agent, inode)| {
                    self.layer.release_directory(&agent, inode, handle.0)
                })
            };
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn mkdir(
            &self,
            req: &Request,
            parent: INodeNo,
            leaf: &OsStr,
            mode: u32,
            umask: u32,
            reply: ReplyEntry,
        ) {
            let result = (|| {
                let (agent, parent) = self.resolve(parent)?;
                let inode = self
                    .layer
                    .mkdir(&agent, parent, name(leaf)?, mode & !umask)?;
                self.owned_entry(req, &agent, inode)
            })();
            match result {
                Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn mknod(
            &self,
            req: &Request,
            parent: INodeNo,
            leaf: &OsStr,
            mode: u32,
            umask: u32,
            _rdev: u32,
            reply: ReplyEntry,
        ) {
            let result = (|| {
                if mode & libc::S_IFMT != libc::S_IFREG {
                    return Err(io_error(libc::EOPNOTSUPP));
                }
                let (agent, parent) = self.resolve(parent)?;
                let inode = self.layer.create_file(
                    &agent,
                    parent,
                    name(leaf)?,
                    mode & !umask & 0o7777,
                    true,
                )?;
                self.owned_entry(req, &agent, inode)
            })();
            match result {
                Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn create(
            &self,
            req: &Request,
            parent: INodeNo,
            leaf: &OsStr,
            mode: u32,
            umask: u32,
            flags: i32,
            reply: ReplyCreate,
        ) {
            let result = (|| {
                let (agent, parent) = self.resolve(parent)?;
                let inode = self.layer.create_file_owned(
                    &agent,
                    parent,
                    name(leaf)?,
                    mode & !umask,
                    flags & libc::O_EXCL != 0,
                    req.uid(),
                    req.gid(),
                )?;
                let attr = self.inode_attr(&agent, inode)?;
                let handle = self.layer.open_file(
                    &agent,
                    inode,
                    flags & libc::O_ACCMODE != libc::O_RDONLY,
                    flags & libc::O_APPEND != 0,
                    flags & libc::O_TRUNC != 0,
                )?;
                Ok((attr, handle))
            })();
            match result {
                Ok((attr, handle)) => reply.created(
                    &TTL,
                    &attr,
                    Generation(0),
                    FileHandle(handle),
                    FopenFlags::empty(),
                ),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn open(&self, _req: &Request, inode: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
            let result = (|| {
                let (agent, inode) = self.resolve(inode)?;
                self.layer.open_file(
                    &agent,
                    inode,
                    flags.0 & libc::O_ACCMODE != libc::O_RDONLY,
                    flags.0 & libc::O_APPEND != 0,
                    flags.0 & libc::O_TRUNC != 0,
                )
            })();
            match result {
                Ok(handle) => reply.opened(FileHandle(handle), FopenFlags::empty()),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn read(
            &self,
            _req: &Request,
            inode: INodeNo,
            handle: FileHandle,
            offset: u64,
            size: u32,
            _flags: OpenFlags,
            _owner: Option<LockOwner>,
            reply: ReplyData,
        ) {
            let result = (|| {
                let (agent, inode) = self.resolve(inode)?;
                if handle.0 == 0 {
                    self.layer
                        .read_file_range(&agent, inode, offset, size as usize)
                } else {
                    self.layer
                        .read_handle(&agent, handle.0, offset, size as usize)
                }
            })();
            match result {
                Ok(data) => reply.data(&data),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn write(
            &self,
            _req: &Request,
            inode: INodeNo,
            handle: FileHandle,
            offset: u64,
            data: &[u8],
            _write_flags: WriteFlags,
            flags: OpenFlags,
            _owner: Option<LockOwner>,
            reply: ReplyWrite,
        ) {
            let result = (|| {
                let (agent, _) = self.resolve(inode)?;
                let written = self.layer.write_handle(&agent, handle.0, offset, data)?;
                if flags.0 & (libc::O_SYNC | libc::O_DSYNC) != 0 {
                    self.layer.sync_handle(&agent, handle.0)?;
                }
                u32::try_from(written).map_err(|_| io_error(libc::EOVERFLOW))
            })();
            match result {
                Ok(written) => reply.written(written),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn flush(
            &self,
            _req: &Request,
            inode: INodeNo,
            handle: FileHandle,
            _owner: LockOwner,
            reply: ReplyEmpty,
        ) {
            let result = self
                .resolve(inode)
                .and_then(|(agent, _)| self.layer.sync_handle(&agent, handle.0));
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn fsync(
            &self,
            _req: &Request,
            inode: INodeNo,
            handle: FileHandle,
            _datasync: bool,
            reply: ReplyEmpty,
        ) {
            let result = self
                .resolve(inode)
                .and_then(|(agent, _)| self.layer.sync_handle(&agent, handle.0));
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn release(
            &self,
            _req: &Request,
            inode: INodeNo,
            handle: FileHandle,
            _flags: OpenFlags,
            _owner: Option<LockOwner>,
            _flush: bool,
            reply: ReplyEmpty,
        ) {
            let result = self
                .resolve(inode)
                .and_then(|(agent, _)| self.layer.release_handle(&agent, handle.0));
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn fsyncdir(
            &self,
            _req: &Request,
            inode: INodeNo,
            handle: FileHandle,
            _datasync: bool,
            reply: ReplyEmpty,
        ) {
            let result = self.resolve(inode).and_then(|(agent, inode)| {
                self.layer.sync_directory_handle(&agent, inode, handle.0)
            });
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn unlink(&self, _req: &Request, parent: INodeNo, leaf: &OsStr, reply: ReplyEmpty) {
            let result = (|| {
                let (agent, parent) = self.resolve(parent)?;
                let leaf = name(leaf)?;
                let inode = self
                    .layer
                    .lookup_dirent(&agent, parent, leaf)?
                    .ok_or_else(|| io_error(libc::ENOENT))?;
                self.layer.unlink(&agent, parent, leaf, inode)
            })();
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn rmdir(&self, _req: &Request, parent: INodeNo, leaf: &OsStr, reply: ReplyEmpty) {
            let result = (|| {
                let (agent, parent) = self.resolve(parent)?;
                self.layer.rmdir(&agent, parent, name(leaf)?)
            })();
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn rename(
            &self,
            _req: &Request,
            parent: INodeNo,
            leaf: &OsStr,
            new_parent: INodeNo,
            new_leaf: &OsStr,
            flags: RenameFlags,
            reply: ReplyEmpty,
        ) {
            let result = (|| {
                let (agent, parent) = self.resolve(parent)?;
                let (other, new_parent) = self.resolve(new_parent)?;
                if agent != other {
                    return Err(io_error(libc::EXDEV));
                }
                self.layer.rename(
                    &agent,
                    parent,
                    name(leaf)?,
                    new_parent,
                    name(new_leaf)?,
                    flags.bits(),
                )
            })();
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn symlink(
            &self,
            req: &Request,
            parent: INodeNo,
            leaf: &OsStr,
            target: &Path,
            reply: ReplyEntry,
        ) {
            let result = (|| {
                let (agent, parent) = self.resolve(parent)?;
                let target = target.to_str().ok_or_else(|| io_error(libc::EINVAL))?;
                let inode = self.layer.symlink(&agent, parent, name(leaf)?, target)?;
                self.owned_entry(req, &agent, inode)
            })();
            match result {
                Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn readlink(&self, _req: &Request, inode: INodeNo, reply: ReplyData) {
            let result = (|| {
                let (agent, inode) = self.resolve(inode)?;
                let data = self
                    .layer
                    .lookup_inode(&agent, inode)?
                    .ok_or_else(|| io_error(libc::ENOENT))?;
                if data.kind != FileKind::Symlink {
                    return Err(io_error(libc::EINVAL));
                }
                Ok(data.symlink_target)
            })();
            match result {
                Ok(target) => reply.data(target.as_bytes()),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn link(
            &self,
            _req: &Request,
            inode: INodeNo,
            parent: INodeNo,
            leaf: &OsStr,
            reply: ReplyEntry,
        ) {
            let result = (|| {
                let (agent, inode) = self.resolve(inode)?;
                let (other, parent) = self.resolve(parent)?;
                if agent != other {
                    return Err(io_error(libc::EXDEV));
                }
                let inode = self.layer.link(&agent, inode, parent, name(leaf)?)?;
                self.inode_attr(&agent, inode)
            })();
            match result {
                Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
                Err(e) => reply.error(errno(e)),
            }
        }

        fn statfs(&self, _req: &Request, _inode: INodeNo, reply: ReplyStatfs) {
            match rustix::fs::statvfs(self.layer.cas().cas_dir()) {
                Ok(stats) => reply.statfs(
                    stats.f_blocks,
                    stats.f_bfree,
                    stats.f_bavail,
                    stats.f_files,
                    stats.f_ffree,
                    stats.f_bsize as u32,
                    255,
                    stats.f_frsize as u32,
                ),
                Err(e) => reply.error(Errno::from_i32(e.raw_os_error())),
            }
        }
    }

    pub fn start_fuse(data_dir: &Path, mountpoint: &Path) -> anyhow::Result<()> {
        let cas = CasStore::open(data_dir)?;
        let meta = MetadataStore::open(data_dir.join("metadata.redb"))?;
        let plane = Arc::new(ArtifactPlane::open(data_dir.join("home.redb"))?);
        let layer = LayerManager::with_artifact_plane(cas, meta, plane);
        layer.init_base_root()?;
        start_fuse_layer(Arc::new(layer), mountpoint)
    }

    pub fn start_fuse_layer(layer: Arc<LayerManager>, mountpoint: &Path) -> anyhow::Result<()> {
        SentinelFuse::new(layer).mount(mountpoint)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn inode_mapping_is_exact_for_large_ids_and_private_agents() {
            let mut registry = AgentRegistry::default();
            let first = registry.map("AGENT-01", u64::MAX).unwrap();
            let second = registry.map("AGENT-02", u64::MAX).unwrap();
            let root = registry.map("AGENT-01", 1).unwrap();
            assert_ne!(first, second);
            assert_ne!(first, root);
            assert_eq!(registry.map("AGENT-01", u64::MAX).unwrap(), first);
            assert_eq!(registry.reverse[&second], ("AGENT-02".to_owned(), u64::MAX));
            assert_eq!(registry.roots["AGENT-01"], root);
        }

        #[test]
        fn inode_mapping_exhaustion_does_not_alias_an_existing_inode() {
            let mut registry = AgentRegistry::default();
            for inode in 0..MAX_MAPPED_INODES as u64 {
                registry.map("AGENT-01", inode).unwrap();
            }
            assert_eq!(
                errno(registry.map("AGENT-01", u64::MAX).unwrap_err()).code(),
                Errno::ENOSPC.code()
            );
            assert_eq!(registry.map("AGENT-01", 0).unwrap(), 2);
        }

        #[test]
        fn leaf_names_reject_namespace_escape_and_preserve_valid_names() {
            for leaf in ["", ".", "..", "a/b", "a\0b"] {
                assert!(name(OsStr::new(leaf)).is_err());
            }
            assert_eq!(name(OsStr::new("package.json")).unwrap(), "package.json");
            assert_eq!(
                errno(name(OsStr::new(&"x".repeat(256))).unwrap_err()).code(),
                Errno::ENAMETOOLONG.code()
            );
        }
    }
}

#[cfg(feature = "fuse-tests")]
pub use inner::*;
